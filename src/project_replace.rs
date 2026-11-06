//! Reviewed, no-write project replacements and atomic multi-document history.
use crate::{
    editor::{Document, RenderSnapshot},
    editor_search::{self, SearchOptions},
    project_search::{self, Overlay, Stats},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};
pub const PLAN_FILES: usize = 1_000;
pub const PLAN_BYTES: usize = 64 * 1024 * 1024;
pub const TEMPLATE_BYTES: usize = 16 * 1024;
pub type Documents = BTreeMap<PathBuf, Document>;
#[derive(Clone)]
pub struct Guard {
    text: RenderSnapshot,
    revision: u64,
    version: i32,
    disk: Option<blake3::Hash>,
}
impl Guard {
    pub fn capture(d: &Document) -> Self {
        Self {
            text: d.render_snapshot(),
            revision: d.revision,
            version: d.version,
            disk: d.disk_hash(),
        }
    }
    pub fn matches(&self, d: &Document) -> bool {
        self.text.matches(d)
            && self.revision == d.revision
            && self.version == d.version
            && self.disk == d.disk_hash()
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct PreviewRow {
    pub line: usize,
    pub column: usize,
    pub before: String,
    pub after: String,
    pub clipped: bool,
}
pub struct FileChange {
    pub path: PathBuf,
    pub before: Document,
    pub after: Document,
    pub dirty: bool,
    pub rows: Vec<PreviewRow>,
}
pub struct Plan {
    pub root: PathBuf,
    pub options: SearchOptions,
    pub replacement: String,
    pub files: Vec<FileChange>,
    pub stats: Stats,
    pub input: BTreeMap<PathBuf, Guard>,
    pub worker: String,
    disk: BTreeMap<PathBuf, Option<blake3::Hash>>,
    identity: (u64, u64),
}
fn open_root(root: &Path) -> Result<(std::fs::File, (u64, u64))> {
    let fd = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(root)?;
    let m = fd.metadata()?;
    Ok((fd, (m.dev(), m.ino())))
}
fn disk_hash(
    root: &std::fs::File,
    path: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<Option<blake3::Hash>> {
    let mut bytes = 0;
    match project_search::read_text(root, path, &cancel, &mut bytes) {
        Ok(b) => Ok(Some(blake3::hash(&b))),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
fn snippet(s: &str) -> (String, bool) {
    let mut chars = s.chars();
    let out = chars.by_ref().take(240).collect::<String>();
    let clipped = chars.next().is_some();
    (
        out.replace('\n', "⏎").replace('\r', "␍").replace('\t', "⇥"),
        clipped,
    )
}
// Match the pinned regex interpolation grammar, but bound BEFORE appending captures.
fn expand(caps: &regex::Captures<'_>, mut template: &str, limit: usize) -> Result<String> {
    let mut out = String::new();
    let append = |out: &mut String, s: &str| -> Result<()> {
        ensure!(
            s.len() <= limit.saturating_sub(out.len()),
            "Replacement exceeds file/output budget"
        );
        out.push_str(s);
        Ok(())
    };
    while let Some(i) = template.find('$') {
        append(&mut out, &template[..i])?;
        template = &template[i..];
        if template.starts_with("$$") {
            append(&mut out, "$")?;
            template = &template[2..];
            continue;
        }
        let (name, end) = if template.starts_with("${") {
            match template[2..].find('}') {
                Some(i) => (&template[2..2 + i], 3 + i),
                None => {
                    append(&mut out, "$")?;
                    template = &template[1..];
                    continue;
                }
            }
        } else {
            let n = template[1..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            if n == 0 {
                append(&mut out, "$")?;
                template = &template[1..];
                continue;
            }
            (&template[1..1 + n], 1 + n)
        };
        let capture = match name.parse::<usize>() {
            Ok(n) => caps.get(n),
            Err(_) => caps.name(name),
        };
        if let Some(m) = capture {
            append(&mut out, m.as_str())?;
        }
        template = &template[end..];
    }
    append(&mut out, template)?;
    Ok(out)
}
fn escapes(s: &str) -> String {
    let mut out = String::new();
    let mut c = s.chars();
    while let Some(x) = c.next() {
        if x == '\\' {
            match c.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(y) => {
                    out.push('\\');
                    out.push(y)
                }
                None => out.push('\\'),
            }
        } else {
            out.push(x)
        }
    }
    out
}
pub fn prepare(
    root: &Path,
    mut docs: Documents,
    options: SearchOptions,
    replacement: String,
    cancel: &dyn Fn() -> bool,
) -> Result<Plan> {
    ensure!(
        !options.query.is_empty(),
        "Enter a search query before preview"
    );
    ensure!(
        replacement.len() <= TEMPLATE_BYTES,
        "Replacement exceeds 16 KiB"
    );
    let root = root.canonicalize()?;
    let (fd, identity) = open_root(&root)?;
    let input = docs
        .iter()
        .map(|(p, d)| (p.clone(), Guard::capture(d)))
        .collect();
    let overlays = docs
        .iter()
        .map(|(p, d)| {
            (
                p.clone(),
                Overlay {
                    text: d.render_snapshot(),
                    dirty: d.dirty(),
                },
            )
        })
        .collect();
    let results = project_search::search(&root, &overlays, &options, cancel)?;
    ensure!(
        !results.stats.limited,
        "Search limit reached; refine the query before replacing"
    );
    ensure!(
        results.stats.read_errors == 0,
        "Read errors; resolve them before project replacement"
    );
    ensure!(
        results.files.len() <= PLAN_FILES,
        "Replacement exceeds 1,000 files; refine query"
    );
    let re = editor_search::compile(&options)?;
    let template = if options.regex {
        escapes(&replacement)
    } else {
        replacement.clone()
    };
    let mut plan = Plan {
        root: root.clone(),
        options: options.clone(),
        replacement,
        files: vec![],
        stats: results.stats,
        input,
        worker: format!("{:?}", thread::current().id()),
        disk: BTreeMap::new(),
        identity,
    };
    let mut budget = 0;
    for found in results.files {
        ensure!(!cancel(), "Replacement cancelled");
        let rel = found.path.strip_prefix(&root)?;
        let disk = disk_hash(&fd, rel, cancel)?;
        let mut before = if let Some(d) = docs.remove(&found.path) {
            ensure!(
                d.disk_hash() == disk,
                "Disk baseline changed: {} · review external change",
                found.path.display()
            );
            d
        } else {
            let mut bytes = 0;
            let text =
                String::from_utf8(project_search::read_text(&fd, rel, &cancel, &mut bytes)?)?;
            ensure!(
                Some(blake3::hash(text.as_bytes())) == disk,
                "Disk changed while preparing preview"
            );
            Document::from_verified_text(found.path.clone(), &text, cancel)?
        };
        ensure!(
            before.content_hash() == found.hash,
            "Search snapshot changed during preview"
        );
        before.defer_syntax();
        budget += before.rope().len_bytes();
        ensure!(
            budget <= PLAN_BYTES,
            "Preview exceeds 64 MiB input/output budget"
        );
        let text = before.rope().to_string();
        let mut edits = vec![];
        let mut rows = vec![];
        let mut output = before.rope().len_bytes();
        let mut inserted = 0;
        for caps in re.captures_iter(&text) {
            ensure!(!cancel(), "Replacement cancelled");
            let m = caps.get(0).unwrap();
            if !editor_search::in_scope(&text, m.start(), m.end(), &options, before.rope()) {
                continue;
            }
            let value = if options.regex {
                expand(
                    &caps,
                    &template,
                    project_search::FILE_BYTES.min(PLAN_BYTES - budget - inserted),
                )?
            } else {
                template.clone()
            };
            inserted += value.len();
            ensure!(
                inserted <= PLAN_BYTES - budget,
                "Replacement exceeds output budget"
            );
            output = output
                .checked_sub(m.len())
                .and_then(|n| n.checked_add(value.len()))
                .context("Replacement size overflow")?;
            if m.as_str() == value {
                continue;
            }
            let start = before.rope().byte_to_char(m.start());
            let end = before.rope().byte_to_char(m.end());
            let line = before.rope().char_to_line(start);
            let (old, a) = snippet(m.as_str());
            let (new, b) = snippet(&value);
            rows.push(PreviewRow {
                line: line + 1,
                column: start - before.rope().line_to_char(line) + 1,
                before: old,
                after: new,
                clipped: a || b,
            });
            edits.push((start..end, value));
        }
        ensure!(
            output <= project_search::FILE_BYTES,
            "Replaced file exceeds 8 MiB"
        );
        budget = budget
            .checked_add(output)
            .context("Preview size overflow")?;
        ensure!(
            budget <= PLAN_BYTES,
            "Preview exceeds 64 MiB input/output budget"
        );
        if edits.is_empty() {
            continue;
        }
        let dirty = before.dirty();
        let mut after = before.fork()?;
        after.edit_batch(&edits)?;
        after.content_hash();
        plan.disk.insert(found.path.clone(), disk);
        plan.files.push(FileChange {
            path: found.path,
            before,
            after,
            dirty,
            rows,
        });
    }
    plan.verify_disk(cancel)?;
    Ok(plan)
}
impl Plan {
    pub fn edits(&self) -> usize {
        self.files.iter().map(|f| f.rows.len()).sum()
    }
    pub fn verify_disk(&self, cancel: &dyn Fn() -> bool) -> Result<()> {
        let (fd, identity) = open_root(&self.root)?;
        ensure!(
            identity == self.identity,
            "Project directory changed; Preview again"
        );
        for (p, h) in &self.disk {
            ensure!(!cancel(), "Replacement cancelled");
            ensure!(
                disk_hash(&fd, p.strip_prefix(&self.root)?, cancel)? == *h,
                "Preview disk changed: {}",
                p.display()
            );
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct HistoryFile {
    pub path: PathBuf,
    pub before: RenderSnapshot,
    pub after: RenderSnapshot,
}
#[derive(Clone)]
pub struct Group {
    pub files: Vec<HistoryFile>,
}
impl Group {
    pub fn from_plan(p: &Plan) -> Self {
        Self {
            files: p
                .files
                .iter()
                .map(|f| HistoryFile {
                    path: f.path.clone(),
                    before: f.before.render_snapshot(),
                    after: f.after.render_snapshot(),
                })
                .collect(),
        }
    }
    pub fn candidate(&self, path: &Path, doc: &Document, redo: bool) -> bool {
        self.files.iter().find(|f| f.path == path).is_some_and(|f| {
            let (current, target) = if redo {
                (&f.before, &f.after)
            } else {
                (&f.after, &f.before)
            };
            current.matches(doc) && doc.history_target_matches(target, redo)
        })
    }
}
pub struct HistoryResult {
    pub guards: BTreeMap<PathBuf, Guard>,
    pub docs: Documents,
}
pub fn history(
    group: &Group,
    docs: Documents,
    redo: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<HistoryResult> {
    let mut result = HistoryResult {
        guards: BTreeMap::new(),
        docs: BTreeMap::new(),
    };
    for f in &group.files {
        ensure!(!cancel(), "Project history cancelled");
        let d = docs
            .get(&f.path)
            .context("An affected tab was closed; no buffers changed")?;
        ensure!(
            group.candidate(&f.path, d, redo),
            "An affected buffer/history changed; no buffers changed"
        );
        result.guards.insert(f.path.clone(), Guard::capture(d));
        let mut next = d.fork()?;
        ensure!(
            if redo { next.redo()? } else { next.undo()? },
            "History step missing"
        );
        result.docs.insert(f.path.clone(), next);
    }
    Ok(result)
}
pub enum Task {
    Preview {
        root: PathBuf,
        docs: Documents,
        options: SearchOptions,
        replacement: String,
    },
    Confirm(Arc<Plan>),
    History {
        group: Group,
        docs: Documents,
        redo: bool,
    },
}
pub enum Output {
    Preview(Arc<Plan>),
    Confirmed(Arc<Plan>),
    History(HistoryResult),
}
pub struct Finished {
    pub id: u64,
    pub result: std::result::Result<Output, String>,
}
struct Job {
    id: u64,
    task: Task,
}
#[derive(Default)]
struct Shared {
    job: Mutex<Option<Job>>,
    done: Mutex<Option<Finished>>,
    changed: Condvar,
    latest: AtomicU64,
    active: AtomicU64,
    stop: AtomicBool,
}
#[derive(Default)]
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<thread::JoinHandle<()>>,
    next: u64,
}
impl Worker {
    pub fn request(&mut self, task: Task, wake: Arc<dyn Fn() + Send + Sync>) -> Result<u64> {
        if self.thread.is_none() {
            let s = self.shared.clone();
            self.thread = Some(
                thread::Builder::new()
                    .name("project-replace".into())
                    .spawn(move || {
                        loop {
                            let mut slot = s.job.lock().unwrap();
                            while slot.is_none() && !s.stop.load(Ordering::Acquire) {
                                slot = s.changed.wait(slot).unwrap();
                            }
                            if s.stop.load(Ordering::Acquire) {
                                break;
                            }
                            let j = slot.take().unwrap();
                            drop(slot);
                            s.active.store(j.id, Ordering::Release);
                            let cancel = || {
                                s.stop.load(Ordering::Acquire)
                                    || s.latest.load(Ordering::Acquire) != j.id
                            };
                            let result = (match j.task {
                                Task::Preview {
                                    root,
                                    docs,
                                    options,
                                    replacement,
                                } => prepare(&root, docs, options, replacement, &cancel)
                                    .map(|p| Output::Preview(Arc::new(p))),
                                Task::Confirm(p) => {
                                    p.verify_disk(&cancel).map(|_| Output::Confirmed(p))
                                }
                                Task::History { group, docs, redo } => {
                                    history(&group, docs, redo, &cancel).map(Output::History)
                                }
                            })
                            .map_err(|e| format!("{e:#}"));
                            s.active.store(0, Ordering::Release);
                            if !cancel() {
                                s.done
                                    .lock()
                                    .unwrap()
                                    .replace(Finished { id: j.id, result });
                                wake();
                            }
                        }
                    })?,
            );
        }
        let id = self
            .next
            .checked_add(1)
            .context("Replacement clock exhausted")?;
        self.next = id;
        self.shared.latest.store(id, Ordering::Release);
        self.shared.job.lock().unwrap().replace(Job { id, task });
        self.shared.changed.notify_one();
        Ok(id)
    }
    pub fn poll(&self) -> Option<Finished> {
        self.shared.done.try_lock().ok()?.take()
    }
    pub fn cancel(&mut self) {
        self.shared.latest.store(0, Ordering::Release);
        self.shared.job.lock().unwrap().take();
        self.shared.changed.notify_one();
    }
    pub fn active_id(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = self.thread.take().unwrap().join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };
    fn opts(q: &str) -> SearchOptions {
        SearchOptions {
            query: q.into(),
            case_sensitive: true,
            ..Default::default()
        }
    }
    fn fixture() -> (tempfile::TempDir, Documents) {
        let d = tempfile::tempdir().unwrap();
        for (n, t) in [
            ("A.kt", "val a = 1\n"),
            ("B.kt", "val b = 2\n"),
            ("C.txt", "😀 needle closed\r\nneedle\n"),
        ] {
            fs::write(d.path().join(n), t).unwrap();
        }
        let docs = [
            ("A.kt", "// needle active\n"),
            ("B.kt", "// needle parked\n"),
        ]
        .into_iter()
        .map(|(n, t)| {
            let p = d.path().join(n);
            let mut doc = Document::open(&p).unwrap();
            doc.edit(0..0, t).unwrap();
            (p, doc)
        })
        .collect();
        (d, docs)
    }
    fn forks(docs: &Documents) -> Documents {
        docs.iter()
            .map(|(p, d)| (p.clone(), d.fork().unwrap()))
            .collect()
    }
    fn applied(p: &Plan) -> Documents {
        p.files
            .iter()
            .map(|f| (f.path.clone(), f.after.fork().unwrap()))
            .collect()
    }
    fn plan(d: &Path, docs: Documents) -> Plan {
        prepare(d, docs, opts("needle"), "новое".into(), &|| false).unwrap()
    }
    #[test]
    fn preview_uses_all_dirty_overlays_and_closed_unicode_without_writing() {
        let (d, docs) = fixture();
        let p = plan(d.path(), forks(&docs));
        assert_eq!(p.files.len(), 3);
        assert_eq!(p.edits(), 4);
        assert_eq!(p.files.iter().filter(|f| f.dirty).count(), 2);
        assert_eq!(p.files[2].rows[0].column, 3);
        assert_eq!(p.files[2].rows[0].line, 1);
        for f in &p.files {
            assert!(f.after.dirty());
            assert!(f.after.rope().to_string().contains("новое"));
            assert!(!f.after.rope().to_string().contains("needle"));
            assert!(f.before.content_hash() != f.after.content_hash());
        }
        assert_eq!(
            fs::read_to_string(d.path().join("A.kt")).unwrap(),
            "val a = 1\n"
        );
        assert_eq!(
            fs::read_to_string(d.path().join("C.txt")).unwrap(),
            "😀 needle closed\r\nneedle\n"
        );
        assert!(
            docs.values()
                .all(|doc| doc.rope().to_string().contains("needle"))
        );
    }
    #[test]
    fn regex_named_numbered_escapes_and_literal_dollars_have_exact_preview() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("x.txt"), "a=12\na=34").unwrap();
        let mut o = opts(r"(?P<name>a)=(\d+)");
        o.regex = true;
        let p = prepare(
            d.path(),
            Documents::new(),
            o,
            r"${name}:$2\n$$".into(),
            &|| false,
        )
        .unwrap();
        assert_eq!(p.files[0].after.rope().to_string(), "a:12\n$\na:34\n$");
        assert_eq!(p.files[0].rows[0].after, "a:12⏎$");
    }
    #[test]
    fn interpolation_matches_pinned_regex_oracle_including_malformed_names() {
        let re = regex::Regex::new(r"(?P<名>x)?(?P<a>abc)(z)?").unwrap();
        let caps = re.captures("abc").unwrap();
        for t in [
            "plain",
            "$0",
            "$1",
            "$2",
            "$3",
            "$99",
            "$a",
            "$名",
            "${名}",
            "${a}",
            "${}",
            "${",
            "$",
            "$$",
            "$$$a",
            "$aZ",
            "${a}Z",
            "${a-b}",
            "$001",
            "$999999999999999999999999999",
            "😀${2}é$-a",
            "${a}}${3}",
        ] {
            let mut expected = String::new();
            caps.expand(t, &mut expected);
            assert_eq!(expand(&caps, t, 1024).unwrap(), expected, "{t}");
        }
    }
    #[test]
    fn capture_expansion_fails_before_appending_unbounded_bytes() {
        let re = regex::Regex::new("(?s)(.*)").unwrap();
        let s = "x".repeat(1024 * 1024);
        let c = re.captures(&s).unwrap();
        assert!(
            expand(&c, "$1$1", 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
        assert_eq!(expand(&c, "$missing", 0).unwrap(), "");
    }
    #[test]
    fn noops_empty_replacement_and_zero_width_are_exact() {
        let (d, docs) = fixture();
        let p = prepare(
            d.path(),
            forks(&docs),
            opts("needle"),
            "needle".into(),
            &|| false,
        )
        .unwrap();
        assert!(p.files.is_empty());
        let p = prepare(d.path(), forks(&docs), opts("needle"), "".into(), &|| false).unwrap();
        assert_eq!(p.edits(), 4);
        assert!(
            p.files
                .iter()
                .all(|f| !f.after.rope().to_string().contains("needle"))
        );
        let mut o = opts("^");
        o.regex = true;
        let p = prepare(d.path(), Documents::new(), o, "+".into(), &|| false).unwrap();
        assert!(p.files.iter().all(|f| f.after.rope().char(0) == '+'));
    }
    #[test]
    fn changed_closed_disk_and_symlink_retarget_reject_confirmation() {
        let (d, docs) = fixture();
        let p = plan(d.path(), docs);
        let c = d.path().join("C.txt");
        fs::write(&c, "changed").unwrap();
        assert!(p.verify_disk(&|| false).is_err());
        fs::remove_file(&c).unwrap();
        let out = tempfile::tempdir().unwrap();
        let target = out.path().join("same");
        fs::write(&target, "😀 needle closed\r\nneedle\n").unwrap();
        std::os::unix::fs::symlink(target, c).unwrap();
        assert!(p.verify_disk(&|| false).is_err());
    }
    #[test]
    fn replacing_root_directory_with_identical_bytes_rejects_confirmation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("x"), "needle").unwrap();
        let p = plan(&root, Documents::new());
        fs::rename(&root, parent.path().join("old")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("x"), "needle").unwrap();
        assert!(
            p.verify_disk(&|| false)
                .unwrap_err()
                .to_string()
                .contains("directory changed")
        );
        fs::remove_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(parent.path().join("old"), &root).unwrap();
        assert!(p.verify_disk(&|| false).is_err());
    }
    #[test]
    fn externally_changed_open_baseline_rejects_even_when_buffer_is_dirty() {
        let (d, docs) = fixture();
        fs::write(d.path().join("A.kt"), "external").unwrap();
        assert!(
            prepare(d.path(), docs, opts("needle"), "next".into(), &|| false)
                .unwrap_err_string()
                .contains("baseline changed")
        );
    }
    trait ErrorText {
        fn unwrap_err_string(self) -> String;
    }
    impl ErrorText for Result<Plan> {
        fn unwrap_err_string(self) -> String {
            match self {
                Ok(_) => panic!("expected error"),
                Err(e) => format!("{e:#}"),
            }
        }
    }
    #[test]
    fn acknowledged_deleted_open_buffer_is_still_replaced_unsaved() {
        let (d, mut docs) = fixture();
        let a = d.path().join("A.kt");
        fs::remove_file(&a).unwrap();
        docs.get_mut(&a).unwrap().acknowledge_disk(None);
        let p = plan(d.path(), docs);
        assert_eq!(p.files.len(), 3);
        assert!(!a.exists());
        assert!(p.files[0].after.dirty());
    }
    #[test]
    fn grouped_history_preserves_dirty_origins_and_original_local_undo() {
        let (d, docs) = fixture();
        let p = plan(d.path(), forks(&docs));
        let g = Group::from_plan(&p);
        let after = applied(&p);
        let undone = history(&g, forks(&after), false, &|| false).unwrap();
        for f in &p.files {
            assert_eq!(
                undone.docs[&f.path].rope().to_string(),
                f.before.rope().to_string()
            );
            assert_eq!(undone.docs[&f.path].dirty(), f.dirty);
            assert!(g.candidate(&f.path, &undone.docs[&f.path], true));
        }
        let redone = history(&g, undone.docs, true, &|| false).unwrap();
        assert!(
            redone
                .docs
                .values()
                .all(|d| d.rope().to_string().contains("новое"))
        );
        let mut old = history(&g, redone.docs, false, &|| false).unwrap().docs;
        let a = old.get_mut(&d.path().join("A.kt")).unwrap();
        assert!(a.undo().unwrap());
        assert_eq!(a.rope().to_string(), "val a = 1\n");
        assert!(!a.dirty());
    }
    #[test]
    fn local_edit_then_local_undo_restores_shared_boundary_and_save_baseline() {
        let (d, docs) = fixture();
        let p = plan(d.path(), docs);
        let g = Group::from_plan(&p);
        let mut after = applied(&p);
        let a = d.path().join("A.kt");
        let doc = after.get_mut(&a).unwrap();
        doc.edit(0..0, "local").unwrap();
        assert!(!g.candidate(&a, doc, false));
        doc.undo().unwrap();
        assert!(g.candidate(&a, doc, false));
        doc.save().unwrap();
        assert!(!doc.dirty());
        let h = history(&g, after, false, &|| false).unwrap();
        assert!(h.docs[&a].dirty());
        let h = history(&g, h.docs, true, &|| false).unwrap();
        assert!(!h.docs[&a].dirty());
    }
    #[test]
    fn one_closed_changed_or_exhausted_file_rejects_entire_history() {
        let (d, docs) = fixture();
        let p = plan(d.path(), docs);
        let g = Group::from_plan(&p);
        let after = applied(&p);
        let last = p.files.last().unwrap().path.clone();
        let mut missing = forks(&after);
        missing.remove(&last);
        assert!(history(&g, missing, false, &|| false).is_err());
        let mut changed = forks(&after);
        changed.get_mut(&last).unwrap().edit(0..0, "local").unwrap();
        assert!(history(&g, changed, false, &|| false).is_err());
        let mut exhausted = forks(&after);
        exhausted.get_mut(&last).unwrap().version = i32::MAX;
        assert!(history(&g, exhausted, false, &|| false).is_err());
        assert!(
            after
                .values()
                .all(|d| d.rope().to_string().contains("новое"))
        );
    }
    #[test]
    fn exact_guards_include_history_clock_and_accepted_disk_baseline() {
        let (d, mut docs) = fixture();
        let a = d.path().join("A.kt");
        let doc = docs.get_mut(&a).unwrap();
        let g = Guard::capture(doc);
        assert!(g.matches(doc));
        doc.set_view(1, 2);
        assert!(g.matches(doc));
        doc.edit(0..0, "x").unwrap();
        doc.undo().unwrap();
        assert!(!g.matches(doc));
        let g = Guard::capture(doc);
        doc.acknowledge_disk(None);
        assert!(!g.matches(doc));
    }
    #[test]
    fn cancellation_invalid_regex_and_template_limit_never_return_partial_plan() {
        let (d, docs) = fixture();
        assert!(
            prepare(
                d.path(),
                forks(&docs),
                opts("needle"),
                "next".into(),
                &|| true
            )
            .is_err()
        );
        let mut o = opts("[");
        o.regex = true;
        assert!(prepare(d.path(), forks(&docs), o, "next".into(), &|| false).is_err());
        assert!(
            prepare(
                d.path(),
                forks(&docs),
                opts("needle"),
                "x".repeat(TEMPLATE_BYTES + 1),
                &|| false
            )
            .is_err()
        );
        assert!(prepare(d.path(), docs, opts(""), "next".into(), &|| false).is_err());
    }
    #[test]
    fn partial_search_and_file_limits_reject_instead_of_applying_a_subset() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("many"),
            "needle\n".repeat(project_search::MATCH_LIMIT + 1),
        )
        .unwrap();
        assert!(
            prepare(
                d.path(),
                Documents::new(),
                opts("needle"),
                "next".into(),
                &|| false
            )
            .unwrap_err_string()
            .contains("limit")
        );
        fs::remove_file(d.path().join("many")).unwrap();
        for n in 0..=PLAN_FILES {
            fs::write(d.path().join(format!("{n}.txt")), "needle").unwrap();
        }
        assert!(
            prepare(
                d.path(),
                Documents::new(),
                opts("needle"),
                "next".into(),
                &|| false
            )
            .unwrap_err_string()
            .contains("1,000")
        );
    }
    #[test]
    fn worker_is_latest_only_and_cancelled_confirmation_never_publishes() {
        let (d, docs) = fixture();
        let p = Arc::new(plan(d.path(), docs));
        let mut w = Worker::default();
        assert!(w.thread.is_none());
        w.request(Task::Confirm(p.clone()), Arc::new(|| {}))
            .unwrap();
        w.cancel();
        let id = w
            .request(
                Task::Preview {
                    root: d.path().into(),
                    docs: Documents::new(),
                    options: opts("missing"),
                    replacement: "next".into(),
                },
                Arc::new(|| {}),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(f) = w.poll()
                && f.id == id
            {
                match f.result.unwrap() {
                    Output::Preview(p) => assert!(p.files.is_empty()),
                    _ => panic!("wrong task"),
                };
                break;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
    }
}
