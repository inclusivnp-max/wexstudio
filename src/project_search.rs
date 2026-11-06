//! Project text search: immutable open-buffer overlays, bounded disk snapshots, one lazy worker.
use crate::{
    editor::RenderSnapshot,
    editor_search::{self, SearchOptions},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    ops::Range,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};
pub const FILE_BYTES: usize = 8 * 1024 * 1024;
pub const TOTAL_BYTES: usize = 256 * 1024 * 1024;
pub const FILE_LIMIT: usize = 20_000;
pub const MATCH_LIMIT: usize = 5_000;
const ENTRY_LIMIT: usize = 100_000;
const EXCLUDED: &[&str] = &[
    ".git",
    ".gradle",
    ".wex",
    "build",
    "target",
    "node_modules",
    ".verification",
];
#[derive(Clone)]
pub struct Overlay {
    pub text: RenderSnapshot,
    pub dirty: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub range: Range<usize>,
    pub line: usize,
    pub column: usize,
    pub snippet: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct FoundFile {
    pub path: PathBuf,
    #[serde(serialize_with = "serialize_hash")]
    pub hash: blake3::Hash,
    pub unsaved: bool,
    pub hits: Vec<Hit>,
}
#[derive(Default, Clone, Debug, Serialize)]
pub struct Stats {
    pub files: usize,
    pub bytes: usize,
    pub matches: usize,
    pub overlays: usize,
    pub skipped_binary: usize,
    pub skipped_large: usize,
    pub skipped_special: usize,
    pub excluded_overlays: usize,
    pub limited: bool,
    pub errors: Vec<String>,
    pub read_errors: usize,
}
#[derive(Default, Debug, Serialize)]
pub struct Results {
    pub files: Vec<FoundFile>,
    pub stats: Stats,
    pub worker_thread: String,
}
fn serialize_hash<S: serde::Serializer>(
    hash: &blake3::Hash,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&hash.to_hex())
}
fn allowed(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c,Component::Normal(n) if !EXCLUDED.iter().any(|x| n==*x)))
}
/// Root fd + O_NOFOLLOW at EVERY component; regular files only, no symlink or FIFO reads.
fn scoped_file(root: &File, path: &Path) -> Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    ensure!(allowed(path), "Excluded/non-relative search path");
    let parts = path.components().collect::<Vec<_>>();
    let mut parent = root.try_clone()?;
    for (i, part) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(part.as_os_str().as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: owned live parent fd and terminated single component; transfer new fd once.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("Open {}", path.display()));
        }
        parent = unsafe { File::from_raw_fd(fd) };
    }
    ensure!(parent.metadata()?.is_file(), "Not a regular text file");
    Ok(parent)
}
pub(crate) fn read_text(
    root: &File,
    relative: &Path,
    cancel: &impl Fn() -> bool,
    total: &mut usize,
) -> Result<Vec<u8>> {
    let mut f = scoped_file(root, relative)?;
    ensure!(
        f.metadata()?.len() <= FILE_BYTES as u64,
        "File exceeds 8 MiB"
    );
    let mut out = Vec::new();
    let mut buf = [0; 65536];
    loop {
        ensure!(!cancel(), "Project search cancelled");
        ensure!(*total < TOTAL_BYTES, "Read budget reached");
        let left = (TOTAL_BYTES - *total).min(buf.len());
        let n = f.read(&mut buf[..left])?;
        *total += n;
        if n == 0 {
            break;
        }
        ensure!(out.len() + n <= FILE_BYTES, "File grew past 8 MiB");
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}
fn error(stats: &mut Stats, path: &Path, e: impl std::fmt::Display) {
    stats.read_errors += 1;
    if stats.errors.len() < 16 {
        stats.errors.push(format!("{}: {e}", path.display()));
    }
}
pub fn search(
    root: &Path,
    overlays: &BTreeMap<PathBuf, Overlay>,
    options: &SearchOptions,
    cancel: impl Fn() -> bool,
) -> Result<Results> {
    ensure!(
        options.within.is_none(),
        "Project search has no active-selection scope"
    );
    ensure!(options.query.len() <= 16 * 1024, "Query exceeds 16 KiB");
    let mut out = Results {
        worker_thread: format!("{:?}", thread::current().id()),
        ..Default::default()
    };
    if options.query.is_empty() {
        return Ok(out);
    }
    let re = editor_search::compile(options)?;
    let root = root.canonicalize()?;
    let fd = File::open(&root)?;
    ensure!(fd.metadata()?.is_dir(), "Project root is not a directory");
    let mut open = BTreeMap::new();
    for (path, o) in overlays {
        ensure!(!cancel(), "Project search cancelled");
        if let Ok(rel) = path.strip_prefix(&root)
            && allowed(rel)
        {
            open.insert(path.clone(), o);
        } else {
            out.stats.excluded_overlays += 1;
        }
    }
    // Open buffers, including deleted-on-disk documents, always win over disk and are searched first.
    let mut paths = open.keys().cloned().collect::<Vec<_>>();
    let mut entries = 0;
    for e in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .max_open(16)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !EXCLUDED.iter().any(|x| e.file_name() == *x))
    {
        ensure!(!cancel(), "Project search cancelled");
        entries += 1;
        if entries > ENTRY_LIMIT {
            out.stats.limited = true;
            break;
        }
        let e = match e {
            Ok(e) => e,
            Err(e) => {
                error(&mut out.stats, Path::new("inventory"), e);
                continue;
            }
        };
        if e.file_type().is_file() {
            if !open.contains_key(e.path()) {
                paths.push(e.into_path());
            }
        } else if !e.file_type().is_dir() {
            out.stats.skipped_special += 1;
        }
        if paths.len() > FILE_LIMIT {
            out.stats.limited = true;
            break;
        }
    }
    // Preserve overlay priority at limits, deterministic file/match order within each source kind.
    let n = open.len();
    paths[n..].sort();
    for path in paths.into_iter().take(FILE_LIMIT) {
        ensure!(!cancel(), "Project search cancelled");
        if out.stats.bytes == TOTAL_BYTES {
            out.stats.limited = true;
            break;
        }
        let overlay = open.get(&path);
        let text = if let Some(o) = overlay {
            if o.text.rope().len_bytes() > FILE_BYTES {
                out.stats.skipped_large += 1;
                continue;
            }
            if out.stats.bytes.saturating_add(o.text.rope().len_bytes()) > TOTAL_BYTES {
                out.stats.limited = true;
                break;
            }
            out.stats.bytes += o.text.rope().len_bytes();
            let mut s = String::with_capacity(o.text.rope().len_bytes());
            for c in o.text.rope().chunks() {
                ensure!(!cancel(), "Project search cancelled");
                s.push_str(c);
            }
            out.stats.overlays += 1;
            s
        } else {
            let rel = path.strip_prefix(&root)?;
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.len() > FILE_BYTES as u64) {
                out.stats.skipped_large += 1;
                continue;
            }
            let bytes = match read_text(&fd, rel, &cancel, &mut out.stats.bytes) {
                Ok(b) => b,
                Err(e) => {
                    ensure!(!cancel(), "Project search cancelled");
                    if out.stats.bytes == TOTAL_BYTES {
                        out.stats.limited = true;
                        break;
                    }
                    error(&mut out.stats, rel, e);
                    continue;
                }
            };
            if bytes.contains(&0) {
                out.stats.skipped_binary += 1;
                continue;
            }
            match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    out.stats.skipped_binary += 1;
                    continue;
                }
            }
        };

        out.stats.files += 1;
        let rope = ropey::Rope::from_str(&text);
        let mut hits = vec![];
        for m in re.find_iter(&text) {
            ensure!(!cancel(), "Project search cancelled");
            if !editor_search::in_scope(&text, m.start(), m.end(), options, &rope) {
                continue;
            }
            if out.stats.matches == MATCH_LIMIT {
                out.stats.limited = true;
                break;
            }
            let a = rope.byte_to_char(m.start());
            let b = rope.byte_to_char(m.end());
            let line = rope.char_to_line(a);
            let column = a - rope.line_to_char(line);
            // Bounded snippet around the match, even on a multi-megabyte line; offsets stay exact.
            let row = rope.line(line);
            let from = column.saturating_sub(40);
            let to = (from + 160).min(row.len_chars());
            let snippet = format!(
                "{}{}{}",
                if from > 0 { "…" } else { "" },
                row.slice(from..to)
                    .to_string()
                    .trim_end_matches(['\r', '\n']),
                if to < row.len_chars() { "…" } else { "" }
            );
            hits.push(Hit {
                range: a..b,
                line: line + 1,
                column: column + 1,
                snippet,
            });
            out.stats.matches += 1;
        }
        if !hits.is_empty() {
            out.files.push(FoundFile {
                path,
                hash: blake3::hash(text.as_bytes()),
                unsaved: overlay.is_some_and(|o| o.dirty),
                hits,
            });
        }
        if out.stats.limited && out.stats.matches == MATCH_LIMIT {
            break;
        }
    }
    if open.len() > FILE_LIMIT {
        out.stats.limited = true
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}
struct Job {
    id: u64,
    root: PathBuf,
    overlays: BTreeMap<PathBuf, Overlay>,
    options: SearchOptions,
}
pub struct Finished {
    pub id: u64,
    pub result: Result<Results, String>,
}
struct Shared {
    latest: AtomicU64,
    active: AtomicU64,
    stop: AtomicBool,
    job: Mutex<Option<Job>>,
    done: Mutex<Option<Finished>>,
    changed: Condvar,
}
pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<thread::JoinHandle<()>>,
    next: u64,
}
impl Default for Worker {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                latest: AtomicU64::new(0),
                active: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                job: Mutex::new(None),
                done: Mutex::new(None),
                changed: Condvar::new(),
            }),
            thread: None,
            next: 0,
        }
    }
}
impl Worker {
    pub fn request(
        &mut self,
        root: PathBuf,
        overlays: BTreeMap<PathBuf, Overlay>,
        options: SearchOptions,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<u64> {
        if self.thread.is_none() {
            let s = self.shared.clone();
            self.thread = Some(
                thread::Builder::new()
                    .name("wex-project-search".into())
                    .spawn(move || {
                        loop {
                            let mut g = s.job.lock().unwrap();
                            while g.is_none() && !s.stop.load(Ordering::Acquire) {
                                g = s.changed.wait(g).unwrap()
                            }
                            if s.stop.load(Ordering::Acquire) {
                                break;
                            }
                            let j = g.take().unwrap();
                            drop(g);
                            s.active.store(j.id, Ordering::Release);
                            let cancelled = || {
                                s.stop.load(Ordering::Acquire)
                                    || s.latest.load(Ordering::Acquire) != j.id
                            };
                            let result = search(&j.root, &j.overlays, &j.options, cancelled)
                                .map_err(|e| format!("{e:#}"));
                            s.active.store(0, Ordering::Release);
                            if !cancelled() {
                                let old = s
                                    .done
                                    .lock()
                                    .unwrap()
                                    .replace(Finished { id: j.id, result });
                                drop(old);
                                wake();
                            }
                        }
                    })
                    .context("Start project search worker")?,
            );
        }
        let id = self
            .next
            .checked_add(1)
            .context("Project search clock exhausted")?;
        self.next = id;
        self.shared.latest.store(id, Ordering::Release);
        let old = self.shared.job.lock().unwrap().replace(Job {
            id,
            root,
            overlays,
            options,
        });
        drop(old);
        self.shared.changed.notify_one();
        Ok(id)
    }
    pub fn cancel(&mut self) {
        self.shared.latest.store(0, Ordering::Release);
        let old = self.shared.job.lock().unwrap().take();
        drop(old);
        self.shared.changed.notify_one();
    }
    pub fn poll(&self) -> Option<Finished> {
        self.shared.done.try_lock().ok()?.take()
    }
    pub fn active_id(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }
    pub fn started(&self) -> bool {
        self.thread.is_some()
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
    use crate::editor::Document;
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };
    fn opts(q: &str) -> SearchOptions {
        SearchOptions {
            query: q.into(),
            case_sensitive: true,
            ..Default::default()
        }
    }
    fn overlay(p: PathBuf, s: &str) -> (PathBuf, Overlay) {
        (
            p,
            Overlay {
                text: Document::new(s).unwrap().render_snapshot(),
                dirty: true,
            },
        )
    }
    #[test]
    fn disk_and_unsaved_active_parked_and_deleted_overlays_have_one_exact_result() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let a = root.join("A.kt");
        let b = root.join("B.xml");
        let c = root.join("C.txt");
        std::fs::write(&a, "old disk").unwrap();
        std::fs::write(&b, "disk needle").unwrap();
        std::fs::write(&c, "closed needle\r\n😀 needle\n").unwrap();
        let overlays = BTreeMap::from([
            overlay(a.clone(), "active needle"),
            overlay(b.clone(), "parked needle"),
            overlay(root.join("Deleted.md"), "deleted needle"),
        ]);
        let r = search(root, &overlays, &opts("needle"), || false).unwrap();
        assert_eq!(r.stats.matches, 5);
        assert_eq!(r.stats.overlays, 3);
        assert_eq!(r.files.len(), 4);
        assert_eq!(r.files[0].hash, blake3::hash(b"active needle"));
        assert!(r.files[0].unsaved);
        assert_eq!(r.files[2].hits[1].line, 2);
        assert_eq!(r.files[2].hits[1].column, 3);
        assert_eq!(r.files[2].hits[1].range, 17..23);
        assert_eq!(std::fs::read_to_string(a).unwrap(), "old disk");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "disk needle");
    }
    #[test]
    fn all_text_extensions_hidden_files_and_unicode_word_regex_multiline() {
        let d = tempfile::tempdir().unwrap();
        for n in ["a.json", "b.xml", "c.txt", "README", ".hidden"] {
            std::fs::write(d.path().join(n), "Привет приветствие привет\n😀 привет\n").unwrap();
        }
        let mut o = opts("привет");
        o.case_sensitive = false;
        o.whole_word = true;
        let r = search(d.path(), &BTreeMap::new(), &o, || false).unwrap();
        assert_eq!(r.stats.matches, 15);
        assert_eq!(r.stats.files, 5);
        assert_eq!(r.files[0].hits[2].column, 3);
        o.query = "(?m)^😀 (привет)$".into();
        o.regex = true;
        o.whole_word = false;
        let r = search(d.path(), &BTreeMap::new(), &o, || false).unwrap();
        assert_eq!(r.stats.matches, 5);
        assert_eq!(r.files[0].hits[0].range, 26..34);
    }
    #[test]
    fn generated_binary_large_and_special_files_are_explicitly_skipped() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "needle").unwrap();
        for n in EXCLUDED {
            let p = d.path().join(n);
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("bad.kt"), "needle").unwrap();
        }
        symlink(outside.path(), d.path().join("link-dir")).unwrap();
        symlink(outside.path().join("secret.txt"), d.path().join("link.txt")).unwrap();
        let f = File::create(d.path().join("huge.txt")).unwrap();
        f.set_len(FILE_BYTES as u64 + 1).unwrap();
        std::fs::write(d.path().join("zero.bin"), b"needle\0").unwrap();
        std::fs::write(d.path().join("bad.bin"), [0xff, 0xfe]).unwrap();
        std::fs::write(d.path().join("text"), "needle").unwrap();
        let n =
            std::ffi::CString::new(d.path().join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(n.as_ptr(), 0o600) }, 0);
        let r = search(d.path(), &BTreeMap::new(), &opts("needle"), || false).unwrap();
        assert_eq!(r.stats.matches, 1);
        assert_eq!(r.stats.skipped_binary, 2);
        assert_eq!(r.stats.skipped_large, 1);
        assert_eq!(r.stats.skipped_special, 3);
    }
    #[test]
    fn outside_and_owned_overlays_cannot_override_root_scope() {
        let d = tempfile::tempdir().unwrap();
        let overlays = BTreeMap::from([
            overlay(d.path().join("../outside.kt"), "needle"),
            overlay(d.path().join(".wex/a.kt"), "needle"),
            overlay(PathBuf::from("/outside.kt"), "needle"),
        ]);
        let r = search(d.path(), &overlays, &opts("needle"), || false).unwrap();
        assert_eq!(r.stats.matches, 0);
        assert_eq!(r.stats.excluded_overlays, 3);
    }
    #[test]
    fn scoped_reader_rejects_intermediate_symlinks_parent_and_nonregular() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("a"), "needle").unwrap();
        symlink(out.path(), d.path().join("dir")).unwrap();
        let root = File::open(d.path()).unwrap();
        assert!(scoped_file(&root, Path::new("dir/a")).is_err());
        assert!(scoped_file(&root, Path::new("../a")).is_err());
        assert!(scoped_file(&root, Path::new(".")).is_err());
    }
    #[test]
    fn regex_errors_empty_query_and_oversized_query_scope_are_not_empty_success() {
        let d = tempfile::tempdir().unwrap();
        let mut o = opts("[");
        o.regex = true;
        assert!(search(d.path(), &BTreeMap::new(), &o, || false).is_err());
        o.query.clear();
        assert_eq!(
            search(d.path(), &BTreeMap::new(), &o, || false)
                .unwrap()
                .stats
                .files,
            0
        );
        o.query = "x".repeat(16385);
        assert!(search(d.path(), &BTreeMap::new(), &o, || false).is_err());
        o = opts("x");
        o.within = Some(0..1);
        assert!(search(d.path(), &BTreeMap::new(), &o, || false).is_err());
    }
    #[test]
    fn match_cap_zero_width_and_bounded_long_line_snippets() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a"), "x ".repeat(MATCH_LIMIT + 1)).unwrap();
        let r = search(d.path(), &BTreeMap::new(), &opts("x"), || false).unwrap();
        assert_eq!(r.stats.matches, MATCH_LIMIT);
        assert!(r.stats.limited);
        assert!(
            r.files[0]
                .hits
                .iter()
                .all(|h| h.snippet.chars().count() <= 162)
        );
        std::fs::write(d.path().join("a"), "😀 a\n").unwrap();
        let mut o = opts("^");
        o.regex = true;
        let r = search(d.path(), &BTreeMap::new(), &o, || false).unwrap();
        assert_eq!(
            r.files[0]
                .hits
                .iter()
                .map(|h| h.range.clone())
                .collect::<Vec<_>>(),
            vec![0..0, 4..4]
        );
    }
    #[test]
    fn byte_budget_counts_binary_reads_and_stops_before_more_bytes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a"), b"\0\0abcdef").unwrap();
        let root = File::open(d.path()).unwrap();
        let mut total = TOTAL_BYTES - 3;
        assert!(read_text(&root, Path::new("a"), &|| false, &mut total).is_err());
        assert_eq!(total, TOTAL_BYTES);
    }
    #[test]
    fn cancellation_is_checked_during_inventory_and_overlay_copy() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        let overlays = BTreeMap::from([overlay(p, &"needle\n".repeat(10000))]);
        assert!(search(d.path(), &overlays, &opts("needle"), || true).is_err());
        let n = Cell::new(0);
        assert!(
            search(d.path(), &overlays, &opts("needle"), || {
                n.set(n.get() + 1);
                n.get() > 10
            })
            .is_err()
        );
        assert!(n.get() > 10);
    }
    #[test]
    fn file_cap_keeps_overlays_first_and_reports_partial() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..FILE_LIMIT {
            File::create(d.path().join(format!("{i:05}.txt"))).unwrap();
        }
        let p = d.path().join("zz-deleted.kt");
        let o = BTreeMap::from([overlay(p.clone(), "needle")]);
        let r = search(d.path(), &o, &opts("needle"), || false).unwrap();
        assert!(r.stats.limited);
        assert_eq!(r.stats.files, FILE_LIMIT);
        assert_eq!(r.files[0].path, p);
    }
    fn wait(w: &Worker, id: u64) -> Finished {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(e) = w.poll()
                && e.id == id
            {
                return e;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn lazy_worker_latest_request_and_cancel_do_not_publish_old_results() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a"), "old latest").unwrap();
        let mut w = Worker::default();
        assert!(!w.started());
        let mut id = 0;
        for i in 0..20 {
            id = w
                .request(
                    d.path().into(),
                    BTreeMap::new(),
                    opts(if i == 19 { "latest" } else { "old" }),
                    Arc::new(|| {}),
                )
                .unwrap();
        }
        let e = wait(&w, id).result.unwrap();
        assert_eq!(e.stats.matches, 1);
        assert_eq!(e.files[0].hits[0].snippet, "old latest");
        assert_ne!(e.worker_thread, format!("{:?}", thread::current().id()));
        w.cancel();
        assert!(w.started());
        let id = w
            .request(
                d.path().into(),
                BTreeMap::new(),
                opts("[").into_regex(),
                Arc::new(|| {}),
            )
            .unwrap();
        assert!(wait(&w, id).result.is_err());
    }
    trait RegexOption {
        fn into_regex(self) -> Self;
    }
    impl RegexOption for SearchOptions {
        fn into_regex(mut self) -> Self {
            self.regex = true;
            self
        }
    }
}

#[cfg(test)]
mod active_cancel_test {
    use super::*;
    #[test]
    fn actual_in_flight_reader_cancel_then_latest_search_recovers() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("large.txt"), vec![b'x'; FILE_BYTES]).unwrap();
        let mut w = Worker::default();
        let options = SearchOptions {
            query: "ABSENT_CANCEL_PROBE".into(),
            ..Default::default()
        };
        let id = w
            .request(
                d.path().into(),
                BTreeMap::new(),
                options.clone(),
                Arc::new(|| {}),
            )
            .unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while w.active_id() != id {
            assert!(std::time::Instant::now() < end);
            thread::yield_now();
        }
        w.cancel();
        let latest = w
            .request(d.path().into(), BTreeMap::new(), options, Arc::new(|| {}))
            .unwrap();
        assert!(latest > id);
        loop {
            if let Some(e) = w.poll() {
                assert_eq!(e.id, latest);
                assert_eq!(e.result.unwrap().stats.matches, 0);
                break;
            }
            assert!(std::time::Instant::now() < end);
            thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
