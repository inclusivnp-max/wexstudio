//! Window close is a live transaction, not a destructive on_exit callback.
use super::*;
use wexandroidstudio::{
    breakpoint_store::{Action, CloseOutcome, CloseStatus},
    material::{self as m, ButtonKind as Kind, Icon},
    text_tracking::TextId,
};
#[derive(Clone, PartialEq, Eq)]
struct Guard {
    active: TextId,
    documents: Vec<(PathBuf, TextId, Option<TextId>, Option<blake3::Hash>)>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Choices,
    Saving,
    Waiting,
    Preparing,
    Backup,
    Failed,
    Committing,
}
#[derive(Default)]
pub(super) struct Ui {
    pub requested: bool,
    pub open: bool,
    pub allow: bool,
    clock: u64,
    session: u64,
    phase: Option<Phase>,
    plan: Option<bool>,
    nonce: Option<u64>,
    guard: Option<Guard>,
    outcome: Option<CloseOutcome>,
    paths: Vec<PathBuf>,
    untitled: bool,
    error: Option<String>,
    confirm_discard: bool,
    pub audit: Option<String>,
    focus_cancel: bool,
    painted: Option<(u64, Option<Phase>, bool)>,
}
impl Ui {
    pub(super) fn authorizes_save(&self, id: u64) -> bool {
        self.open && self.session == id && self.phase == Some(Phase::Saving)
    }
    pub(super) fn phase(&self) -> Option<Phase> {
        self.phase
    }
    pub(super) fn source_count(&self) -> usize {
        self.paths.len() + usize::from(self.untitled)
    }
    pub(super) fn discard_confirmation(&self) -> bool {
        self.confirm_discard
    }
    pub(super) fn choices_painted(&self) -> bool {
        self.open
            && self.phase == Some(Phase::Choices)
            && self.painted == Some((self.session, Some(Phase::Choices), false))
    }
    pub(super) fn received(&mut self, report: &wexandroidstudio::breakpoint_store::Report) {
        if self.open
            && let Some(outcome) = &report.close_outcome
            && self.nonce == Some(outcome.id)
        {
            self.outcome = Some(outcome.clone());
        }
    }
    fn next(&mut self) -> Option<u64> {
        let n = self.clock.checked_add(1)?;
        self.clock = n;
        Some(n)
    }
}
impl Workbench {
    fn close_choices_heading(&self) -> String {
        if self.close_ui.source_count() > 0 {
            format!("Unsaved buffers: {}", self.close_ui.source_count())
        } else if self.debug.connection.is_some() || self.debug.busy {
            "Stop Debug before closing?".into()
        } else {
            "Sources changed · confirm closing again".into()
        }
    }
    fn close_primary_label(&self) -> &'static str {
        if self.close_ui.source_count() > 0 {
            "Save all and close"
        } else if self.debug.connection.is_some() || self.debug.busy {
            "Stop Debug and close"
        } else {
            "Close window"
        }
    }
    pub(super) fn closing_requested(&self) -> bool {
        self.close_ui.requested || self.close_ui.open || self.close_ui.allow
    }
    fn close_work_pending(&self) -> bool {
        self.building
            || self.running
            || self.debug.busy
            || self.debug.stop_error.is_some()
            || self.preview.busy
            || self.file_ui.busy
            || self.git.busy
            || self.git_transaction_active()
            || self.save_ui.pending.is_some()
    }
    fn close_guard(&self) -> Guard {
        Guard {
            active: self.doc.text_id().clone(),
            documents: self.breakpoint_signature(),
        }
    }
    pub(super) fn close_other_modal(&self) -> bool {
        self.file_ui.open
            || self.projects.open
            || self.chrome.settings
            || self.variant_open
            || self.chrome.device_open
            || self.pending_close.is_some()
            || self.editor_ui.pending_reload.is_some()
            || self.refactor.dialog
            || self.preplace.dialog
            || self.preplace.busy
            || self.format.open
            || self.debug.bp_review.open
            || self.git.open
    }
    pub(super) fn request_window_close(&mut self) {
        if self.close_ui.allow || self.close_ui.requested || self.close_ui.open {
            return;
        }
        self.close_ui.requested = true;
        self.cancel_deferred_for_window_close();
        self.debug.bp_launch = None;
        self.context.request_repaint();
    }
    pub(super) fn close_raw_input(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) -> bool {
        let event = input
            .viewports
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| v.close_requested());
        if event && !self.close_ui.allow {
            self.request_window_close();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.legacy_close_input(ctx, input, event)
    }
    pub(super) fn close_fast_exit(&mut self, ctx: &egui::Context) -> bool {
        if self.close_ui.allow {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return true;
        }
        if self.close_ui.requested
            && (self.welcome || self.boot.pending)
            && self.save_ui.pending.is_none()
        {
            self.close_ui.allow = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return true;
        }
        false
    }
    fn close_refresh_paths(&mut self) {
        self.close_ui.paths = self
            .tabs
            .iter()
            .filter(|p| self.dirty_path(p))
            .cloned()
            .collect();
        self.close_ui.untitled = self.doc.path.is_none() && self.doc.rope().len_chars() > 0;
    }
    pub(super) fn cancel_window_close(&mut self) {
        if let Some(id) = self.close_ui.nonce {
            self.breakpoint_request(Some(Action::CancelClose(id)));
        }
        self.cancel_window_save_after(self.close_ui.session);
        self.close_ui.requested = false;
        self.close_ui.open = false;
        self.close_ui.plan = None;
        self.close_ui.nonce = None;
        self.close_ui.guard = None;
        self.close_ui.outcome = None;
        self.close_ui.phase = None;
        self.close_ui.confirm_discard = false;
        self.editor_ui.focus_editor = true;
        self.status = "Window stays open · accepted saves are not rolled back".into();
    }
    pub(super) fn window_save_finished(&mut self, id: u64, ok: bool) {
        if !self.close_ui.authorizes_save(id) {
            return;
        }
        self.close_refresh_paths();
        if ok && self.close_ui.paths.is_empty() && !self.close_ui.untitled {
            self.close_ui.phase = Some(Phase::Waiting);
        } else {
            self.close_ui.plan = None;
            self.close_ui.phase = Some(Phase::Choices);
            self.close_ui.error = Some(
                self.save_ui
                    .error
                    .clone()
                    .unwrap_or_else(|| "Some text is still unsaved; review before closing".into()),
            );
        }
    }
    fn close_choose(&mut self, save: bool) {
        self.close_refresh_paths();
        self.close_ui.error = None;
        self.close_ui.plan = Some(save);
        if save {
            if self.close_ui.untitled {
                self.close_ui.error = Some("Save the untitled buffer to a named file first".into());
                self.close_ui.plan = None;
                return;
            }
            let paths = self.close_ui.paths.clone();
            self.close_ui.phase = Some(Phase::Saving);
            let id = self.close_ui.session;
            if self.request_save_paths(paths, save_ui::After::WindowClose(id)) {
                self.window_save_finished(id, true);
            } else if self.save_ui.pending.is_none() {
                self.window_save_finished(id, false);
            }
        } else {
            self.close_ui.phase = Some(Phase::Waiting);
        }
    }
    fn close_probe(&mut self) {
        if !self.debug.bp_service.active() && !self.debug.bp_service.pending() {
            self.close_allow(false, None);
            return;
        }
        let Some(id) = self.close_ui.next() else {
            self.close_ui.error = Some("Closing request counter exhausted; window retained".into());
            self.close_ui.phase = Some(Phase::Failed);
            return;
        };
        self.close_ui.nonce = Some(id);
        self.close_ui.guard = Some(self.close_guard());
        self.close_ui.outcome = None;
        self.close_ui.phase = Some(Phase::Preparing);
        if !self.breakpoint_request(Some(Action::PrepareClose(id))) {
            self.close_ui.phase = Some(Phase::Failed);
            self.close_ui.error = Some(self.status.clone());
        }
    }
    fn close_commit(&mut self, discard: bool) {
        let Some(id) = self.close_ui.nonce else {
            return;
        };
        self.close_ui.outcome = None;
        self.close_ui.confirm_discard = false;
        self.close_ui.phase = Some(Phase::Committing);
        if !self.breakpoint_request(Some(Action::CommitClose(id, discard))) {
            self.close_ui.phase = Some(Phase::Failed);
            self.close_ui.error = Some(self.status.clone());
        }
    }
    fn close_allow(&mut self, discarded: bool, backup: Option<PathBuf>) {
        self.close_ui.audit = Some(format!(
            "WINDOW_CLOSE_COMMITTED source_save={} discarded_markers={discarded} backup={}",
            self.close_ui.plan.unwrap_or(true),
            backup
                .as_ref()
                .map_or("none".into(), |p| p.display().to_string())
        ));
        self.close_ui.allow = true;
        self.close_ui.open = false;
        self.close_ui.requested = false;
        self.context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    pub(super) fn window_close_step(&mut self) {
        if self.close_ui.allow {
            return;
        }
        if self.close_ui.requested && !self.close_ui.open {
            // Do not hide another review or prevent its approved write from finishing.
            if self.close_other_modal()
                || self.save_ui.pending.is_some()
                || self.git_transaction_active()
            {
                self.status = "Close requested · finish the current operation, or Keep open".into();
                return;
            }
            let Some(id) = self.close_ui.next() else {
                self.close_ui.requested = false;
                self.status = "Closing request counter exhausted · window retained".into();
                return;
            };
            self.cancel_document_open();
            self.close_ui.session = id;
            self.close_ui.open = true;
            self.close_ui.focus_cancel = true;
            self.close_ui.painted = None;
            self.close_ui.requested = false;
            self.close_ui.error = None;
            self.close_ui.outcome = None;
            self.close_ui.nonce = None;
            self.close_ui.guard = None;
            self.close_refresh_paths();
            self.close_ui.phase = Some(Phase::Choices);
            self.close_ui.plan = None;
            if self.close_ui.paths.is_empty()
                && !self.close_ui.untitled
                && self.debug.connection.is_none()
                && !self.debug.busy
            {
                self.close_ui.plan = Some(true);
                self.close_ui.phase = Some(Phase::Waiting);
            }
        }
        if !self.close_ui.open {
            return;
        }
        if self
            .close_ui
            .guard
            .as_ref()
            .is_some_and(|g| *g != self.close_guard())
        {
            if let Some(id) = self.close_ui.nonce {
                self.breakpoint_request(Some(Action::CancelClose(id)));
            }
            self.close_ui.guard = None;
            self.close_ui.nonce = None;
            self.close_ui.outcome = None;
            self.close_ui.plan = None;
            self.close_ui.phase = Some(Phase::Choices);
            self.close_refresh_paths();
            self.close_ui.error =
                Some("Sources changed; review the current buffers before closing".into());
            return;
        }
        match self.close_ui.phase {
            Some(Phase::Waiting) => {
                if self.close_work_pending() {
                    return;
                }
                if self.debug.connection.is_some() {
                    self.debug_control("disconnect");
                    return;
                }
                if self.close_ui.plan == Some(true) {
                    self.close_refresh_paths();
                    if !self.close_ui.paths.is_empty() || self.close_ui.untitled {
                        self.close_ui.phase = Some(Phase::Choices);
                        self.close_ui.plan = None;
                        self.close_ui.error =
                            Some("Unsaved text changed; choose Save or Discard again".into());
                        return;
                    }
                }
                self.close_probe();
            }
            Some(Phase::Preparing | Phase::Committing) => {
                if self.debug.bp_service.pending() {
                    return;
                }
                if let Some(outcome) = self.close_ui.outcome.take() {
                    match outcome.status {
                        CloseStatus::Clean => self.close_commit(false),
                        CloseStatus::Backup(_) => {
                            self.close_ui.outcome = Some(outcome);
                            self.close_ui.phase = Some(Phase::Backup);
                        }
                        CloseStatus::Failed(error) => {
                            self.close_ui.error = Some(error);
                            self.close_ui.outcome = Some(CloseOutcome {
                                status: CloseStatus::Failed(self.close_ui.error.clone().unwrap()),
                                ..outcome
                            });
                            self.close_ui.phase = Some(Phase::Failed);
                        }
                        CloseStatus::Committed { backup, discarded } => {
                            if self.close_work_pending()
                                || self.close_other_modal()
                                || self.debug.connection.is_some()
                            {
                                if let Some(id) = self.close_ui.nonce {
                                    self.breakpoint_request(Some(Action::CancelClose(id)));
                                }
                                self.close_ui.phase = Some(Phase::Failed);
                                self.close_ui.error =
                                    Some("Work changed during closing; check again".into());
                            } else {
                                self.close_allow(discarded, backup);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    pub(super) fn window_close_wait_bar(&mut self, ctx: &egui::Context) {
        if self.close_ui.requested && !self.close_ui.open {
            egui::TopBottomPanel::top("close-wait").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Close requested · finishing the current operation");
                    if self
                        .chrome
                        .button(
                            ui,
                            "close-wait-cancel",
                            "Keep open",
                            Icon::Close,
                            Kind::Tonal,
                            true,
                            148.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.cancel_window_close();
                    }
                });
            });
        }
    }
    pub(super) fn window_close_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_ui.open {
            return;
        }
        let mut cancel = false;
        let mut save = false;
        let mut discard = false;
        let mut retry = false;
        let mut retry_stop = false;
        let mut close_backup = false;
        let mut discard_markers = false;
        let mut stop_saves = false;
        let ime = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))));
        if ime {
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key {
                            key: Key::Enter,
                            ..
                        }
                    )
                })
            });
        }
        let phase = self.close_ui.phase;
        let confirmed = self.close_ui.confirm_discard;
        let modal=egui::Modal::new(egui::Id::new("window-close")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-112.).clamp(320.,560.));ui.heading("Before you close");
            ui.label(RichText::new("Your source text and breakpoints have separate save checkpoints").color(m::MUTED));
            if self.debug.connection.is_some()||self.debug.busy{ui.small("Confirming close stops Debug; Keep open leaves the session or pending launch active.");}
            if confirmed{
                let changes=self.close_ui.outcome.as_ref().map_or(0,|o|o.changes);ui.colored_label(m::ERROR,format!("Discard {changes} unsaved marker changes without a backup?"));ui.label("Files on disk remain untouched. Keep open retains the local markers and editor Undo.");
            }else{
                match phase{
                    Some(Phase::Choices)=>{ui.label(self.close_choices_heading());egui::ScrollArea::vertical().id_salt("close-source-list").max_height((ctx.content_rect().height()-310.).clamp(40.,180.)).show_rows(ui,24.,self.close_ui.paths.len(),|ui,range|{for p in &self.close_ui.paths[range]{ui.add(egui::Label::new(p.strip_prefix(&self.root).unwrap_or(p).display().to_string()).truncate()).on_hover_text_passive(p.display().to_string());}});if self.close_ui.untitled{ui.colored_label(m::ERROR,"Untitled buffer · choose a named file before Save all");}},
                    Some(Phase::Saving)=>{ui.label("Saving the explicitly captured source snapshots…");ui.small("Already committed writes are kept if you cancel closing.");},
                    Some(Phase::Waiting)=>{if self.debug.stop_error.is_some(){ui.colored_label(m::ERROR,"Debug stop failed · the window stays open");ui.label("Retry stops only the original serial/package/PID session. Debug settings are preserved until success.");}else{ui.label("Finishing owned work before closing…");ui.small("An active Debug session will be stopped. Normal Run apps stay running.");}},
                    Some(Phase::Preparing)=>{ui.label("Checking marker persistence and recovery off-thread…");},
                    Some(Phase::Committing)=>{ui.label("Verifying the final closing checkpoint…");},
                    Some(Phase::Backup)=>{ui.colored_label(m::PRIMARY,"Marker write failed · a recovery draft is stored");if let Some(CloseOutcome{status:CloseStatus::Backup(path),..})=&self.close_ui.outcome{ui.add(egui::Label::new(path.display().to_string()).truncate()).on_hover_text_passive(path.display().to_string());}ui.small("The next window can review this draft. Disk preferences were not overwritten.");},
                    Some(Phase::Failed)=>{ui.colored_label(m::ERROR,"Marker changes are not verified as saved or backed up");ui.label("Keep this window open, retry, or explicitly review discarding local marker changes.");},
                    None=>{},
                }
            }
            if let Some(error)=&self.close_ui.error{ui.add(egui::Label::new(RichText::new(error).color(m::ERROR)).truncate()).on_hover_text_passive(error);}
            ui.small("Discard text never overwrites disk files. Terminal sessions and analysis stop after confirmation.");ui.separator();
            ui.horizontal_wrapped(|ui|{
                let keep=self.chrome.button(ui,"window-close-cancel","Keep open",Icon::Close,Kind::Ghost,true,136.,self.reduced_motion);if self.close_ui.focus_cancel{keep.request_focus();self.close_ui.focus_cancel=false;}cancel|=keep.clicked();
                if confirmed{if self.chrome.button(ui,"window-close-discard-markers","Discard and close",Icon::Close,Kind::Tonal,true,204.,self.reduced_motion).clicked(){discard_markers=true;}}
                else{match phase{
                    Some(Phase::Choices)=>{if self.close_ui.source_count()>0{discard|=self.chrome.button(ui,"window-close-discard-text","Discard text",Icon::Close,Kind::Tonal,true,156.,self.reduced_motion).clicked();}save|=self.chrome.button(ui,"window-close-save",self.close_primary_label(),if self.close_ui.source_count()>0{Icon::Save}else{Icon::Close},Kind::Filled,!self.close_ui.untitled,202.,self.reduced_motion).clicked();},
                    Some(Phase::Backup)=>{close_backup|=self.chrome.button(ui,"window-close-backup","Close with backup",Icon::Save,Kind::Filled,true,212.,self.reduced_motion).clicked();},
                    Some(Phase::Failed)=>{retry|=self.chrome.button(ui,"window-close-retry","Retry backup",Icon::Refresh,Kind::Tonal,true,164.,self.reduced_motion).clicked();self.close_ui.confirm_discard|=self.chrome.button(ui,"window-close-review-discard","Review discard",Icon::Code,Kind::Ghost,true,164.,self.reduced_motion).clicked();},
                    Some(Phase::Waiting)=>{if self.debug.stop_error.is_some(){retry_stop|=self.chrome.button(ui,"window-close-retry-stop","Retry Stop Debug",Icon::Refresh,Kind::Tonal,true,196.,self.reduced_motion).clicked();}else{ui.small("An active Debug session will be stopped. Normal Run apps stay running.");}},
                    Some(Phase::Saving)=>{stop_saves|=self.chrome.button(ui,"window-close-stop-saves","Stop remaining",Icon::Close,Kind::Tonal,true,184.,self.reduced_motion).clicked();},
                    _=>{},
                }}
            });
        });
        self.close_ui.painted = Some((self.close_ui.session, phase, confirmed));
        let escape = modal.is_top_modal
            && !modal.any_popup_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if cancel || escape {
            self.cancel_window_close();
        } else if stop_saves {
            self.stop_remaining_saves();
        } else if retry_stop {
            self.stop_debug();
        } else if save {
            self.close_choose(true);
        } else if discard {
            self.close_choose(false);
        } else if retry {
            self.close_probe();
        } else if close_backup {
            self.close_commit(false);
        } else if discard_markers {
            self.close_commit(true);
        }
        if matches!(
            phase,
            Some(Phase::Saving | Phase::Waiting | Phase::Preparing | Phase::Committing)
        ) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
#[path = "window_close_tests.rs"]
mod tests;
