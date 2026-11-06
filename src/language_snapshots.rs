//! Cheap foreground Rope publication; exact full-text materialization on blocking workers.
use super::Snapshot;
use crate::{
    document_retirement::{self, Payload},
    workspace_edit::Buffer,
};
use anyhow::{Context, Result, ensure};
use ropey::Rope;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct RopeBuffer {
    pub path: PathBuf,
    pub text: Rope,
    pub version: i32,
}
#[derive(Clone, Debug)]
pub struct RopeSnapshot {
    pub path: PathBuf,
    pub text: Rope,
    pub version: i32,
    pub others: Vec<RopeBuffer>,
}
#[derive(Clone)]
enum Input {
    Owned(Option<Snapshot>),
    Rope(Option<RopeSnapshot>),
}
#[derive(Default)]
struct Counters {
    active: AtomicU64,
    published: AtomicU64,
    materialized: AtomicU64,
    cancelled: AtomicU64,
    bytes: AtomicU64,
    thread: Mutex<Option<String>>,
}
#[derive(serde::Serialize, Debug)]
pub struct Metrics {
    pub active: u64,
    pub published: u64,
    pub materialized: u64,
    pub cancelled: u64,
    pub materialized_bytes: u64,
    pub materializer_thread: Option<String>,
}
struct Envelope {
    input: Input,
    cancelled: AtomicBool,
    cached: OnceLock<Option<Snapshot>>,
    serial: Mutex<()>,
    counters: Arc<Counters>,
}
impl Drop for Envelope {
    fn drop(&mut self) {
        let mut ropes = vec![];
        let mut texts = vec![];
        match std::mem::replace(&mut self.input, Input::Owned(None)) {
            Input::Owned(s) => collect(s, &mut texts),
            Input::Rope(Some(s)) => {
                ropes.push(s.text);
                ropes.extend(s.others.into_iter().map(|b| b.text));
            }
            Input::Rope(None) => (),
        }
        if let Some(cached) = self.cached.take() {
            collect(cached, &mut texts);
        }
        if ropes
            .iter()
            .any(|r| r.len_bytes() >= document_retirement::MIN_BYTES)
            || texts
                .iter()
                .any(|t| t.len() >= document_retirement::MIN_BYTES)
        {
            document_retirement::retire(Payload::Language { ropes, texts });
        }
    }
}
fn collect(s: Option<Snapshot>, texts: &mut Vec<String>) {
    if let Some(s) = s {
        texts.push(s.text);
        texts.extend(s.others.into_iter().map(|b| b.text));
    }
}
impl Envelope {
    fn new(input: Input, counters: Arc<Counters>) -> Self {
        let empty = matches!(&input, Input::Owned(None) | Input::Rope(None));
        Self {
            input,
            cancelled: AtomicBool::new(false),
            cached: if empty {
                OnceLock::from(None)
            } else {
                OnceLock::new()
            },
            serial: Mutex::new(()),
            counters,
        }
    }
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "K2 snapshot superseded or connection stopped"
        );
        Ok(())
    }
    fn materialize(&self) -> Result<Option<Snapshot>> {
        self.check()?;
        if let Some(s) = self.cached.get() {
            return Ok(s.clone());
        }
        let _guard = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        self.check()?;
        if let Some(s) = self.cached.get() {
            return Ok(s.clone());
        }
        struct Active<'a>(&'a AtomicU64);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Release);
            }
        }
        self.counters.active.fetch_add(1, Ordering::Release);
        let _active = Active(&self.counters.active);
        let result = match &self.input {
            Input::Owned(s) => s.clone(),
            Input::Rope(s) => s
                .as_ref()
                .map(|s| {
                    Ok::<_, anyhow::Error>(Snapshot {
                        path: s.path.clone(),
                        text: copy(&s.text, || self.check())?,
                        version: s.version,
                        others: s
                            .others
                            .iter()
                            .map(|b| {
                                Ok(Buffer {
                                    path: b.path.clone(),
                                    text: copy(&b.text, || self.check())?,
                                    version: b.version,
                                })
                            })
                            .collect::<Result<_>>()?,
                    })
                })
                .transpose()?,
        };
        self.check()?;
        let bytes = result.as_ref().map_or(0, |s| {
            s.text.len() + s.others.iter().map(|b| b.text.len()).sum::<usize>()
        });
        self.counters.materialized.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        *self
            .counters
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            Some(format!("{:?}", std::thread::current().id()));
        let _ = self.cached.set(result);
        Ok(self.cached.get().unwrap().clone())
    }
}
fn copy(rope: &Rope, check: impl Fn() -> Result<()>) -> Result<String> {
    check()?;
    let mut text = String::new();
    text.try_reserve_exact(rope.len_bytes())
        .context("Allocate K2 snapshot; editor buffer retained")?;
    for chunk in rope.chunks() {
        check()?;
        text.push_str(chunk);
    }
    check()?;
    Ok(text)
}
/// Not a watch guard: no full-text work or destruction happens inside a watch lock.
pub struct SnapshotSender {
    raw: watch::Sender<Arc<Envelope>>,
    counters: Arc<Counters>,
}
#[derive(Clone)]
pub struct Receiver {
    raw: watch::Receiver<Arc<Envelope>>,
}
pub(super) fn channel() -> (SnapshotSender, Receiver) {
    let counters = Arc::new(Counters::default());
    let (raw, receiver) = watch::channel(Arc::new(Envelope::new(
        Input::Owned(None),
        counters.clone(),
    )));
    (SnapshotSender { raw, counters }, Receiver { raw: receiver })
}
impl Default for SnapshotSender {
    fn default() -> Self {
        channel().0
    }
}
impl SnapshotSender {
    pub fn subscribe(&self) -> Receiver {
        Receiver {
            raw: self.raw.subscribe(),
        }
    }
    /// Compatibility publication for non-GUI callers that already own String buffers.
    pub fn send_replace(&self, snapshot: Option<Snapshot>) {
        self.publish(Input::Owned(snapshot));
    }
    pub fn send_rope(&self, snapshot: Option<RopeSnapshot>) {
        self.publish(Input::Rope(snapshot));
    }
    fn publish(&self, input: Input) {
        self.counters.published.fetch_add(1, Ordering::Relaxed);
        let old = self
            .raw
            .send_replace(Arc::new(Envelope::new(input, self.counters.clone())));
        old.cancelled.store(true, Ordering::Release);
        // Last-owner Envelope Drop transfers heavy Rope/String payloads to retirement.
        drop(old);
    }
    pub fn metrics(&self) -> Metrics {
        Metrics {
            active: self.counters.active.load(Ordering::Relaxed),
            published: self.counters.published.load(Ordering::Relaxed),
            materialized: self.counters.materialized.load(Ordering::Relaxed),
            cancelled: self.counters.cancelled.load(Ordering::Relaxed),
            materialized_bytes: self.counters.bytes.load(Ordering::Relaxed),
            materializer_thread: self
                .counters
                .thread
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}
impl Drop for SnapshotSender {
    fn drop(&mut self) {
        self.raw.borrow().cancelled.store(true, Ordering::Release);
    }
}
impl Receiver {
    pub async fn changed(&mut self) -> Result<()> {
        self.raw.changed().await.context("editor snapshots closed")
    }
    pub async fn latest(&mut self) -> Result<Option<Snapshot>> {
        loop {
            let envelope = { self.raw.borrow_and_update().clone() };
            let input = envelope.clone();
            let result = tokio::task::spawn_blocking(move || input.materialize())
                .await
                .context("K2 snapshot worker")?;
            let latest = { self.raw.borrow().clone() };
            if !Arc::ptr_eq(&envelope, &latest) {
                envelope.counters.cancelled.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            return result;
        }
    }
    pub fn matches(&self, value: &Option<Snapshot>) -> bool {
        let latest = { self.raw.borrow().clone() };
        !latest.cancelled.load(Ordering::Acquire)
            && latest
                .cached
                .get()
                .is_some_and(|s| super::reload::same(value, s))
    }
}
pub(super) async fn materialize_buffers(buffers: Vec<RopeBuffer>) -> Result<Vec<Buffer>> {
    tokio::task::spawn_blocking(move || {
        buffers
            .iter()
            .map(|b| {
                Ok(Buffer {
                    path: b.path.clone(),
                    text: copy(&b.text, || Ok(()))?,
                    version: b.version,
                })
            })
            .collect::<Result<_>>()
    })
    .await
    .context("semantic snapshot worker")?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(version: i32, text: &str) -> RopeSnapshot {
        RopeSnapshot {
            path: "/A.kt".into(),
            text: Rope::from_str(text),
            version,
            others: vec![RopeBuffer {
                path: "/B.kt".into(),
                text: Rope::from_str("val привет=\"👋\"\n"),
                version: 3,
            }],
        }
    }
    #[tokio::test]
    async fn immutable_unicode_rope_publication_is_lazy_and_copies_on_different_thread() {
        let (sender, mut receiver) = channel();
        let mut s = snapshot(8, "val 世界=\"👋\"\n");
        sender.send_rope(Some(s.clone()));
        assert_eq!(sender.metrics().materialized, 0);
        assert!(!receiver.matches(&None));
        s.text.insert(0, "// changed local clone\n");
        let ready = receiver.latest().await.unwrap().unwrap();
        assert_eq!(ready.text, "val 世界=\"👋\"\n");
        assert_eq!(ready.others[0].text, "val привет=\"👋\"\n");
        assert_eq!(ready.version, 8);
        let metrics = sender.metrics();
        assert_eq!(metrics.materialized, 1);
        assert_eq!(metrics.active, 0);
        assert_ne!(
            metrics.materializer_thread.unwrap(),
            format!("{:?}", std::thread::current().id())
        );
        assert!(receiver.matches(&Some(ready)));
        receiver.latest().await.unwrap();
        assert_eq!(sender.metrics().materialized, 1);
    }
    #[tokio::test]
    async fn pending_raw_input_fences_candidates_and_latest_coalesces_versions_and_clear() {
        let (sender, mut receiver) = channel();
        sender.send_rope(Some(snapshot(1, "val a=1")));
        let old = receiver.latest().await.unwrap();
        assert!(receiver.matches(&old));
        sender.send_rope(Some(snapshot(2, "val a=2")));
        sender.send_rope(Some(snapshot(3, "val a=3")));
        assert!(!receiver.matches(&old));
        let new = receiver.latest().await.unwrap();
        assert_eq!(new.as_ref().unwrap().version, 3);
        assert!(receiver.matches(&new));
        assert_eq!(sender.metrics().materialized, 2);
        sender.send_rope(None);
        assert!(!receiver.matches(&new));
        assert!(receiver.latest().await.unwrap().is_none());
        assert!(receiver.matches(&None));
    }
    #[tokio::test]
    async fn candidate_and_active_readers_share_one_successful_materialization() {
        let (sender, mut active) = channel();
        let mut candidate = active.clone();
        sender.send_rope(Some(snapshot(5, &"// comment\n".repeat(100000))));
        let (a, b) = tokio::join!(active.latest(), candidate.latest());
        assert!(super::super::reload::same(&a.unwrap(), &b.unwrap()));
        assert_eq!(sender.metrics().materialized, 1);
    }
    #[tokio::test]
    async fn owned_string_compatibility_and_sender_drop_keep_exact_clocks_and_stop_readers() {
        let (sender, mut receiver) = channel();
        sender.send_replace(Some(Snapshot {
            path: "/C.kt".into(),
            text: "val c=7".into(),
            version: 27,
            others: vec![],
        }));
        assert_eq!(receiver.latest().await.unwrap().unwrap().version, 27);
        drop(sender);
        assert!(receiver.latest().await.is_err());
        assert!(receiver.changed().await.is_err());
    }
    #[test]
    fn cancellation_during_real_chunk_copy_never_returns_partial_text() {
        let rope = Rope::from_str(&"// actual Unicode 👋\n".repeat(10000));
        let count = std::cell::Cell::new(0);
        let r = copy(&rope, || {
            count.set(count.get() + 1);
            ensure!(count.get() < 8, "cancelled");
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(count.get(), 8);
        assert!(rope.len_bytes() > 100000);
    }
    #[tokio::test]
    async fn semantic_rope_buffers_materialize_exact_versions_off_foreground() {
        let s = snapshot(8, "val a=1");
        let b = materialize_buffers(s.others).await.unwrap();
        assert_eq!(b[0].version, 3);
        assert_eq!(b[0].text, "val привет=\"👋\"\n");
    }
    #[tokio::test]
    async fn actual_inflight_large_copy_is_superseded_without_synthetic_delay() {
        let (sender, mut receiver) = channel();
        let rope = Rope::from_str(&"// real serialization input\n".repeat(2500000));
        sender.send_rope(Some(RopeSnapshot {
            path: "/Large.kt".into(),
            text: rope,
            version: 1,
            others: vec![],
        }));
        let task = tokio::spawn(async move { receiver.latest().await });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while sender.metrics().active == 0 {
            assert!(
                std::time::Instant::now() < deadline && !task.is_finished(),
                "real copy completed before observation"
            );
            tokio::task::yield_now().await;
        }
        sender.send_rope(Some(snapshot(2, "val newest=2")));
        let newest = task.await.unwrap().unwrap().unwrap();
        assert_eq!(newest.version, 2);
        assert_eq!(newest.text, "val newest=2");
        assert!(sender.metrics().cancelled > 0);
        assert_eq!(sender.metrics().active, 0);
    }
}
