//! M3 reviewed replacement modal; atomic publication and grouped Undo/Redo, no implicit Save.
use super::*;
use anyhow::Context;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    project_replace::{self as pr, Group, Guard, Output, Plan, Task},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Preview,
    Apply,
    History,
}
#[derive(Default)]
pub(super) struct ReplaceUi {
    pub dialog: bool,
    pub replacement: String,
    pub busy: bool,
    pub id: u64,
    pub error: String,
    pub plan: Option<Arc<Plan>>,
    pub worker: pr::Worker,
    pub undo: Vec<Group>,
    pub redo: Vec<Group>,
    history_redo: Option<bool>,
    pub applied: usize,
    pub undone: usize,
    pub redone: usize,
    pub rows: Vec<(usize, Option<usize>)>,
    pub cancelled: usize,
    pub rejected: usize,
    phase: Option<Phase>,
    focus: Option<egui::Id>,
    return_focus: Option<egui::Id>,
    return_editor: bool,
    outdated: bool,
    details: bool,
    ime_field: Option<egui::Id>,
    ime_composing: bool,
}
impl Workbench {
    pub(super) fn replace_blocked(&self) -> bool {
        self.preplace.dialog
            || self.preplace.busy
            || self.format.open
            || self.debug.bp_review.open
            || self.close_ui.open
            || self.close_ui.allow
    }
    fn replace_docs(&self) -> anyhow::Result<pr::Documents> {
        self.doc
            .path
            .iter()
            .map(|p| (p, &self.doc, &self.edit))
            .chain(self.parked.iter().map(|(p, (d, e))| (p, d, e)))
            .map(|(p, d, e)| {
                let mut d = d.fork()?;
                d.set_view(e.anchor, e.cursor);
                Ok((p.clone(), d))
            })
            .collect()
    }
    fn replace_guard_current(&self, guards: &BTreeMap<PathBuf, Guard>, all: bool) -> bool {
        let docs = self
            .doc
            .path
            .iter()
            .map(|p| (p, &self.doc))
            .chain(self.parked.iter().map(|(p, (d, _))| (p, d)))
            .collect::<BTreeMap<_, _>>();
        (!all || docs.len() == guards.len())
            && guards
                .iter()
                .all(|(p, g)| docs.get(p).is_some_and(|d| g.matches(d)))
    }
    fn replace_available(&self) -> bool {
        !self.replace_blocked()
            && !self.git.open
            && !self.chrome.settings
            && !self.chrome.device_open
            && !self.boot.pending
            && !self.welcome
            && !self.file_ui.open
            && !self.file_ui.busy
            && !self.projects.open
            && !self.refactor.dialog
            && !self.refactor.busy
            && !self.building
            && !self.running
            && !self.debug.busy
            && self.debug.connection.is_none()
            && !self.preview.busy
            && self.save_ui.pending.is_none()
            && self.open_ui.pending.is_none()
            && self.pending_close.is_none()
            && self.editor_ui.pending_reload.is_none()
    }
    pub(super) fn show_project_replace(&mut self) {
        if !self.replace_available() {
            self.status =
                "Finish pending file/Save/Build/Run/Debug operations before replacing".into();
            return;
        }
        self.replace_capture_focus();
        self.ime_cancel_external();
        self.cancel_deferred_input();
        self.edit.cancel_composition();
        self.editor_ui.focus_editor = false;
        self.psearch.focus = false;
        self.psearch.handoff.clear();
        self.preplace.dialog = true;
        self.preplace.error.clear();
        self.preplace.plan = None;
        self.preplace.outdated = false;
        self.preplace.details = false;
        self.preplace.ime_composing = false;
        self.preplace.ime_field = None;
        self.preplace.focus = Some(egui::Id::new(if self.psearch.options.query.is_empty() {
            "project-replace-query"
        } else {
            "project-replacement"
        }));
    }
    fn replace_capture_focus(&mut self) {
        self.preplace.return_focus = self.context.memory(|m| m.focused());
        self.preplace.return_editor = self.editor_ui.focus_editor
            || self
                .editor_ui
                .input_id
                .is_some_and(|id| self.preplace.return_focus == Some(id));
    }
    pub(super) fn cancel_project_replace(&mut self) {
        self.ime_cancel_external();
        self.preplace.worker.cancel();
        self.preplace.busy = false;
        self.preplace.dialog = false;
        self.preplace.plan = None;
        self.preplace.history_redo = None;
        self.preplace.phase = None;
        self.preplace.focus = None;
        self.preplace.ime_composing = false;
        self.preplace.cancelled += 1;
        self.editor_ui.focus_editor = self.preplace.return_editor;
        if !self.preplace.return_editor
            && let Some(id) = self.preplace.return_focus.take()
        {
            self.context.memory_mut(|m| m.request_focus(id));
        }
        self.status = "Project replacement cancelled · buffers unchanged".into();
    }
    fn replace_request(&mut self, task: Task) -> bool {
        let phase = match &task {
            Task::Preview { .. } => Phase::Preview,
            Task::Confirm(_) => Phase::Apply,
            Task::History { .. } => Phase::History,
        };
        let ctx = self.context.clone();
        match self
            .preplace
            .worker
            .request(task, Arc::new(move || ctx.request_repaint()))
        {
            Ok(id) => {
                self.preplace.id = id;
                self.preplace.busy = true;
                self.preplace.phase = Some(phase);
                self.preplace.error.clear();
                self.preplace.details = false;
                self.status = match phase {
                    Phase::Preview => "Preparing replacement preview…",
                    Phase::Apply => "Checking reviewed changes…",
                    Phase::History => "Restoring project changes…",
                }
                .into();
                true
            }
            Err(e) => {
                self.preplace.error = format!("{e:#}");
                false
            }
        }
    }
    pub(super) fn preview_project_replace(&mut self) {
        if self.preplace.busy {
            if self.preplace.phase != Some(Phase::Preview) {
                return;
            }
            self.preplace.worker.cancel();
            self.preplace.busy = false;
            self.preplace.phase = None;
        }
        match self.replace_docs() {
            Ok(docs) => {
                self.replace_request(Task::Preview {
                    root: self.root.clone(),
                    docs,
                    options: self.psearch.options.clone(),
                    replacement: self.preplace.replacement.clone(),
                });
            }
            Err(e) => self.preplace.error = e.to_string(),
        }
    }
    pub(super) fn apply_project_replace(&mut self) {
        let Some(p) = self.preplace.plan.clone() else {
            return;
        };
        if self.preplace.busy || p.files.is_empty() {
            return;
        }
        if self.psearch.options != p.options
            || self.preplace.replacement != p.replacement
            || !self.replace_guard_current(&p.input, true)
        {
            self.preplace.error =
                "Buffers/query changed · Preview again; no changes applied".into();
            self.preplace.rejected += 1;
            return;
        }
        if p.files
            .iter()
            .any(|f| self.editor_ui.conflicts.contains_key(&f.path))
        {
            self.preplace.error = "Review external file changes before applying".into();
            return;
        }
        self.replace_request(Task::Confirm(p));
    }
    fn install_replace_docs(&mut self, docs: pr::Documents, history: bool) {
        for (path, mut d) in docs {
            if history {
                let view = d.view();
                if self.doc.path.as_ref() == Some(&path) {
                    self.doc = d;
                    if let Some((a, c)) = view {
                        self.edit.select(a, c);
                    }
                    self.edit.clamp(&self.doc);
                } else if let Some((old, e)) = self.parked.get_mut(&path) {
                    *old = d;
                    if let Some((a, c)) = view {
                        e.select(a, c);
                    }
                    e.clamp(old);
                }
            } else if self.doc.path.as_ref() == Some(&path) {
                d.acknowledge_disk(Some(&self.doc));
                self.doc = d;
                self.edit.clamp(&self.doc);
            } else {
                let e = self
                    .parked
                    .remove(&path)
                    .map(|(old, e)| {
                        d.acknowledge_disk(Some(&old));
                        e
                    })
                    .unwrap_or_default();
                self.parked.insert(path.clone(), (d, e));
                if !self.tabs.contains(&path) {
                    self.tabs.push(path);
                }
            }
        }
        self.did_edit();
        self.sync_watches();
        self.last_sent = None;
        self.send_snapshot();
    }
    pub(super) fn project_replace_step(&mut self, ctx: &egui::Context) {
        let Some(done) = self.preplace.worker.poll() else {
            if self.preplace.busy {
                ctx.request_repaint_after(Duration::from_millis(25));
            }
            return;
        };
        if !self.preplace.busy || done.id != self.preplace.id {
            return;
        }
        self.preplace.busy = false;
        self.preplace.phase = None;
        let result = (|| -> anyhow::Result<()> {
            match done.result.map_err(anyhow::Error::msg)? {
                Output::Preview(p) => {
                    anyhow::ensure!(
                        self.preplace.dialog
                            && self.replace_guard_current(&p.input, true)
                            && p.options == self.psearch.options
                            && p.replacement == self.preplace.replacement,
                        "Preview inputs changed · Preview again"
                    );
                    self.preplace.rows = p
                        .files
                        .iter()
                        .enumerate()
                        .flat_map(|(i, f)| {
                            std::iter::once((i, None))
                                .chain((0..f.rows.len()).map(move |n| (i, Some(n))))
                        })
                        .collect();
                    self.status = if p.files.is_empty() {
                        "Preview ready · no changes to apply".into()
                    } else {
                        format!(
                            "Preview ready · {} changes in {} files · unsaved",
                            p.edits(),
                            p.files.len()
                        )
                    };
                    self.preplace.plan = Some(p);
                    self.preplace.outdated = false;
                }
                Output::Confirmed(p) => {
                    anyhow::ensure!(
                        self.preplace.dialog
                            && self.replace_guard_current(&p.input, true)
                            && p.options == self.psearch.options
                            && p.replacement == self.preplace.replacement,
                        "Apply inputs changed; no buffers changed"
                    );
                    anyhow::ensure!(
                        !p.files
                            .iter()
                            .any(|f| self.editor_ui.conflicts.contains_key(&f.path)),
                        "External file conflict; no buffers changed"
                    );
                    let docs = p
                        .files
                        .iter()
                        .map(|f| Ok((f.path.clone(), f.after.fork()?)))
                        .collect::<anyhow::Result<pr::Documents>>()?;
                    let group = Group::from_plan(&p);
                    let n = group.files.len();
                    self.install_replace_docs(docs, false);
                    self.preplace.undo.push(group);
                    if self.preplace.undo.len() > 16 {
                        self.preplace.undo.remove(0);
                    }
                    self.preplace.redo.clear();
                    self.preplace.applied += 1;
                    self.editor_ui.focus_editor = true;
                    self.preplace.dialog = false;
                    self.preplace.plan = None;
                    self.status = format!("Replaced in {n} files · unsaved · Undo all available");
                }
                Output::History(h) => {
                    anyhow::ensure!(
                        self.replace_guard_current(&h.guards, false),
                        "Buffers changed during project history; no buffers changed"
                    );
                    let redo = self
                        .preplace
                        .history_redo
                        .context("Missing project history action")?;
                    // Take the whole group before publishing any document.
                    let g = if redo {
                        self.preplace.redo.pop()
                    } else {
                        self.preplace.undo.pop()
                    }
                    .context("Missing project history group")?;
                    self.install_replace_docs(h.docs, true);
                    if redo {
                        self.preplace.undo.push(g);
                        self.preplace.redone += 1;
                    } else {
                        self.preplace.redo.push(g);
                        self.preplace.undone += 1;
                    }
                    self.preplace.history_redo = None;
                    self.preplace.dialog = false;
                    // Return keyboard ownership after a sidebar-button initiated history action.
                    self.editor_ui.focus_editor = true;
                    self.status = if redo {
                        "Project replacement redone · all buffers restored"
                    } else {
                        "Project replacement undone · all buffers restored"
                    }
                    .into();
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.preplace.error = format!("{e:#}");
            self.status = "Project operation not applied · see Details and preview again".into();
            self.preplace.rejected += 1;
        }
    }
    pub(super) fn project_replace_history(&mut self, redo: bool) {
        if self.preplace.busy || !self.replace_available() {
            return;
        }
        let group = if redo {
            self.preplace.redo.last()
        } else {
            self.preplace.undo.last()
        };
        let Some(group) = group.cloned() else {
            return;
        };
        match self.replace_docs() {
            Ok(docs) => {
                self.replace_capture_focus();
                self.ime_cancel_external();
                self.preplace.dialog = true;
                self.preplace.plan = None;
                self.preplace.history_redo = Some(redo);
                self.replace_request(Task::History { group, docs, redo });
            }
            Err(e) => self.status = e.to_string(),
        }
    }
    pub(super) fn project_replace_history_key(&mut self, redo: bool) -> bool {
        let group = if redo {
            self.preplace.redo.last()
        } else {
            self.preplace.undo.last()
        };
        let candidate = self
            .doc
            .path
            .as_ref()
            .is_some_and(|p| group.is_some_and(|g| g.candidate(p, &self.doc, redo)));
        if candidate {
            self.project_replace_history(redo);
        }
        candidate
    }
    pub(super) fn project_replace_controls(&mut self, ui: &mut egui::Ui) {
        let mut open = false;
        let mut initiator = None;
        let mut undo = false;
        let mut redo = false;
        ui.horizontal_wrapped(|ui| {
            let r = self.chrome.button(
                ui,
                "ps-replace",
                "Replace…",
                Icon::Code,
                Kind::Tonal,
                self.replace_available(),
                110.,
                self.reduced_motion,
            );
            open = r.clicked();
            if open {
                initiator = Some(r.id);
            }
            undo = self
                .chrome
                .button(
                    ui,
                    "ps-undo",
                    "Undo all",
                    Icon::Refresh,
                    Kind::Ghost,
                    !self.preplace.undo.is_empty() && !self.preplace.busy,
                    110.,
                    self.reduced_motion,
                )
                .clicked();
            if !self.preplace.redo.is_empty() {
                redo = self
                    .chrome
                    .button(
                        ui,
                        "ps-redo",
                        "Redo all",
                        Icon::Refresh,
                        Kind::Ghost,
                        !self.preplace.busy,
                        110.,
                        self.reduced_motion,
                    )
                    .clicked();
            }
        });
        if open {
            self.show_project_replace();
            if self.preplace.dialog {
                // A pointer-activated custom button need not own egui keyboard focus.
                // Return to the actual initiator, not an earlier source TextEdit.
                self.preplace.return_focus = initiator;
                self.preplace.return_editor = false;
            }
        } else if undo {
            self.project_replace_history(false);
        } else if redo {
            self.project_replace_history(true);
        }
    }
    fn replace_inputs_changed(&mut self) {
        if self.preplace.phase == Some(Phase::Preview) {
            self.preplace.worker.cancel();
            self.preplace.busy = false;
            self.preplace.phase = None;
        }
        self.preplace.outdated = true;
        self.preplace.error.clear();
        self.preplace.details = false;
    }
    fn replace_review_ready(&self) -> bool {
        !self.preplace.busy
            && !self.preplace.outdated
            && self.preplace.error.is_empty()
            && self.preplace.plan.as_ref().is_some_and(|p| {
                !p.files.is_empty()
                    && p.options == self.psearch.options
                    && p.replacement == self.preplace.replacement
                    && self.replace_guard_current(&p.input, true)
            })
    }
    // Keep candidate-accepting Enter out of the Preview/Apply shortcuts, including
    // Preedit in a previous frame and Commit+Enter in the current frame.
    fn replace_keyboard(&mut self, ctx: &egui::Context) -> (bool, bool) {
        let field = ctx.memory(|m| m.focused()).filter(|id| {
            *id == egui::Id::new("project-replace-query")
                || *id == egui::Id::new("project-replacement")
        });
        if field != self.preplace.ime_field {
            self.preplace.ime_field = field;
            self.preplace.ime_composing = false;
        }
        let was_composing = self.preplace.ime_composing;
        let mut ime_in_frame = false;
        ctx.input(|i| {
            if !i.focused {
                self.preplace.ime_composing = false;
            }
            for e in &i.events {
                if let egui::Event::Ime(e) = e {
                    ime_in_frame = true;
                    match e {
                        egui::ImeEvent::Preedit(s) if !s.is_empty() => {
                            self.preplace.ime_composing = true
                        }
                        egui::ImeEvent::Enabled
                        | egui::ImeEvent::Commit(_)
                        | egui::ImeEvent::Disabled => self.preplace.ime_composing = false,
                        _ => {}
                    }
                }
            }
        });
        if was_composing
            || self.preplace.ime_composing
            || ime_in_frame
            || self.preplace.history_redo.is_some()
        {
            return (false, false);
        }
        let apply = Self::consume_exact_key(ctx, egui::Modifiers::COMMAND, Key::Enter)
            || Self::consume_exact_key(ctx, egui::Modifiers::CTRL, Key::Enter);
        let enter =
            field.is_some() && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Enter);
        let preview = enter && !self.psearch.options.query.is_empty();
        if enter && !preview {
            self.preplace.focus = Some(egui::Id::new("project-replace-query"));
        }
        (preview, apply && self.replace_review_ready())
    }
    pub(super) fn project_replace_dialog(&mut self, ctx: &egui::Context) {
        if !self.preplace.dialog {
            return;
        }
        // Close before registering this frame's modal layer, so the restored
        // initiator can receive the very next keyboard event without a dead frame.
        if !egui::Popup::is_any_open(ctx)
            && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Escape)
        {
            self.cancel_project_replace();
            return;
        }
        let (mut preview, mut apply) = self.replace_keyboard(ctx);
        let mut cancel = false;
        let mut changed = false;
        let mut query_changed = false;
        egui::Modal::new(egui::Id::new("project-replace-modal"))
            .frame(m::card().fill(m::HIGH).inner_margin(20))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 100.).clamp(280., 760.));
                ui.heading(if self.preplace.history_redo.is_some() { "Project history" } else { "Replace in Files" });
                ui.label("Review first. Changes stay unsaved until you save.");
                let body_height = if self.preplace.history_redo.is_some() { (ctx.content_rect().height() - 230.).clamp(80., 130.) } else { (ctx.content_rect().height() - 230.).clamp(120., 520.) };
                let body = egui::ScrollArea::vertical().id_salt("project-replace-body")
                    .max_height(body_height).auto_shrink([false, false]).show(ui, |ui| {
                        if self.preplace.history_redo.is_none() {
                            let enabled = !self.preplace.busy || self.preplace.phase == Some(Phase::Preview);
                            let label = ui.label("Find");
                            let r = ui.add_enabled(enabled, egui::TextEdit::singleline(&mut self.psearch.options.query)
                                .id(egui::Id::new("project-replace-query")).char_limit(4096)
                                .margin(egui::vec2(10., 8.)).desired_width(f32::INFINITY).hint_text("Text or pattern to find"))
                                .labelled_by(label.id);
                            self.chrome.hits.insert("pr-query".into(), r.rect);
                            query_changed |= r.changed();
                            if self.preplace.focus == Some(r.id) { r.request_focus(); self.preplace.focus = None; }
                            let label = ui.label("Replace with");
                            let r = ui.add_enabled(enabled, egui::TextEdit::singleline(&mut self.preplace.replacement)
                                .id(egui::Id::new("project-replacement")).char_limit(4096)
                                .margin(egui::vec2(10., 8.)).desired_width(f32::INFINITY).hint_text("Leave empty to delete matches"))
                                .labelled_by(label.id);
                            self.chrome.hits.insert("pr-text".into(), r.rect);
                            changed |= r.changed();
                            if self.preplace.focus == Some(r.id) { r.request_focus(); self.preplace.focus = None; }
                            ui.add_enabled_ui(enabled, |ui| { ui.horizontal_wrapped(|ui| {
                                for (key, label, hint, value) in [
                                    ("pr-case", "Aa", "Match case", &mut self.psearch.options.case_sensitive),
                                    ("pr-word", "Whole word", "Match whole words", &mut self.psearch.options.whole_word),
                                    ("pr-regex", ".* Regex", "Use regular expressions", &mut self.psearch.options.regex)
                                ] {
                                    let r = ui.selectable_label(*value, label).on_hover_text_passive(hint);
                                    self.chrome.hits.insert(key.into(), r.rect);
                                    if r.clicked() { *value = !*value; query_changed = true; }
                                }
                            }); });
                            if self.psearch.options.regex { ui.small(r"Replacement: $1 / ${name}, $$ for $, \n / \r / \t. Empty deletes matches."); }
                        }
                        if self.preplace.busy {
                            ui.label(match self.preplace.phase {
                                Some(Phase::Preview) => "Finding and reviewing changes…",
                                Some(Phase::Apply) => "Checking files before applying…",
                                _ => "Restoring project changes…"
                            });
                            m::loading(ui, ui.available_rect_before_wrap().with_max_y(ui.cursor().min.y + 24.), self.reduced_motion);
                            ui.add_space(24.);
                        }
                        if !self.preplace.error.is_empty() {
                            ui.colored_label(m::ERROR, "Operation not applied. Your edits are kept.");
                            let r = ui.toggle_value(&mut self.preplace.details, "Details");
                            self.chrome.hits.insert("pr-details".into(), r.rect);
                            if self.preplace.details {
                                egui::ScrollArea::vertical().id_salt("project-replace-error").max_height(85.).show(ui, |ui| {
                                    ui.label(&self.preplace.error);
                                });
                            }
                        }
                        if let Some(p) = self.preplace.plan.clone() {
                            if self.preplace.outdated || p.options != self.psearch.options || p.replacement != self.preplace.replacement {
                                ui.colored_label(m::WARNING, "Previous preview · inputs changed · Preview again");
                            }
                            ui.label(format!("{} changes in {} files · {} unsaved sources · {} skipped files",
                                p.edits(), p.files.len(), p.files.iter().filter(|f| f.dirty).count(),
                                p.stats.skipped_binary + p.stats.skipped_large + p.stats.skipped_special));
                            if p.files.is_empty() {
                                ui.label(if p.stats.matches == 0 { "No matches. Adjust Find or its options and preview again.".into() }
                                    else { format!("{} matches already have this text. Nothing to change.", p.stats.matches) });
                            } else { self.project_replace_rows(ui, body_height - 195., &p); }
                        } else if !self.preplace.busy && self.preplace.history_redo.is_none() {
                            ui.colored_label(m::MUTED, "Preview to review changes by file before applying.");
                        }
                    });
                self.chrome.hits.insert("pr-body".into(), body.inner_rect);
                // The footer is outside every scrolling region and reserves a stable
                // position even for a long query, full diagnostic or changing preview.
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if self.preplace.history_redo.is_none() {
                        preview |= self.chrome.button(ui, "pr-preview", "Preview", Icon::Search, Kind::Tonal,
                            (!self.preplace.busy || self.preplace.phase == Some(Phase::Preview)) && !self.psearch.options.query.is_empty(),
                            120., self.reduced_motion).on_hover_text_passive("Review changes · Enter in either text field").clicked();
                        let label = self.preplace.plan.as_ref().filter(|_| self.replace_review_ready())
                            .map(|p| format!("Apply {} changes", p.edits())).unwrap_or_else(|| "Apply changes".into());
                        apply |= self.chrome.button(ui, "pr-apply", &label, Icon::Check, Kind::Filled,
                            !changed && !query_changed && self.replace_review_ready(), 180., self.reduced_motion)
                            .on_hover_text_passive("Apply the current review to unsaved tabs · Ctrl+Enter").clicked();
                    }
                    cancel = self.chrome.button(ui, "pr-cancel", "Cancel", Icon::Close, Kind::Ghost, true, 105., self.reduced_motion).clicked();
                });
                ui.small(if self.preplace.history_redo.is_some() { "Esc cancels this operation" } else { "Enter previews · Ctrl+Enter applies a current review · Esc cancels" });
            });
        if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Escape) {
            cancel = true;
        }
        if changed || query_changed {
            self.replace_inputs_changed();
            apply = false;
        }
        if query_changed {
            self.invalidate_project_search();
        }
        if cancel {
            self.cancel_project_replace();
        } else if preview {
            self.preview_project_replace();
        } else if apply {
            self.apply_project_replace();
        }
    }
    fn project_replace_rows(&self, ui: &mut egui::Ui, height: f32, p: &Plan) {
        egui::ScrollArea::vertical()
            .id_salt("project-replace-preview")
            .max_height(height.max(60.))
            .show_rows(ui, 62., self.preplace.rows.len(), |ui, range| {
                for i in range {
                    let (fi, mi) = self.preplace.rows[i];
                    let f = &p.files[fi];
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 62.),
                        egui::Sense::hover(),
                    );
                    response.on_hover_text_passive(f.path.display().to_string());
                    if let Some(mi) = mi {
                        let row = &f.rows[mi];
                        m::label_clipped(
                            ui,
                            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 20.)),
                            &format!(
                                "{}:{}{}",
                                row.line,
                                row.column,
                                if row.clipped {
                                    " · clipped context"
                                } else {
                                    ""
                                }
                            ),
                            11.,
                            m::MUTED,
                        );
                        for (y, text, color) in [
                            (20., format!("− {}", row.before), m::ERROR),
                            (40., format!("+ {}", row.after), m::SUCCESS),
                        ] {
                            m::label_clipped(
                                ui,
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(0., y),
                                    egui::vec2(rect.width(), 20.),
                                ),
                                &text,
                                12.,
                                color,
                            );
                        }
                    } else {
                        wexandroidstudio::file_icons::paint(
                            ui,
                            egui::Rect::from_center_size(
                                rect.left_center() + egui::vec2(12., 0.),
                                egui::Vec2::splat(20.),
                            ),
                            &f.path,
                            false,
                            false,
                            true,
                        );
                        let name = f.path.file_name().unwrap_or_default().to_string_lossy();
                        m::label_clipped(
                            ui,
                            egui::Rect::from_min_size(
                                rect.min + egui::vec2(30., 6.),
                                egui::vec2(rect.width() - 30., 24.),
                            ),
                            &format!(
                                "{} · {} change{}{}",
                                name,
                                f.rows.len(),
                                if f.rows.len() == 1 { "" } else { "s" },
                                if f.dirty { " · unsaved" } else { "" }
                            ),
                            13.,
                            m::PRIMARY,
                        );
                        let rel = f.path.strip_prefix(&self.root).unwrap_or(&f.path);
                        let parent = rel.parent().unwrap_or(Path::new(""));
                        let folder = if parent.as_os_str().is_empty() {
                            "Project root".into()
                        } else {
                            parent.display().to_string()
                        };
                        m::label_clipped(
                            ui,
                            egui::Rect::from_min_size(
                                rect.min + egui::vec2(30., 32.),
                                egui::vec2(rect.width() - 30., 22.),
                            ),
                            &folder,
                            11.,
                            m::MUTED,
                        );
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn fixture() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        for (n, t) in [
            ("Main.kt", "val a=1\n"),
            ("Park.kt", "val b=2\n"),
            ("Closed.txt", "😀 needle closed\n"),
        ] {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        let ctx = egui::Context::default();
        m::install(&ctx);
        let mut a = Workbench::new(d.path().into(), ctx, None, false);
        a.doc = Document::open(d.path().join("Main.kt")).unwrap();
        a.doc.defer_syntax();
        a.doc.edit(0..0, "// needle active\n").unwrap();
        a.tabs = vec![d.path().join("Main.kt"), d.path().join("Park.kt")];
        let p = d.path().join("Park.kt");
        let mut doc = Document::open(&p).unwrap();
        doc.defer_syntax();
        doc.edit(0..0, "// needle parked\n").unwrap();
        a.parked.insert(p, (doc, Default::default()));
        a.psearch.options.query = "needle".into();
        a.preplace.replacement = "replacement".into();
        (d, a)
    }
    fn wait(a: &mut Workbench) {
        let end = Instant::now() + Duration::from_secs(10);
        while a.preplace.busy {
            let ctx = a.context.clone();
            a.project_replace_step(&ctx);
            assert!(Instant::now() < end, "{}", a.preplace.error);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn preview(a: &mut Workbench) {
        a.show_project_replace();
        assert!(a.preplace.dialog);
        a.preview_project_replace();
        wait(a);
        assert!(a.preplace.plan.is_some(), "{}", a.preplace.error);
    }
    fn apply(a: &mut Workbench) {
        preview(a);
        a.apply_project_replace();
        wait(a);
        assert_eq!(a.preplace.applied, 1, "{}", a.preplace.error);
    }
    fn frame(a: &mut Workbench, events: Vec<egui::Event>) {
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 800.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                a.semantic_shortcuts(ctx);
                a.editor_shortcuts(ctx);
                a.draw_shell(ctx);
                a.finish_editor_frame(ctx);
                a.project_replace_step(ctx);
                a.ime_finish(ctx);
            },
        );
    }
    #[test]
    fn reviewed_apply_opens_closed_buffers_without_any_implicit_disk_save() {
        let (d, mut a) = fixture();
        let original = a.doc.content_hash();
        preview(&mut a);
        assert_eq!(a.doc.content_hash(), original);
        assert!(!a.parked.contains_key(&d.path().join("Closed.txt")));
        assert_eq!(a.preplace.plan.as_ref().unwrap().files.len(), 3);
        a.apply_project_replace();
        wait(&mut a);
        assert_eq!(a.preplace.applied, 1);
        assert_eq!(a.tabs.len(), 3);
        assert!(a.doc.dirty());
        assert!(a.parked.values().all(|(d, _)| d.dirty()));
        assert!(a.doc.rope().to_string().contains("replacement"));
        assert!(
            a.parked[&d.path().join("Park.kt")]
                .0
                .rope()
                .to_string()
                .contains("replacement")
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("Main.kt")).unwrap(),
            "val a=1\n"
        );
        assert!(!a.save_ui.saver.worker_started());
    }
    #[test]
    fn common_undo_and_redo_restore_dirty_active_parked_and_clean_closed_origins() {
        let (d, mut a) = fixture();
        let hash = a.doc.content_hash();
        apply(&mut a);
        a.project_replace_history(false);
        wait(&mut a);
        assert_eq!(a.preplace.undone, 1);
        assert_eq!(a.doc.content_hash(), hash);
        assert!(a.doc.dirty());
        assert!(a.parked[&d.path().join("Park.kt")].0.dirty());
        assert!(!a.parked[&d.path().join("Closed.txt")].0.dirty());
        a.project_replace_history(true);
        wait(&mut a);
        assert_eq!(a.preplace.redone, 1);
        assert!(a.parked.values().all(|(d, _)| d.dirty()));
    }
    #[test]
    fn stale_parked_buffer_or_query_rejects_apply_before_any_document_changes() {
        let (d, mut a) = fixture();
        preview(&mut a);
        let hash = a.doc.content_hash();
        a.parked
            .get_mut(&d.path().join("Park.kt"))
            .unwrap()
            .0
            .edit(0..0, "local")
            .unwrap();
        a.apply_project_replace();
        assert!(!a.preplace.busy);
        assert_eq!(a.preplace.rejected, 1);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.preplace.applied, 0);
        a.preview_project_replace();
        wait(&mut a);
        a.psearch.options.query = "other".into();
        a.apply_project_replace();
        assert_eq!(a.preplace.rejected, 2);
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn disk_changed_after_preview_rejects_without_opening_or_changing_any_buffer() {
        let (d, mut a) = fixture();
        preview(&mut a);
        let hash = a.doc.content_hash();
        std::fs::write(d.path().join("Closed.txt"), "external").unwrap();
        a.apply_project_replace();
        wait(&mut a);
        assert_eq!(a.preplace.rejected, 1);
        assert_eq!(a.preplace.applied, 0);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.tabs.len(), 2);
    }
    #[test]
    fn escape_in_completion_frame_wins_over_queued_apply() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        let hash = a.doc.content_hash();
        a.apply_project_replace();
        std::thread::sleep(Duration::from_millis(100));
        frame(
            &mut a,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(a.preplace.cancelled, 1);
        assert_eq!(a.preplace.applied, 0);
        assert_eq!(a.doc.content_hash(), hash);
        assert!(!a.preplace.dialog);
    }
    #[test]
    fn changed_replacement_discards_old_preview_and_does_not_apply() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        a.preplace.replacement = "different".into();
        a.apply_project_replace();
        assert_eq!(a.preplace.rejected, 1);
        assert_eq!(a.preplace.applied, 0);
    }
    #[test]
    fn closed_affected_tab_rejects_shared_undo_atomically() {
        let (d, mut a) = fixture();
        apply(&mut a);
        let hash = a.doc.content_hash();
        a.parked.remove(&d.path().join("Closed.txt"));
        a.project_replace_history(false);
        wait(&mut a);
        assert_eq!(a.preplace.undone, 0);
        assert_eq!(a.preplace.rejected, 1);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.preplace.undo.len(), 1);
    }
    #[test]
    fn missing_history_group_rejects_before_installing_prepared_documents() {
        let (_d, mut a) = fixture();
        apply(&mut a);
        let hash = a.doc.content_hash();
        a.project_replace_history(false);
        a.preplace.undo.clear();
        wait(&mut a);
        assert_eq!(a.preplace.undone, 0);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.preplace.rejected, 1);
    }
    #[test]
    fn local_undo_then_project_boundary_uses_shared_history_and_retains_saved_baseline() {
        let (_d, mut a) = fixture();
        apply(&mut a);
        a.doc.edit(0..0, "local").unwrap();
        assert!(!a.project_replace_history_key(false));
        a.doc.undo().unwrap();
        assert!(a.project_replace_history_key(false));
        wait(&mut a);
        assert_eq!(a.preplace.undone, 1);
        assert!(a.project_replace_history_key(true));
        wait(&mut a);
        assert_eq!(a.preplace.redone, 1);
    }
    #[test]
    fn modal_blocks_save_build_and_file_actions_without_losing_buffers() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        assert!(a.editor_blocked());
        let hash = a.doc.content_hash();
        a.build();
        a.run();
        assert!(!a.building && !a.running);
        assert_eq!(a.doc.content_hash(), hash);
        a.cancel_project_replace();
        assert!(!a.replace_blocked());
    }
    #[test]
    fn shared_history_button_returns_keyboard_focus_to_source_for_following_local_undo() {
        let (_d, mut a) = fixture();
        apply(&mut a);
        a.editor_ui.focus_editor = false;
        a.context
            .memory_mut(|m| m.request_focus(egui::Id::new("sidebar-history-button")));
        a.project_replace_history(false);
        wait(&mut a);
        assert!(
            a.editor_ui.focus_editor,
            "History modal must return keyboard focus to source, not the sidebar button"
        );
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![egui::Event::Key {
                key: Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            }],
        );
        assert_eq!(a.doc.rope().to_string(), "val a=1\n");
        assert!(!a.doc.dirty());
    }
    #[test]
    fn text_after_shared_undo_in_same_input_frame_survives_and_rejects_entire_stale_history() {
        let (d, mut a) = fixture();
        apply(&mut a);
        frame(&mut a, vec![]);
        let parked_hash = a.parked[&d.path().join("Park.kt")].0.content_hash();
        frame(
            &mut a,
            vec![
                egui::Event::Key {
                    key: Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                },
                egui::Event::Text("FOLLOWING_TEXT".into()),
            ],
        );
        wait(&mut a);
        assert!(a.doc.rope().to_string().contains("FOLLOWING_TEXT"));
        assert!(a.doc.rope().to_string().contains("replacement"));
        assert_eq!(a.preplace.undone, 0);
        assert_eq!(a.preplace.rejected, 1);
        assert_eq!(
            a.parked[&d.path().join("Park.kt")].0.content_hash(),
            parked_hash
        );
    }
    #[test]
    fn minimum_modal_keeps_actions_visible_with_long_query_and_diagnostic() {
        let (_d, mut a) = fixture();
        a.psearch.options.query = "find ".repeat(600);
        a.show_project_replace();
        a.preplace.error = format!(
            "Preview could not finish:\n{}",
            "long diagnostic context ".repeat(600)
        );
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760., 560.));
        for _ in 0..3 {
            let ctx = a.context.clone();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| a.project_replace_dialog(ctx),
            );
        }
        for k in ["pr-preview", "pr-apply", "pr-cancel"] {
            let rect = a.chrome.hits[k];
            assert!(
                screen.contains_rect(rect),
                "{k} is unreachable at minimum window: {rect:?}"
            );
        }
    }
    fn press(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    #[test]
    fn opening_focuses_replacement_or_empty_find_without_changing_source() {
        let (_d, mut a) = fixture();
        let hash = a.doc.content_hash();
        a.show_project_replace();
        frame(&mut a, vec![]);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("project-replacement")))
        );
        a.cancel_project_replace();
        a.psearch.options.query.clear();
        a.show_project_replace();
        frame(&mut a, vec![]);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("project-replace-query")))
        );
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn enter_reviews_and_command_enter_applies_only_a_current_review() {
        let (_d, mut a) = fixture();
        a.show_project_replace();
        frame(&mut a, vec![]);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        wait(&mut a);
        assert!(a.replace_review_ready());
        assert_eq!(a.preplace.applied, 0);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        wait(&mut a);
        assert_eq!(a.preplace.applied, 0);
        frame(
            &mut a,
            vec![press(
                Key::Enter,
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            )],
        );
        wait(&mut a);
        assert_eq!(a.preplace.applied, 1);
        assert!(a.doc.dirty());
    }
    #[test]
    fn editing_find_keeps_last_good_review_disabled_and_zero_matches_are_not_applyable() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        let hash = a.doc.content_hash();
        let old = a.preplace.plan.clone().unwrap();
        a.preplace.focus = Some(egui::Id::new("project-replace-query"));
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![
                press(Key::A, egui::Modifiers::CTRL | egui::Modifiers::COMMAND),
                egui::Event::Paste("absent-query".into()),
            ],
        );
        assert_eq!(a.psearch.options.query, "absent-query");
        assert!(Arc::ptr_eq(a.preplace.plan.as_ref().unwrap(), &old));
        assert!(!a.replace_review_ready());
        assert!(a.preplace.outdated);
        frame(
            &mut a,
            vec![press(
                Key::Enter,
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            )],
        );
        assert_eq!(a.preplace.applied, 0);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        wait(&mut a);
        assert_eq!(a.preplace.plan.as_ref().unwrap().stats.matches, 0);
        assert!(!a.replace_review_ready());
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn changing_inputs_cancels_queued_review_but_keeps_previous_preview_and_all_buffers() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        let old = a.preplace.plan.clone().unwrap();
        let hash = a.doc.content_hash();
        a.preview_project_replace();
        assert_eq!(a.preplace.phase, Some(Phase::Preview));
        a.preplace.replacement = "new-input".into();
        a.replace_inputs_changed();
        assert!(!a.preplace.busy);
        assert!(Arc::ptr_eq(a.preplace.plan.as_ref().unwrap(), &old));
        assert!(!a.replace_review_ready());
        let ctx = a.context.clone();
        for _ in 0..20 {
            a.project_replace_step(&ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.preplace.applied, 0);
    }
    #[test]
    fn cancel_restores_source_focus_and_rejects_late_modal_ime_commit() {
        let (_d, mut a) = fixture();
        frame(&mut a, vec![]);
        let hash = a.doc.content_hash();
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        a.show_project_replace();
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("候補".into())),
            ],
        );
        a.cancel_project_replace();
        assert!(a.editor_ui.focus_editor);
        frame(
            &mut a,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("候補".into()))],
        );
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn modal_ime_candidate_enter_and_commit_enter_never_submit_a_review() {
        let (_d, mut a) = fixture();
        a.show_project_replace();
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("候補".into())),
            ],
        );
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        assert_eq!(a.preplace.id, 0);
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("候補".into())),
                press(Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::COMMAND),
            ],
        );
        assert_eq!(a.preplace.id, 0);
        assert_eq!(a.preplace.applied, 0);
        frame(&mut a, vec![egui::Event::Ime(egui::ImeEvent::Disabled)]);
        a.preplace.focus = Some(egui::Id::new("project-replacement"));
        frame(&mut a, vec![]);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        wait(&mut a);
        assert!(a.preplace.plan.is_some());
    }
    #[test]
    fn expanded_details_with_a_real_review_stay_bounded_at_minimum_window() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        a.preplace.error = "many details\n".repeat(1000);
        a.preplace.details = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760., 560.));
        for _ in 0..3 {
            let ctx = a.context.clone();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| a.project_replace_dialog(ctx),
            );
        }
        for key in ["pr-query", "pr-text", "pr-preview", "pr-apply", "pr-cancel"] {
            assert!(screen.contains_rect(a.chrome.hits[key]), "{key}");
        }
        assert!(a.chrome.hits["pr-body"].height() <= 331.);
        assert!(!a.replace_review_ready());
    }
    #[test]
    fn sidebar_search_debounce_waits_for_replace_modal_then_resumes() {
        let (_d, mut a) = fixture();
        a.psearch.open = true;
        a.show_project_replace();
        a.invalidate_project_search();
        std::thread::sleep(Duration::from_millis(270));
        let ctx = a.context.clone();
        a.project_search_step(&ctx);
        assert!(!a.psearch.pending);
        a.cancel_project_replace();
        a.project_search_step(&ctx);
        assert!(a.psearch.pending);
    }
    #[test]
    fn empty_find_enter_keeps_focus_without_starting_a_worker() {
        let (_d, mut a) = fixture();
        a.psearch.options.query.clear();
        a.show_project_replace();
        frame(&mut a, vec![]);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        assert_eq!(a.preplace.id, 0);
        assert!(!a.preplace.busy);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("project-replace-query")))
        );
    }
    #[test]
    fn same_frame_input_and_apply_shortcut_do_not_apply_the_previous_review() {
        let (_d, mut a) = fixture();
        preview(&mut a);
        frame(&mut a, vec![]);
        let hash = a.doc.content_hash();
        frame(
            &mut a,
            vec![
                press(Key::A, egui::Modifiers::CTRL | egui::Modifiers::COMMAND),
                egui::Event::Paste("new replacement".into()),
                press(Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::COMMAND),
            ],
        );
        assert_eq!(a.preplace.replacement, "new replacement");
        assert_eq!(a.preplace.applied, 0);
        assert!(!a.replace_review_ready());
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn cancel_returns_to_the_actual_replace_button_for_keyboard_reopen() {
        let (_d, mut a) = fixture();
        a.psearch.open = true;
        frame(&mut a, vec![]);
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        let p = a.chrome.hits["ps-replace"].center();
        let click = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &mut a,
            vec![egui::Event::PointerMoved(p), click(true), click(false)],
        );
        assert!(a.preplace.dialog);
        frame(&mut a, vec![press(Key::Escape, egui::Modifiers::NONE)]);
        assert!(!a.preplace.dialog);
        frame(&mut a, vec![press(Key::Enter, egui::Modifiers::NONE)]);
        assert!(
            a.preplace.dialog,
            "Cancel must restore the clicked Replace button, not the earlier source focus"
        );
    }
    #[test]
    fn full_regex_diagnostic_stays_in_details_not_the_global_status_bar() {
        let (_d, mut a) = fixture();
        a.psearch.options.query = "(".repeat(1800);
        a.psearch.options.regex = true;
        a.show_project_replace();
        a.preview_project_replace();
        wait(&mut a);
        assert!(a.preplace.error.contains("regex"));
        assert!(a.preplace.error.len() > 1800);
        assert!(!a.status.contains('\n'));
        assert!(a.status.len() < 160);
        a.psearch.options.query = "needle".into();
        a.preview_project_replace();
        wait(&mut a);
        assert!(a.status.starts_with("Preview ready"));
        assert!(!a.status.contains("regex"));
    }
}
