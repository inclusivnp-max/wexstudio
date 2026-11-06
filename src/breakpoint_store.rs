//! Source-bound breakpoint anchors and a lazy serialized off-GPU preference worker.
use crate::{
    debugger::Breakpoints,
    editor::TrackingDocument,
    text_tracking::{Projection, TextAnchor, TextId},
};
use anyhow::{Context, Result, ensure};
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
};

mod close;
mod recovery;
pub use close::{CloseOutcome, CloseStatus};
mod review;
pub use recovery::Recovery;
pub use review::Outcome as ReviewOutcome;
pub use review::{Difference, Entry};
pub use review::{Mode as ReviewMode, Review, Side as ReviewSide};
type Signature = Vec<(PathBuf, TextId, Option<TextId>, Option<blake3::Hash>)>;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE: usize = 8 * 1024 * 1024;
const MAX_MARKERS: usize = 10_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoredMarker {
    pub id: u64,
    pub line: u32,
    pub source_hash: Option<String>,
    pub span: Option<[usize; 2]>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema: u32,
    #[serde(deserialize_with = "unique_map")]
    pub files: BTreeMap<PathBuf, Vec<StoredMarker>>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema: 2,
            files: BTreeMap::new(),
        }
    }
}
pub struct Loaded {
    preferences: Preferences,
    expected: Option<blake3::Hash>,
    error: Option<String>,
    recoveries: Vec<Recovery>,
    recovery_root: PathBuf,
}
fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let f = match o.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(f.metadata()?.is_file(), "Expected a regular file");
    let mut b = vec![];
    f.take(limit as u64 + 1).read_to_end(&mut b)?;
    ensure!(
        b.len() <= limit,
        "File exceeds the explicit {} byte limit",
        limit
    );
    Ok(Some(b))
}
fn valid(root: &Path, p: &Preferences) -> Result<()> {
    ensure!(p.schema == 2, "Unsupported breakpoint preference schema");
    ensure!(p.files.len() <= 512, "Breakpoint file limit512 exceeded");
    let mut ids = BTreeSet::new();
    let mut count = 0;
    for (path, markers) in &p.files {
        ensure!(
            path.is_absolute()
                && path.starts_with(root)
                && path.components().all(|c| !matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )),
            "Breakpoint path must be canonical and inside the project"
        );
        ensure!(
            markers.len() <= 2000,
            "Breakpoint per-file limit2000 exceeded"
        );
        count += markers.len();
        for m in markers {
            ensure!(
                m.id > 0 && ids.insert(m.id) && m.line > 0,
                "Invalid/duplicate breakpoint identity or line"
            );
            ensure!(
                m.reason.as_ref().is_none_or(|s| s.len() <= 512),
                "Breakpoint reason exceeds512 bytes"
            );
            if let Some(h) = &m.source_hash {
                ensure!(
                    h.len() == 64
                        && h.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                    "Invalid breakpoint source hash"
                );
            }
            if let Some([a, b]) = m.span {
                ensure!(
                    a < b && b <= MAX_SOURCE && m.source_hash.is_some(),
                    "Invalid breakpoint source span"
                );
            }
        }
    }
    ensure!(count <= MAX_MARKERS, "Breakpoint total limit10000 exceeded");
    Ok(())
}
fn unique_map<'de, D, K, V>(d: D) -> std::result::Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct Unique<K, V>(std::marker::PhantomData<(K, V)>);
    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> serde::de::Visitor<'de> for Unique<K, V> {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object without duplicate keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((k, v)) = map.next_entry()? {
                if result.insert(k, v).is_some() {
                    return Err(serde::de::Error::custom("duplicate preference path"));
                }
            }
            Ok(result)
        }
    }
    d.deserialize_map(Unique(std::marker::PhantomData))
}
pub fn load(root: &Path) -> Loaded {
    let mut result = Loaded {
        preferences: Preferences::default(),
        expected: None,
        error: None,
        recoveries: recovery::list(root)
            .unwrap_or_else(|error| vec![Recovery::unreadable(error.to_string())]),
        recovery_root: recovery::directory(root),
    };
    let parsed = (|| -> Result<Preferences> {
        let Some(b) = read(&root.join(".wex/breakpoints.json"), MAX_BYTES)? else {
            return Ok(Preferences::default());
        };
        result.expected = Some(blake3::hash(&b));
        let v: serde_json::Value = serde_json::from_slice(&b)?;
        let p = if v.get("schema").is_some() {
            serde_json::from_slice(&b)?
        } else {
            #[derive(Deserialize)]
            struct Legacy(#[serde(deserialize_with = "unique_map")] Breakpoints);
            let Legacy(old) = serde_json::from_slice(&b)?;
            let mut p = Preferences::default();
            let mut id = 1;
            for (path, lines) in old {
                let markers = lines
                    .into_iter()
                    .map(|line| {
                        let m = StoredMarker {
                            id,
                            line,
                            source_hash: None,
                            span: None,
                            reason: Some("Legacy marker · rebind to current source".into()),
                        };
                        id += 1;
                        m
                    })
                    .collect();
                p.files.insert(path, markers);
            }
            p
        };
        valid(root, &p)?;
        Ok(p)
    })();
    match parsed {
        Ok(p) => result.preferences = p,
        Err(e) => {
            result.error = Some(format!(
                "Breakpoint preferences: {e:#} · restore the file, then Retry"
            ))
        }
    };
    result
}
fn position(rope: &Rope, line: u32) -> Result<Range<usize>> {
    let line = usize::try_from(line)
        .context("Invalid line")?
        .checked_sub(1)
        .context("Line must be positive")?;
    ensure!(line < rope.len_lines(), "Line no longer exists");
    let start = rope.line_to_char(line);
    let text = rope.line(line);
    let mut a = None;
    let mut b = 0;
    ensure!(
        text.len_chars() <= 4096,
        "Breakpoint line exceeds4096 characters; choose a shorter code line"
    );
    for (i, c) in text.chars().enumerate() {
        if !c.is_whitespace() {
            a.get_or_insert(start + i);
            b = start + i + 1;
        }
    }
    Ok(a.context("Choose a nonempty code line")?..b)
}
fn surviving(rope: &Rope, range: Range<usize>) -> Result<(u32, Range<usize>)> {
    ensure!(
        range.start < range.end && range.end <= rope.len_chars(),
        "Source anchor is out of bounds"
    );
    let first = rope
        .slice(range.clone())
        .chars()
        .take(4097)
        .position(|c| !c.is_whitespace())
        .context("Code removed or anchor whitespace exceeds4096 characters")?;
    ensure!(first <= 4096, "Anchor whitespace exceeds4096 characters");
    let first = range.start + first;
    let line = u32::try_from(rope.char_to_line(first) + 1)?;
    Ok((line, first..range.end))
}
struct Marker {
    saved: StoredMarker,
    anchor: Option<TextAnchor>,
    last_line: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub path: PathBuf,
    pub id: u64,
    pub line: u32,
    pub resolved: bool,
    pub issue: Option<String>,
}
pub struct Report {
    pub ticket: u64,
    pub lines: Breakpoints,
    pub rows: Arc<Vec<Row>>,
    pub sources: BTreeMap<PathBuf, String>,
    pub documents: BTreeMap<PathBuf, TextId>,
    pub error: Option<String>,
    pub validated: bool,
    pub review: Option<Arc<Review>>,
    pub review_outcome: Option<ReviewOutcome>,
    pub close_outcome: Option<CloseOutcome>,
    pub recoveries: Arc<Vec<Recovery>>,
}
pub enum Action {
    Toggle(PathBuf, u32, TrackingDocument),
    Rebind(PathBuf, u64, u32, TrackingDocument),
    Remove(PathBuf, u64),
    Remap(PathBuf, PathBuf),
    Refresh,
    Validate,
    Retry,
    Review(Option<String>),
    ApplyReview(u64, ReviewMode, BTreeMap<u64, ReviewSide>),
    CancelReview,
    PrepareClose(u64),
    CommitClose(u64, bool),
    CancelClose(u64),
}
type Documents = Vec<(PathBuf, TrackingDocument)>;
struct State {
    root: PathBuf,
    files: BTreeMap<PathBuf, Vec<Marker>>,
    next_id: u64,
    expected: Option<blake3::Hash>,
    persisted: Preferences,
    error: Option<String>,
    load_failed: bool,
    failed_payload: Option<Preferences>,
    review: Option<review::Prepared>,
    close: close::Tracker,
    review_clock: u64,
    review_outcome: Option<ReviewOutcome>,
    recoveries: Vec<Recovery>,
    recovery_root: PathBuf,
}
impl State {
    fn new(root: PathBuf, l: Loaded) -> Self {
        let next_id = l
            .preferences
            .files
            .values()
            .flatten()
            .map(|m| m.id)
            .max()
            .unwrap_or(0);
        let files = l
            .preferences
            .files
            .iter()
            .map(|(p, ms)| {
                (
                    p.clone(),
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
        Self {
            root,
            files,
            next_id,
            expected: l.expected,
            persisted: l.preferences,
            error: l.error.clone(),
            load_failed: l.error.is_some(),
            failed_payload: None,
            review: None,
            close: Default::default(),
            review_clock: 0,
            review_outcome: None,
            recoveries: l.recoveries,
            recovery_root: l.recovery_root,
        }
    }
    fn preferences(&self) -> Preferences {
        Preferences {
            schema: 2,
            files: self
                .files
                .iter()
                .filter(|(_, m)| !m.is_empty())
                .map(|(p, ms)| (p.clone(), ms.iter().map(|m| m.saved.clone()).collect()))
                .collect(),
        }
    }
    fn bind(marker: &mut Marker, doc: &TrackingDocument) -> Result<()> {
        let (_, current) = doc.current();
        if marker
            .anchor
            .as_mut()
            .is_some_and(|a| !matches!(current.project(a), Ok(Projection::Unrelated) | Err(_)))
        {
            return Ok(());
        }
        let (rope, snapshot, hash) = doc.saved().context("Source has no saved baseline")?;
        let [a, b] = marker.saved.span.context(
            marker
                .saved
                .reason
                .clone()
                .unwrap_or_else(|| "Marker has no saved code · rebind marker".into()),
        )?;
        ensure!(
            marker.saved.source_hash.as_deref() == Some(hash.to_hex().as_str()),
            "Source bytes changed · rebind this marker"
        );
        let (line, _) = surviving(rope, a..b)?;
        ensure!(
            line == marker.saved.line,
            "Stored source line/span conflict · rebind marker"
        );
        marker.anchor = Some(snapshot.anchor(a..b)?);
        Ok(())
    }
    fn action(&mut self, a: Action) -> Result<bool> {
        let retry = matches!(a, Action::Retry);
        self.review = None;
        self.close
            .invalidate("Marker action changed the closing checkpoint");
        if self.load_failed {
            if retry {
                let clock = self.review_clock;
                *self = Self::new(self.root.clone(), load(&self.root));
                self.review_clock = clock;
                return Ok(true);
            }
            anyhow::bail!("Repair breakpoint preferences and Retry before changing markers");
        }
        match a {
            Action::Toggle(p, line, doc) => {
                self.validate_action_path(&p, &doc)?;
                ensure!(
                    self.files.contains_key(&p) || self.files.len() < 512,
                    "Breakpoint file limit512 reached"
                );
                let (rope, snapshot) = doc.current();
                let span = position(rope, line)?;
                let existing=self.files.get_mut(&p).map(|markers|markers.iter_mut().filter_map(|m|{
                    let _=Self::bind(m,&doc);
                    m.anchor.as_mut().is_some_and(|a|matches!(snapshot.project(a),Ok(Projection::Mapped(r)) if surviving(rope,r.clone()).is_ok_and(|(l,_)|l==line))).then_some(m.saved.id)
                }).collect::<BTreeSet<_>>()).unwrap_or_default();
                if !existing.is_empty() {
                    self.files
                        .get_mut(&p)
                        .unwrap()
                        .retain(|m| !existing.contains(&m.saved.id));
                } else {
                    ensure!(
                        self.files.values().map(Vec::len).sum::<usize>() < MAX_MARKERS,
                        "Breakpoint total limit10000 reached"
                    );
                    ensure!(
                        self.files.get(&p).map_or(0, Vec::len) < 2000,
                        "Breakpoint per-file limit2000 reached"
                    );
                    let id = self
                        .next_id
                        .checked_add(1)
                        .context("Breakpoint identity exhausted")?;
                    let mut marker = Marker {
                        saved: StoredMarker {
                            id,
                            line,
                            span: None,
                            source_hash: None,
                            reason: Some("Unsaved code · save source before Debug".into()),
                        },
                        anchor: Some(snapshot.anchor(span)?),
                        last_line: line,
                    };
                    Self::update_saved(&mut marker, &doc.saved());
                    self.files.entry(p).or_default().push(marker);
                    self.next_id = id;
                }
            }
            Action::Rebind(p, id, line, doc) => {
                self.validate_action_path(&p, &doc)?;
                let (rope, snapshot) = doc.current();
                let span = position(rope, line)?;
                let m = self
                    .files
                    .get_mut(&p)
                    .and_then(|v| v.iter_mut().find(|m| m.saved.id == id))
                    .context("Marker changed")?;
                m.anchor = Some(snapshot.anchor(span)?);
                m.last_line = line;
                Self::update_saved(m, &doc.saved());
            }
            Action::Remove(p, id) => {
                if let Some(ms) = self.files.get_mut(&p) {
                    ms.retain(|m| m.saved.id != id);
                }
            }
            Action::Remap(from, to) => {
                let mut files = BTreeMap::<PathBuf, Vec<Marker>>::new();
                for (p, ms) in std::mem::take(&mut self.files) {
                    let path = match p.strip_prefix(&from) {
                        Ok(s) if s.as_os_str().is_empty() => to.clone(),
                        Ok(s) => to.join(s),
                        Err(_) => p,
                    };
                    files.entry(path).or_default().extend(ms);
                }
                self.files = files;
            }
            Action::Refresh | Action::Validate => {
                if matches!(a, Action::Refresh) {
                    self.recoveries = self.list_drafts()?;
                }
                if read(&self.root.join(".wex/breakpoints.json"), MAX_BYTES)?
                    .as_deref()
                    .map(blake3::hash)
                    != self.expected
                {
                    self.error =
                        Some("Breakpoint preferences changed externally · Review changes".into());
                }
            }
            Action::Retry => {
                self.recoveries = self.list_drafts()?;
            }
            Action::Review(_)
            | Action::ApplyReview(..)
            | Action::CancelReview
            | Action::PrepareClose(_)
            | Action::CommitClose(..)
            | Action::CancelClose(_) => {
                anyhow::bail!("Review action requires a request snapshot")
            }
        }
        self.files.retain(|_, m| !m.is_empty());
        Ok(retry)
    }
    fn validate_action_path(&self, p: &Path, doc: &TrackingDocument) -> Result<()> {
        ensure!(
            p.is_absolute() && p.starts_with(&self.root) && doc.path() == Some(p),
            "Breakpoint source/capture path mismatch"
        );
        ensure!(
            p.extension().is_some_and(|e| e == "kt" || e == "java"),
            "Choose a Kotlin or Java code line"
        );
        Ok(())
    }
    fn update_saved(
        marker: &mut Marker,
        saved: &Option<(&Rope, crate::text_tracking::TrackingSnapshot, blake3::Hash)>,
    ) {
        if let Some((saved_rope, saved_snapshot, hash)) = &saved
            && let Some(anchor) = marker.anchor.as_mut()
        {
            match saved_snapshot.project(anchor) {
                Ok(Projection::Mapped(r)) => {
                    if let Ok((line, r)) = surviving(saved_rope, r) {
                        marker.saved.line = line;
                        marker.saved.span = Some([r.start, r.end]);
                        marker.saved.source_hash = Some(hash.to_hex().to_string());
                        marker.saved.reason = None;
                    }
                }
                Ok(Projection::Removed) => {
                    marker.saved.span = None;
                    marker.saved.source_hash = Some(hash.to_hex().to_string());
                    marker.saved.reason =
                        Some("Code absent from saved source · Undo/save or rebind".into());
                }
                _ => {}
            }
        }
    }
    fn reconcile(&mut self, ticket: u64, docs: Documents, validate: bool, retry: bool) -> Report {
        let docs: BTreeMap<_, _> = docs.into_iter().collect();
        let mut rows = vec![];
        let mut lines = Breakpoints::new();
        let mut sources = BTreeMap::new();
        for (path, markers) in &mut self.files {
            if let Some(doc) = docs.get(path) {
                let (rope, snapshot) = doc.current();
                let saved = doc.saved();
                for marker in markers {
                    let projected = (|| -> Result<u32> {
                        Self::bind(marker, doc)?;
                        let anchor = marker.anchor.as_mut().unwrap();
                        let range = match snapshot.project(anchor)? {
                            Projection::Mapped(r) => r,
                            Projection::Removed => {
                                anyhow::bail!("Code deleted · Undo or rebind marker")
                            }
                            Projection::Unrelated => {
                                anyhow::bail!("Source history disconnected · rebind marker")
                            }
                        };
                        let (line, _) = surviving(rope, range)?;
                        Ok(line)
                    })();
                    Self::update_saved(marker, &saved);
                    let (resolved, issue) = match projected {
                        Ok(line) => {
                            marker.last_line = line;
                            lines.entry(path.clone()).or_default().insert(line);
                            (true, None)
                        }
                        Err(e) => (false, Some(e.to_string())),
                    };
                    rows.push(Row {
                        path: path.clone(),
                        id: marker.saved.id,
                        line: marker.last_line,
                        resolved,
                        issue,
                    });
                }
            } else {
                for m in markers {
                    rows.push(Row {
                        path: path.clone(),
                        id: m.saved.id,
                        line: m.saved.line,
                        resolved: false,
                        issue: Some("Closed source · checked before Debug".into()),
                    });
                }
            }
        }
        if validate {
            for (path, markers) in &self.files {
                let checked = (|| -> Result<(Rope, String)> {
                    ensure!(path.canonicalize()? == *path, "Source path is aliased");
                    let bytes = read(path, MAX_SOURCE)?.context("Source file is missing")?;
                    let h = blake3::hash(&bytes).to_hex().to_string();
                    let text = std::str::from_utf8(&bytes)?;
                    Ok((Rope::from_str(text), h))
                })();
                for m in markers {
                    let issue = (|| -> Result<()> {
                        let (rope, hash) =
                            checked.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?;
                        ensure!(
                            m.saved.source_hash.as_ref() == Some(hash),
                            "Saved source bytes changed · review/rebind marker"
                        );
                        let [a, b] = m
                            .saved
                            .span
                            .context("Marker is absent from saved code · Undo/save or rebind")?;
                        let (line, _) = surviving(rope, a..b)?;
                        ensure!(line == m.saved.line, "Stored line/span conflict");
                        Ok(())
                    })();
                    let row = rows.iter_mut().find(|r| r.id == m.saved.id).unwrap();
                    if let Err(e) = issue {
                        row.issue = Some(e.to_string());
                        row.resolved = false;
                        lines.entry(path.clone()).or_default().remove(&m.saved.line);
                    } else if !docs.contains_key(path) {
                        row.issue = None;
                        row.resolved = true;
                        lines.entry(path.clone()).or_default().insert(m.saved.line);
                    }
                }
                if let Ok((_, h)) = checked {
                    sources.insert(path.clone(), h);
                }
            }
        }
        let desired = self.preferences();
        if !self.load_failed
            && desired != self.persisted
            && (retry || self.failed_payload.as_ref() != Some(&desired))
        {
            match self.persist(&desired) {
                Ok(()) => {
                    self.persisted = desired;
                    self.failed_payload = None;
                    self.error = None;
                }
                Err(e) => {
                    self.error = Some(format!("Breakpoint preferences: {e:#} · Retry"));
                    self.failed_payload = Some(desired);
                }
            }
        }
        lines.clear();
        for row in rows.iter().filter(|r| r.resolved) {
            lines.entry(row.path.clone()).or_default().insert(row.line);
        }
        let ready = validate
            && self.error.is_none()
            && self
                .review_outcome
                .as_ref()
                .is_none_or(|o| o.error.is_none())
            && rows.iter().all(|r| r.resolved && r.issue.is_none());
        Report {
            ticket,
            lines,
            rows: Arc::new(rows),
            sources,
            documents: docs
                .iter()
                .map(|(p, d)| (p.clone(), d.id().clone()))
                .collect(),
            error: self
                .error
                .clone()
                .or_else(|| self.review_outcome.as_ref().and_then(|o| o.error.clone())),
            validated: ready,
            review: self.review.as_ref().map(|p| p.view.clone()),
            review_outcome: self.review_outcome.clone(),
            close_outcome: self.close.outcome.clone(),
            recoveries: Arc::new(self.recoveries.clone()),
        }
    }
    fn persist(&mut self, p: &Preferences) -> Result<()> {
        valid(&self.root, p)?;
        let dir = self.root.join(".wex");
        std::fs::create_dir_all(&dir)?;
        ensure!(
            dir.canonicalize()? == dir,
            "Preference directory is aliased"
        );
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .mode(0o600);
        }
        let lock = options.open(dir.join("breakpoints.lock"))?;
        ensure!(lock.metadata()?.is_file(), "Preference lock is not regular");
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            ensure!(
                unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
                "Preferences busy; another writer owns the lock"
            );
        }
        let target = dir.join("breakpoints.json");
        let check = || -> Result<()> {
            ensure!(
                read(&target, MAX_BYTES)?.as_deref().map(blake3::hash) == self.expected,
                "Preferences changed externally; local markers retained"
            );
            Ok(())
        };
        check()?;
        let b = serde_json::to_vec_pretty(p)?;
        ensure!(b.len() <= MAX_BYTES, "Preferences exceed4MiB");
        let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
        temp.write_all(&b)?;
        temp.as_file().sync_all()?;
        check()?;
        if self.expected.is_none() {
            temp.persist_noclobber(target)?;
        } else {
            temp.persist(target)?;
        }
        self.expected = Some(blake3::hash(&b));
        Ok(())
    }
}

struct Request {
    ticket: u64,
    docs: Documents,
    actions: Vec<Action>,
    signature: Signature,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Request>,
    report: Option<Report>,
    stop: bool,
}
type Worker = thread::JoinHandle<Result<(), String>>;
/// No thread or filesystem work until a real project has markers or an explicit action.
#[derive(Default)]
pub struct Service {
    seed: Option<(PathBuf, Loaded)>,
    mail: Arc<Mutex<Mailbox>>,
    wake: Option<mpsc::SyncSender<()>>,
    signature: Signature,
    pub requested: u64,
    pub delivered: u64,
    thread: Option<Worker>,
}
fn retired() -> &'static Mutex<Vec<Worker>> {
    static WORKERS: OnceLock<Mutex<Vec<Worker>>> = OnceLock::new();
    WORKERS.get_or_init(Mutex::default)
}
pub fn drain_on_exit() -> Result<()> {
    let mut errors = vec![];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    for t in std::mem::take(&mut *retired().lock().unwrap()) {
        while !t.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(5));
        }
        if !t.is_finished() {
            errors.push(
                "Breakpoint preference flush did not finish within12s; completion is unverified"
                    .to_owned(),
            );
            continue;
        }
        match t.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => errors.push(e),
            Err(_) => errors.push("Breakpoint worker failed while flushing preferences".to_owned()),
        }
    }
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    Ok(())
}

impl Drop for Service {
    fn drop(&mut self) {
        self.mail.lock().unwrap().stop = true;
        if let Some(w) = &self.wake {
            let _ = w.try_send(());
        }
        if let Some(t) = self.thread.take() {
            retired().lock().unwrap().push(t);
        }
    }
}
impl Service {
    pub fn new(root: PathBuf, loaded: Loaded) -> Self {
        Self {
            seed: Some((root, loaded)),
            mail: Arc::new(Mutex::new(Mailbox::default())),
            wake: None,
            signature: vec![],
            requested: 0,
            delivered: 0,
            thread: None,
        }
    }
    pub fn active(&self) -> bool {
        self.wake.is_some()
            || self.seed.as_ref().is_some_and(|(_, s)| {
                s.expected.is_some() || s.error.is_some() || !s.recoveries.is_empty()
            })
    }
    pub fn pending(&self) -> bool {
        self.requested != self.delivered
    }
    pub fn signature_changed(
        &self,
        s: &[(PathBuf, TextId, Option<TextId>, Option<blake3::Hash>)],
    ) -> bool {
        self.signature != s
    }
    pub fn submit(
        &mut self,
        docs: Documents,
        signature: Signature,
        action: Option<Action>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<bool> {
        if action.is_none() && (!self.active() || self.signature == signature) {
            return Ok(false);
        }
        let ticket = self
            .requested
            .checked_add(1)
            .context("Breakpoint request counter exhausted")?;
        if self.wake.is_none() {
            let seed = Arc::new(Mutex::new(Some(
                self.seed.take().context("No project for breakpoints")?,
            )));
            let owned = seed.clone();
            let (tx, rx) = mpsc::sync_channel(1);
            let mail = self.mail.clone();
            let spawned = thread::Builder::new()
                .name("wex-breakpoints".into())
                .spawn(move || {
                    let (root, loaded) = owned.lock().unwrap().take().unwrap();
                    let mut state = State::new(root, loaded);
                    while rx.recv().is_ok() {
                        let request = {
                            let mut m = mail.lock().unwrap();
                            if m.stop && m.pending.is_none() {
                                break;
                            }
                            m.pending.take()
                        };
                        let Some(req) = request else { continue };
                        state.expire_review(&req.signature);
                        state.close.signature(&req.signature);
                        let mut validate = false;
                        let mut retry = false;
                        let mut action_error = None;
                        let mut prepare = None;
                        let mut closing = None;
                        for a in req.actions {
                            match a {
                                Action::PrepareClose(id) => {
                                    closing = Some((id, None));
                                    retry = true;
                                    continue;
                                }
                                Action::CommitClose(id, discard) => {
                                    closing = Some((id, Some(discard)));
                                    continue;
                                }
                                Action::CancelClose(id) => {
                                    state.close.cancel(id);
                                    if closing.as_ref().is_some_and(|(old, _)| *old == id) {
                                        closing = None;
                                    }
                                    continue;
                                }

                                Action::Review(name) => {
                                    state.close.invalidate("Marker review superseded closing");
                                    prepare = Some(name);
                                    continue;
                                }
                                Action::CancelReview => {
                                    prepare = None;
                                    state.review = None;
                                    state.review_outcome = None;
                                    continue;
                                }
                                Action::ApplyReview(id, mode, choices) => {
                                    state.close.invalidate("Marker review superseded closing");
                                    let error = state
                                        .apply_review(id, mode, choices, &req.signature)
                                        .err()
                                        .map(|e| format!("Breakpoint review: {e:#}"));
                                    state.review_outcome = Some(ReviewOutcome {
                                        id,
                                        error: error.clone(),
                                    });
                                    if let Some(error) = error {
                                        action_error = Some(error);
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                            validate |= matches!(a, Action::Validate | Action::Refresh);
                            match state.action(a) {
                                Ok(r) => retry |= r,
                                Err(e) => action_error = Some(format!("Breakpoint action: {e:#}")),
                            }
                        }
                        let mut report = state.reconcile(req.ticket, req.docs, validate, retry);
                        if let Some(name) = prepare {
                            match state.prepare_review(req.signature.clone(), name) {
                                Ok(()) => {
                                    report.review = state.review.as_ref().map(|p| p.view.clone())
                                }
                                Err(e) => action_error = Some(format!("Breakpoint review: {e:#}")),
                            }
                        }
                        if let Some((id, commit)) = closing {
                            if let Some(discard) = commit {
                                state.commit_close(id, discard, &req.signature);
                            } else {
                                state.prepare_close(id, &req.signature);
                            }
                        }
                        report.close_outcome = state.close.outcome.clone();
                        if let Some(e) = action_error {
                            report.error = Some(e);
                            report.validated = false;
                        }
                        let mut m = mail.lock().unwrap();
                        m.report = Some(report);
                        let stop = m.stop && m.pending.is_none();
                        drop(m);
                        repaint();
                        if stop {
                            break;
                        }
                    }
                    state.finish().map_err(|e| format!("{e:#}"))
                });
            match spawned {
                Ok(t) => self.thread = Some(t),
                Err(e) => {
                    self.seed = seed.lock().unwrap().take();
                    return Err(e.into());
                }
            }
            self.wake = Some(tx);
        }
        let mut m = self.mail.lock().unwrap();
        let pending = m.pending.get_or_insert_with(|| Request {
            ticket,
            docs: vec![],
            actions: vec![],
            signature: signature.clone(),
        });
        ensure!(
            pending.actions.len() < 256,
            "Breakpoint action queue full; wait for current changes"
        );
        pending.ticket = ticket;
        pending.docs = docs;
        pending.signature = signature.clone();
        if let Some(a) = action {
            pending.actions.push(a);
        }
        drop(m);
        self.requested = ticket;
        self.signature = signature;
        let _ = self.wake.as_ref().unwrap().try_send(());
        Ok(true)
    }
    pub fn poll(&mut self) -> Option<Report> {
        let report = self.mail.lock().unwrap().report.take()?;
        if report.ticket != self.requested {
            return None;
        }
        self.delivered = report.ticket;
        Some(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Document;
    pub(super) fn fixture(text: &str) -> (tempfile::TempDir, PathBuf, Document, State) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("Main.kt");
        std::fs::write(&p, text).unwrap();
        let doc = Document::open(&p).unwrap();
        let state = State::new(d.path().to_owned(), load(d.path()));
        (d, p, doc, state)
    }
    pub(super) fn sync(state: &mut State, p: &Path, d: &Document) -> Report {
        state.reconcile(1, vec![(p.to_owned(), d.tracking_capture())], false, false)
    }
    fn toggle(state: &mut State, p: &Path, d: &Document, line: u32) -> Report {
        state
            .action(Action::Toggle(p.to_owned(), line, d.tracking_capture()))
            .unwrap();
        sync(state, p, d)
    }
    #[test]
    fn dirty_movement_persists_only_saved_coordinates_then_save_and_reopen_move_them() {
        let (dir, p, mut d, mut s) = fixture("target();\n");
        toggle(&mut s, &p, &d, 1);
        d.edit(0..0, "// 😀\r\n").unwrap();
        let r = sync(&mut s, &p, &d);
        assert!(r.lines[&p].contains(&2));
        let disk: Preferences = serde_json::from_slice(
            &std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(disk.files[&p][0].line, 1);
        let mut reopened = State::new(dir.path().to_owned(), load(dir.path()));
        let original = Document::open(&p).unwrap();
        assert!(sync(&mut reopened, &p, &original).lines[&p].contains(&1));
        d.save().unwrap();
        sync(&mut s, &p, &d);
        let mut reopened = State::new(dir.path().to_owned(), load(dir.path()));
        let fresh = Document::open(&p).unwrap();
        assert!(sync(&mut reopened, &p, &fresh).lines[&p].contains(&2));
    }
    #[test]
    fn deletion_and_undo_are_explicit_without_clamping_to_neighboring_code() {
        let (_dir, p, mut d, mut s) = fixture("first();\ntarget();\nlast();\n");
        toggle(&mut s, &p, &d, 2);
        d.edit(9..19, "").unwrap();
        let r = sync(&mut s, &p, &d);
        assert!(!r.lines.contains_key(&p));
        assert!(r.rows[0].issue.as_ref().unwrap().contains("deleted"));
        d.save().unwrap();
        sync(&mut s, &p, &d);
        assert!(s.preferences().files[&p][0].span.is_none());
        d.undo().unwrap();
        assert!(sync(&mut s, &p, &d).lines[&p].contains(&2));
        d.save().unwrap();
        sync(&mut s, &p, &d);
        assert!(s.preferences().files[&p][0].span.is_some());
    }
    #[test]
    fn unsaved_inserted_marker_survives_redo_but_toggle_removal_is_not_text_history() {
        let (_dir, p, mut d, mut s) = fixture("base();\n");
        d.edit(0..0, "newCode();\n").unwrap();
        toggle(&mut s, &p, &d, 1);
        assert!(s.preferences().files[&p][0].span.is_none());
        d.undo().unwrap();
        assert!(!sync(&mut s, &p, &d).rows[0].resolved);
        d.redo().unwrap();
        assert!(sync(&mut s, &p, &d).rows[0].resolved);
        toggle(&mut s, &p, &d, 1);
        d.undo().unwrap();
        d.redo().unwrap();
        assert!(sync(&mut s, &p, &d).rows.is_empty());
    }
    #[test]
    fn joined_line_keeps_two_identities_until_explicit_toggle_removes_both() {
        let (_dir, p, mut d, mut s) = fixture("first();\nsecond();\n");
        toggle(&mut s, &p, &d, 1);
        toggle(&mut s, &p, &d, 2);
        d.edit(8..9, " ").unwrap();
        let r = sync(&mut s, &p, &d);
        assert_eq!(r.lines[&p], BTreeSet::from([1]));
        assert_eq!(r.rows.len(), 2);
        d.undo().unwrap();
        assert_eq!(sync(&mut s, &p, &d).lines[&p], BTreeSet::from([1, 2]));
        d.redo().unwrap();
        toggle(&mut s, &p, &d, 1);
        d.undo().unwrap();
        assert!(sync(&mut s, &p, &d).rows.is_empty());
    }
    #[test]
    fn malformed_preferences_and_legacy_markers_require_explicit_repair_or_rebind() {
        let (dir, p, d, _s) = fixture("target();\n");
        std::fs::create_dir_all(dir.path().join(".wex")).unwrap();
        let pref = dir.path().join(".wex/breakpoints.json");
        std::fs::write(&pref, b"broken").unwrap();
        let mut s = State::new(dir.path().to_owned(), load(dir.path()));
        assert!(
            s.action(Action::Toggle(p.clone(), 1, d.tracking_capture()))
                .is_err()
        );
        sync(&mut s, &p, &d);
        assert_eq!(std::fs::read(&pref).unwrap(), b"broken");
        std::fs::write(
            &pref,
            serde_json::to_vec(&BTreeMap::from([(p.clone(), BTreeSet::from([1]))])).unwrap(),
        )
        .unwrap();
        s.action(Action::Retry).unwrap();
        let r = sync(&mut s, &p, &d);
        assert!(r.rows[0].issue.as_ref().unwrap().contains("Legacy"));
        s.action(Action::Rebind(
            p.clone(),
            r.rows[0].id,
            1,
            d.tracking_capture(),
        ))
        .unwrap();
        let r = sync(&mut s, &p, &d);
        assert!(r.rows[0].resolved);
        assert!(s.error.is_none());
        assert_eq!(
            serde_json::from_slice::<Preferences>(&std::fs::read(pref).unwrap())
                .unwrap()
                .schema,
            2
        );
    }
    #[test]
    fn external_preference_writer_is_not_clobbered_and_retry_preserves_local_markers() {
        let (dir, p, d, mut s) = fixture("one();\ntwo();\n");
        toggle(&mut s, &p, &d, 1);
        let pref = dir.path().join(".wex/breakpoints.json");
        let original = std::fs::read(&pref).unwrap();
        let status = std::process::Command::new("python3")
            .args([
                "-c",
                "import pathlib,sys;pathlib.Path(sys.argv[1]).write_bytes(b'external writer')",
            ])
            .arg(&pref)
            .status()
            .unwrap();
        assert!(status.success());
        let r = toggle(&mut s, &p, &d, 2);
        assert!(r.error.unwrap().contains("externally"));
        assert_eq!(r.rows.len(), 2);
        assert_eq!(std::fs::read(&pref).unwrap(), b"external writer");
        s.action(Action::Retry).unwrap();
        let r = s.reconcile(2, vec![(p.clone(), d.tracking_capture())], false, true);
        assert!(r.error.is_some());
        std::fs::write(&pref, original).unwrap();
        s.action(Action::Retry).unwrap();
        let r = s.reconcile(3, vec![(p.clone(), d.tracking_capture())], false, true);
        assert!(r.error.is_none());
        assert_eq!(
            serde_json::from_slice::<Preferences>(&std::fs::read(pref).unwrap())
                .unwrap()
                .files[&p]
                .len(),
            2
        );
    }
    #[test]
    fn independent_fd_lock_holder_releases_without_lock_filename_deletion() {
        use std::io::{BufRead, BufReader};
        let (dir, p, d, mut s) = fixture("target();\n");
        std::fs::create_dir_all(dir.path().join(".wex")).unwrap();
        let lock = dir.path().join(".wex/breakpoints.lock");
        let mut holder=std::process::Command::new("python3").args(["-u","-c","import fcntl,sys;f=open(sys.argv[1],'a+');fcntl.flock(f,fcntl.LOCK_EX);print('LOCKED');sys.stdin.read(1)"]).arg(&lock).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let mut ready = String::new();
        BufReader::new(holder.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "LOCKED\n");
        let r = toggle(&mut s, &p, &d, 1);
        assert!(r.error.unwrap().contains("busy"));
        assert!(!dir.path().join(".wex/breakpoints.json").exists());
        holder.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(holder.wait().unwrap().success());
        assert!(lock.exists());
        s.action(Action::Retry).unwrap();
        let r = s.reconcile(2, vec![(p, d.tracking_capture())], false, true);
        assert!(r.error.is_none());
    }
    #[test]
    fn closed_source_hash_validation_blocks_stale_lines_and_recovers_exact_bytes() {
        let (_dir, p, d, mut s) = fixture("target();\n");
        toggle(&mut s, &p, &d, 1);
        let original = std::fs::read(&p).unwrap();
        std::fs::write(&p, "other();\n").unwrap();
        let r = s.reconcile(2, vec![], true, false);
        assert!(!r.validated);
        assert!(r.rows[0].issue.as_ref().unwrap().contains("changed"));
        assert!(r.lines.is_empty());
        std::fs::write(&p, original).unwrap();
        let r = s.reconcile(3, vec![], true, false);
        assert!(r.validated);
        crate::run::verify_source_guard(p.parent().unwrap(), &r.sources).unwrap();
        std::fs::write(&p, "late();\n").unwrap();
        let e = crate::run::verify_source_guard(p.parent().unwrap(), &r.sources).unwrap_err();
        assert!(e.downcast_ref::<crate::run::SourceGuardChanged>().is_some());
    }
    #[test]
    fn path_remap_preserves_anchor_identity_and_new_saved_preference_path() {
        let (dir, p, mut d, mut s) = fixture("target();\n");
        toggle(&mut s, &p, &d, 1);
        let to = dir.path().join("Moved.kt");
        std::fs::rename(&p, &to).unwrap();
        d.path = Some(to.clone());
        s.action(Action::Remap(p.clone(), to.clone())).unwrap();
        let r = sync(&mut s, &to, &d);
        assert!(r.lines[&to].contains(&1));
        assert!(!s.preferences().files.contains_key(&p));
        assert!(
            s.reconcile(2, vec![(to, d.tracking_capture())], true, false)
                .validated
        );
    }
    #[test]
    fn coalesced_worker_actions_bind_their_actual_captured_revision_and_flush_on_close() {
        let (dir, p, mut d, _s) = fixture("target();\n");
        let mut service = Service::new(dir.path().to_owned(), load(dir.path()));
        let sig = |d: &Document| {
            vec![(
                p.clone(),
                d.text_id().clone(),
                d.saved_text_id().cloned(),
                d.disk_hash(),
            )]
        };
        service
            .submit(
                vec![(p.clone(), d.tracking_capture())],
                sig(&d),
                Some(Action::Toggle(p.clone(), 1, d.tracking_capture())),
                Arc::new(|| {}),
            )
            .unwrap();
        d.edit(0..0, "// newer\n").unwrap();
        service
            .submit(
                vec![(p.clone(), d.tracking_capture())],
                sig(&d),
                None,
                Arc::new(|| {}),
            )
            .unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let report = loop {
            if let Some(r) = service.poll() {
                break r;
            }
            assert!(std::time::Instant::now() < end);
            thread::sleep(std::time::Duration::from_millis(2));
        };
        assert!(report.lines[&p].contains(&2));
        service
            .submit(
                vec![(p.clone(), d.tracking_capture())],
                sig(&d),
                Some(Action::Remove(p.clone(), report.rows[0].id)),
                Arc::new(|| {}),
            )
            .unwrap();
        drop(service);
        drain_on_exit().unwrap();
        assert!(load(dir.path()).preferences.files.is_empty());
    }
    #[test]
    fn empty_lines_path_mismatch_and_identity_exhaustion_reject_before_mutation() {
        let (_dir, p, d, mut s) = fixture("\ntarget();\n");
        assert!(
            s.action(Action::Toggle(p.clone(), 1, d.tracking_capture()))
                .is_err()
        );
        assert!(s.files.is_empty());
        s.next_id = u64::MAX;
        assert!(
            s.action(Action::Toggle(p.clone(), 2, d.tracking_capture()))
                .is_err()
        );
        assert!(s.files.is_empty());
        assert!(
            s.action(Action::Toggle(
                p.with_file_name("Other.kt"),
                2,
                d.tracking_capture()
            ))
            .is_err()
        );
    }
    #[test]
    fn queued_toggle_retains_saved_coordinates_even_if_tab_closed_before_reconciliation() {
        let (dir, p, d, mut state) = fixture("target();\n");
        state
            .action(Action::Toggle(p.clone(), 1, d.tracking_capture()))
            .unwrap();
        let report = state.reconcile(1, vec![], true, false);
        assert!(report.validated);
        assert!(report.lines[&p].contains(&1));
        let mut reopened = State::new(dir.path().to_owned(), load(dir.path()));
        assert!(sync(&mut reopened, &p, &d).rows[0].resolved);
    }
    #[tokio::test]
    async fn stale_source_rejects_debug_before_any_adb_connection_or_tool_preparation() {
        let (dir, p, _doc, _state) = fixture("target();\n");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let adb = crate::adb::AdbClient::new(listener.local_addr().unwrap().to_string());
        let options = crate::run::RunOptions {
            serial: "EXACT_TEST_SERIAL".into(),
            module: ":app".into(),
            variant: "debug".into(),
            offline: true,
        };
        let result = crate::debugger::launch_checked(
            dir.path(),
            &adb,
            &options,
            &Breakpoints::new(),
            &|_| {},
            &BTreeMap::from([(p, "0".repeat(64))]),
        )
        .await;
        assert!(
            result
                .err()
                .unwrap()
                .downcast_ref::<crate::run::SourceGuardChanged>()
                .is_some()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!dir.path().join(".wex").exists());
    }
}
