//! Background file loading with explicit navigation intents and last-good active/parked buffers.
use super::*;
use std::sync::Arc;
use wexandroidstudio::{
    document_jobs::{DocumentOpener, Ticket},
    material::{self as m, ButtonKind as Kind, Icon},
};
#[derive(Clone)]
pub(super) enum Intent {
    File,
    ProjectSearch {
        id: u64,
        hash: blake3::Hash,
        range: std::ops::Range<usize>,
    },
    Definition(kotlin::Position),
    Usage(kotlin::Range, Option<blake3::Hash>),
    Debug {
        session: u64,
        epoch: u64,
        frame: u64,
        line: u32,
    },
}
pub(super) struct Pending {
    pub path: PathBuf,
    pub ticket: Ticket,
    intent: Intent,
}
#[derive(Default)]
pub(super) struct OpenUi {
    pub navigation: u64,
    pub opener: DocumentOpener,
    pub pending: Option<Pending>,
    pub error: Option<(PathBuf, String)>,
    retry: Option<Intent>,
}
impl Workbench {
    pub(super) fn open_file(&mut self, path: PathBuf) -> bool {
        self.open_at(path, Intent::File)
    }
    pub(super) fn open_at(&mut self, path: PathBuf, intent: Intent) -> bool {
        if !self.intent_current(&intent) {
            return false;
        }
        self.open_ui.navigation = self.open_ui.navigation.wrapping_add(1);
        let path = if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        };
        if let Intent::ProjectSearch { id, hash, .. } = &intent
            && let Some(d) = if self.doc.path.as_ref() == Some(&path) {
                Some(&self.doc)
            } else {
                self.parked.get(&path).map(|(d, _)| d)
            }
            && !self.search_candidate_current(*id, &path, *hash, d)
        {
            self.search_navigation_rejected();
            return false;
        }
        if self.doc.path.as_ref() == Some(&path) {
            self.cancel_document_open();
            self.apply_open_intent(intent);
            return true;
        }
        if self.parked.contains_key(&path) {
            self.cancel_document_open();
            let (d, e) = self.parked.remove(&path).unwrap();
            self.activate_document(d, e);
            self.apply_open_intent(intent);
            return true;
        }
        if let Some(p) = &mut self.open_ui.pending
            && p.path == path
        {
            p.intent = intent;
            return true;
        }
        let ctx = self.context.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        match self.open_ui.opener.request(path.clone(), wake) {
            Ok(ticket) => {
                self.open_ui.pending = Some(Pending {
                    path,
                    ticket,
                    intent,
                });
                self.open_ui.error = None;
                self.open_ui.retry = None;
                self.context.request_repaint();
                true
            }
            Err(e) => {
                self.status = format!("Open file: {e:#}");
                false
            }
        }
    }
    pub(super) fn cancel_document_open(&mut self) {
        self.open_ui.navigation = self.open_ui.navigation.wrapping_add(1);
        self.open_ui.opener.cancel();
        self.open_ui.pending = None;
        self.open_ui.error = None;
        self.open_ui.retry = None;
    }
    pub(super) fn cancel_project_search_open(&mut self) {
        if self
            .open_ui
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.intent, Intent::ProjectSearch { .. }))
        {
            self.cancel_document_open();
        }
    }
    fn intent_current(&self, intent: &Intent) -> bool {
        match intent {
            Intent::ProjectSearch { id, .. } => self.search_intent_current(*id),
            Intent::Debug {
                session,
                epoch,
                frame,
                ..
            } => {
                *session == self.debug.session
                    && *epoch == self.debug.epoch
                    && self.debug.paused
                    && self.debug.selected_frame == Some(*frame)
            }
            _ => true,
        }
    }
    fn activate_document(&mut self, next: Document, edit: EditState) {
        let Some(path) = next.path.clone() else {
            return;
        };
        self.edit.cancel_composition();
        let old = std::mem::replace(&mut self.doc, next);
        if let Some(p) = old.path.clone() {
            self.parked.insert(p, (old, self.edit.clone()));
        }
        self.edit = edit;
        self.editor_switched();
        self.chrome.reveal_tab = true;
        if !self.tabs.contains(&path) {
            self.tabs.push(path);
        }
        self.last_sent = None;
        self.completions.clear();
        self.diagnostics.clear();
        self.diagnostic_snapshot = None;
        self.scroll_to_cursor = true;
        self.sync_watches();
        self.send_snapshot();
    }
    pub(super) fn definition_is_current(
        &self,
        navigation: u64,
        path: &std::path::Path,
        version: i32,
    ) -> bool {
        !self.editor_blocked()
            && navigation == self.open_ui.navigation
            && self.doc.path.as_deref() == Some(path)
            && self.doc.version == version
            && self.semantic_source_eligible(path, &self.doc)
    }
    pub(super) fn cancel_semantic_open(&mut self) {
        self.open_ui.navigation = self.open_ui.navigation.wrapping_add(1);
        if self
            .open_ui
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.intent, Intent::Definition(_) | Intent::Usage(..)))
        {
            self.cancel_document_open();
        }
    }
    pub(super) fn document_open_updates(&mut self) {
        for _ in 0..8 {
            let Some(e) = self.open_ui.opener.poll() else {
                break;
            };
            let Some(p) = self.open_ui.pending.as_ref() else {
                continue;
            };
            if p.ticket.id != e.id || p.path != e.requested {
                continue;
            }
            let pending = self.open_ui.pending.take().unwrap();
            if !self.intent_current(&pending.intent) {
                self.status = "Source navigation superseded · active buffer retained".into();
                continue;
            }
            match e.result {
                Err(error) => {
                    self.status = format!("Open file: {error}");
                    self.open_ui.error = Some((pending.path, error));
                    self.open_ui.retry = Some(pending.intent);
                }
                Ok(doc) => {
                    if let Intent::ProjectSearch { id, hash, .. } = &pending.intent {
                        if doc.path.as_ref() != Some(&pending.path) {
                            self.search_navigation_rejected();
                            continue;
                        }
                        let path = doc.path.as_ref().unwrap();
                        let candidate = self.parked.get(path).map_or(doc.as_ref(), |(d, _)| d);
                        if !self.search_candidate_current(*id, path, *hash, candidate) {
                            self.search_navigation_rejected();
                            continue;
                        }
                    }
                    let path = doc.path.clone().unwrap();
                    // A symlink/alias or another operation may have opened this URI meanwhile.
                    // Existing buffers, especially dirty ones, always win over the disk candidate.
                    if self.doc.path.as_ref() != Some(&path) {
                        let (next, edit) = self
                            .parked
                            .remove(&path)
                            .unwrap_or((*doc, Default::default()));
                        self.activate_document(next, edit);
                    }
                    self.status = format!(
                        "Opened {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    if let Intent::ProjectSearch { id, .. } = &pending.intent {
                        self.search_accept_opened_guard(*id);
                    }
                    self.apply_open_intent(pending.intent);
                }
            }
        }
    }
    fn apply_open_intent(&mut self, intent: Intent) {
        match intent {
            Intent::File => {}
            Intent::ProjectSearch { id, hash, range } => {
                self.finish_search_navigation(id, hash, range)
            }
            Intent::Definition(pos) => {
                self.edit
                    .collapse(self.doc.char_at_lsp(pos.line, pos.character).unwrap_or(0));
                self.scroll_to_cursor = true;
                self.status = format!(
                    "Definition · {}",
                    self.doc
                        .path
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                );
            }
            Intent::Usage(range, expected) => {
                if expected != Some(self.doc.content_hash()) {
                    self.status = "Usage location is stale; find usages again".into();
                    return;
                }
                if let (Ok(a), Ok(b)) = (
                    self.doc
                        .char_at_lsp(range.start.line, range.start.character),
                    self.doc.char_at_lsp(range.end.line, range.end.character),
                ) {
                    self.edit.select(a, b);
                    self.scroll_to_cursor = true;
                }
            }
            Intent::Debug {
                session,
                epoch,
                frame,
                line,
            } => {
                if !self.intent_current(&Intent::Debug {
                    session,
                    epoch,
                    frame,
                    line,
                }) {
                    return;
                }
                let path = self.doc.path.clone().unwrap();
                if self
                    .debug
                    .built_sources
                    .get(&path)
                    .is_some_and(|h| self.doc.content_hash().to_hex().as_str() == h)
                {
                    let row = (line as usize)
                        .saturating_sub(1)
                        .min(self.doc.rope().len_lines() - 1);
                    self.edit.collapse(self.doc.rope().line_to_char(row));
                    self.scroll_to_cursor = true;
                    self.debug.execution = Some((path, line));
                } else {
                    self.debug.source_changed = true;
                }
            }
        }
    }
    pub(super) fn document_open_bar(&mut self, ui: &mut egui::Ui) {
        let pending = self.open_ui.pending.as_ref().map(|p| p.path.clone());
        let error = self.open_ui.error.clone();
        if pending.is_none() && error.is_none() {
            return;
        }
        let mut cancel = false;
        let mut retry = false;
        m::card().fill(m::CONTAINER).inner_margin(8).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(path) = &pending {
                    if self.reduced_motion {
                        ui.label(RichText::new("Opening").color(m::PRIMARY));
                    } else {
                        ui.add(egui::Spinner::new().color(m::PRIMARY));
                    }
                    ui.add_sized(
                        [(ui.available_width() - 130.).max(80.), 24.],
                        egui::Label::new(format!(
                            "Opening {} · current tab stays editable",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ))
                        .truncate(),
                    )
                    .on_hover_text_passive(path.to_string_lossy());
                }
                if let Some((path, e)) = &error {
                    ui.add_sized(
                        [(ui.available_width() - 230.).max(80.), 24.],
                        egui::Label::new(
                            RichText::new(format!(
                                "{}: {e}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ))
                            .color(m::ERROR),
                        )
                        .truncate(),
                    )
                    .on_hover_text_passive(e);
                    if self
                        .chrome
                        .button(
                            ui,
                            "open-retry",
                            "Retry",
                            Icon::Refresh,
                            Kind::Tonal,
                            true,
                            100.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        retry = true;
                    }
                }
                if self
                    .chrome
                    .button(
                        ui,
                        "open-cancel",
                        "Cancel",
                        Icon::Close,
                        Kind::Ghost,
                        true,
                        100.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.cancel_document_open();
            self.status = "File opening cancelled · current tab retained".into();
        } else if retry && let Some((path, _)) = error {
            let intent = self.open_ui.retry.clone().unwrap_or(Intent::File);
            self.open_at(path, intent);
        }
    }
    #[cfg(test)]
    pub(super) fn open_file_wait(&mut self, path: PathBuf) -> bool {
        if !self.open_file(path) {
            return false;
        }
        let end = std::time::Instant::now() + Duration::from_secs(8);
        while self.open_ui.pending.is_some() {
            assert!(std::time::Instant::now() < end);
            self.document_open_updates();
            std::thread::sleep(Duration::from_millis(1));
        }
        self.open_ui.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    fn app() -> (tempfile::TempDir, Workbench, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A.kt");
        let z = d.path().join("Z.kt");
        std::fs::write(&a, "val original=1\n").unwrap();
        std::fs::write(&z, "val prefix = \"😀\"; val sought=42\n").unwrap();
        let app = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        (d, app, a, z)
    }
    #[test]
    fn active_edits_survive_background_commit_and_cached_switch_undo() {
        let (_d, mut a, old, next) = app();
        assert!(a.open_file(next.clone()));
        assert_eq!(a.doc.path.as_ref(), Some(&old));
        a.doc.edit(0..0, "// edit during open\n").unwrap();
        assert!(a.open_file_wait(next));
        assert!(a.parked[&old].0.dirty());
        a.open_file(old);
        assert!(a.doc.rope().to_string().starts_with("// edit"));
        assert!(a.doc.undo().unwrap());
        assert_eq!(a.doc.rope().to_string(), "val original=1\n");
    }
    #[test]
    fn queued_result_dropped_by_cancel_keeps_active_and_tabs() {
        let (_d, mut a, old, next) = app();
        a.open_file(next);
        let t = a.open_ui.pending.as_ref().unwrap().ticket.clone();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !t.done.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        a.cancel_document_open();
        a.document_open_updates();
        assert_eq!(a.doc.path, Some(old));
        assert_eq!(a.tabs.len(), 1);
    }
    #[test]
    fn alias_never_replaces_dirty_cached_buffer() {
        use std::os::unix::fs::symlink;
        let (d, mut a, old, _) = app();
        a.doc.edit(0..0, "// dirty\n").unwrap();
        let alias = d.path().join("Alias.kt");
        symlink(&old, &alias).unwrap();
        assert!(a.open_file_wait(alias));
        assert!(a.doc.dirty());
        assert!(a.doc.rope().to_string().starts_with("// dirty"));
        assert_eq!(a.tabs.len(), 1);
    }
    #[test]
    fn deferred_definition_uses_target_utf16_position_not_old_document() {
        let (_d, mut a, _, next) = app();
        let prefix = "val prefix = \"😀\"; val ";
        a.open_at(
            next.clone(),
            Intent::Definition(kotlin::Position {
                line: 0,
                character: prefix.encode_utf16().count() as u32,
            }),
        );
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            assert!(std::time::Instant::now() < end);
            a.document_open_updates();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(a.doc.path, Some(next));
        assert_eq!(a.edit.cursor, prefix.chars().count());
        assert!(a.status.starts_with("Definition"));
    }
    #[test]
    fn later_open_or_cancel_fences_definition_while_origin_still_active() {
        let (_d, mut a, old, next) = app();
        let nav = a.open_ui.navigation;
        let version = a.doc.version;
        assert!(a.definition_is_current(nav, &old, version));
        a.open_file(next);
        assert_eq!(a.doc.path.as_ref(), Some(&old));
        assert!(!a.definition_is_current(nav, &old, version));
        let nav = a.open_ui.navigation;
        a.cancel_document_open();
        assert!(!a.definition_is_current(nav, &old, version));
        let nav = a.open_ui.navigation;
        a.cancel_semantic_open();
        assert!(!a.definition_is_current(nav, &old, version));
    }
    #[test]
    fn stale_debug_epoch_cannot_switch_source() {
        let (_d, mut a, old, next) = app();
        a.debug.session = 1;
        a.debug.epoch = 2;
        a.debug.paused = true;
        a.debug.selected_frame = Some(3);
        a.open_at(
            next,
            Intent::Debug {
                session: 1,
                epoch: 2,
                frame: 3,
                line: 1,
            },
        );
        a.debug.epoch = 3;
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            assert!(std::time::Instant::now() < end);
            a.document_open_updates();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(a.doc.path, Some(old));
        assert!(a.debug.execution.is_none());
    }
}
