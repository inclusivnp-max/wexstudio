//! Three-way preference review. Disk bytes are checked again under the writer lock.
use super::*;
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub marker: StoredMarker,
}
#[derive(Clone, Debug, Serialize)]
pub struct Difference {
    pub id: u64,
    pub base: Option<Entry>,
    pub local: Option<Entry>,
    pub disk: Option<Entry>,
    pub conflict: bool,
}
impl Difference {
    /// Outer None means an unresolved conflict; inner None is an explicit deletion.
    pub fn selected(&self, mode: Mode, side: Option<Side>) -> Option<Option<&Entry>> {
        if mode == Mode::Local {
            return Some(self.local.as_ref());
        }
        if mode == Mode::Disk {
            return Some(self.disk.as_ref());
        }
        if self.local == self.disk || self.disk == self.base {
            return Some(self.local.as_ref());
        }
        if self.local == self.base {
            return Some(self.disk.as_ref());
        }
        side.map(|s| match s {
            Side::Local => self.local.as_ref(),
            Side::Disk => self.disk.as_ref(),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
pub enum Mode {
    #[default]
    Merge,
    Local,
    Disk,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Side {
    Local,
    Disk,
}
#[derive(Clone, Debug, Serialize)]
pub struct Review {
    pub id: u64,
    pub differences: Vec<Difference>,
    pub local_count: usize,
    pub disk_count: usize,
    pub conflicts: usize,
    pub merge_unconflicted_count: usize,
    pub recovery: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub id: u64,
    pub error: Option<String>,
}
pub(super) struct Prepared {
    pub(super) view: Arc<Review>,
    local_guard: Preferences,
    base: Preferences,
    local: Preferences,
    disk: Preferences,
    expected: Option<blake3::Hash>,
    signature: Signature,
    recovery: Option<(String, blake3::Hash)>,
}
fn entries(p: &Preferences) -> BTreeMap<u64, Entry> {
    p.files
        .iter()
        .flat_map(|(p, ms)| {
            ms.iter().map(move |m| {
                (
                    m.id,
                    Entry {
                        path: p.clone(),
                        marker: m.clone(),
                    },
                )
            })
        })
        .collect()
}
pub(super) fn changed_count(base: &Preferences, local: &Preferences) -> usize {
    let b = entries(base);
    let l = entries(local);
    b.keys()
        .chain(l.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|id| b.get(id) != l.get(id))
        .count()
}
fn differences(base: &Preferences, local: &Preferences, disk: &Preferences) -> Vec<Difference> {
    let b = entries(base);
    let l = entries(local);
    let d = entries(disk);
    b.keys()
        .chain(l.keys())
        .chain(d.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|id| {
            let (base, local, disk) = (
                b.get(&id).cloned(),
                l.get(&id).cloned(),
                d.get(&id).cloned(),
            );
            (base != local || base != disk).then(|| Difference {
                id,
                conflict: local != disk && local != base && disk != base,
                base,
                local,
                disk,
            })
        })
        .collect()
}
fn merge(p: &Prepared, mode: Mode, choices: &BTreeMap<u64, Side>) -> Result<Preferences> {
    if mode == Mode::Local {
        return Ok(p.local.clone());
    }
    if mode == Mode::Disk {
        return Ok(p.disk.clone());
    }
    let mut chosen = entries(&p.base);
    let conflicts = p
        .view
        .differences
        .iter()
        .filter(|r| r.conflict)
        .map(|r| r.id)
        .collect::<BTreeSet<_>>();
    ensure!(
        choices.keys().all(|id| conflicts.contains(id)),
        "Review choices no longer match conflicts"
    );
    for row in &p.view.differences {
        match row
            .selected(mode, choices.get(&row.id).copied())
            .context("Choose Local or Disk for every conflicting marker")?
        {
            Some(entry) => {
                chosen.insert(row.id, entry.clone());
            }
            None => {
                chosen.remove(&row.id);
            }
        }
    }
    let mut result = Preferences::default();
    for entry in chosen.into_values() {
        result
            .files
            .entry(entry.path)
            .or_default()
            .push(entry.marker);
    }

    Ok(result)
}
impl State {
    pub(super) fn expire_review(&mut self, signature: &Signature) {
        if self
            .review
            .as_ref()
            .is_some_and(|p| &p.signature != signature)
        {
            self.review = None;
        }
    }
    pub(super) fn prepare_review(
        &mut self,
        signature: Signature,
        recovery: Option<String>,
    ) -> Result<()> {
        let disk = load(&self.root);
        ensure!(
            disk.error.is_none(),
            "Repair disk preferences before review: {}",
            disk.error.unwrap_or_default()
        );
        let local_guard = self.preferences();
        let (base, local, recovery) = if let Some(name) = recovery {
            ensure!(
                local_guard == self.persisted,
                "Resolve current unsaved marker changes before recovery"
            );
            let (draft, hash) = self.read_draft(&name)?;
            (draft.base, draft.local, Some((name, hash)))
        } else {
            (self.persisted.clone(), local_guard.clone(), None)
        };
        let id = self
            .review_clock
            .checked_add(1)
            .context("Breakpoint review counter exhausted")?;
        let diff = differences(&base, &local, &disk.preferences);
        let conflicts = diff.iter().filter(|d| d.conflict).count();
        let merge_unconflicted_count = entries(&base).len()
            - diff.iter().filter(|r| r.base.is_some()).count()
            + diff
                .iter()
                .filter(|r| r.selected(Mode::Merge, None).flatten().is_some())
                .count();
        let view = Arc::new(Review {
            id,
            local_count: entries(&local).len(),
            disk_count: entries(&disk.preferences).len(),
            differences: diff,
            conflicts,
            merge_unconflicted_count,
            recovery: recovery.as_ref().map(|(n, _)| n.clone()),
        });
        self.review = Some(Prepared {
            view,
            local_guard,
            base,
            local,
            disk: disk.preferences,
            expected: disk.expected,
            signature,
            recovery,
        });
        self.review_clock = id;
        self.review_outcome = None;
        Ok(())
    }
    pub(super) fn apply_review(
        &mut self,
        id: u64,
        mode: Mode,
        choices: BTreeMap<u64, Side>,
        signature: &Signature,
    ) -> Result<()> {
        let p = self
            .review
            .as_ref()
            .context("Review expired; open it again")?;
        ensure!(
            p.view.id == id && &p.signature == signature && p.local_guard == self.preferences(),
            "Markers or source snapshots changed; review again"
        );
        let desired = merge(p, mode, &choices)?;
        valid(&self.root, &desired)?;
        let disk_expected = p.expected;
        let recovery = p.recovery.clone();
        // The explicit decision grants only this reviewed byte checkpoint, never a
        // general permission to overwrite a later external edit.
        let local_paths = entries(&self.preferences());
        let prior = self.expected;
        self.expected = disk_expected;
        if let Err(e) = self.persist(&desired) {
            self.expected = prior;
            return Err(e);
        }
        let anchors = self
            .files
            .values_mut()
            .flatten()
            .filter_map(|m| m.anchor.take().map(|a| (m.saved.id, (m.saved.clone(), a))))
            .collect::<BTreeMap<_, _>>();
        self.files = desired
            .files
            .iter()
            .map(|(path, ms)| {
                (
                    path.clone(),
                    ms.iter()
                        .map(|m| Marker {
                            saved: m.clone(),
                            anchor: None,
                            last_line: m.line,
                        })
                        .collect(),
                )
            })
            .collect();
        // Existing exact local records keep their character identity/Undo mapping.
        let mut anchors = anchors;
        for (path, markers) in &mut self.files {
            for m in markers {
                if let Some((old, anchor)) = anchors.remove(&m.saved.id)
                    && old == m.saved
                    && local_paths
                        .get(&m.saved.id)
                        .is_some_and(|e| e.path == *path)
                {
                    m.anchor = Some(anchor)
                }
            }
        }
        self.next_id = self
            .next_id
            .max(entries(&desired).keys().next_back().copied().unwrap_or(0));
        self.persisted = desired;
        self.failed_payload = None;
        self.error = None;
        self.load_failed = false;
        self.review = None;
        if let Some((name, hash)) = recovery {
            self.remove_draft(&name, hash)?;
        }
        self.recoveries = self.list_drafts()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Document;
    fn setup() -> (tempfile::TempDir, PathBuf, Document, State) {
        let (dir, p, d, mut state) = super::super::tests::fixture("a();\nb();\nc();\n");
        state.recovery_root = dir.path().join("drafts");
        state
            .action(Action::Toggle(p.clone(), 1, d.tracking_capture()))
            .unwrap();
        super::super::tests::sync(&mut state, &p, &d);
        (dir, p, d, state)
    }
    fn external(root: &Path, p: &Preferences) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(p).unwrap();
        let output=std::process::Command::new("python3").arg("-c").arg("import pathlib,sys;pathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read());print('EXTERNAL_WRITER_OK')").arg(root.join(".wex/breakpoints.json")).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().and_then(|mut child|{child.stdin.take().unwrap().write_all(&bytes)?;child.wait_with_output()}).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"EXTERNAL_WRITER_OK\n");
        bytes
    }
    #[test]
    fn displayed_result_and_merge_share_exhaustive_three_way_choices() {
        let entry = |line| Entry {
            path: PathBuf::from("/fixture/Main.kt"),
            marker: StoredMarker {
                id: 1,
                line,
                source_hash: None,
                span: None,
                reason: None,
            },
        };
        let values = [None, Some(entry(1)), Some(entry(2))];
        for base in &values {
            for local in &values {
                for disk in &values {
                    let ours_changed = local != base;
                    let theirs_changed = disk != base;
                    let conflict = ours_changed && theirs_changed && local != disk;
                    let row = Difference {
                        id: 1,
                        base: base.clone(),
                        local: local.clone(),
                        disk: disk.clone(),
                        conflict,
                    };
                    for mode in [Mode::Merge, Mode::Local, Mode::Disk] {
                        for side in [None, Some(Side::Local), Some(Side::Disk)] {
                            let expected = match mode {
                                Mode::Local => Some(local.as_ref()),
                                Mode::Disk => Some(disk.as_ref()),
                                Mode::Merge => {
                                    if !conflict {
                                        Some(if ours_changed {
                                            local.as_ref()
                                        } else {
                                            disk.as_ref()
                                        })
                                    } else {
                                        side.map(|s| match s {
                                            Side::Local => local.as_ref(),
                                            Side::Disk => disk.as_ref(),
                                        })
                                    }
                                }
                            };
                            assert_eq!(row.selected(mode, side), expected);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn independent_writer_three_way_merge_keeps_nonconflicting_additions_and_local_anchor() {
        let (dir, p, d, mut s) = setup();
        let mut disk = s.preferences();
        let mut added = disk.files[&p][0].clone();
        added.id = 90;
        added.line = 3;
        added.span = Some([10, 14]);
        disk.files.get_mut(&p).unwrap().push(added);
        external(dir.path(), &disk);
        s.action(Action::Toggle(p.clone(), 2, d.tracking_capture()))
            .unwrap();
        let report = super::super::tests::sync(&mut s, &p, &d);
        assert!(report.error.is_some());
        assert!(s.files[&p][0].anchor.is_some());
        s.prepare_review(vec![], None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        assert_eq!(s.review.as_ref().unwrap().view.conflicts, 0);
        s.apply_review(id, Mode::Merge, BTreeMap::new(), &vec![])
            .unwrap();
        assert_eq!(s.preferences().files[&p].len(), 3);
        assert!(s.files[&p][0].anchor.is_some());
        assert_eq!(load(dir.path()).preferences, s.preferences());
        assert!(s.finish().is_ok());
    }
    #[test]
    fn delete_modify_and_add_add_conflicts_require_explicit_side_for_each_id() {
        let (dir, p, d, mut s) = setup();
        s.action(Action::Remove(p.clone(), 1)).unwrap();
        let mut disk = s.persisted.clone();
        disk.files.get_mut(&p).unwrap()[0].line = 2;
        external(dir.path(), &disk);
        s.prepare_review(vec![], None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        assert_eq!(s.review.as_ref().unwrap().view.conflicts, 1);
        let before = std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap();
        assert!(
            s.apply_review(id, Mode::Merge, BTreeMap::new(), &vec![])
                .is_err()
        );
        assert_eq!(
            before,
            std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap()
        );
        s.apply_review(id, Mode::Merge, BTreeMap::from([(1, Side::Local)]), &vec![])
            .unwrap();
        assert!(s.preferences().files.is_empty());
        s.action(Action::Toggle(p.clone(), 1, d.tracking_capture()))
            .unwrap();
        let mut disk = s.persisted.clone();
        let mut m = s.preferences().files[&p][0].clone();
        m.line = 3;
        disk.files.insert(p.clone(), vec![m]);
        external(dir.path(), &disk);
        s.prepare_review(vec![], None).unwrap();
        assert_eq!(s.review.as_ref().unwrap().view.conflicts, 1);
        let id = s.review.as_ref().unwrap().view.id;
        s.apply_review(id, Mode::Merge, BTreeMap::from([(2, Side::Disk)]), &vec![])
            .unwrap();
        assert_eq!(s.preferences().files[&p][0].line, 3);
    }
    #[test]
    fn later_disk_writer_and_changed_local_marker_never_get_overwritten_by_review() {
        let (dir, p, d, mut s) = setup();
        s.prepare_review(vec![], None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        let mut disk = s.preferences();
        disk.files.clear();
        let bytes = external(dir.path(), &disk);
        assert!(
            s.apply_review(id, Mode::Local, BTreeMap::new(), &vec![])
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap(),
            bytes
        );
        s.prepare_review(vec![], None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        s.action(Action::Toggle(p, 2, d.tracking_capture()))
            .unwrap();
        assert!(
            s.apply_review(id, Mode::Local, BTreeMap::new(), &vec![])
                .is_err()
        );
    }
    #[test]
    fn dirty_snapshot_epoch_and_saved_identity_fence_review_even_if_preferences_equal() {
        let (_dir, p, mut d, mut s) = setup();
        let sig = vec![(
            p.clone(),
            d.text_id().clone(),
            d.saved_text_id().cloned(),
            d.disk_hash(),
        )];
        s.prepare_review(sig, None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        d.edit(0..0, "// new\n").unwrap();
        let sig = vec![(
            p,
            d.text_id().clone(),
            d.saved_text_id().cloned(),
            d.disk_hash(),
        )];
        assert!(
            s.apply_review(id, Mode::Local, BTreeMap::new(), &sig)
                .is_err()
        );
        s.expire_review(&sig);
        assert!(s.review.is_none());
    }
    #[test]
    fn keep_local_and_use_disk_are_whole_snapshot_explicit_choices() {
        for mode in [Mode::Local, Mode::Disk] {
            let (dir, p, d, mut s) = setup();
            let mut disk = s.preferences();
            disk.files.clear();
            external(dir.path(), &disk);
            s.action(Action::Toggle(p, 2, d.tracking_capture()))
                .unwrap();
            let local = s.preferences();
            s.prepare_review(vec![], None).unwrap();
            let id = s.review.as_ref().unwrap().view.id;
            s.apply_review(id, mode, BTreeMap::new(), &vec![]).unwrap();
            assert_eq!(
                s.preferences(),
                if mode == Mode::Local { local } else { disk }
            );
        }
    }
    #[test]
    fn reviewed_checkpoint_still_obeys_an_independent_live_fd_lock() {
        use std::io::{BufRead, BufReader};
        let (dir, _p, _d, mut s) = setup();
        s.prepare_review(vec![], None).unwrap();
        let id = s.review.as_ref().unwrap().view.id;
        let script = "import fcntl,sys;f=open(sys.argv[1],'a+');fcntl.flock(f,fcntl.LOCK_EX);print('LOCKED',flush=True);sys.stdin.readline()";
        let mut holder = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(dir.path().join(".wex/breakpoints.lock"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(holder.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "LOCKED\n");
        let before = std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap();
        let result = s.apply_review(id, Mode::Local, BTreeMap::new(), &vec![]);
        holder
            .stdin
            .take()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        assert!(holder.wait().unwrap().success());
        assert!(result.unwrap_err().to_string().contains("busy"));
        assert_eq!(
            before,
            std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap()
        );
        s.apply_review(id, Mode::Local, BTreeMap::new(), &vec![])
            .unwrap();
        assert!(dir.path().join(".wex/breakpoints.lock").exists());
    }
    #[test]
    fn malformed_duplicate_paths_and_unknown_schema_stay_intact() {
        let (dir, p, _d, mut s) = setup();
        let path = serde_json::to_string(&p).unwrap();
        let raws = [
            format!("{{\"schema\":2,\"files\":{{{path}:[],{path}:[]}}}}"),
            "{\"schema\":2,\"schema\":2,\"files\":{}}".into(),
            "{\"schema\":999,\"files\":{}}".into(),
            "not json".into(),
        ];
        for raw in raws {
            std::fs::write(dir.path().join(".wex/breakpoints.json"), &raw).unwrap();
            assert!(load(dir.path()).error.is_some());
            assert!(s.prepare_review(vec![], None).is_err());
            assert_eq!(
                std::fs::read_to_string(dir.path().join(".wex/breakpoints.json")).unwrap(),
                raw
            );
        }
    }
}
