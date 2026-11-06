//! Bounded local Git operations. All callers run this on a worker, never a paint pass.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
pub mod branches;
const OUTPUT: u64 = 16 * 1024 * 1024;
const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const TOTAL: u64 = 256 * 1024 * 1024;
const FILES: usize = 20_000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub original: Option<PathBuf>,
    pub index: char,
    pub worktree: char,
    pub conflict: bool,
    pub submodule: bool,
}
impl Entry {
    pub fn staged(&self) -> bool {
        self.index != '.' && self.index != '?'
    }
    pub fn unstaged(&self) -> bool {
        self.worktree != '.' || self.index == '?'
    }
    fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.path).chain(self.original.iter())
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub branch: String,
    pub entries: Vec<Entry>,
    head: Vec<u8>,
    index: PathBuf,
    index_bytes: Option<Vec<u8>>,
    identity: (u64, u64),
    files: BTreeMap<PathBuf, blake3::Hash>,
    positions: BTreeMap<PathBuf, usize>,
}
#[derive(Clone)]
pub enum Task {
    Refresh(PathBuf),
    Observe(Arc<Snapshot>, Option<PathBuf>, bool, bool),
    Diff(Arc<Snapshot>, usize, bool),
    Stage(Arc<Snapshot>, usize, bool),
    Commit(Arc<Snapshot>, String),
    Branches(PathBuf),
    Branch(Arc<branches::Catalog>, branches::Action),
}
pub struct Review {
    pub snapshot: Arc<Snapshot>,
    pub selected: Option<usize>,
    pub text: String,
    pub lines: Vec<String>,
}
pub enum Reply {
    Unchanged,
    Observed(Review),
    Status(Arc<Snapshot>),
    Diff(String, Vec<String>),
    Changed(Arc<Snapshot>, String),
    Branches(Arc<branches::Catalog>, String, bool),
}
fn command(root: &Path) -> Command {
    let mut c = Command::new("git");
    // Inherited Git routing must not redirect the selected project to another index/repo.
    for (k, _) in std::env::vars_os() {
        if k.as_bytes().starts_with(b"GIT_") {
            c.env_remove(k);
        }
    }
    c.current_dir(root)
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "color.ui=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C");
    c
}
fn run(
    root: &Path,
    args: &[&OsStr],
    index: Option<&Path>,
    input: Option<&[u8]>,
    stop: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    ensure!(!stop.load(Ordering::Acquire), "Git read stopped");
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut c = command(root);
    c.args(args)
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .stdin(Stdio::null())
        .process_group(0);
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    if let Some(bytes) = input {
        let mut f = tempfile::tempfile()?;
        f.write_all(bytes)?;
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(0))?;
        c.stdin(f);
    }
    let mut child = c.spawn().context("Start installed Git")?;
    let start = Instant::now();
    let limit = if input.is_some() {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(30)
    };
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if stop.load(Ordering::Acquire)
            || start.elapsed() > limit
            || stdout.metadata()?.len() > OUTPUT
            || stderr.metadata()?.len() > OUTPUT
        {
            // Own process group only: hooks/helpers cannot retain pipes or outlive a timed-out job.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!(
                "Git stopped, timed out or exceeded 16MiB output; refresh actual repository state"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    fn bytes(mut f: File) -> Result<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        f.seek(SeekFrom::Start(0))?;
        let mut b = vec![];
        f.take(OUTPUT + 1).read_to_end(&mut b)?;
        ensure!(b.len() as u64 <= OUTPUT, "Git output exceeds 16MiB");
        Ok(b)
    }
    Ok((status.code().unwrap_or(-1), bytes(stdout)?, bytes(stderr)?))
}
fn os(s: &str) -> &OsStr {
    OsStr::new(s)
}
fn success(
    root: &Path,
    args: &[&OsStr],
    index: Option<&Path>,
    input: Option<&[u8]>,
    stop: &AtomicBool,
) -> Result<Vec<u8>> {
    let (code, out, err) = run(root, args, index, input, stop)?;
    ensure!(
        code == 0,
        "Git exited {code}: {}",
        String::from_utf8_lossy(&err)
            .chars()
            .take(8192)
            .collect::<String>()
    );
    Ok(out)
}
fn path_line(mut b: Vec<u8>) -> Result<PathBuf> {
    ensure!(b.pop() == Some(b'\n'), "Malformed Git path");
    Ok(PathBuf::from(OsString::from_vec(b)))
}
use std::os::unix::ffi::OsStringExt;
fn bounded_read(p: &Path) -> Result<Option<Vec<u8>>> {
    let f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(p)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        f.metadata()?.is_file(),
        "Not a regular Git file: {}",
        p.display()
    );
    let mut b = vec![];
    f.take(FILE_LIMIT + 1).read_to_end(&mut b)?;
    ensure!(
        b.len() as u64 <= FILE_LIMIT,
        "Git file exceeds 64MiB: {}",
        p.display()
    );
    Ok(Some(b))
}
use std::os::unix::fs::OpenOptionsExt;
fn valid_path(p: &Path) -> bool {
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}
fn fingerprint(root: &Path, p: &Path, budget: &mut u64) -> Result<blake3::Hash> {
    ensure!(valid_path(p), "Invalid Git relative path");
    let mut parent = root.to_path_buf();
    let parts = p.components().collect::<Vec<_>>();
    for c in &parts[..parts.len() - 1] {
        parent.push(c);
        let m = match fs::symlink_metadata(&parent) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(blake3::hash(b"missing"));
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(
            !m.file_type().is_symlink(),
            "Symlink parent changed: {}",
            p.display()
        );
    }
    let p = root.join(p);
    let mut h = blake3::Hasher::new();
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            h.update(b"missing");
        }
        Err(e) => return Err(e.into()),
        Ok(m) if m.file_type().is_symlink() => {
            h.update(b"link");
            h.update(fs::read_link(p)?.as_os_str().as_bytes());
        }
        Ok(m) if m.is_file() => {
            let b = bounded_read(&p)?.context("File disappeared during Git snapshot")?;
            *budget = budget
                .checked_add(b.len() as u64)
                .context("Git input overflow")?;
            ensure!(*budget <= TOTAL, "Changed Git inputs exceed 256MiB");
            h.update(&(m.mode() & 0o111).to_le_bytes());
            h.update(&b);
        }
        Ok(_) => {
            h.update(b"directory/submodule");
        }
    };
    Ok(h.finalize())
}
fn parse(b: &[u8]) -> Result<(String, Vec<Entry>)> {
    let mut records = b.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut branch = String::new();
    let mut entries = vec![];
    while let Some(r) = records.next() {
        if let Some(name) = r.strip_prefix(b"# branch.head ") {
            branch = String::from_utf8_lossy(name).into_owned();
            continue;
        }
        if r.starts_with(b"# ") {
            continue;
        }
        let (kind, fields) = match r.first() {
            Some(b'1') => (1, 9),
            Some(b'2') => (2, 10),
            Some(b'u') => (3, 11),
            Some(b'?') => (4, 2),
            _ => bail!("Unsupported Git status record"),
        };
        let v = r.splitn(fields, |b| *b == b' ').collect::<Vec<_>>();
        ensure!(v.len() == fields, "Incomplete Git status record");
        let path = PathBuf::from(OsString::from_vec(v[fields - 1].to_vec()));
        ensure!(valid_path(&path), "Invalid Git status path");
        let (index, worktree) = if kind == 4 {
            ('?', '?')
        } else {
            ensure!(v[1].len() == 2, "Invalid Git XY status");
            (v[1][0] as char, v[1][1] as char)
        };
        let original = if kind == 2 {
            let p = PathBuf::from(OsString::from_vec(
                records.next().context("Missing rename source")?.to_vec(),
            ));
            ensure!(valid_path(&p), "Invalid Git rename source");
            Some(p)
        } else {
            None
        };
        entries.push(Entry {
            path,
            original,
            index,
            worktree,
            conflict: kind == 3,
            submodule: kind != 4
                && (v[2].first() == Some(&b'S')
                    || v[3..fields - 1].contains(&b"160000".as_slice())),
        });
        ensure!(entries.len() <= FILES, "Git status exceeds 20,000 files");
    }
    Ok((branch, entries))
}
fn head(root: &Path, stop: &AtomicBool) -> Result<Vec<u8>> {
    let (c, o, _) = run(
        root,
        &[os("rev-parse"), os("--verify"), os("HEAD")],
        None,
        None,
        stop,
    )?;
    if c == 0 {
        let (code, reference, _) = run(
            root,
            &[os("symbolic-ref"), os("-q"), os("HEAD")],
            None,
            None,
            stop,
        )?;
        ensure!(code == 0 || code == 1, "Cannot inspect Git HEAD reference");
        // An equal OID on a different branch/detached HEAD is a different commit
        // destination. Bind that identity too, not merely the current tree.
        Ok([
            b"born:".as_slice(),
            &o,
            b"reference:",
            if code == 0 {
                &reference
            } else {
                b"(detached)\n"
            },
        ]
        .concat())
    } else {
        let b = success(
            root,
            &[os("symbolic-ref"), os("-q"), os("HEAD")],
            None,
            None,
            stop,
        )?;
        Ok([b"unborn:".as_slice(), &b].concat())
    }
}
fn status_bytes(root: &Path, stop: &AtomicBool) -> Result<Vec<u8>> {
    success(
        root,
        &[
            os("status"),
            os("--porcelain=v2"),
            os("-z"),
            os("--branch"),
            os("--untracked-files=all"),
        ],
        None,
        None,
        stop,
    )
}
pub fn refresh(project: &Path, stop: &AtomicBool) -> Result<Snapshot> {
    let root = path_line(
        success(
            project,
            &[os("rev-parse"), os("--show-toplevel")],
            None,
            None,
            stop,
        )
        .context("No Git repository found for this project")?,
    )?;
    let index = path_line(success(
        &root,
        &[
            os("rev-parse"),
            os("--path-format=absolute"),
            os("--git-path"),
            os("index"),
        ],
        None,
        None,
        stop,
    )?)?;
    let m = fs::metadata(&root)?;
    let identity = (m.dev(), m.ino());
    let h = head(&root, stop)?;
    let idx = bounded_read(&index)?;
    let raw = status_bytes(&root, stop)?;
    let (branch, entries) = parse(&raw)?;
    let mut files = BTreeMap::new();
    let mut budget = 0;
    for e in &entries {
        for p in e.paths() {
            files.insert(p.clone(), fingerprint(&root, p, &mut budget)?);
        }
    }
    ensure!(
        h == head(&root, stop)?
            && idx == bounded_read(&index)?
            && raw == status_bytes(&root, stop)?,
        "Repository changed while reading; Refresh"
    );
    let positions = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.path.clone(), i))
        .collect();
    Ok(Snapshot {
        root,
        branch,
        entries,
        head: h,
        index,
        index_bytes: idx,
        identity,
        positions,
        files,
    })
}
impl Snapshot {
    pub fn entry_index(&self, path: &Path) -> Option<usize> {
        self.positions
            .get(path.strip_prefix(&self.root).ok()?)
            .copied()
    }

    fn same_state(&self, other: &Self) -> bool {
        self.root == other.root
            && self.index == other.index
            && self.identity == other.identity
            && self.head == other.head
            && self.index_bytes == other.index_bytes
            && self.files == other.files
            && self.entries == other.entries
            && self.branch == other.branch
    }
    fn check(&self, stop: &AtomicBool) -> Result<()> {
        let m = fs::metadata(&self.root)?;
        ensure!(
            (m.dev(), m.ino()) == self.identity,
            "Repository identity changed"
        );
        let current = path_line(success(
            &self.root,
            &[
                os("rev-parse"),
                os("--path-format=absolute"),
                os("--git-path"),
                os("index"),
            ],
            None,
            None,
            stop,
        )?)?;
        ensure!(
            current == self.index
                && self.head == head(&self.root, stop)?
                && self.index_bytes == bounded_read(&self.index)?,
            "Git HEAD/index changed; Refresh before applying"
        );
        let mut budget = 0;
        for (p, h) in &self.files {
            ensure!(
                *h == fingerprint(&self.root, p, &mut budget)?,
                "File changed since review: {} · Refresh",
                p.display()
            );
        }
        Ok(())
    }
}
fn diff(s: &Snapshot, i: usize, staged: bool, stop: &AtomicBool) -> Result<String> {
    s.check(stop)?;
    let e = s.entries.get(i).context("No selected Git file")?;
    let mut args = vec![
        os("diff"),
        os("--no-ext-diff"),
        os("--no-textconv"),
        os("--no-color"),
    ];
    if staged {
        args.push(os("--cached"));
    }
    let untracked = e.index == '?';
    if untracked {
        ensure!(!staged, "Untracked file has no staged diff");
        args.extend([
            os("--no-index"),
            os("--"),
            os("/dev/null"),
            e.path.as_os_str(),
        ]);
    } else {
        args.push(os("--"));
        args.extend(e.paths().map(|p| p.as_os_str()));
    }
    let (code, b, err) = run(&s.root, &args, None, None, stop)?;
    ensure!(
        code == 0 || untracked && code == 1,
        "Git diff failed: {}",
        String::from_utf8_lossy(&err)
    );
    ensure!(
        b.len() <= 4 * 1024 * 1024,
        "Diff exceeds 4MiB; use an external viewer"
    );
    s.check(stop)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}
struct Lock(PathBuf, (u64, u64));
impl Drop for Lock {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.0).is_ok_and(|m| (m.dev(), m.ino()) == self.1) {
            let _ = fs::remove_file(&self.0);
        }
    }
}
fn stage(s: &Snapshot, i: usize, undo: bool, stop: &AtomicBool) -> Result<()> {
    let e = s.entries.get(i).context("No selected Git file")?;
    ensure!(
        !e.conflict && !e.submodule,
        "Resolve conflicts/submodule changes in Git before staging here"
    );
    ensure!(
        if undo { e.staged() } else { e.unstaged() },
        "Selected file has no applicable changes"
    );
    let lock = PathBuf::from(OsString::from_vec(
        [s.index.as_os_str().as_bytes(), b".lock"].concat(),
    ));
    let mut locked = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .context("Git index is locked; finish the other Git operation and Refresh")?;
    if let Ok(meta) = fs::metadata(&s.index) {
        locked.set_permissions(meta.permissions())?;
    }
    let meta = locked.metadata()?;
    let _guard = Lock(lock.clone(), (meta.dev(), meta.ino()));
    s.check(stop)?;
    let index_modified = if s.index_bytes.is_some() {
        Some(fs::metadata(&s.index)?.modified()?)
    } else {
        None
    };
    let temp =
        tempfile::NamedTempFile::new_in(s.index.parent().context("Missing index directory")?)?;
    let private = temp.into_temp_path();
    if let Some(b) = &s.index_bytes {
        fs::write(&private, b)?;
        // Git's stat cache uses the original index timestamp to detect racily
        // clean entries. A freshly written copy can otherwise skip content and
        // required clean filters on an equal-size edit. Preserve that checkpoint.
        File::open(&private)?
            .set_modified(index_modified.context("Missing Git index timestamp")?)?;
    } else {
        fs::remove_file(&private)?;
        success(
            &s.root,
            &[os("read-tree"), os("--empty")],
            Some(&private),
            None,
            stop,
        )?;
    }
    let mut args = if !undo {
        vec![os("add"), os("--")]
    } else if s.head.starts_with(b"unborn:") {
        vec![os("rm"), os("--cached"), os("--ignore-unmatch"), os("--")]
    } else {
        vec![os("reset"), os("-q"), os("HEAD"), os("--")]
    };
    args.extend(e.paths().map(|p| p.as_os_str()));
    let result = success(&s.root, &args, Some(&private), None, stop);
    // Clean only our private index lock, never a foreign real index.lock.
    let _ = fs::remove_file(PathBuf::from(OsString::from_vec(
        [private.as_os_str().as_bytes(), b".lock"].concat(),
    )));
    result?;
    s.check(stop)?;
    let bytes = bounded_read(&private)?.context("Missing private Git index")?;
    locked.write_all(&bytes)?;
    locked.sync_all()?;
    fs::rename(&lock, &s.index)?;
    File::open(s.index.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn reviewed_diff(
    s: &Snapshot,
    i: usize,
    cached: bool,
    stop: &AtomicBool,
) -> Result<(String, Vec<String>)> {
    let text = diff(s, i, cached, stop)?;
    ensure!(
        text.lines().all(|l| l.len() <= 16384),
        "Diff contains a line over16KiB; use an external viewer"
    );
    let lines = text.lines().map(str::to_owned).collect();
    Ok((text, lines))
}
pub fn execute(task: Task, stop: &AtomicBool) -> Result<Reply> {
    match task {
        Task::Branches(root) => Ok(Reply::Branches(
            Arc::new(branches::read(&root, stop)?),
            "Local branches updated · no files saved".into(),
            false,
        )),
        Task::Branch(c, action) => {
            let (next, note) = branches::apply(&c, action, stop)?;
            Ok(Reply::Branches(Arc::new(next), note, true))
        }
        Task::Refresh(p) => Ok(Reply::Status(Arc::new(refresh(&p, stop)?))),
        Task::Observe(old, path, staged, force) => {
            let next = Arc::new(refresh(&old.root, stop)?);
            if !force && old.same_state(&next) {
                return Ok(Reply::Unchanged);
            }
            let selected = path.as_ref().and_then(|p| {
                next.entries
                    .iter()
                    .position(|e| &e.path == p && if staged { e.staged() } else { e.unstaged() })
            });
            let (text, lines) = if let Some(i) = selected {
                reviewed_diff(&next, i, staged, stop)?
            } else {
                (String::new(), vec![])
            };
            Ok(Reply::Observed(Review {
                snapshot: next,
                selected,
                text,
                lines,
            }))
        }
        Task::Diff(s, i, cached) => {
            let (text, lines) = reviewed_diff(&s, i, cached, stop)?;
            Ok(Reply::Diff(text, lines))
        }
        Task::Stage(s, i, undo) => {
            stage(&s, i, undo, stop)?;
            Ok(Reply::Changed(
                Arc::new(refresh(&s.root, stop)?),
                if undo {
                    "File unstaged · working files kept"
                } else {
                    "Disk version staged · editor buffers kept"
                }
                .into(),
            ))
        }
        Task::Commit(s, message) => {
            ensure!(
                !message.trim().is_empty() && message.len() <= 16 * 1024,
                "Enter a commit message (max16KiB)"
            );
            ensure!(
                s.entries.iter().any(Entry::staged),
                "No staged changes to commit"
            );
            ensure!(
                !s.entries.iter().any(|e| e.conflict),
                "Resolve conflicts before committing"
            );
            s.check(stop)?;
            let out = success(
                &s.root,
                &[os("commit"), os("-F"), os("-")],
                None,
                Some(message.as_bytes()),
                stop,
            )
            .context("Commit did not finish successfully; Refresh to inspect actual Git state")?;
            Ok(Reply::Changed(
                Arc::new(refresh(&s.root, stop)?),
                String::from_utf8_lossy(&out)
                    .lines()
                    .next()
                    .unwrap_or("Commit created")
                    .chars()
                    .take(180)
                    .collect(),
            ))
        }
    }
}
#[derive(Default)]
pub struct Worker {
    rx: Option<mpsc::Receiver<Result<Reply, String>>>,
    stop: Option<Arc<AtomicBool>>,
}
impl Worker {
    pub fn cancel(&self) {
        if let Some(c) = &self.stop {
            c.store(true, Ordering::Release);
        }
    }
    pub fn start(&mut self, task: Task, wake: Arc<dyn Fn() + Send + Sync>) -> bool {
        if self.rx.is_some() {
            return false;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let c = stop.clone();
        std::thread::spawn(move || {
            let reply = execute(task, &c).map_err(|e| format!("{e:#}"));
            let _ = tx.send(reply);
            wake();
        });
        self.rx = Some(rx);
        self.stop = Some(stop);
        true
    }
    pub fn poll(&mut self) -> Option<Result<Reply, String>> {
        let r = match self.rx.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(_) => Err("Git worker disconnected; Refresh".into()),
        };
        self.rx = None;
        self.stop = None;
        Some(r)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(s) = &self.stop {
            s.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stop() -> AtomicBool {
        AtomicBool::new(false)
    }
    pub(super) struct Repo(tempfile::TempDir);
    impl Repo {
        pub(super) fn new(commit: bool) -> Self {
            let r = Self(tempfile::tempdir().unwrap());
            r.git(&["init", "-q"]);
            r.git(&["config", "user.name", "Wex Fixture"]);
            r.git(&["config", "user.email", "fixture@localhost"]);
            r.git(&["config", "commit.gpgsign", "false"]);
            if commit {
                fs::write(r.p().join("a.txt"), "before\n").unwrap();
                r.git(&["add", "a.txt"]);
                r.git(&["commit", "-qm", "initial"]);
            }
            r
        }
        pub(super) fn p(&self) -> &Path {
            self.0.path()
        }
        pub(super) fn git(&self, args: &[&str]) -> Vec<u8> {
            success(
                self.p(),
                &args.iter().map(|s| os(s)).collect::<Vec<_>>(),
                None,
                None,
                &stop(),
            )
            .unwrap()
        }
        fn snap(&self) -> Arc<Snapshot> {
            Arc::new(refresh(self.p(), &stop()).unwrap())
        }
    }
    fn change(s: Arc<Snapshot>, i: usize, undo: bool) -> Arc<Snapshot> {
        match execute(Task::Stage(s, i, undo), &stop()).unwrap() {
            Reply::Changed(s, _) => s,
            _ => panic!(),
        }
    }
    #[test]
    fn real_modified_diff_stage_unstage_and_commit() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "after\n").unwrap();
        let s = r.snap();
        assert_eq!(s.entries.len(), 1);
        assert!(diff(&s, 0, false, &stop()).unwrap().contains("+after"));
        let s = change(s, 0, false);
        assert!(s.entries[0].staged());
        assert!(diff(&s, 0, true, &stop()).unwrap().contains("+after"));
        let s = change(s, 0, true);
        assert!(!s.entries[0].staged());
        let s = change(s, 0, false);
        execute(Task::Commit(s, "reviewed change\n".into()), &stop()).unwrap();
        assert!(r.snap().entries.is_empty());
        assert_eq!(r.git(&["log", "-1", "--format=%s"]), b"reviewed change\n");
    }
    #[test]
    fn unborn_index_stage_unstage_and_initial_commit() {
        let r = Repo::new(false);
        fs::write(r.p().join("new.txt"), "new\n").unwrap();
        let s = change(r.snap(), 0, false);
        assert!(s.entries[0].staged());
        let s = change(s, 0, true);
        assert_eq!(s.entries[0].index, '?');
        assert!(
            fs::read_to_string(r.p().join("new.txt"))
                .unwrap()
                .contains("new")
        );
        let s = change(s, 0, false);
        execute(Task::Commit(s, "initial from UI".into()), &stop()).unwrap();
        assert!(r.snap().entries.is_empty());
    }
    #[test]
    fn literal_untracked_unicode_newline_and_non_utf8_paths() {
        let r = Repo::new(true);
        for n in [
            OsString::from("猫 \n[abc].txt"),
            OsString::from_vec(b"bad-\xff.txt".to_vec()),
        ] {
            fs::write(r.p().join(n), "hello\n").unwrap();
        }
        let mut s = r.snap();
        assert_eq!(s.entries.len(), 2);
        assert!(diff(&s, 0, false, &stop()).unwrap().contains("+hello"));
        for _ in 0..2 {
            let i = s.entries.iter().position(|e| e.index == '?').unwrap();
            s = change(s, i, false);
        }
        assert_eq!(s.entries.iter().filter(|e| e.staged()).count(), 2);
    }
    #[test]
    fn rename_and_deletion_unstage_only_selected_paths() {
        let r = Repo::new(true);
        r.git(&["mv", "a.txt", "renamed.txt"]);
        let s = r.snap();
        assert_eq!(s.entries[0].original, Some("a.txt".into()));
        let s = change(s, 0, true);
        assert!(s.entries.iter().all(|e| !e.staged()));
        assert!(r.p().join("renamed.txt").exists());
        fs::remove_file(r.p().join("renamed.txt")).unwrap();
        let s = r.snap();
        let i = s
            .entries
            .iter()
            .position(|e| e.path == Path::new("a.txt"))
            .unwrap();
        let s = change(s, i, false);
        assert!(s.entries[i].staged());
    }
    #[test]
    fn external_body_change_rejects_whole_operation() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        fs::write(r.p().join("a.txt"), "external\n").unwrap();
        assert!(execute(Task::Stage(s, 0, false), &stop()).is_err());
        assert!(r.git(&["diff", "--cached"]).is_empty());
        assert!(!r.p().join(".git/index.lock").exists());
    }
    #[test]
    fn external_index_and_head_change_rejected() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        r.git(&["add", "a.txt"]);
        assert!(execute(Task::Stage(s, 0, false), &stop()).is_err());
        let s = r.snap();
        r.git(&["commit", "-qm", "external"]);
        assert!(execute(Task::Commit(s, "old review".into()), &stop()).is_err());
    }
    #[test]
    fn foreign_lock_is_not_removed() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        let lock = r.p().join(".git/index.lock");
        fs::write(&lock, "foreign").unwrap();
        assert!(
            execute(Task::Stage(s, 0, false), &stop())
                .unwrap_err_string()
                .contains("locked")
        );
        assert_eq!(fs::read(lock).unwrap(), b"foreign");
    }
    trait ErrString {
        fn unwrap_err_string(self) -> String;
    }
    impl ErrString for Result<Reply> {
        fn unwrap_err_string(self) -> String {
            match self {
                Err(e) => format!("{e:#}"),
                _ => panic!("expected error"),
            }
        }
    }
    #[test]
    fn failing_filter_does_not_publish_private_index() {
        let r = Repo::new(true);
        fs::write(r.p().join(".gitattributes"), "*.txt filter=reject\n").unwrap();
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        r.git(&["config", "filter.reject.clean", "false"]);
        r.git(&["config", "filter.reject.required", "true"]);
        let old = fs::read(r.p().join(".git/index")).unwrap();
        let i = s
            .entries
            .iter()
            .position(|e| e.path == Path::new("a.txt"))
            .unwrap();
        assert!(execute(Task::Stage(s, i, false), &stop()).is_err());
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old);
        assert!(!r.p().join(".git/index.lock").exists());
    }
    #[test]
    fn private_index_keeps_git_racy_timestamp_content_and_filter_checks() {
        let r = Repo::new(false);
        let file = r.p().join("a.txt");
        let when = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        fs::write(&file, "before\n").unwrap();
        File::open(&file).unwrap().set_modified(when).unwrap();
        r.git(&["add", "a.txt"]);
        r.git(&["commit", "-qm", "initial"]);
        assert!(
            String::from_utf8(r.git(&["ls-files", "--debug"]))
                .unwrap()
                .contains("size: 7")
        );
        r.git(&["config", "core.trustctime", "false"]);
        r.git(&["config", "core.checkStat", "minimal"]);
        let index = r.p().join(".git/index");
        fs::write(r.p().join(".gitattributes"), "*.txt filter=reject\n").unwrap();
        fs::write(&file, "review\n").unwrap(); // Same length as the committed bytes.
        File::open(&file).unwrap().set_modified(when).unwrap();
        File::open(&index).unwrap().set_modified(when).unwrap();
        let s = r.snap();
        r.git(&["config", "filter.reject.clean", "false"]);
        r.git(&["config", "filter.reject.required", "true"]);
        let old = fs::read(&index).unwrap();
        // The real original index forces Git to examine the racily clean entry.
        let (code, _, _) = run(
            r.p(),
            &[os("add"), os("--"), os("a.txt")],
            None,
            None,
            &stop(),
        )
        .unwrap();
        assert_ne!(code, 0);
        assert_eq!(fs::read(&index).unwrap(), old);
        let i = s.entry_index(&file).unwrap();
        assert!(execute(Task::Stage(s, i, false), &stop()).is_err());
        assert_eq!(fs::read(&index).unwrap(), old);
        assert!(!r.p().join(".git/index.lock").exists());
    }
    #[test]
    fn reviewed_reference_rejects_other_branch_at_the_same_commit_before_stage() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        let oid = r.git(&["rev-parse", "HEAD"]);
        let old_index = fs::read(r.p().join(".git/index")).unwrap();
        r.git(&["branch", "fixture-other"]);
        r.git(&["symbolic-ref", "HEAD", "refs/heads/fixture-other"]);
        assert_eq!(r.git(&["rev-parse", "HEAD"]), oid);
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old_index);
        assert!(execute(Task::Stage(s, 0, false), &stop()).is_err());
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old_index);
        assert!(!r.p().join(".git/index.lock").exists());
    }
    #[test]
    fn reviewed_reference_rejects_detached_same_commit_before_commit() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        r.git(&["add", "a.txt"]);
        let s = r.snap();
        let oid = r.git(&["rev-parse", "HEAD"]);
        let old_index = fs::read(r.p().join(".git/index")).unwrap();
        let name = String::from_utf8(oid.clone()).unwrap();
        r.git(&["update-ref", "--no-deref", "HEAD", name.trim()]);
        assert_eq!(r.git(&["rev-parse", "HEAD"]), oid);
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old_index);
        assert!(execute(Task::Commit(s, "wrong target".into()), &stop()).is_err());
        assert_eq!(r.git(&["rev-parse", "HEAD"]), oid);
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old_index);
    }
    #[test]
    fn hooks_are_honored_failure_keeps_index_and_message_reusable() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = change(r.snap(), 0, false);
        let hook = r.p().join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\necho fixture-hook-rejected >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            execute(Task::Commit(s, "keep draft".into()), &stop())
                .unwrap_err_string()
                .contains("fixture-hook-rejected")
        );
        assert!(r.snap().entries[0].staged());
        assert_eq!(r.git(&["log", "-1", "--format=%s"]), b"initial\n");
    }
    #[test]
    fn absent_repository_is_read_only() {
        let p = tempfile::tempdir().unwrap();
        assert!(refresh(p.path(), &stop()).is_err());
        assert_eq!(fs::read_dir(p.path()).unwrap().count(), 0);
    }
    #[test]
    fn no_staged_empty_message_and_conflicts_cannot_commit() {
        let r = Repo::new(true);
        assert!(execute(Task::Commit(r.snap(), "message".into()), &stop()).is_err());
        fs::write(r.p().join("a.txt"), "after\n").unwrap();
        let s = change(r.snap(), 0, false);
        assert!(execute(Task::Commit(s, "  \n".into()), &stop()).is_err());
    }
    #[test]
    fn linked_worktree_has_its_own_index_and_head() {
        let r = Repo::new(true);
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("linked");
        r.git(&[
            "worktree",
            "add",
            "-qb",
            "fixture-linked",
            path.to_str().unwrap(),
        ]);
        fs::write(path.join("a.txt"), "linked\n").unwrap();
        let s = Arc::new(refresh(&path, &stop()).unwrap());
        assert_eq!(s.branch, "fixture-linked");
        change(s, 0, false);
        assert!(r.git(&["diff", "--cached"]).is_empty());
    }
    #[test]
    fn cancelled_read_does_not_launch_or_mutate() {
        let r = Repo::new(true);
        let s = AtomicBool::new(true);
        assert!(refresh(r.p(), &s).is_err());
        assert!(r.snap().entries.is_empty());
    }
    #[test]
    fn parser_rejects_traversal_truncated_rename_and_excess() {
        assert!(parse(b"? ../bad\0").is_err());
        assert!(parse(b"2 R. N... 100644 100644 100644 a b R100 dst\0").is_err());
        let mut b = vec![];
        for i in 0..=FILES {
            b.extend(format!("? f{i}\0").as_bytes());
        }
        assert!(parse(&b).is_err());
    }
    #[test]
    fn deleted_parent_directories_remain_reviewable() {
        let r = Repo::new(true);
        fs::create_dir(r.p().join("nested")).unwrap();
        fs::write(r.p().join("nested/leaf.txt"), "leaf").unwrap();
        r.git(&["add", "nested"]);
        r.git(&["commit", "-qm", "nested"]);
        fs::remove_dir_all(r.p().join("nested")).unwrap();
        let s = r.snap();
        assert_eq!(s.entries[0].worktree, 'D');
        change(s, 0, false);
    }
    #[test]
    fn genuine_merge_conflict_is_visible_and_not_staged_or_committed() {
        let r = Repo::new(true);
        let branch = r.snap().branch.clone();
        r.git(&["checkout", "-qb", "other"]);
        fs::write(r.p().join("a.txt"), "other\n").unwrap();
        r.git(&["commit", "-qam", "other"]);
        r.git(&["checkout", "-q", &branch]);
        fs::write(r.p().join("a.txt"), "main\n").unwrap();
        r.git(&["commit", "-qam", "main"]);
        let (code, _, _) = run(r.p(), &[os("merge"), os("other")], None, None, &stop()).unwrap();
        assert_ne!(code, 0);
        let s = r.snap();
        assert!(s.entries[0].conflict);
        assert!(execute(Task::Stage(s.clone(), 0, false), &stop()).is_err());
        assert!(execute(Task::Commit(s, "no conflict commit".into()), &stop()).is_err());
    }
    #[test]
    fn worker_is_lazy_serialized_and_cancelled_read_drains() {
        let r = Repo::new(true);
        let mut w = Worker::default();
        assert!(w.poll().is_none());
        assert!(w.start(Task::Refresh(r.p().into()), Arc::new(|| {})));
        assert!(!w.start(Task::Refresh(r.p().into()), Arc::new(|| {})));
        w.cancel();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = w.poll() {
                assert!(result.is_err());
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(r.snap().entries.is_empty());
    }
    #[test]
    fn diff_disables_external_driver_and_textconv() {
        let r = Repo::new(true);
        let marker = r.p().join("CALLED");
        r.git(&[
            "config",
            "diff.evil.command",
            &format!("touch {}", marker.display()),
        ]);
        r.git(&[
            "config",
            "diff.evil.textconv",
            &format!("touch {}", marker.display()),
        ]);
        fs::write(r.p().join(".gitattributes"), "*.txt diff=evil\n").unwrap();
        fs::write(r.p().join("a.txt"), "after\n").unwrap();
        let s = r.snap();
        let i = s
            .entries
            .iter()
            .position(|e| e.path == Path::new("a.txt"))
            .unwrap();
        assert!(diff(&s, i, false, &stop()).unwrap().contains("+after"));
        assert!(!marker.exists());
    }
    fn observed(s: Arc<Snapshot>, path: &str, staged: bool, force: bool) -> Review {
        match execute(Task::Observe(s, Some(path.into()), staged, force), &stop()).unwrap() {
            Reply::Observed(r) => r,
            _ => panic!("expected changed review"),
        }
    }
    #[test]
    fn observation_noop_preserves_snapshot_and_index_without_publishing() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        let old = fs::read(r.p().join(".git/index")).unwrap();
        assert!(matches!(
            execute(Task::Observe(s, None, false, false), &stop()).unwrap(),
            Reply::Unchanged
        ));
        assert_eq!(fs::read(r.p().join(".git/index")).unwrap(), old);
    }
    #[test]
    fn observation_binds_new_body_and_selection_after_row_indices_shift() {
        let r = Repo::new(true);
        fs::write(r.p().join("0-before.txt"), "initial row\n").unwrap();
        r.git(&["add", "0-before.txt"]);
        r.git(&["commit", "-qm", "tracked insertion baseline"]);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        fs::write(r.p().join("0-before.txt"), "new row\n").unwrap();
        fs::write(r.p().join("a.txt"), "external body\n").unwrap();
        let review = observed(s, "a.txt", false, false);
        let i = review.selected.unwrap();
        assert_eq!(review.snapshot.entries[i].path, Path::new("a.txt"));
        assert_eq!(review.snapshot.entry_index(&r.p().join("a.txt")), Some(i));
        assert!(i > 0);
        assert!(review.text.contains("+external body") && !review.text.contains("+review\n"));
        assert!(review.lines.iter().any(|l| l == "+external body"));
    }
    #[test]
    fn observation_fences_staged_view_and_removed_selection() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        r.git(&["add", "a.txt"]);
        let review = observed(s.clone(), "a.txt", false, false);
        assert!(review.selected.is_none() && review.text.is_empty());
        let review = observed(s, "a.txt", true, false);
        assert!(review.selected.is_some() && review.text.contains("+review"));
        r.git(&["commit", "-qm", "external"]);
        let gone = observed(review.snapshot, "a.txt", true, false);
        assert!(gone.selected.is_none() && gone.lines.is_empty());
    }
    #[test]
    fn forced_review_keeps_selection_without_mutation() {
        let r = Repo::new(true);
        fs::write(r.p().join("a.txt"), "review\n").unwrap();
        let s = r.snap();
        let review = observed(s, "a.txt", false, true);
        assert!(review.selected.is_some() && review.text.contains("+review"));
        assert!(r.git(&["diff", "--cached"]).is_empty());
    }
}
