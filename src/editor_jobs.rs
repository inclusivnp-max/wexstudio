//! Bounded editor workers: one latest search/replace job and a debounced content watcher.
//! Disk reads, hashing, parsing and project scans never run on the render thread.
use crate::{
    editing::{EditState, map_position},
    editor::Document,
    editor_search::{self, SearchOptions, SearchResults},
};
use ropey::Rope;
use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fingerprint {
    Present(blake3::Hash),
    Missing,
    Error(String),
}
pub enum DiskState {
    Present(Box<Document>),
    Missing,
    Error(String),
}
pub enum EditorEvent {
    Found {
        id: u64,
        epoch: u64,
        revision: u64,
        result: SearchResults,
    },
    Replaced {
        id: u64,
        epoch: u64,
        revision: u64,
        result: Result<(Box<Document>, EditState, isize, usize), String>,
    },
    Disk {
        generation: u64,
        path: PathBuf,
        state: DiskState,
    },
    Tree {
        started: Instant,
        inventory: crate::project::ExplorerInventory,
    },
}
enum Job {
    Search {
        id: u64,
        epoch: u64,
        revision: u64,
        rope: Rope,
        options: SearchOptions,
    },
    Replace {
        id: u64,
        epoch: u64,
        revision: u64,
        doc: Box<Document>,
        edit: EditState,
        options: SearchOptions,
        replacement: String,
        only: Option<std::ops::Range<usize>>,
    },
}
type WatchList = (u64, Vec<(PathBuf, Option<blake3::Hash>)>);
struct Shared {
    stop: AtomicBool,
    latest: AtomicU64,
    job: Mutex<Option<Job>>,
    changed: Condvar,
    watch: Mutex<WatchList>,
}
pub struct EditorServices {
    shared: Arc<Shared>,
    events: Option<mpsc::Receiver<EditorEvent>>,
    threads: Vec<thread::JoinHandle<()>>,
    next_id: u64,
    watch_generation: u64,
}
impl EditorServices {
    /// Welcome/loading have no documents; defer both search and watcher workers.
    pub fn inert() -> Self {
        Self {
            shared: Arc::new(Shared {
                stop: AtomicBool::new(false),
                latest: AtomicU64::new(0),
                job: Mutex::new(None),
                changed: Condvar::new(),
                watch: Mutex::new((0, vec![])),
            }),
            events: None,
            threads: vec![],
            next_id: 0,
            watch_generation: 0,
        }
    }
    pub fn active_workers(&self) -> usize {
        self.threads.len()
    }

    pub fn new(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_inventory(root, wake, true)
    }
    pub fn without_inventory(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_inventory(root, wake, false)
    }
    fn with_inventory(root: PathBuf, wake: Arc<dyn Fn() + Send + Sync>, inventory: bool) -> Self {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            latest: AtomicU64::new(0),
            job: Mutex::new(None),
            changed: Condvar::new(),
            watch: Mutex::new((0, vec![])),
        });
        let (tx, rx) = mpsc::sync_channel(32);
        let s = shared.clone();
        let sender = tx.clone();
        let w = wake.clone();
        let search = thread::Builder::new()
            .name("wex-editor-jobs".into())
            .spawn(move || {
                loop {
                    let mut guard = s.job.lock().unwrap();
                    while guard.is_none() && !s.stop.load(Ordering::Relaxed) {
                        guard = s.changed.wait(guard).unwrap();
                    }
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let job = guard.take().unwrap();
                    drop(guard);
                    let id = match &job {
                        Job::Search { id, .. } | Job::Replace { id, .. } => *id,
                    };
                    let cancel =
                        || s.stop.load(Ordering::Relaxed) || s.latest.load(Ordering::Relaxed) != id;
                    let event = match job {
                        Job::Search {
                            id,
                            epoch,
                            revision,
                            rope,
                            options,
                        } => {
                            let result = editor_search::search(&rope, &options, cancel);
                            if cancel() {
                                continue;
                            }
                            EditorEvent::Found {
                                id,
                                epoch,
                                revision,
                                result: result.unwrap_or_else(|e| SearchResults {
                                    error: Some(e.to_string()),
                                    ..Default::default()
                                }),
                            }
                        }
                        Job::Replace {
                            id,
                            epoch,
                            revision,
                            mut doc,
                            mut edit,
                            options,
                            replacement,
                            only,
                        } => {
                            let result = (|| -> anyhow::Result<_> {
                                let caret = only.as_ref().map(|r| r.end).unwrap_or(edit.cursor);
                                let edits = editor_search::replacements(
                                    doc.rope(),
                                    &options,
                                    &replacement,
                                    only,
                                    cancel,
                                )?;
                                anyhow::ensure!(!cancel(), "replacement cancelled");
                                let count = edits.len();
                                let delta = edits
                                    .iter()
                                    .map(|(r, t)| {
                                        t.chars().count() as isize - (r.end - r.start) as isize
                                    })
                                    .sum();
                                doc.set_view(edit.anchor, edit.cursor);
                                let cursor = map_position(caret, &edits);
                                doc.edit_batch_parsed(&edits)?;
                                edit.collapse(cursor);
                                doc.set_view(cursor, cursor);
                                Ok((doc, edit, delta, count))
                            })();
                            EditorEvent::Replaced {
                                id,
                                epoch,
                                revision,
                                result: result.map_err(|e| e.to_string()),
                            }
                        }
                    };
                    if s.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    w();
                    if sender.send(event).is_err() {
                        break;
                    }
                    w();
                }
            })
            .expect("editor job worker");
        let s = shared.clone();
        let watcher = thread::Builder::new()
            .name("wex-file-watch".into())
            .spawn(move || watch_loop(root, s, tx, wake, inventory))
            .expect("file watcher");
        Self {
            shared,
            events: Some(rx),
            threads: vec![search, watcher],
            next_id: 0,
            watch_generation: 0,
        }
    }
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.shared.latest.store(self.next_id, Ordering::Relaxed);
        self.next_id
    }
    pub fn search(&mut self, epoch: u64, doc: &Document, options: SearchOptions) -> u64 {
        let id = self.id();
        *self.shared.job.lock().unwrap() = Some(Job::Search {
            id,
            epoch,
            revision: doc.revision,
            rope: doc.rope().clone(),
            options,
        });
        self.shared.changed.notify_one();
        id
    }
    #[allow(clippy::too_many_arguments)]
    pub fn replace(
        &mut self,
        epoch: u64,
        doc: &Document,
        edit: &EditState,
        options: SearchOptions,
        replacement: String,
        only: Option<std::ops::Range<usize>>,
    ) -> anyhow::Result<u64> {
        let fork = Box::new(doc.fork()?);
        let id = self.id();
        *self.shared.job.lock().unwrap() = Some(Job::Replace {
            id,
            epoch,
            revision: doc.revision,
            doc: fork,
            edit: edit.clone(),
            options,
            replacement,
            only,
        });
        self.shared.changed.notify_one();
        Ok(id)
    }
    pub fn cancel_search(&mut self) {
        self.id();
        self.shared.job.lock().unwrap().take();
    }
    pub fn watch(&mut self, paths: Vec<(PathBuf, Option<blake3::Hash>)>) -> u64 {
        self.watch_generation += 1;
        *self.shared.watch.lock().unwrap() = (self.watch_generation, paths);
        self.watch_generation
    }
    pub fn poll(&self) -> Option<EditorEvent> {
        self.events.as_ref()?.try_recv().ok()
    }
}
impl Drop for EditorServices {
    fn drop(&mut self) {
        self.events.take();
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.latest.fetch_add(1, Ordering::Relaxed);
        self.shared.changed.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    identity: u64,
    error: Option<String>,
}
fn stamp(path: &std::path::Path) -> Stamp {
    match std::fs::metadata(path) {
        Ok(m) => {
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                m.ino()
            };
            #[cfg(not(unix))]
            let identity = 0;
            Stamp {
                length: m.len(),
                modified: m.modified().ok(),
                identity,
                error: None,
            }
        }
        Err(e) => Stamp {
            length: 0,
            modified: None,
            identity: 0,
            error: Some(e.to_string()),
        },
    }
}
fn fingerprint(path: &std::path::Path, stop: &AtomicBool) -> Fingerprint {
    match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut h = blake3::Hasher::new();
            let mut buffer = [0; 65536];
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Fingerprint::Error("watcher stopped".into());
                }
                match file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        h.update(&buffer[..n]);
                    }
                    Err(e) => return Fingerprint::Error(e.to_string()),
                }
            }
            Fingerprint::Present(h.finalize())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Fingerprint::Missing,
        Err(e) => Fingerprint::Error(e.to_string()),
    }
}
struct Observed {
    fingerprint: Fingerprint,
    stamp: Option<Stamp>,
    scrub: Instant,
    pending: Option<(Fingerprint, Instant, Instant)>,
}
fn watch_loop(
    root: PathBuf,
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<EditorEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    inventory: bool,
) {
    let mut generation = 0;
    let mut entries: BTreeMap<PathBuf, Observed> = BTreeMap::new();
    let mut files = None;
    let mut scan = Instant::now() - Duration::from_secs(2);
    while !shared.stop.load(Ordering::Relaxed) {
        let (g, paths) = shared.watch.lock().unwrap().clone();
        if g != generation {
            generation = g;
            entries = paths
                .into_iter()
                .map(|(p, h)| {
                    (
                        p,
                        Observed {
                            fingerprint: h
                                .map(Fingerprint::Present)
                                .unwrap_or(Fingerprint::Missing),
                            stamp: None,
                            scrub: Instant::now() - Duration::from_secs(3),
                            pending: None,
                        },
                    )
                })
                .collect();
        }
        for (path, e) in &mut entries {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            let now = Instant::now();
            let s = stamp(path);
            if e.stamp.as_ref() == Some(&s)
                && e.pending.is_none()
                && e.scrub.elapsed() < Duration::from_secs(2)
            {
                continue;
            }
            let fp = fingerprint(path, &shared.stop);
            e.stamp = Some(s);
            e.scrub = now;
            if fp == e.fingerprint {
                e.pending = None;
                continue;
            }
            match &mut e.pending {
                Some((candidate, _, changed)) => {
                    if *candidate != fp {
                        *candidate = fp.clone();
                        *changed = now;
                    }
                }
                None => e.pending = Some((fp.clone(), now, now)),
            }
            let (_, first, changed) = e.pending.as_ref().unwrap();
            if changed.elapsed() < Duration::from_millis(250)
                && first.elapsed() < Duration::from_millis(1500)
            {
                continue;
            }
            let state = match fp {
                Fingerprint::Present(_) => match Document::open(path) {
                    Ok(d) => {
                        e.fingerprint = Fingerprint::Present(d.disk_hash().unwrap());
                        DiskState::Present(Box::new(d))
                    }
                    Err(err) => {
                        let message = err.to_string();
                        e.fingerprint = Fingerprint::Error(message.clone());
                        DiskState::Error(message)
                    }
                },
                Fingerprint::Missing => {
                    e.fingerprint = Fingerprint::Missing;
                    DiskState::Missing
                }
                Fingerprint::Error(message) => {
                    e.fingerprint = Fingerprint::Error(message.clone());
                    DiskState::Error(message)
                }
            };
            e.pending = None;
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            wake();
            if sender
                .send(EditorEvent::Disk {
                    generation,
                    path: path.clone(),
                    state,
                })
                .is_err()
            {
                return;
            }
            wake();
        }
        if inventory && scan.elapsed() > Duration::from_secs(1) {
            scan = Instant::now();
            let started = Instant::now();
            let next = crate::project::explorer_entries(&root);
            if files.as_ref() != Some(&next) {
                files = Some(next.clone());
                wake();
                if sender
                    .send(EditorEvent::Tree {
                        started,
                        inventory: next,
                    })
                    .is_err()
                {
                    return;
                }
                wake();
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait(s: &EditorServices, mut predicate: impl FnMut(EditorEvent) -> bool) {
        let end = Instant::now() + Duration::from_secs(6);
        loop {
            if let Some(e) = s.poll()
                && predicate(e)
            {
                return;
            }
            assert!(Instant::now() < end, "worker event timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn watcher_detects_atomic_replace_delete_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Main.kt");
        std::fs::write(&path, "val x = 1\n").unwrap();
        let doc = Document::open(&path).unwrap();
        let mut s = EditorServices::new(dir.path().to_owned(), Arc::new(|| {}));
        let generation = s.watch(vec![(path.clone(), doc.disk_hash())]);
        let temp = dir.path().join("save.tmp");
        std::fs::write(&temp, "val x = 2\n").unwrap();
        std::fs::rename(temp, &path).unwrap();
        wait(
            &s,
            |e| matches!(e,EditorEvent::Disk{generation:g,state:DiskState::Present(ref d),..}if g==generation&&d.rope().to_string().contains("2")),
        );
        std::fs::remove_file(&path).unwrap();
        wait(&s, |e| {
            matches!(
                e,
                EditorEvent::Disk {
                    state: DiskState::Missing,
                    ..
                }
            )
        });
        std::fs::write(&path, "val x = 3\n").unwrap();
        wait(
            &s,
            |e| matches!(e,EditorEvent::Disk{state:DiskState::Present(ref d),..}if d.rope().to_string().contains("3")),
        );
    }
    #[test]
    fn search_and_replacement_run_on_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let doc = Document::new("abc abc\n").unwrap();
        let mut s = EditorServices::new(dir.path().to_owned(), Arc::new(|| {}));
        let o = SearchOptions {
            query: "abc".into(),
            ..Default::default()
        };
        let id = s.search(7, &doc, o.clone());
        wait(
            &s,
            |e| matches!(e,EditorEvent::Found{id:i,epoch:7,ref result,..}if i==id&&result.matches.len()==2),
        );
        let id = s
            .replace(7, &doc, &EditState::default(), o, "привет".into(), None)
            .unwrap();
        wait(
            &s,
            |e| matches!(e,EditorEvent::Replaced{id:i,result:Ok((ref d,_,_,2)),..}if i==id&&d.rope()=="привет привет\n"),
        );
        assert_eq!(doc.rope().to_string(), "abc abc\n");
    }
    #[test]
    fn dropping_workers_with_a_full_event_queue_joins() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = EditorServices::new(dir.path().to_owned(), Arc::new(|| {}));
        let d = Document::new("a").unwrap();
        for i in 0..100 {
            s.search(
                i,
                &d,
                SearchOptions {
                    query: "a".into(),
                    ..Default::default()
                },
            );
            thread::sleep(Duration::from_millis(1));
        }
        let now = Instant::now();
        drop(s);
        assert!(now.elapsed() < Duration::from_secs(2));
    }
}
