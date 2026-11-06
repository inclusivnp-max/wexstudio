//! One lazy latest-state parser. Tree.edit and parsing both stay off the GPU thread.
use crate::editor::SyntaxSnapshot;
use anyhow::{Context, Result};
use std::{
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
    snapshot: SyntaxSnapshot,
    done: Arc<AtomicBool>,
}
struct Shared {
    generation: AtomicU64,
    stop: AtomicBool,
    active: AtomicU64,
    completed: AtomicU64,
    job: Mutex<Option<Job>>,
    changed: Condvar,
}
#[derive(Clone)]
pub struct Ticket {
    pub id: u64,
    pub done: Arc<AtomicBool>,
}
pub struct Parsed {
    pub id: u64,
    pub snapshot: SyntaxSnapshot,
    pub result: Result<Duration, String>,
}
pub struct SyntaxParser {
    shared: Arc<Shared>,
    rx: Option<mpsc::Receiver<Parsed>>,
    thread: Option<thread::JoinHandle<()>>,
    next: u64,
}
impl Default for SyntaxParser {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                generation: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                active: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                job: Mutex::new(None),
                changed: Condvar::new(),
            }),
            rx: None,
            thread: None,
            next: 0,
        }
    }
}
impl SyntaxParser {
    fn start(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        let (tx, rx) = mpsc::sync_channel(2);
        let s = self.shared.clone();
        let t = thread::Builder::new()
            .name("wex-syntax-parser".into())
            .spawn(move || {
                let mut parser = tree_sitter::Parser::new();
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
                    let cancel = || {
                        s.stop.load(Ordering::Acquire)
                            || s.generation.load(Ordering::Acquire) != job.id
                    };
                    let start = Instant::now();
                    let result = (|| -> Result<_> {
                        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())?;
                        job.snapshot.parse(&mut parser, cancel)?;
                        Ok(start.elapsed())
                    })()
                    .map_err(|e| format!("{e:#}"));
                    if result.is_err() {
                        parser.reset();
                    }
                    s.active.store(0, Ordering::Release);
                    job.done.store(true, Ordering::Release);
                    if cancel() {
                        continue;
                    }
                    if result.is_ok() {
                        s.completed.fetch_add(1, Ordering::Relaxed);
                    }
                    wake();
                    if tx
                        .send(Parsed {
                            id: job.id,
                            snapshot: job.snapshot,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            })
            .context("Start syntax parser")?;
        self.rx = Some(rx);
        self.thread = Some(t);
        Ok(())
    }
    pub fn request(
        &mut self,
        snapshot: SyntaxSnapshot,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Ticket> {
        self.start(wake)?;
        let id = self
            .next
            .checked_add(1)
            .context("Syntax request clock exhausted")?;
        self.next = id;
        self.shared.generation.store(id, Ordering::Release);
        let done = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.shared.job.lock().unwrap().replace(Job {
            id,
            snapshot,
            done: done.clone(),
        }) {
            old.done.store(true, Ordering::Release);
        }
        self.shared.changed.notify_one();
        Ok(Ticket { id, done })
    }
    pub fn cancel(&self) {
        self.shared.generation.store(0, Ordering::Release);
        if let Some(old) = self.shared.job.lock().unwrap().take() {
            old.done.store(true, Ordering::Release);
        }
        self.shared.changed.notify_one();
    }
    pub fn poll(&self) -> Option<Parsed> {
        self.rx.as_ref()?.try_recv().ok()
    }
    pub fn active_id(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }
    pub fn worker_started(&self) -> bool {
        self.thread.is_some()
    }
    pub fn completed(&self) -> u64 {
        self.shared.completed.load(Ordering::Relaxed)
    }
}
impl Drop for SyntaxParser {
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
    use crate::editor::Document;
    fn wait(p: &SyntaxParser) -> Parsed {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(e) = p.poll() {
                return e;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn lazy_worker_parses_latest_actual_rope_without_mutating_ui_clocks() {
        let mut d = Document::new("val x=1\n").unwrap();
        d.defer_syntax();
        d.edit(6..7, "2").unwrap();
        let mut p = SyntaxParser::default();
        assert!(!p.worker_started());
        let snap = d.syntax_snapshot().unwrap();
        let t = p.request(snap, Arc::new(|| {})).unwrap();
        let clocks = (d.version, d.revision);
        let e = wait(&p);
        assert_eq!(e.id, t.id);
        assert!(e.result.is_ok() && e.snapshot.matches(&d) && t.done.load(Ordering::Acquire));
        assert!(d.tree().is_some());
        assert_eq!(clocks, (d.version, d.revision));
        assert_eq!(p.completed(), 1);
    }
    #[test]
    fn real_large_parser_is_superseded_and_cancelled_without_fake_delay() {
        let mut d = Document::new(&"private val x=1\n".repeat(60000)).unwrap();
        d.defer_syntax();
        d.edit(0..0, "// old\n").unwrap();
        let mut p = SyntaxParser::default();
        let old = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while p.active_id() != old.id {
            assert!(Instant::now() < end);
            thread::yield_now();
        }
        d.edit(0..d.rope().len_chars(), "val latest=7\n").unwrap();
        let latest = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        loop {
            let e = wait(&p);
            if e.id == latest.id {
                assert!(e.result.is_ok() && e.snapshot.matches(&d));
                break;
            }
        }
        assert!(old.done.load(Ordering::Acquire));
        assert!(d.tree().is_some());
        assert_eq!(d.rope(), "val latest=7\n");
    }
    #[test]
    fn cancel_marks_queued_tickets_and_drop_never_joins_active_parse() {
        let mut d = Document::new(&"private val x=1\n".repeat(50000)).unwrap();
        d.defer_syntax();
        d.edit(0..0, "// updated\n").unwrap();
        let mut p = SyntaxParser::default();
        let a = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        let b = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        p.cancel();
        let start = Instant::now();
        drop(p);
        assert!(start.elapsed() < Duration::from_millis(100));
        let end = Instant::now() + Duration::from_secs(4);
        while !(a.done.load(Ordering::Acquire) && b.done.load(Ordering::Acquire)) {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod backpressure_tests {
    use super::*;
    use crate::editor::Document;
    #[test]
    fn full_result_queue_and_replaced_pending_jobs_drop_without_join() {
        let mut d = Document::new("val x=0\n").unwrap();
        d.defer_syntax();
        let mut p = SyntaxParser::default();
        for n in 1..=3 {
            d.edit(6..7, &n.to_string()).unwrap();
            let t = p
                .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
                .unwrap();
            let end = Instant::now() + Duration::from_secs(3);
            while !t.done.load(Ordering::Acquire) {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(1));
            }
        }
        d.edit(6..7, "4").unwrap();
        let replaced = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        d.edit(6..7, "5").unwrap();
        let last = p
            .request(d.syntax_snapshot().unwrap(), Arc::new(|| {}))
            .unwrap();
        let start = Instant::now();
        drop(p);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(replaced.done.load(Ordering::Acquire));
        assert!(last.done.load(Ordering::Acquire));
    }
}
