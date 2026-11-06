//! One lazy writer; accepted snapshots are never silently replaced/dropped.
//! Completed writes always return receipts, including when remaining work is stopped.
use crate::editor::{SaveReceipt, SaveSnapshot};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
struct Job {
    id: u64,
    root: PathBuf,
    snapshots: Vec<SaveSnapshot>,
    done: Arc<AtomicBool>,
}
struct Shared {
    stop: AtomicBool,
    cancel: AtomicBool,
    active: AtomicU64,
    job: Mutex<Option<Job>>,
    changed: Condvar,
}
#[derive(Clone)]
pub struct Ticket {
    pub id: u64,
    pub done: Arc<AtomicBool>,
}
pub enum Event {
    Phase {
        id: u64,
        path: PathBuf,
        index: usize,
        total: usize,
        waiting: bool,
    },
    Finished {
        id: u64,
        committed: Vec<SaveReceipt>,
        error: Option<String>,
        cancelled: bool,
    },
}
pub struct DocumentSaver {
    shared: Arc<Shared>,
    rx: Option<mpsc::Receiver<Event>>,
    thread: Option<thread::JoinHandle<()>>,
    next: u64,
}
fn lock_key(path: &Path) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(path.as_os_str().as_encoded_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn lock_path(root: &Path, path: &Path) -> PathBuf {
    root.join(".wex/editor-saves/locks")
        .join(format!("{}.lock", lock_key(path)))
}
impl Default for DocumentSaver {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                stop: AtomicBool::new(false),
                cancel: AtomicBool::new(false),
                active: AtomicU64::new(0),
                job: Mutex::new(None),
                changed: Condvar::new(),
            }),
            rx: None,
            thread: None,
            next: 0,
        }
    }
}
impl DocumentSaver {
    fn start(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        let (tx, rx) = mpsc::sync_channel(8);
        let s = self.shared.clone();
        let thread = thread::Builder::new()
            .name("wex-document-save".into())
            .spawn(move || {
                loop {
                    let mut guard = s.job.lock().unwrap();
                    while guard.is_none() && !s.stop.load(Ordering::Acquire) {
                        guard = s.changed.wait(guard).unwrap();
                    }
                    if s.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let job = guard.take().unwrap();
                    drop(guard);
                    let cancel =
                        || s.stop.load(Ordering::Acquire) || s.cancel.load(Ordering::Acquire);
                    let mut committed = vec![];
                    let mut error = None;
                    let total = job.snapshots.len();
                    for (index, snapshot) in job.snapshots.into_iter().enumerate() {
                        let path = snapshot.path().to_owned();
                        let phase = |waiting| {
                            let _ = tx.try_send(Event::Phase {
                                id: job.id,
                                path: path.clone(),
                                index,
                                total,
                                waiting,
                            });
                            wake();
                        };
                        phase(false);
                        let result = (|| -> Result<_> {
                            ensure!(!cancel(), "Save remaining cancelled");
                            let key = lock_key(&path);
                            let start = Instant::now();
                            let mut waiting = false;
                            let _lock = loop {
                                ensure!(!cancel(), "Save remaining cancelled");
                                if let Some(lock) =
                                    crate::sdk_cache::Guard::editor_save(&job.root, &key)?
                                {
                                    break lock;
                                }
                                if !waiting {
                                    phase(true);
                                    waiting = true;
                                }
                                ensure!(
                                    start.elapsed() < Duration::from_secs(30),
                                    "Another save is busy · Retry"
                                );
                                thread::sleep(Duration::from_millis(20));
                            };
                            if waiting {
                                phase(false);
                            }
                            snapshot.write(cancel)
                        })();
                        match result {
                            Ok(receipt) => committed.push(receipt),
                            Err(e) => {
                                error = Some(format!("{}: {e:#}", path.display()));
                                break;
                            }
                        }
                    }
                    let cancelled = cancel();
                    s.active.store(0, Ordering::Release);
                    job.done.store(true, Ordering::Release);
                    wake();
                    if tx
                        .send(Event::Finished {
                            id: job.id,
                            committed,
                            error,
                            cancelled,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            })
            .context("Start document writer")?;
        self.rx = Some(rx);
        self.thread = Some(thread);
        Ok(())
    }
    pub fn request(
        &mut self,
        root: PathBuf,
        snapshots: Vec<SaveSnapshot>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Ticket> {
        ensure!(!snapshots.is_empty(), "No save snapshots");
        self.start(wake)?;
        let id = self
            .next
            .checked_add(1)
            .context("Save request clock exhausted")?;
        ensure!(
            self.shared
                .active
                .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "A Save is already pending"
        );
        self.next = id;
        self.shared.cancel.store(false, Ordering::Release);
        let done = Arc::new(AtomicBool::new(false));
        *self.shared.job.lock().unwrap() = Some(Job {
            id,
            root,
            snapshots,
            done: done.clone(),
        });
        self.shared.changed.notify_one();
        Ok(Ticket { id, done })
    }
    pub fn cancel_remaining(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        self.shared.changed.notify_all();
    }
    pub fn active_id(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }
    pub fn worker_started(&self) -> bool {
        self.thread.is_some()
    }
    pub fn poll(&self) -> Option<Event> {
        self.rx.as_ref()?.try_recv().ok()
    }
}
impl Drop for DocumentSaver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.cancel_remaining();
        self.rx = None;
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
    fn edited(root: &Path, name: &str) -> Document {
        let p = root.join(name);
        std::fs::write(&p, "val x=1\n").unwrap();
        let mut d = Document::open(p).unwrap();
        d.edit(6..7, "2").unwrap();
        d
    }
    fn wait(s: &DocumentSaver) -> Event {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(e) = s.poll() {
                return e;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn finish(s: &DocumentSaver) -> (Vec<SaveReceipt>, Option<String>, bool) {
        loop {
            if let Event::Finished {
                committed,
                error,
                cancelled,
                ..
            } = wait(s)
            {
                return (committed, error, cancelled);
            }
        }
    }
    fn hold(root: &Path, path: &Path) -> crate::sdk_cache::Guard {
        let key = lock_key(path);
        crate::sdk_cache::Guard::editor_save(root, &key)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn lazy_worker_real_lock_partial_stop_and_retry_preserve_committed_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut a = edited(root, "A.kt");
        let mut b = edited(root, "B.kt");
        let mut c = edited(root, "C.kt");
        let lock = hold(root, b.path.as_ref().unwrap());
        let mut saver = DocumentSaver::default();
        assert!(!saver.worker_started());
        let ticket = saver
            .request(
                root.to_owned(),
                vec![
                    a.save_snapshot().unwrap(),
                    b.save_snapshot().unwrap(),
                    c.save_snapshot().unwrap(),
                ],
                Arc::new(|| {}),
            )
            .unwrap();
        loop {
            if let Event::Phase {
                path,
                waiting: true,
                ..
            } = wait(&saver)
            {
                assert_eq!(path, b.path.clone().unwrap());
                break;
            }
        }
        assert_eq!(
            std::fs::read_to_string(a.path.as_ref().unwrap()).unwrap(),
            "val x=2\n"
        );
        assert_eq!(
            std::fs::read_to_string(b.path.as_ref().unwrap()).unwrap(),
            "val x=1\n"
        );
        assert!(
            saver
                .request(
                    root.to_owned(),
                    vec![c.save_snapshot().unwrap()],
                    Arc::new(|| {})
                )
                .is_err()
        );
        saver.cancel_remaining();
        let (done, error, cancelled) = finish(&saver);
        assert_eq!(done.len(), 1);
        assert!(error.is_some() && cancelled && ticket.done.load(Ordering::Acquire));
        a.accept_saved(&done[0]).unwrap();
        assert!(!a.dirty());
        assert!(b.dirty() && c.dirty());
        drop(lock);
        saver
            .request(
                root.to_owned(),
                vec![b.save_snapshot().unwrap(), c.save_snapshot().unwrap()],
                Arc::new(|| {}),
            )
            .unwrap();
        let (done, error, cancelled) = finish(&saver);
        assert_eq!(done.len(), 2);
        assert!(error.is_none() && !cancelled);
        b.accept_saved(&done[0]).unwrap();
        c.accept_saved(&done[1]).unwrap();
        assert!(!b.dirty() && !c.dirty());
    }
    #[test]
    fn stop_after_completed_write_does_not_drop_queued_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = edited(dir.path(), "A.kt");
        let mut s = DocumentSaver::default();
        let ticket = s
            .request(
                dir.path().to_owned(),
                vec![d.save_snapshot().unwrap()],
                Arc::new(|| {}),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !ticket.done.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        s.cancel_remaining();
        let (done, error, _) = finish(&s);
        assert_eq!(done.len(), 1);
        assert!(error.is_none());
        d.accept_saved(&done[0]).unwrap();
        assert!(!d.dirty());
    }
    #[test]
    fn drop_does_not_join_a_real_lock_waiter() {
        let dir = tempfile::tempdir().unwrap();
        let d = edited(dir.path(), "A.kt");
        let lock = hold(dir.path(), d.path.as_ref().unwrap());
        let mut s = DocumentSaver::default();
        let ticket = s
            .request(
                dir.path().to_owned(),
                vec![d.save_snapshot().unwrap()],
                Arc::new(|| {}),
            )
            .unwrap();
        loop {
            if matches!(wait(&s), Event::Phase { waiting: true, .. }) {
                break;
            }
        }
        let start = Instant::now();
        drop(s);
        assert!(start.elapsed() < Duration::from_millis(100));
        let end = Instant::now() + Duration::from_secs(3);
        while !ticket.done.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        drop(lock);
        assert_eq!(
            std::fs::read_to_string(d.path.as_ref().unwrap()).unwrap(),
            "val x=1\n"
        );
    }
}
