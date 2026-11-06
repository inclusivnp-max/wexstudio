//! One lazy, latest-request-wins document reader/parser. Nothing touches disk on UI request.
use crate::editor::Document;
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};
struct Job {
    id: u64,
    path: PathBuf,
    done: Arc<AtomicBool>,
}
struct Shared {
    generation: AtomicU64,
    stop: AtomicBool,
    active: AtomicU64,
    job: Mutex<Option<Job>>,
    changed: Condvar,
}
#[derive(Clone)]
pub struct Ticket {
    pub id: u64,
    pub done: Arc<AtomicBool>,
}
pub struct Opened {
    pub id: u64,
    pub requested: PathBuf,
    pub result: Result<Box<Document>, String>,
}
pub struct DocumentOpener {
    shared: Arc<Shared>,
    rx: Option<mpsc::Receiver<Opened>>,
    thread: Option<thread::JoinHandle<()>>,
    next: u64,
}
impl Default for DocumentOpener {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                generation: AtomicU64::new(0),
                stop: AtomicBool::new(false),
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
impl DocumentOpener {
    fn start(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        let (tx, rx) = mpsc::sync_channel(2);
        let s = self.shared.clone();
        let thread = thread::Builder::new()
            .name("wex-document-open".into())
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
                    s.active.store(job.id, Ordering::Release);
                    let cancelled = || {
                        s.stop.load(Ordering::Acquire)
                            || s.generation.load(Ordering::Acquire) != job.id
                    };
                    let result = Document::open_cancellable(&job.path, cancelled)
                        .map(Box::new)
                        .map_err(|e| format!("{e:#}"));
                    s.active.store(0, Ordering::Release);
                    job.done.store(true, Ordering::Release);
                    if cancelled() {
                        continue;
                    }
                    wake();
                    if tx
                        .send(Opened {
                            id: job.id,
                            requested: job.path,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            })
            .context("Start document reader")?;
        self.rx = Some(rx);
        self.thread = Some(thread);
        Ok(())
    }
    pub fn request(&mut self, path: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Ticket> {
        self.start(wake)?;
        let id = self
            .next
            .checked_add(1)
            .context("Document request clock exhausted")?;
        self.next = id;
        self.shared.generation.store(id, Ordering::Release);
        let done = Arc::new(AtomicBool::new(false));
        let old = self.shared.job.lock().unwrap().replace(Job {
            id,
            path,
            done: done.clone(),
        });
        if let Some(old) = old {
            old.done.store(true, Ordering::Release);
        }
        self.shared.changed.notify_one();
        Ok(Ticket { id, done })
    }
    pub fn cancel(&mut self) {
        self.shared.generation.store(0, Ordering::Release);
        if let Some(job) = self.shared.job.lock().unwrap().take() {
            job.done.store(true, Ordering::Release);
        }
        self.shared.changed.notify_one();
    }
    pub fn poll(&self) -> Option<Opened> {
        self.rx.as_ref()?.try_recv().ok()
    }
    pub fn active_id(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }
    pub fn worker_started(&self) -> bool {
        self.thread.is_some()
    }
}
impl Drop for DocumentOpener {
    fn drop(&mut self) {
        self.cancel();
        self.shared.stop.store(true, Ordering::Release);
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
    use std::time::{Duration, Instant};
    fn wait(o: &DocumentOpener) -> Opened {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(e) = o.poll() {
                return e;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn lazy_worker_opens_actual_utf8_and_returns_errors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        std::fs::write(&p, "// 日本語\nval a=1\n").unwrap();
        let mut o = DocumentOpener::default();
        assert!(!o.worker_started());
        let ticket = o.request(p.clone(), Arc::new(|| {})).unwrap();
        let result = wait(&o);
        assert_eq!(ticket.id, result.id);
        assert!(ticket.done.load(Ordering::Acquire));
        let doc = result.result.unwrap();
        assert_eq!(doc.path, Some(p));
        assert_eq!(doc.rope().to_string(), "// 日本語\nval a=1\n");
        o.request(d.path().join("missing.kt"), Arc::new(|| {}))
            .unwrap();
        assert!(wait(&o).result.is_err());
    }
    #[test]
    fn cancelled_real_parse_cannot_replace_latest_request() {
        let d = tempfile::tempdir().unwrap();
        let large = d.path().join("Large.kt");
        std::fs::write(&large, "private val example = 42\n".repeat(150000)).unwrap();
        let small = d.path().join("Small.kt");
        std::fs::write(&small, "val small=7\n").unwrap();
        let mut o = DocumentOpener::default();
        let old = o.request(large, Arc::new(|| {})).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while o.active_id() != old.id {
            assert!(Instant::now() < end);
            thread::yield_now();
        }
        let latest = o.request(small.clone(), Arc::new(|| {})).unwrap();
        let e = wait(&o);
        assert_eq!(e.id, latest.id);
        assert_eq!(e.result.unwrap().path, Some(small));
        assert!(old.done.load(Ordering::Acquire));
    }
    #[test]
    fn drop_never_waits_on_a_full_result_queue() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        std::fs::write(&p, "val a=1\n").unwrap();
        let mut o = DocumentOpener::default();
        for _ in 0..3 {
            let t = o.request(p.clone(), Arc::new(|| {})).unwrap();
            let end = Instant::now() + Duration::from_secs(2);
            while !t.done.load(Ordering::Acquire) {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(1));
            }
        }
        o.request(p.clone(), Arc::new(|| {})).unwrap();
        o.request(p, Arc::new(|| {})).unwrap();
        let start = Instant::now();
        drop(o);
        assert!(start.elapsed() < Duration::from_millis(100));
    }
}
