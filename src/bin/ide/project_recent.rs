//! Serialized background history tasks; UI keeps last-good data on malformed/busy storage.
use super::*;
use std::collections::VecDeque;
use wexandroidstudio::project_history::{Recent, Store};
#[derive(Clone)]
pub(super) enum Action {
    Load,
    Record(PathBuf),
    Remove(PathBuf),
}
#[derive(Default)]
pub(super) struct HistoryUi {
    pub store: Store,
    pub records: Vec<Recent>,
    pub error: Option<String>,
    pub busy: bool,
    loaded: bool,
    queue: VecDeque<Action>,
    rx: Option<mpsc::Receiver<Result<Vec<Recent>, String>>>,
}
impl HistoryUi {
    pub fn submit(&mut self, action: Action) {
        if self.queue.len() >= 32 {
            self.error = Some("Recent project queue full; retry".into());
            return;
        }
        self.queue.push_back(action);
        self.busy = true;
    }
    pub fn ensure_loaded(&mut self) {
        if !self.loaded && !self.busy {
            self.submit(Action::Load);
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(r) => {
                    self.rx = None;
                    self.loaded = true;
                    match r {
                        Ok(records) => {
                            self.records = records;
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.error = Some("Recent projects worker stopped; retry".into());
                    self.loaded = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.rx.is_none()
            && let Some(action) = self.queue.pop_front()
        {
            let store = self.store.clone();
            let (tx, rx) = mpsc::sync_channel(1);
            self.rx = Some(rx);
            std::thread::spawn(move || {
                let result = match action {
                    Action::Load => store.load(),
                    Action::Record(p) => store.record(&p),
                    Action::Remove(p) => store.remove(&p),
                };
                let _ = tx.send(result.map_err(|e| format!("{e:#}")));
            });
        }
        self.busy = self.rx.is_some() || !self.queue.is_empty();
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn settle(h: &mut HistoryUi) {
        let ctx = egui::Context::default();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while h.busy {
            assert!(std::time::Instant::now() < end);
            h.poll(&ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn error_retains_last_good_and_retry_recovers() {
        let t = tempfile::tempdir().unwrap();
        let p = wexandroidstudio::scaffold::create(t.path(), &Default::default())
            .unwrap()
            .root;
        let mut h = HistoryUi {
            store: Store::new(t.path().join("history.json")).unwrap(),
            ..Default::default()
        };
        h.submit(Action::Record(p));
        settle(&mut h);
        let bytes = std::fs::read(&h.store.path).unwrap();
        std::fs::write(&h.store.path, "bad").unwrap();
        h.submit(Action::Load);
        settle(&mut h);
        assert_eq!(h.records.len(), 1);
        assert!(h.error.is_some());
        std::fs::write(&h.store.path, bytes).unwrap();
        h.submit(Action::Load);
        settle(&mut h);
        assert_eq!(h.records.len(), 1);
        assert!(h.error.is_none());
    }
    #[test]
    fn queued_remove_is_not_overwritten_by_older_load() {
        let t = tempfile::tempdir().unwrap();
        let p = wexandroidstudio::scaffold::create(t.path(), &Default::default())
            .unwrap()
            .root;
        let mut h = HistoryUi {
            store: Store::new(t.path().join("history.json")).unwrap(),
            ..Default::default()
        };
        h.submit(Action::Record(p.clone()));
        h.submit(Action::Load);
        h.submit(Action::Remove(p.clone()));
        settle(&mut h);
        assert!(h.records.is_empty());
        assert!(p.join("wex.json").is_file());
    }
}
