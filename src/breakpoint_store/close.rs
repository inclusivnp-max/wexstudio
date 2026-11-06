//! Live, cancelable pre-close checkpoint. No marker discard happens before exit.
use super::*;
#[derive(Clone, Debug, Serialize)]
pub enum CloseStatus {
    Clean,
    Backup(PathBuf),
    Failed(String),
    Committed {
        backup: Option<PathBuf>,
        discarded: bool,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct CloseOutcome {
    pub id: u64,
    pub changes: usize,
    pub status: CloseStatus,
}
struct Guard {
    id: u64,
    signature: Signature,
    local: Preferences,
    base: Preferences,
    changes: usize,
    recovery: Option<(String, blake3::Hash)>,
}
enum Sealed {
    Clean,
    Backup(super::recovery::Seal),
    Discard,
}
struct Commit {
    guard: Guard,
    kind: Sealed,
}
#[derive(Default)]
pub(super) struct Tracker {
    prepared: Option<Guard>,
    sealed: Option<Commit>,
    pub(super) outcome: Option<CloseOutcome>,
    signature: Signature,
}
impl Tracker {
    pub fn invalidate(&mut self, reason: &str) {
        let active = self
            .prepared
            .as_ref()
            .or_else(|| self.sealed.as_ref().map(|s| &s.guard));
        if let Some(g) = active {
            self.outcome = Some(CloseOutcome {
                id: g.id,
                changes: g.changes,
                status: CloseStatus::Failed(reason.into()),
            });
        }
        self.prepared = None;
        self.sealed = None;
    }
    pub fn signature(&mut self, signature: &Signature) {
        if self.signature != *signature {
            self.invalidate("Sources changed; check closing again");
            self.signature = signature.clone();
        }
    }
    pub fn cancel(&mut self, id: u64) {
        if self.outcome.as_ref().is_some_and(|o| o.id == id) {
            self.prepared = None;
            self.sealed = None;
            self.outcome = None;
        }
    }
}
impl State {
    pub(super) fn prepare_close(&mut self, id: u64, signature: &Signature) {
        self.close.signature(signature);
        self.close.prepared = None;
        self.close.sealed = None;
        let local = self.preferences();
        let changes = review::changed_count(&self.persisted, &local);
        let mut recovery = None;
        let status = if local == self.persisted {
            CloseStatus::Clean
        } else {
            match self.backup_for_close(&local) {
                Ok((path, name, hash)) => {
                    recovery = Some((name, hash));
                    CloseStatus::Backup(path)
                }
                Err(e) => CloseStatus::Failed(format!(
                    "{} · Backup: {e:#}",
                    self.error
                        .as_deref()
                        .unwrap_or("Marker changes are not saved")
                )),
            }
        };
        self.close.prepared = Some(Guard {
            id,
            signature: signature.clone(),
            local,
            base: self.persisted.clone(),
            changes,
            recovery,
        });
        self.close.outcome = Some(CloseOutcome {
            id,
            changes,
            status,
        });
    }
    pub(super) fn commit_close(&mut self, id: u64, discard: bool, signature: &Signature) {
        let result = (|| -> Result<(Guard, Sealed, Option<PathBuf>)> {
            let guard = self
                .close
                .prepared
                .as_ref()
                .context("Closing check expired; try again")?;
            ensure!(
                guard.id == id
                    && guard.signature == *signature
                    && guard.local == self.preferences()
                    && guard.base == self.persisted,
                "Source or marker baseline changed; review closing again"
            );
            let (kind, backup) = if discard {
                (Sealed::Discard, None)
            } else if guard.local == self.persisted {
                (Sealed::Clean, None)
            } else {
                let (name, hash) = guard
                    .recovery
                    .as_ref()
                    .context("Marker changes have no verified backup")?;
                let seal = self.seal_backup(name, *hash, &guard.local)?;
                let path = seal.path();
                (Sealed::Backup(seal), Some(path))
            };
            let guard = self.close.prepared.take().unwrap();
            Ok((guard, kind, backup))
        })();
        match result {
            Ok((guard, kind, backup)) => {
                let changes = guard.changes;
                self.close.sealed = Some(Commit { guard, kind });
                self.close.outcome = Some(CloseOutcome {
                    id,
                    changes,
                    status: CloseStatus::Committed {
                        backup,
                        discarded: discard,
                    },
                });
            }
            Err(e) => {
                let changes = self.close.prepared.as_ref().map_or(0, |g| g.changes);
                self.close.outcome = Some(CloseOutcome {
                    id,
                    changes,
                    status: CloseStatus::Failed(format!("{e:#}")),
                });
            }
        }
    }
    pub(super) fn finish_prepared_close(&self) -> Option<Result<()>> {
        let commit = self.close.sealed.as_ref()?;
        if commit.guard.signature != self.close.signature
            || commit.guard.local != self.preferences()
            || commit.guard.base != self.persisted
        {
            return None;
        }
        Some(match &commit.kind {
            Sealed::Clean | Sealed::Discard => Ok(()),
            Sealed::Backup(seal) => seal.check(&self.root, &commit.guard.local).and_then(|()| {
                Err(anyhow::anyhow!(
                    "{}; marker changes preserved for explicit recovery at {}",
                    self.error
                        .as_deref()
                        .unwrap_or("Marker preference write failed"),
                    seal.path().display()
                ))
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Document;
    fn fixture() -> (tempfile::TempDir, PathBuf, Document, State, Signature) {
        let (dir, path, doc, mut state) = super::super::tests::fixture("first();\nsecond();\n");
        state.recovery_root = dir.path().join("journal");
        state
            .action(Action::Toggle(path.clone(), 1, doc.tracking_capture()))
            .unwrap();
        super::super::tests::sync(&mut state, &path, &doc);
        let output=std::process::Command::new("python3").arg("-c").arg("import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('{\"schema\":2,\"files\":{}}');print('EXTERNAL_WRITE_OK')").arg(dir.path().join(".wex/breakpoints.json")).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"EXTERNAL_WRITE_OK\n");
        state
            .action(Action::Toggle(path.clone(), 2, doc.tracking_capture()))
            .unwrap();
        assert!(
            super::super::tests::sync(&mut state, &path, &doc)
                .error
                .is_some()
        );
        let signature = vec![(
            path.clone(),
            doc.text_id().clone(),
            doc.saved_text_id().cloned(),
            doc.disk_hash(),
        )];
        (dir, path, doc, state, signature)
    }
    #[test]
    fn failed_live_check_keeps_local_markers_and_explicit_discard_is_only_an_exit_seal() {
        let (dir, _path, _doc, mut s, sig) = fixture();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"foreign").unwrap();
        s.recovery_root = blocked;
        let wanted = s.preferences();
        let disk = std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap();
        s.prepare_close(1, &sig);
        assert!(matches!(
            s.close.outcome.as_ref().unwrap().status,
            CloseStatus::Failed(_)
        ));
        assert_eq!(s.close.outcome.as_ref().unwrap().changes, 1);
        assert_eq!(s.preferences(), wanted);
        s.commit_close(1, true, &sig);
        assert!(matches!(
            s.close.outcome.as_ref().unwrap().status,
            CloseStatus::Committed {
                discarded: true,
                ..
            }
        ));
        assert_eq!(
            s.preferences(),
            wanted,
            "discard must not mutate a live window"
        );
        assert!(s.finish().is_ok());
        assert_eq!(
            std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap(),
            disk
        );
        s.close.cancel(1);
        assert_eq!(s.preferences(), wanted);
        assert!(
            s.finish()
                .unwrap_err()
                .to_string()
                .contains("recovery write failed")
        );
    }
    #[test]
    fn backup_is_checked_then_leased_until_exit_and_cancel_releases_the_os_lock() {
        let (_dir, _path, _doc, mut s, sig) = fixture();
        s.prepare_close(2, &sig);
        let path = match &s.close.outcome.as_ref().unwrap().status {
            CloseStatus::Backup(p) => p.clone(),
            other => panic!("{other:?}"),
        };
        let bytes = std::fs::read(&path).unwrap();
        s.commit_close(2, false, &sig);
        assert!(matches!(
            s.close.outcome.as_ref().unwrap().status,
            CloseStatus::Committed {
                discarded: false,
                ..
            }
        ));
        assert!(s.list_drafts().unwrap_err().to_string().contains("busy"));
        let result = s.finish().unwrap_err().to_string();
        assert!(result.contains("preserved for explicit recovery"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        s.close.cancel(2);
        assert_eq!(s.list_drafts().unwrap().len(), 1);
        assert!(s.preferences() != s.persisted);
    }
    #[test]
    fn tampered_backup_and_changed_source_signature_cannot_commit_closing() {
        let (_dir, path, mut doc, mut s, sig) = fixture();
        s.prepare_close(3, &sig);
        let backup = match &s.close.outcome.as_ref().unwrap().status {
            CloseStatus::Backup(p) => p.clone(),
            other => panic!("{other:?}"),
        };
        let mut bytes = std::fs::read(&backup).unwrap();
        bytes.push(b' ');
        std::fs::write(&backup, &bytes).unwrap();
        s.commit_close(3, false, &sig);
        assert!(matches!(
            s.close.outcome.as_ref().unwrap().status,
            CloseStatus::Failed(_)
        ));
        assert!(s.close.sealed.is_none());
        s.prepare_close(4, &sig);
        doc.edit(0..0, "// edit\n").unwrap();
        let changed = vec![(
            path,
            doc.text_id().clone(),
            doc.saved_text_id().cloned(),
            doc.disk_hash(),
        )];
        s.commit_close(4, true, &changed);
        assert!(matches!(
            s.close.outcome.as_ref().unwrap().status,
            CloseStatus::Failed(_)
        ));
        assert!(s.close.sealed.is_none());
    }
    #[test]
    fn late_marker_action_invalidates_a_seal_instead_of_losing_the_new_intent() {
        let (_dir, path, doc, mut s, sig) = fixture();
        s.prepare_close(5, &sig);
        s.commit_close(5, true, &sig);
        s.action(Action::Remove(path, 1)).unwrap();
        assert!(s.close.sealed.is_none());
        assert!(
            s.finish()
                .unwrap_err()
                .to_string()
                .contains("preserved for explicit recovery")
        );
        let _ = doc;
    }
    #[test]
    fn deleted_marker_changes_are_counted_and_clean_close_needs_no_journal() {
        let (dir, path, _doc, mut s, sig) = fixture();
        s.action(Action::Remove(path.clone(), 1)).unwrap();
        s.action(Action::Remove(path, 2)).unwrap();
        s.prepare_close(6, &sig);
        assert_eq!(
            s.close.outcome.as_ref().unwrap().changes,
            1,
            "the accepted base has one marker; its pending deletion is one change"
        );
        let clean = State::new(dir.path().to_owned(), load(dir.path()));
        let mut clean = clean;
        clean.prepare_close(7, &vec![]);
        assert!(matches!(
            clean.close.outcome.as_ref().unwrap().status,
            CloseStatus::Clean
        ));
        clean.commit_close(7, false, &vec![]);
        assert!(clean.finish().is_ok());
    }
}
