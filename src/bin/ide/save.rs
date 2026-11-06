//! Asynchronous snapshot Save; UI never writes, fsyncs or hashes whole buffers here.
use super::*;
use anyhow::Context;
use std::{collections::BTreeSet, sync::Arc};
use wexandroidstudio::{
    document_saves::{DocumentSaver, Event, Ticket},
    material::{self as m, ButtonKind as Kind, Icon},
};
#[derive(Clone)]
pub(super) enum After {
    None,
    Build,
    Run,
    Debug,
    Close(PathBuf),
    Files(String),
    Git(Vec<PathBuf>),
    WindowClose(u64),
}
pub(super) struct Pending {
    pub ticket: Ticket,
    pub paths: Vec<PathBuf>,
    stop_requested: bool,
    after: After,
    module: String,
    variant: String,
    serial: Option<String>,
}
#[derive(Default)]
pub(super) struct SaveUi {
    pub saver: DocumentSaver,
    pub pending: Option<Pending>,
    again: BTreeSet<PathBuf>,
    pub(super) phase: Option<(PathBuf, usize, usize, bool)>,
    pub error: Option<String>,
    notice: Option<String>,
    retry: Vec<PathBuf>,
    pub last_succeeded: bool,
}
impl SaveUi {
    pub fn contains(&self, path: &std::path::Path) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.paths.iter().any(|p| p == path))
    }
}
impl Workbench {
    pub(super) fn cancel_deferred_for_window_close(&mut self) {
        if let Some(p) = &mut self.save_ui.pending
            && matches!(p.after, After::Build | After::Run | After::Debug)
        {
            p.after = After::None;
        }
    }
    pub(super) fn cancel_window_save_after(&mut self, id: u64) {
        if let Some(p) = &mut self.save_ui.pending
            && matches!(p.after,After::WindowClose(old)if old==id)
        {
            p.after = After::None;
        }
    }

    pub(super) fn request_save_paths(&mut self, paths: Vec<PathBuf>, after: After) -> bool {
        let git_save = matches!(&after,After::Git(wanted) if self.git.open && self.git.saving.as_ref()==Some(wanted) && wanted==&paths && !self.git.busy && self.save_ui.pending.is_none());
        let window_save = matches!(after,After::WindowClose(id)
            if self.close_ui.authorizes_save(id) && !self.close_other_modal());
        if (self.replace_blocked() && !window_save) || (self.git.open && !git_save) {
            self.status = "Close the current review before saving other files".into();
            return false;
        }
        if paths.is_empty() {
            return true;
        }
        if let Some(pending) = &mut self.save_ui.pending {
            if matches!(after, After::None) {
                self.save_ui.again.extend(paths);
                pending.after = After::None;
                self.save_ui.notice =
                    Some("New save queued · restart Build / Run / Debug after it finishes".into());
            }
            self.status = "Save is pending · current editor remains available".into();
            return false;
        }
        let paths = paths
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let result = (|| -> anyhow::Result<_> {
            for p in &paths {
                anyhow::ensure!(
                    !self.editor_ui.conflicts.contains_key(p),
                    "Review external file changes before saving"
                );
            }
            let snapshots = paths
                .iter()
                .map(|p| {
                    let doc = if self.doc.path.as_ref() == Some(p) {
                        Some(&self.doc)
                    } else {
                        self.parked.get(p).map(|(d, _)| d)
                    }
                    .context("Save tab changed; try again")?;
                    doc.save_snapshot()
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let ctx = self.context.clone();
            self.save_ui.saver.request(
                self.root.clone(),
                snapshots,
                Arc::new(move || ctx.request_repaint()),
            )
        })();
        match result {
            Ok(ticket) => {
                self.save_ui.pending = Some(Pending {
                    ticket,
                    paths,
                    stop_requested: false,
                    after,
                    module: self.app_module.clone(),
                    variant: self.variant.clone(),
                    serial: self.selected.clone(),
                });
                self.save_ui.error = None;
                self.save_ui.phase = None;
                self.save_ui.last_succeeded = false;
                self.status = "Saving immutable snapshots · newer edits stay in buffers".into();
                self.context.request_repaint();
                false
            }
            Err(e) => {
                self.save_ui.error = Some(format!("Save: {e:#}"));
                self.status = self.save_ui.error.clone().unwrap();
                self.save_ui.retry = paths;
                self.save_ui.last_succeeded = false;
                false
            }
        }
    }
    pub(super) fn save_before(&mut self, after: After) -> bool {
        if self.save_ui.pending.is_some() {
            self.status = "Wait for pending Save".into();
            return false;
        }
        let disk_action = matches!(after, After::Build | After::Run);
        if self.editor_ui.conflicts.iter().any(|(path, state)| {
            // A clean tab retained for inspection after checkout is not a build
            // input. Never save/recreate it; drafts and every other conflict still
            // require review. In particular, an unknown path is not a clean tab.
            let clean_missing = matches!(state, wexandroidstudio::editor_jobs::DiskState::Missing)
                && if self.doc.path.as_ref() == Some(path) {
                    !self.doc.dirty()
                } else {
                    self.parked.get(path).is_some_and(|(doc, _)| !doc.dirty())
                };
            !(disk_action && clean_missing)
        }) {
            self.status = "Review external file changes before Build / Run".into();
            return false;
        }
        let paths = self
            .tabs
            .iter()
            .filter(|p| self.dirty_path(p))
            .cloned()
            .collect();
        self.request_save_paths(paths, after)
    }
    pub(super) fn document_save_updates(&mut self) {
        for _ in 0..16 {
            let Some(event) = self.save_ui.saver.poll() else {
                break;
            };
            match event {
                Event::Phase {
                    id,
                    path,
                    index,
                    total,
                    waiting,
                } => {
                    if self
                        .save_ui
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.ticket.id == id)
                    {
                        self.save_ui.phase = Some((path, index, total, waiting));
                    }
                }
                Event::Finished {
                    id,
                    committed,
                    error,
                    cancelled,
                } => {
                    if self
                        .save_ui
                        .pending
                        .as_ref()
                        .is_none_or(|p| p.ticket.id != id)
                    {
                        self.save_ui.error = Some("Unexpected completed Save · review disk".into());
                        continue;
                    }
                    let pending = self.save_ui.pending.take().unwrap();
                    let mut error = error;
                    let count = committed.len();
                    for receipt in &committed {
                        let p = receipt.path();
                        let doc = if self.doc.path.as_deref() == Some(p) {
                            Some(&mut self.doc)
                        } else {
                            self.parked.get_mut(p).map(|(d, _)| d)
                        };
                        match doc.map(|d| d.accept_saved(receipt)) {
                            Some(Ok(())) => {}
                            Some(Err(e)) => error = Some(format!("Save receipt: {e:#}")),
                            None => error = Some("Saved tab changed · review disk".into()),
                        }
                    }
                    self.save_ui.phase = None;
                    let cancelled = cancelled || pending.stop_requested;
                    self.save_ui.last_succeeded = error.is_none() && !cancelled;
                    // Own writes may already have been seen by the watcher. Re-subscribe
                    // even on failure so a suppressed remote event is never lost forever.
                    self.editor_ui.signature.clear();
                    self.sync_watches();
                    if count > 0 {
                        // A receipt changes the disk baseline, not the source snapshot.
                        // Re-publishing unchanged bytes advances the UI clock and can
                        // invalidate an already queued F12 before K2 dispatches it.
                        self.saved_language();
                    }
                    let dirty = pending.paths.iter().any(|p| self.dirty_path(p));
                    self.save_ui.retry = pending.paths.clone();
                    self.save_ui.error = error;
                    self.status = if let Some(e) = &self.save_ui.error {
                        format!("Saved {count}/{} snapshots · {e}", pending.paths.len())
                    } else if cancelled {
                        format!(
                            "Saved {count}/{} · remaining saves stopped",
                            pending.paths.len()
                        )
                    } else if dirty {
                        "Snapshot saved · newer edits remain unsaved".into()
                    } else {
                        format!("Saved {count} snapshot(s)")
                    };
                    if dirty || cancelled {
                        self.save_ui.notice = Some(self.status.clone());
                    } else if self.save_ui.error.is_none() {
                        self.save_ui.notice = None;
                    }
                    if matches!(pending.after, After::Files(_)) {
                        self.file_ui.busy = false;
                        if !self.save_ui.last_succeeded {
                            self.file_ui.error = Some(self.status.clone());
                        }
                    }
                    if matches!(&pending.after, After::Git(_)) {
                        self.git_save_finished(self.save_ui.last_succeeded);
                    }
                    if let After::WindowClose(id) = pending.after {
                        self.window_save_finished(
                            id,
                            self.save_ui.last_succeeded && !dirty && !cancelled,
                        );
                    }
                    let context =
                        !matches!(pending.after, After::Build | After::Run | After::Debug)
                            || (self.app_module == pending.module
                                && self.variant == pending.variant
                                && (!matches!(pending.after, After::Run | After::Debug)
                                    || self.selected == pending.serial));
                    if self.save_ui.last_succeeded && context {
                        match pending.after {
                            After::None | After::Git(_) | After::WindowClose(_) => {}
                            After::Close(path) => {
                                if self.pending_close.as_ref() == Some(&path)
                                    && !self.dirty_path(&path)
                                {
                                    self.close_tab(path, true);
                                }
                            }
                            After::Files(signature) => {
                                if !dirty {
                                    self.file_review_after_save(&signature);
                                } else {
                                    self.file_ui.error = Some(self.status.clone());
                                }
                            }
                            After::Build | After::Run | After::Debug => {
                                if self.tabs.iter().any(|p| self.dirty_path(p))
                                    || !self.save_ui.again.is_empty()
                                {
                                    self.status="New edits arrived during Save · action not started; press Build / Run / Debug again".into();
                                    self.save_ui.notice = Some(self.status.clone());
                                } else {
                                    match pending.after {
                                        After::Build => self.build(),
                                        After::Run => self.run(),
                                        After::Debug => self.start_debug(),
                                        _ => unreachable!(),
                                    }
                                }
                            }
                        }
                    } else if self.save_ui.last_succeeded && !context {
                        self.status = "Context changed during Save · action not started".into();
                        self.save_ui.notice = Some(self.status.clone());
                    }
                    let again = std::mem::take(&mut self.save_ui.again)
                        .into_iter()
                        .filter(|p| self.tabs.contains(p))
                        .collect::<Vec<_>>();
                    if !again.is_empty() {
                        self.request_save_paths(again, After::None);
                    }
                }
            }
        }
    }
    pub(super) fn stop_remaining_saves(&mut self) {
        if let Some(p) = &mut self.save_ui.pending {
            p.stop_requested = true;
        }
        self.save_ui.again.clear();
        self.save_ui.saver.cancel_remaining();
    }
    pub(super) fn document_save_bar(&mut self, ui: &mut egui::Ui) {
        if self.save_ui.pending.is_none()
            && self.save_ui.error.is_none()
            && self.save_ui.notice.is_none()
        {
            return;
        }
        let mut stop = false;
        let mut retry = false;
        let mut dismiss = false;
        m::card().fill(m::CONTAINER).inner_margin(8).show(ui,|ui|{
            ui.horizontal(|ui|{
                if self.save_ui.pending.is_some(){
                    if self.reduced_motion{ui.label(RichText::new("Saving").color(m::PRIMARY));}else{ui.add(egui::Spinner::new().color(m::PRIMARY));}
                    let text=self.save_ui.phase.as_ref().map(|(p,i,n,waiting)|format!("{} {}/{} · {}",if *waiting{"Waiting for other save"}else{"Saving"},i+1,n,p.file_name().unwrap_or_default().to_string_lossy())).unwrap_or_else(||"Preparing Save · current tab remains editable".into());
                    ui.add_sized([(ui.available_width()-160.).max(60.),24.],egui::Label::new(text).truncate()).on_hover_text_passive("New edits stay unsaved. Stop remaining does not undo already completed writes.");
                    if self.chrome.button(ui,"save-stop","Stop remaining",Icon::Close,Kind::Outline,true,150.,self.reduced_motion).clicked(){stop=true;}
                }else{
                    let text=self.save_ui.error.clone().or_else(||self.save_ui.notice.clone()).unwrap_or_default();
                    ui.add_sized([(ui.available_width()-164.).max(60.),24.],egui::Label::new(RichText::new(&text).color(if self.save_ui.error.is_some(){m::ERROR}else{m::MUTED})).truncate()).on_hover_text_passive(text);
                    if self.chrome.button(ui,"save-retry","Save latest",Icon::Save,Kind::Tonal,!self.save_ui.retry.is_empty(),114.,self.reduced_motion).clicked(){retry=true;}
                    if self.chrome.button(ui,"save-dismiss","",Icon::Close,Kind::Ghost,true,40.,self.reduced_motion).clicked(){dismiss=true;}
                }
            });
        });
        ui.add_space(4.);
        if stop {
            self.stop_remaining_saves();
        }
        if dismiss {
            self.save_ui.error = None;
            self.save_ui.notice = None;
        }
        if retry {
            let paths = self.save_ui.retry.clone();
            self.request_save_paths(paths, After::None);
        }
    }
    #[cfg(test)]
    pub(super) fn wait_for_save(&mut self) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(8);
        while self.save_ui.pending.is_some() {
            self.document_save_updates();
            assert!(
                std::time::Instant::now() < end,
                "Save worker did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        self.save_ui.last_succeeded
    }
    #[cfg(test)]
    pub(super) fn save_path(&mut self, path: &std::path::Path) -> bool {
        self.request_save_paths(vec![path.to_owned()], After::None);
        self.wait_for_save()
    }
    #[cfg(test)]
    pub(super) fn save_all_documents(&mut self) -> bool {
        if self.save_before(After::None) {
            true
        } else if self.save_ui.pending.is_some() {
            self.wait_for_save()
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{File, OpenOptions},
        os::fd::AsRawFd,
        time::Instant,
    };
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "val x=1\n").unwrap();
        let a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        (d, a)
    }
    fn hold_lock(a: &Workbench) -> File {
        let p = wexandroidstudio::document_saves::lock_path(&a.root, a.doc.path.as_ref().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(p)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        f
    }
    fn waiting(a: &mut Workbench) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            a.document_save_updates();
            if a.save_ui.phase.as_ref().is_some_and(|p| p.3) {
                return;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn real_lock_pending_save_keeps_newer_edits_undo_and_selection() {
        let (_d, mut a) = app();
        let lock = hold_lock(&a);
        a.apply_edit(6, 7, "2");
        a.save_document();
        waiting(&mut a);
        assert!(a.save_ui.pending.is_some());
        a.apply_edit(6, 7, "3");
        let clocks = (a.doc.version, a.doc.revision);
        let view = (a.edit.anchor, a.edit.cursor);
        drop(lock);
        assert!(a.wait_for_save());
        assert_eq!(a.doc.rope(), "val x=3\n");
        assert!(a.doc.dirty());
        assert_eq!((a.doc.version, a.doc.revision), clocks);
        assert_eq!((a.edit.anchor, a.edit.cursor), view);
        assert_eq!(
            std::fs::read_to_string(a.doc.path.as_ref().unwrap()).unwrap(),
            "val x=2\n"
        );
        a.doc.undo().unwrap();
        assert!(!a.doc.dirty());
        a.doc.redo().unwrap();
        assert!(a.doc.dirty());
    }
    #[test]
    fn queued_completed_receipt_is_merged_but_ui_stop_cancels_deferred_build() {
        let (_d, mut a) = app();
        a.apply_edit(6, 7, "2");
        assert!(!a.save_before(After::Build));
        let done = a.save_ui.pending.as_ref().unwrap().ticket.done.clone();
        let end = Instant::now() + Duration::from_secs(5);
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        a.stop_remaining_saves();
        assert!(!a.wait_for_save());
        assert!(!a.doc.dirty());
        assert!(!a.building && !a.running);
        assert!(a.last_build.is_none());
    }
    #[test]
    fn cancel_close_while_saving_prevents_late_close() {
        let (_d, mut a) = app();
        let lock = hold_lock(&a);
        let p = a.doc.path.clone().unwrap();
        a.apply_edit(6, 7, "2");
        a.pending_close = Some(p.clone());
        a.request_save_paths(vec![p.clone()], After::Close(p.clone()));
        waiting(&mut a);
        a.pending_close = None;
        drop(lock);
        assert!(a.wait_for_save());
        assert_eq!(a.doc.path, Some(p));
        assert_eq!(a.tabs.len(), 1);
        assert!(!a.doc.dirty());
    }
    #[test]
    fn new_edits_or_changed_serial_never_start_stale_build_or_run() {
        let (_d, mut a) = app();
        let lock = hold_lock(&a);
        a.apply_edit(6, 7, "2");
        a.save_before(After::Build);
        waiting(&mut a);
        a.apply_edit(6, 7, "3");
        drop(lock);
        assert!(a.wait_for_save());
        assert!(a.doc.dirty());
        assert!(!a.building && !a.running);
        assert!(a.status.contains("New edits"));
        let lock = hold_lock(&a);
        a.selected = Some("emulator-5580".into());
        a.save_before(After::Run);
        waiting(&mut a);
        a.selected = Some("not-the-requested-transport".into());
        drop(lock);
        assert!(a.wait_for_save());
        assert!(!a.running && !a.building);
        assert!(a.status.contains("Context changed"));
        assert_eq!(a.selected.as_deref(), Some("not-the-requested-transport"));
    }
    #[test]
    fn repeated_ctrl_save_coalesces_latest_snapshot_without_losing_edits() {
        let (_d, mut a) = app();
        let lock = hold_lock(&a);
        a.apply_edit(6, 7, "2");
        a.save_document();
        waiting(&mut a);
        a.apply_edit(6, 7, "3");
        for _ in 0..5 {
            a.save_document();
        }
        assert_eq!(a.save_ui.again.len(), 1);
        drop(lock);
        assert!(a.wait_for_save());
        assert!(!a.doc.dirty());
        assert_eq!(
            std::fs::read_to_string(a.doc.path.as_ref().unwrap()).unwrap(),
            "val x=3\n"
        );
        a.doc.undo().unwrap();
        assert_eq!(a.doc.rope(), "val x=2\n");
        assert!(a.doc.dirty());
    }
    #[test]
    fn save_failure_resubscribes_suppressed_actual_external_change() {
        let (_d, mut a) = app();
        a.sync_watches();
        let old = a.editor_ui.generation;
        let lock = hold_lock(&a);
        a.apply_edit(6, 7, "2");
        a.save_document();
        waiting(&mut a);
        let path = a.doc.path.clone().unwrap();
        std::fs::write(&path, "val remote=7\n").unwrap();
        let until = Instant::now() + Duration::from_millis(900);
        while Instant::now() < until {
            a.editor_updates(&a.context.clone());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!a.editor_ui.conflicts.contains_key(&path));
        drop(lock);
        assert!(!a.wait_for_save());
        assert!(a.editor_ui.generation > old);
        let end = Instant::now() + Duration::from_secs(5);
        while !a.editor_ui.conflicts.contains_key(&path) {
            a.editor_updates(&a.context.clone());
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.doc.rope(), "val x=2\n");
        assert!(a.doc.dirty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "val remote=7\n");
    }
    #[test]
    fn disk_build_run_ignore_only_retained_clean_missing_tabs_and_never_recreate_them() {
        let (d, mut a) = app();
        let path = d.path().join("Removed.kt");
        std::fs::write(&path, "class Removed\n").unwrap();
        let doc = Document::open(path.clone()).unwrap();
        a.parked.insert(path.clone(), (doc, Default::default()));
        a.tabs.push(path.clone());
        std::fs::remove_file(&path).unwrap();
        a.editor_ui.conflicts.insert(
            path.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        assert!(!a.save_before(After::None));
        assert!(!a.save_before(After::Debug));
        assert!(
            a.save_before(After::Build),
            "a clean deleted inspection tab must not block compiling the selected disk tree"
        );
        assert!(a.save_before(After::Run));
        assert!(!a.save_ui.saver.worker_started());
        assert!(!path.exists());
        assert_eq!(a.parked[&path].0.rope().to_string(), "class Removed\n");
        a.parked
            .get_mut(&path)
            .unwrap()
            .0
            .edit(0..0, "// intentional draft\n")
            .unwrap();
        assert!(!a.save_before(After::Build));
        assert!(!a.save_before(After::Run));
        assert!(!path.exists());
    }
    #[test]
    fn disk_build_run_still_block_unknown_missing_and_io_errors() {
        let (d, mut a) = app();
        let path = d.path().join("unknown.kt");
        a.editor_ui.conflicts.insert(
            path.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        assert!(!a.save_before(After::Build));
        assert!(!a.save_before(After::Run));
        a.editor_ui.conflicts.remove(&path);
        let active = a.doc.path.clone().unwrap();
        a.editor_ui.conflicts.insert(
            active,
            wexandroidstudio::editor_jobs::DiskState::Error("io failure".into()),
        );
        assert!(!a.save_before(After::Build));
        assert!(!a.save_before(After::Run));
        assert!(!a.save_ui.saver.worker_started());

        // A changed on-disk file is not the clean-deleted inspection exception.
        let active = a.doc.path.clone().unwrap();
        std::fs::write(&active, "val remote=8\n").unwrap();
        a.editor_ui.conflicts.insert(
            active.clone(),
            wexandroidstudio::editor_jobs::DiskState::Present(Box::new(
                Document::open(active.clone()).unwrap(),
            )),
        );
        assert!(!a.save_before(After::Build));
        assert!(!a.save_before(After::Run));
        assert_eq!(std::fs::read_to_string(&active).unwrap(), "val remote=8\n");
        // The same rule covers the currently active deleted clean tab, not only
        // parked ones. Explicit Save and Debug still require their own review.
        std::fs::remove_file(&active).unwrap();
        a.editor_ui.conflicts.insert(
            active.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        assert!(a.save_before(After::Build));
        assert!(a.save_before(After::Run));
        assert!(!a.save_before(After::None));
        assert!(!a.save_before(After::Debug));
        assert!(!active.exists());
        assert!(!a.save_ui.saver.worker_started());
    }
}
