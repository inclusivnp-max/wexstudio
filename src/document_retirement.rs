//! Ownership-based destruction of large desktop revisions, away from the GPU thread.
//! This is not tracing GC: last-owner Drop transfers already allocated Rope/tree/parser
//! values to one lazy worker. The queue has no hard aggregate memory cap; its logical
//! text-byte counter is NOT a measurement of unique/shared heap allocations.
use ropey::Rope;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering::SeqCst},
    mpsc,
};
use tree_sitter::{InputEdit, Parser, Tree};

pub(crate) const MIN_BYTES: usize = 64 * 1024;
pub(crate) enum Payload {
    Text {
        rope: Rope,
        tree: Option<Tree>,
        seed: Option<(Tree, Vec<InputEdit>)>,
        tracking: Option<Arc<crate::text_tracking::TextChange>>,
    },
    Parser(Parser),
    #[cfg(feature = "desktop")]
    Layout {
        galley: Arc<eframe::egui::Galley>,
        image: Arc<eframe::egui::ColorImage>,
    },
    Language {
        ropes: Vec<Rope>,
        texts: Vec<String>,
    },
    Tracking {
        edits: Vec<crate::text_tracking::Edit>,
        removed: Vec<std::ops::Range<usize>>,
    },
}
impl Payload {
    fn bytes(&self) -> u64 {
        match self {
            Self::Text { rope, .. } => rope.len_bytes() as u64,
            Self::Parser(_) => 0,
            Self::Tracking { .. } => 0,
            #[cfg(feature = "desktop")]
            Self::Layout { .. } => 0,
            Self::Language { ropes, texts } => {
                ropes.iter().map(|r| r.len_bytes() as u64).sum::<u64>()
                    + texts.iter().map(|t| t.len() as u64).sum::<u64>()
            }
        }
    }
    fn release(self) {
        // Explicit fields make the worker's ownership/destruction boundary visible.
        match self {
            Self::Text {
                rope,
                tree,
                seed,
                tracking,
            } => drop((rope, tree, seed, tracking)),
            Self::Parser(parser) => drop(parser),
            #[cfg(feature = "desktop")]
            Self::Layout { galley, image } => drop((galley, image)),
            Self::Language { ropes, texts } => drop((ropes, texts)),
            Self::Tracking { edits, removed } => drop((edits, removed)),
        }
    }
}
#[derive(Default)]
struct Counters {
    workers: AtomicU64,
    submitted: AtomicU64,
    reclaimed: AtomicU64,
    text_reclaimed: AtomicU64,
    language_reclaimed: AtomicU64,
    parsers_reclaimed: AtomicU64,
    layouts_reclaimed: AtomicU64,
    tracking_reclaimed: AtomicU64,
    worker_thread: OnceLock<String>,
    pending: AtomicU64,
    peak_pending: AtomicU64,
    logical_bytes_pending: AtomicU64,
    peak_logical_bytes: AtomicU64,
    synchronous_fallbacks: AtomicU64,
}
/// Diagnostic counters only; never wait for this worker in an input handler.
#[derive(Debug, serde::Serialize)]
pub struct Snapshot {
    pub workers: u64,
    pub submitted: u64,
    pub reclaimed: u64,
    pub text_reclaimed: u64,
    pub language_reclaimed: u64,
    pub parsers_reclaimed: u64,
    pub layouts_reclaimed: u64,
    pub tracking_reclaimed: u64,
    pub worker_thread: Option<String>,
    pub pending: u64,
    pub peak_pending: u64,
    pub logical_bytes_pending: u64,
    pub peak_logical_bytes: u64,
    pub synchronous_fallbacks: u64,
}
impl Counters {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            workers: self.workers.load(SeqCst),
            submitted: self.submitted.load(SeqCst),
            reclaimed: self.reclaimed.load(SeqCst),
            text_reclaimed: self.text_reclaimed.load(SeqCst),
            language_reclaimed: self.language_reclaimed.load(SeqCst),
            parsers_reclaimed: self.parsers_reclaimed.load(SeqCst),
            layouts_reclaimed: self.layouts_reclaimed.load(SeqCst),
            tracking_reclaimed: self.tracking_reclaimed.load(SeqCst),
            worker_thread: self.worker_thread.get().cloned(),
            pending: self.pending.load(SeqCst),
            peak_pending: self.peak_pending.load(SeqCst),
            logical_bytes_pending: self.logical_bytes_pending.load(SeqCst),
            peak_logical_bytes: self.peak_logical_bytes.load(SeqCst),
            synchronous_fallbacks: self.synchronous_fallbacks.load(SeqCst),
        }
    }
    fn finish(&self, bytes: u64) {
        self.logical_bytes_pending.fetch_sub(bytes, SeqCst);
        self.pending.fetch_sub(1, SeqCst);
    }
}
enum Message {
    Retire(Payload),
    #[cfg(test)]
    Barrier(mpsc::Sender<std::thread::ThreadId>),
}
struct Worker {
    sender: mpsc::Sender<Message>,
}
impl Worker {
    fn start(counters: Arc<Counters>) -> std::io::Result<(Self, std::thread::JoinHandle<()>)> {
        let (sender, receiver) = mpsc::channel();
        let worker_counters = counters.clone();
        let join = std::thread::Builder::new()
            .name("wex-document-retirement".into())
            .spawn(move || {
                let _ = worker_counters
                    .worker_thread
                    .set(format!("{:?}", std::thread::current().id()));
                for message in receiver {
                    match message {
                        Message::Retire(payload) => {
                            let bytes = payload.bytes();
                            let parser = matches!(payload, Payload::Parser(_));
                            let language = matches!(payload, Payload::Language { .. });
                            let tracking = matches!(payload, Payload::Tracking { .. });
                            #[cfg(feature = "desktop")]
                            let layout = matches!(payload, Payload::Layout { .. });
                            #[cfg(not(feature = "desktop"))]
                            let layout = false;
                            payload.release();
                            if layout {
                                worker_counters.layouts_reclaimed.fetch_add(1, SeqCst);
                            } else if parser {
                                worker_counters.parsers_reclaimed.fetch_add(1, SeqCst);
                            } else if language {
                                worker_counters.language_reclaimed.fetch_add(1, SeqCst);
                            } else if tracking {
                                worker_counters.tracking_reclaimed.fetch_add(1, SeqCst);
                            } else {
                                worker_counters.text_reclaimed.fetch_add(1, SeqCst);
                            }
                            worker_counters.reclaimed.fetch_add(1, SeqCst);
                            worker_counters.finish(bytes);
                        }
                        #[cfg(test)]
                        Message::Barrier(reply) => {
                            let _ = reply.send(std::thread::current().id());
                        }
                    }
                }
            })?;
        counters.workers.fetch_add(1, SeqCst);
        Ok((Self { sender }, join))
    }
}
fn counters() -> &'static Arc<Counters> {
    static COUNTERS: OnceLock<Arc<Counters>> = OnceLock::new();
    COUNTERS.get_or_init(|| Arc::new(Counters::default()))
}
pub fn snapshot() -> Snapshot {
    counters().snapshot()
}
pub(crate) fn retire(payload: Payload) {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| match Worker::start(counters().clone()) {
        Ok((worker, _join)) => Some(worker), // Detached; never join on the UI thread.
        Err(e) => {
            eprintln!("Document retirement worker: {e}; using counted synchronous release");
            None
        }
    });
    submit(worker.as_ref(), counters(), payload);
}
fn submit(worker: Option<&Worker>, counters: &Counters, payload: Payload) {
    let bytes = payload.bytes();
    counters.submitted.fetch_add(1, SeqCst);
    let pending = counters.pending.fetch_add(1, SeqCst) + 1;
    counters.peak_pending.fetch_max(pending, SeqCst);
    let pending_bytes = counters.logical_bytes_pending.fetch_add(bytes, SeqCst) + bytes;
    counters.peak_logical_bytes.fetch_max(pending_bytes, SeqCst);
    let message = Message::Retire(payload);
    let rejected = match worker {
        Some(worker) => worker.sender.send(message).err().map(|e| e.0),
        None => Some(message),
    };
    if let Some(rejected) = rejected {
        // Retain ownership on resource/thread failure: no leak, lost state or panic.
        counters.synchronous_fallbacks.fetch_add(1, SeqCst);
        drop(rejected);
        counters.finish(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text() -> Payload {
        let source = "val number = 123\n".repeat(5000);
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        Payload::Text {
            rope: Rope::from_str(&source),
            seed: Some((tree.clone(), vec![])),
            tree: Some(tree),
            tracking: None,
        }
    }
    #[test]
    fn actual_owned_rope_trees_and_parser_are_reclaimed_on_one_different_thread() {
        let counters = Arc::new(Counters::default());
        let (worker, join) = Worker::start(counters.clone()).unwrap();
        for _ in 0..4 {
            submit(Some(&worker), &counters, text());
        }
        submit(Some(&worker), &counters, Payload::Parser(Parser::new()));
        let (tx, rx) = mpsc::channel();
        worker.sender.send(Message::Barrier(tx)).unwrap();
        assert_ne!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            std::thread::current().id()
        );
        let s = counters.snapshot();
        assert_eq!(
            (
                s.workers,
                s.submitted,
                s.reclaimed,
                s.pending,
                s.logical_bytes_pending,
                s.synchronous_fallbacks
            ),
            (1, 5, 5, 0, 0, 0)
        );
        assert!(s.peak_pending > 0 && s.peak_logical_bytes >= 80000);
        drop(worker);
        join.join().unwrap();
    }
    #[test]
    fn unavailable_or_disconnected_worker_preserves_ownership_and_counts_fallback() {
        let counters = Counters::default();
        submit(None, &counters, text());
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        submit(Some(&Worker { sender }), &counters, text());
        let s = counters.snapshot();
        assert_eq!(
            (
                s.submitted,
                s.reclaimed,
                s.pending,
                s.logical_bytes_pending,
                s.synchronous_fallbacks
            ),
            (2, 0, 0, 0, 2)
        );
    }
    #[test]
    fn diagnostic_observation_does_not_start_a_worker() {
        let counters = Counters::default();
        assert_eq!(counters.snapshot().workers, 0);
    }
}
