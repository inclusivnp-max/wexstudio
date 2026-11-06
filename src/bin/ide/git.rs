//! Local Git review: explicit saves, keyboard/list ownership and bounded external refresh.
use super::*;
use std::{collections::BTreeSet, sync::Arc, time::Instant};
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    vcs::{self, Reply, Snapshot, Task, Worker},
};
#[path = "git_branches.rs"]
mod branches;
const LIST: &str = "git-file-list";
const REFRESH: Duration = Duration::from_secs(3);
#[derive(Default)]
pub(super) struct GitUi {
    pub open: bool,
    pub(super) branch_ui: branches::BranchUi,
    pub busy: bool,
    pub snapshot: Option<Arc<Snapshot>>,
    pub selected: Option<usize>,
    pub path: Option<PathBuf>,
    pub staged: bool,
    pub diff: String,
    diff_lines: Vec<String>,
    mutating: bool,
    close_pending: bool,
    open_pending: bool,
    reload_pending: bool,
    pub saving: Option<Vec<PathBuf>>,
    save_done: Option<bool>,
    save_after_read: Option<Vec<PathBuf>>,
    pub message: String,
    pub note: String,
    pub error: String,
    pub stale: bool,
    pub mutations: usize,
    pub observations: usize,
    pub observed_changes: usize,
    pub saved: usize,
    pub opened: usize,
    worker: Worker,
    return_focus: Option<egui::Id>,
    focus_message: bool,
    focus_list: bool,
    committing: bool,
    observing: bool,
    pub(super) details: bool,
    checked: Option<Instant>,
    reveal: bool,
    list_height: f32,
    row_snapshot: Option<Arc<Snapshot>>,
    row_staged: bool,
    row_dirty: BTreeSet<PathBuf>,
    rows: Arc<Vec<PathBuf>>,
    handoff: Option<(PathBuf, u64, Vec<egui::Event>)>,
    open_input: Vec<egui::Event>,
    row_click: Option<(PathBuf, egui::Pos2, f64)>,
}
impl Workbench {
    pub(super) fn git_transaction_active(&self) -> bool {
        self.git.busy && self.git.mutating
    }
    pub(super) fn git_available(&self) -> bool {
        !self.editor_blocked()
            && !self.boot.pending
            && !self.welcome
            && !self.building
            && !self.running
            && self.save_ui.pending.is_none()
            && self.open_ui.pending.is_none()
            && !self.file_ui.busy
            && !self.refactor.busy
            && !self.preview.busy
    }
    pub(super) fn show_git(&mut self, origin: Option<egui::Id>) {
        if !self.git_available() {
            self.status =
                "Finish the current file, Save or build operation before Git review".into();
            return;
        }
        self.ime_cancel_external();
        self.cancel_deferred_input();
        self.edit.cancel_composition();
        self.terminal.focus = false;
        self.editor_ui.focus_editor = false;
        self.git.open = true;
        self.git.row_click = None;
        self.git.return_focus = origin;
        self.git.focus_list = true;
        self.git_refresh();
    }
    fn git_start(&mut self, task: Task, note: &str) {
        let mutating = matches!(&task, Task::Stage(..) | Task::Commit(..) | Task::Branch(..));
        let ctx = self.context.clone();
        if self
            .git
            .worker
            .start(task, Arc::new(move || ctx.request_repaint()))
        {
            self.git.busy = true;
            self.git.mutating = mutating;
            self.git.note = note.into();
            self.git.error.clear();
            self.git.details = false;
        }
    }
    fn git_observe(&mut self, force: bool) {
        if self.git.branch_ui.open {
            self.git_start(Task::Branches(self.root.clone()), "Reading local branches…");
            return;
        }
        let Some(s) = self.git.snapshot.clone() else {
            self.git_start(Task::Refresh(self.root.clone()), "Reading repository…");
            return;
        };
        let path = self
            .git
            .path
            .as_ref()
            .and_then(|p| p.strip_prefix(&s.root).ok())
            .map(PathBuf::from);
        self.git.observing = !force;
        self.git_start(
            Task::Observe(s, path, self.git.staged, force),
            if force {
                "Refreshing selected review…"
            } else {
                "Checking outside changes…"
            },
        );
    }
    pub(super) fn git_refresh(&mut self) {
        if self.git.close_pending || self.git.saving.is_some() || self.git.mutating {
            return;
        }
        self.git.stale = true;
        if self.git.busy {
            self.git.worker.cancel();
            self.git.reload_pending = true;
        } else {
            self.git_observe(true);
        }
    }
    // Absolute path is the selection identity. Porcelain row indices can change outside the IDE.
    pub(super) fn git_flow_paths(&self) -> Vec<PathBuf> {
        self.git_paths()
    }
    fn git_paths(&self) -> Vec<PathBuf> {
        let Some(s) = &self.git.snapshot else {
            return vec![];
        };
        let mut out = s
            .entries
            .iter()
            .filter(|e| {
                if self.git.staged {
                    e.staged()
                } else {
                    e.unstaged()
                }
            })
            .map(|e| s.root.join(&e.path))
            .collect::<Vec<_>>();
        if !self.git.staged {
            let seen = out.iter().cloned().collect::<BTreeSet<_>>();
            let mut dirty = self
                .parked
                .iter()
                .filter(|(_, (d, _))| d.dirty())
                .map(|(p, _)| p.clone())
                .collect::<BTreeSet<_>>();
            if self.doc.dirty() {
                dirty.extend(self.doc.path.iter().cloned());
            }
            out.extend(
                dirty
                    .into_iter()
                    .filter(|p| p.starts_with(&s.root) && !seen.contains(p)),
            );
        }
        out
    }
    fn git_row_paths(&mut self) -> Arc<Vec<PathBuf>> {
        let mut dirty = self
            .parked
            .iter()
            .filter(|(_, (d, _))| d.dirty())
            .map(|(p, _)| p.clone())
            .collect::<BTreeSet<_>>();
        if self.doc.dirty() {
            dirty.extend(self.doc.path.iter().cloned());
        }
        let same = match (&self.git.row_snapshot, &self.git.snapshot) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same || self.git.row_staged != self.git.staged || dirty != self.git.row_dirty {
            self.git.rows = Arc::new(self.git_paths());
            self.git.row_snapshot = self.git.snapshot.clone();
            self.git.row_staged = self.git.staged;
            self.git.row_dirty = dirty;
        }
        self.git.rows.clone()
    }
    fn git_set_path(&mut self, path: PathBuf) {
        if self.git.close_pending || self.git.mutating || self.git.saving.is_some() {
            return;
        }
        self.git.path = Some(path.clone());
        self.git.reveal = true;
        self.git.diff.clear();
        self.git.diff_lines.clear();
        self.git.selected = self.git.snapshot.as_ref().and_then(|s| {
            s.entry_index(&path).filter(|&i| {
                if self.git.staged {
                    s.entries[i].staged()
                } else {
                    s.entries[i].unstaged()
                }
            })
        });
        if self.git.busy {
            self.git.stale = true;
            self.git.worker.cancel();
            self.git.reload_pending = true;
        } else if self.git.stale {
            self.git_observe(true);
        } else if let (Some(s), Some(i)) = (self.git.snapshot.clone(), self.git.selected) {
            self.git_start(Task::Diff(s, i, self.git.staged), "Reading selected diff…");
        } else {
            self.git.note =
                "Unsaved editor content only · Save selected, then review its disk diff".into();
        }
    }
    pub(super) fn git_select(&mut self, i: usize) {
        if let Some(path) = self
            .git
            .snapshot
            .as_ref()
            .and_then(|s| s.entries.get(i).map(|e| s.root.join(&e.path)))
        {
            self.git_set_path(path);
        }
    }
    fn git_dirty_selected(&self) -> Vec<PathBuf> {
        let Some(path) = &self.git.path else {
            return vec![];
        };
        let mut paths = BTreeSet::from([path.clone()]);
        if let (Some(s), Some(i)) = (&self.git.snapshot, self.git.selected)
            && let Some(p) = &s.entries[i].original
        {
            paths.insert(s.root.join(p));
        }
        paths.into_iter().filter(|p| self.dirty_path(p)).collect()
    }
    pub(super) fn git_stage(&mut self, undo: bool) {
        if self.git.busy || self.git.stale || self.git.saving.is_some() {
            return;
        }
        let (Some(s), Some(i)) = (self.git.snapshot.clone(), self.git.selected) else {
            return;
        };
        if !undo && !self.git_dirty_selected().is_empty() {
            self.git.error = "Unsaved editor changes are kept. Save selected, then review its disk diff before staging.".into();
            return;
        }
        self.git_start(
            Task::Stage(s, i, undo),
            if undo {
                "Unstaging selected file…"
            } else {
                "Staging reviewed disk version…"
            },
        );
    }
    pub(super) fn git_save_selected(&mut self) {
        if self.git.mutating || self.git.saving.is_some() || !self.git.open {
            return;
        }
        let paths = self.git_dirty_selected();
        if paths.is_empty() {
            return;
        }
        self.git.saving = Some(paths.clone());
        self.git.stale = true;
        self.git.error.clear();
        if self.git.busy {
            self.git.worker.cancel();
            self.git.save_after_read = Some(paths);
            self.git.reload_pending = false;
            self.git.open_pending = false;
            self.git.note = "Preparing explicit selected Save…".into();
        } else {
            self.git_accept_save(paths);
        }
    }
    fn git_accept_save(&mut self, paths: Vec<PathBuf>) {
        self.git.note = "Saving selected editor snapshots only · nothing is staged…".into();
        self.request_save_paths(paths.clone(), save_ui::After::Git(paths));
        if self.save_ui.pending.is_none() {
            self.git_save_finished(false);
        }
    }
    pub(super) fn git_save_finished(&mut self, succeeded: bool) {
        self.git.saving = None;
        self.git.save_done = Some(succeeded);
        if !succeeded {
            self.git.error = self.save_ui.error.clone().unwrap_or_else(|| {
                "Save stopped · completed writes are kept; Refresh to inspect disk".into()
            });
            self.git.stale = true;
        } else {
            self.git.saved += 1;
        }
        self.context.request_repaint();
    }
    pub(super) fn git_commit(&mut self) {
        if self.git.busy
            || self.git.stale
            || self.git.saving.is_some()
            || self.git.message.trim().is_empty()
        {
            return;
        }
        let Some(s) = self.git.snapshot.clone() else {
            return;
        };
        if !s.entries.iter().any(vcs::Entry::staged) {
            return;
        }
        self.git.committing = true;
        self.git_start(
            Task::Commit(s, self.git.message.clone()),
            "Committing staged files · hooks/signing may take a moment…",
        );
    }
    pub(super) fn git_step(&mut self, ctx: &egui::Context) {
        if Self::consume_exact_key(ctx, egui::Modifiers::CTRL | egui::Modifiers::SHIFT, Key::G)
            || Self::consume_exact_key(
                ctx,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                Key::G,
            )
        {
            self.show_git(None);
        }
        if let Some(reply) = self.git.worker.poll() {
            self.git.busy = false;
            self.git.mutating = false;
            self.git.checked = Some(Instant::now());
            if std::mem::take(&mut self.git.close_pending) {
                self.git.stale = true;
                self.git_close(ctx);
                return;
            }
            if let Some(paths) = self.git.save_after_read.take() {
                self.git_accept_save(paths);
                return;
            }
            if std::mem::take(&mut self.git.open_pending) {
                self.git_open_selected(ctx);
                return;
            }
            if std::mem::take(&mut self.git.reload_pending) {
                self.git_observe(true);
                return;
            }
            let committing = std::mem::take(&mut self.git.committing);
            let observing = std::mem::take(&mut self.git.observing);
            match reply {
                Ok(Reply::Branches(c, note, changed)) => {
                    self.git_branch_received(c);
                    self.git.note = note;
                    self.git.mutations += usize::from(changed);
                }
                Ok(Reply::Unchanged) => {
                    self.git.observations += 1;
                    self.git.note = "Up to date · editor buffers and message kept".into();
                }
                Ok(Reply::Observed(review)) => {
                    let path = self.git.path.clone();
                    self.git.snapshot = Some(review.snapshot);
                    self.git.selected = review.selected;
                    self.git.diff = review.text;
                    self.git.diff_lines = review.lines;
                    self.git.stale = false;
                    if observing {
                        self.git.observations += 1;
                        self.git.observed_changes += 1;
                    }
                    if path.as_ref().is_some_and(|p| self.git_paths().contains(p)) {
                        self.git.path = path;
                        // Explicit Refresh/Save may move an unsaved-only row into
                        // Git's tracked section. Keep that chosen file visible.
                        // Idle observations preserve the user's existing scroll.
                        self.git.reveal |= !observing;
                    } else {
                        self.git.path = None;
                        self.git.selected = None;
                    }
                    self.git.note =
                        "Review updated · editor buffers and commit message kept".into();
                }
                Ok(Reply::Status(s)) => {
                    self.git.snapshot = Some(s);
                    self.git.stale = false;
                    self.git.selected = None;
                    self.git.path = None;
                    self.git.diff.clear();
                    self.git.diff_lines.clear();
                    self.git.note =
                        "Choose a file · X = staged, Y = disk · outside changes refresh every3s"
                            .into();
                }
                Ok(Reply::Diff(d, lines)) => {
                    self.git.diff = d;
                    self.git.diff_lines = lines;
                    self.git.note = "Diff ready · editor buffers are unchanged".into();
                }
                Ok(Reply::Changed(s, n)) => {
                    self.git.snapshot = Some(s);
                    self.git.stale = false;
                    self.git.selected = None;
                    self.git.path = None;
                    self.git.diff.clear();
                    self.git.diff_lines.clear();
                    self.git.note = n;
                    self.git.mutations += 1;
                    if committing {
                        self.git.message.clear();
                    }
                }
                Err(e) => {
                    self.git.error = e;
                    self.git.stale = true;
                    self.git.note = "Operation did not finish · Refresh actual Git state".into();
                }
            }
        }
        if let Some(ok) = self.git.save_done.take() {
            if std::mem::take(&mut self.git.close_pending) {
                self.git_close(ctx);
                return;
            }
            if ok {
                self.git_observe(true);
            }
        }
        if self.git.open
            && !self.git.busy
            && !self.git.stale
            && self.git.error.is_empty()
            && self.git.saving.is_none()
            && self.git.checked.is_some_and(|t| t.elapsed() >= REFRESH)
        {
            self.git_observe(false);
        }
        if self.git.open {
            ctx.request_repaint_after(if self.git.busy || self.git.saving.is_some() {
                Duration::from_millis(25)
            } else {
                Duration::from_millis(250)
            });
        }
    }
    fn git_close(&mut self, ctx: &egui::Context) {
        self.git.row_click = None;
        self.git.open_input.clear();
        self.git.open_pending = false;
        self.git.reload_pending = false;
        if self.git.save_after_read.take().is_some() {
            self.git.saving = None;
            self.git.worker.cancel();
            self.git.close_pending = true;
            return;
        }
        if self.git.saving.is_some() {
            self.stop_remaining_saves();
            self.git.close_pending = true;
            self.git.note = "Stopping remaining Save work…".into();
            return;
        }
        if self.git.busy {
            if !self.git.mutating {
                self.git.worker.cancel();
                self.git.close_pending = true;
                self.git.note = "Stopping Git read…".into();
            }
            return;
        }
        self.ime_cancel_external();
        self.git.open = false;
        if let Some(id) = self.git.return_focus {
            ctx.memory_mut(|m| m.request_focus(id));
        } else {
            self.editor_ui.focus_editor = true;
        }
    }
    fn git_open_selected(&mut self, ctx: &egui::Context) {
        if self.git.close_pending || self.git.mutating || self.git.saving.is_some() {
            self.git.open_input.clear();
            return;
        }
        let Some(path) = self.git.path.clone() else {
            self.git.open_input.clear();
            return;
        };
        if self.git.busy {
            self.git.worker.cancel();
            self.git.open_pending = true;
            return;
        }
        let following = std::mem::take(&mut self.git.open_input);
        self.git_close(ctx);
        self.editor_ui.focus_editor = true;
        if self.open_file(path.clone()) {
            self.git.opened += 1;
            if !following.is_empty() {
                self.git.handoff = Some((path, self.open_ui.navigation, following));
            }
        }
    }
    fn git_keys(&mut self, ctx: &egui::Context, paths: &[PathBuf]) {
        let events = ctx.input(|i| i.events.clone());
        let mut consumed = BTreeSet::new();
        let mut open_at = None;
        for (at, event) in events.iter().enumerate() {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            self.git.row_click = None;
            if *key == Key::F6 && modifiers.matches_exact(egui::Modifiers::NONE) {
                consumed.insert(at);
                self.ime_cancel_external();
                if ctx.memory(|m| m.has_focus(egui::Id::new(LIST))) {
                    self.git.focus_message = true;
                    self.git.focus_list = false;
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new("git-message")));
                } else {
                    self.git.focus_list = true;
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new(LIST)));
                }
                continue;
            }
            if !ctx.memory(|m| m.has_focus(egui::Id::new(LIST)))
                || self.git.mutating
                || self.git.saving.is_some()
            {
                continue;
            }
            if modifiers.matches_exact(egui::Modifiers::NONE) {
                if *key == Key::Enter {
                    consumed.insert(at);
                    open_at = Some(at);
                    break;
                }
                if matches!(
                    key,
                    Key::ArrowUp
                        | Key::ArrowDown
                        | Key::PageUp
                        | Key::PageDown
                        | Key::Home
                        | Key::End
                ) {
                    consumed.insert(at);
                    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                    if paths.is_empty() {
                        continue;
                    }
                    let old = self
                        .git
                        .path
                        .as_ref()
                        .and_then(|p| paths.iter().position(|x| x == p));
                    let page = (self.git.list_height / 44.).floor().max(1.) as usize;
                    let n = match key {
                        Key::Home => 0,
                        Key::End => paths.len() - 1,
                        Key::ArrowUp => old.unwrap_or(1).saturating_sub(1),
                        Key::ArrowDown => old.map_or(0, |n| n + 1),
                        Key::PageUp => old.unwrap_or(page).saturating_sub(page),
                        Key::PageDown => old.map_or(0, |n| n + page),
                        _ => unreachable!(),
                    }
                    .min(paths.len() - 1);
                    self.git_set_path(paths[n].clone());
                    self.git.focus_list = true;
                }
            } else if *key == Key::S
                && (modifiers.matches_exact(egui::Modifiers::CTRL)
                    || modifiers.matches_exact(egui::Modifiers::COMMAND))
            {
                consumed.insert(at);
                self.git_save_selected();
            }
        }
        let mut following = vec![];
        if let Some(at) = open_at {
            for (i, e) in events.iter().enumerate().skip(at + 1) {
                if matches!(
                    e,
                    egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Key { .. }
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Ime(_)
                ) {
                    following.push(e.clone());
                    consumed.insert(i);
                }
            }
        }
        ctx.input_mut(|i| {
            let mut at = 0;
            i.events.retain(|_| {
                let keep = !consumed.contains(&at);
                at += 1;
                keep
            });
        });
        if open_at.is_some() {
            self.git.open_input = following;
            self.git_open_selected(ctx);
        }
    }
    pub(super) fn git_handoff_input(&mut self, input: &mut egui::RawInput) {
        let Some((path, navigation, events)) = self.git.handoff.take() else {
            return;
        };
        if navigation != self.open_ui.navigation || self.git.open || self.open_ui.error.is_some() {
            return;
        }
        if self.open_ui.pending.is_some() {
            self.git.handoff = Some((path, navigation, events));
            return;
        }
        if self.doc.path.as_ref() == Some(&path) {
            let mut queued = events;
            queued.append(&mut input.events);
            input.events = queued;
            self.editor_ui.focus_editor = true;
        }
    }
    pub(super) fn git_dialog(&mut self, ctx: &egui::Context) {
        if !self.git.open {
            return;
        }
        if self.git.branch_ui.open {
            self.git_branches_dialog(ctx);
            return;
        }
        if !ctx.input(|i| i.focused) {
            self.git.row_click = None;
        }
        if ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
                self.chrome
                    .hits
                    .get("git-list-viewport")
                    .is_some_and(|r| r.contains(p))
            })
        {
            self.git.row_click = None;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
            && !self.git.mutating
        {
            self.git_close(ctx);
            return;
        }
        if ctx.memory(|m| m.has_focus(egui::Id::new("git-message"))) {
            self.git.focus_list = false;
        }
        if self.git.focus_list {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new(LIST)));
            // Keep the request through the modal sizing pass; the actual list consumes it.
        }
        let paths = self.git_row_paths();
        self.git_keys(ctx, &paths);
        if !self.git.open {
            return;
        }
        let width = (ctx.content_rect().width() - 80.).clamp(600., 1000.);
        let extra = if self.git.error.is_empty() {
            0.
        } else if self.git.details {
            70.
        } else {
            24.
        };
        let body = (ctx.content_rect().height() - 430. - extra).clamp(60., 390.);
        if body < self.git.list_height
            && self
                .chrome
                .hits
                .get("git-selected-row")
                .is_some_and(|selected| {
                    self.chrome
                        .hits
                        .get("git-list-viewport")
                        .is_some_and(|view| view.contains_rect(*selected))
                })
        {
            // Preserve a visible selection through resize/expanded diagnostics,
            // but do not jump back if the user deliberately scrolled away.
            self.git.reveal = true;
        }
        self.git.list_height = body;
        egui::Modal::new(egui::Id::new("git-review")).frame(m::card().inner_margin(20)).show(ctx, |ui| {
            ui.set_width(width);
            self.git_header(ui,width);
            let snapshot = self.git.snapshot.clone();
            ui.horizontal_top(|ui| {
                let left=(width*0.35).clamp(205.,320.);
                self.git_file_list(ui,ctx,&paths,left,body);
                ui.allocate_exact_size(egui::vec2(1.,body),egui::Sense::hover());
                let w=ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(w,body),egui::Layout::top_down(egui::Align::Min),|ui| {
                    ui.set_min_height(body);
ui.set_max_height(body);
ui.set_width(w);
                    egui::ScrollArea::both().id_salt(("git-diff",&self.git.path,self.git.staged)).min_scrolled_height(body).max_height(body).auto_shrink([false,false])
                        .show_rows(ui,17.,self.git.diff_lines.len().max(1),|ui,range| {
                            if self.git.diff_lines.is_empty() {
 ui.label(if !self.git_dirty_selected().is_empty(){
"Unsaved editor content is not in this disk diff. Save selected to review it."}
else if self.git.path.is_some(){
"No text diff (binary, mode-only, or unchanged)."}
else{
"Select a file to review its diff."}
);
 }
                            else {
 for n in range {
 let line=&self.git.diff_lines[n];
ui.label(RichText::new(line).monospace().size(12.).color(if line.starts_with('+'){
m::PRIMARY}
else if line.starts_with('-'){
m::ERROR}
else{
m::MUTED}
));
 }
 }
                        }
);
                }
);
            }
);
            let entry=snapshot.as_ref().and_then(|s|self.git.selected.and_then(|i|s.entries.get(i)));
            let dirty= !self.git_dirty_selected().is_empty();
            let ready= !self.git.busy&&!self.git.stale&&self.git.saving.is_none();
            ui.horizontal(|ui| {
                if self.chrome.button(ui,"git-stage","Stage file",Icon::Add,Kind::Tonal,ready&&!dirty&&entry.is_some_and(|e|e.unstaged()&&!e.conflict&&!e.submodule),126.,self.reduced_motion).on_hover_text_passive("Stage reviewed disk bytes only; Save selected explicitly first if unsaved").clicked(){
self.git_stage(false);
}
                if self.chrome.button(ui,"git-unstage","Unstage file",Icon::Close,Kind::Ghost,ready&&entry.is_some_and(|e|e.staged()&&!e.conflict&&!e.submodule),140.,self.reduced_motion).clicked(){
self.git_stage(true);
}
                if self.chrome.button(ui,"git-save","Save selected",Icon::Save,Kind::Tonal,!self.git.mutating&&self.git.saving.is_none()&&dirty,150.,self.reduced_motion).on_hover_text_passive("Explicitly save only this file's unsaved tabs, including a rename source; never stage or commit · Ctrl+S in files").clicked(){
self.git_save_selected();
}
                if self.chrome.button(ui,"git-open-file","Open file",Icon::Code,Kind::Ghost,!self.git.mutating&&self.git.saving.is_none()&&self.git.path.is_some(),125.,self.reduced_motion).on_hover_text_passive("Return to editor without saving; preserve dirty tabs and Undo · Enter in files").clicked(){
self.git_open_selected(ctx);
}
            }
);
            ui.label(RichText::new("Commit message · only the staged files will be included").small());
            let r=ui.add_enabled(!self.git.mutating&&self.git.saving.is_none(),egui::TextEdit::singleline(&mut self.git.message).id(egui::Id::new("git-message")).desired_width(width).char_limit(16*1024).hint_text("Describe this change…").margin(egui::vec2(10.,8.)));
            self.chrome.hits.insert("git-message".into(),r.rect);
            if self.git.focus_message {
 r.request_focus();
self.git.focus_message=false;
 }
            let status=if self.git.error.is_empty(){
&self.git.note}
else{
"Operation did not finish. Your edits and message are kept · Details / Refresh."}
;
            let(r,_)=ui.allocate_exact_size(egui::vec2(width,18.),egui::Sense::hover());
m::label_clipped(ui,r,status,11.,if self.git.error.is_empty(){
m::MUTED}
else{
m::ERROR}
);
            if !self.git.error.is_empty() {
                let r=ui.selectable_label(self.git.details,"Error details");
self.chrome.hits.insert("git-details".into(),r.rect);
                if r.clicked(){
self.git.details= !self.git.details;
}
                if self.git.details {
 egui::ScrollArea::vertical().id_salt("git-errors").min_scrolled_height(0.).max_height(45.).show(ui,|ui|{
ui.label(RichText::new(&self.git.error).small().color(m::ERROR));
}
);
 }
            }
            ui.horizontal(|ui| {
                let staged=snapshot.as_ref().is_some_and(|s|s.entries.iter().any(vcs::Entry::staged)&&!s.entries.iter().any(|e|e.conflict));
                if self.chrome.button(ui,"git-commit","Commit staged",Icon::Check,Kind::Filled,ready&&staged&&!self.git.message.trim().is_empty(),170.,self.reduced_motion).on_hover_text_passive("Create a local commit using normal hooks/signing; no push").clicked(){
self.git_commit();
}
                let label=if self.git.saving.is_some(){
"Stop Save"}
else if self.git.busy{
"Stop read"}
else{
"Close"}
;
                if self.chrome.button(ui,"git-close",label,Icon::Close,Kind::Ghost,!self.git.mutating,115.,self.reduced_motion).clicked(){
self.git_close(ctx);
}
                ui.label(RichText::new("F6 files / message · Enter opens file").small().color(m::MUTED));
            }
);
        }
);
    }
    fn git_header(&mut self, ui: &mut egui::Ui, width: f32) {
        ui.horizontal(|ui| {
            m::icon(
                ui,
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(24., 24.)),
                Icon::Git,
                m::PRIMARY,
            );
            ui.add_space(30.);
            ui.heading("Git changes");
            if let Some(s) = &self.git.snapshot {
                let (r, _) = ui.allocate_exact_size(
                    egui::vec2((width - 355.).max(80.), 24.),
                    egui::Sense::hover(),
                );
                m::label_clipped(ui, r, &s.branch, 14., m::PRIMARY);
            }
            let branch_button = self.chrome.button(
                ui,
                "git-branches",
                "Branches",
                Icon::Git,
                Kind::Tonal,
                self.git.snapshot.is_some()
                    && !self.git.busy
                    && !self.git.stale
                    && self.git.saving.is_none(),
                116.,
                self.reduced_motion,
            );
            if branch_button.clicked() {
                self.git.branch_ui.return_focus = Some(branch_button.id);
                self.git_branches_open();
            }
        });
        let root = self
            .git
            .snapshot
            .as_ref()
            .map(|s| s.root.as_path())
            .unwrap_or(&self.root);
        let (r, _) = ui.allocate_exact_size(egui::vec2(width, 18.), egui::Sense::hover());
        m::label_clipped(ui, r, &root.to_string_lossy(), 11., m::MUTED);
        ui.interact(r, ui.id().with("git-root"), egui::Sense::hover())
            .on_hover_text_passive(root.to_string_lossy());
        let dirty =
            usize::from(self.doc.dirty()) + self.parked.values().filter(|(d, _)| d.dirty()).count();
        ui.label(RichText::new(format!("{dirty} unsaved editor tabs · disk/index separate · nothing saved or staged automatically")).small().color(if dirty>0{
m::WARNING}
else{
m::MUTED}
));
        ui.horizontal(|ui| {
            if self
                .chrome
                .button(
                    ui,
                    "git-refresh",
                    "Refresh",
                    Icon::Refresh,
                    Kind::Tonal,
                    !self.git.mutating && self.git.saving.is_none(),
                    108.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.git_refresh();
            }
            for (key, label, value) in [
                ("git-working", "Working tree", false),
                ("git-staged", "Staged", true),
            ] {
                let r = ui.selectable_label(self.git.staged == value, label);
                self.chrome.hits.insert(key.into(), r.rect);
                if r.clicked() && !self.git.mutating && self.git.saving.is_none() {
                    self.git.staged = value;
                    self.git.path = None;
                    self.git.selected = None;
                    self.git.diff.clear();
                    self.git.diff_lines.clear();
                    if self.git.busy {
                        self.git_refresh();
                    }
                    self.git.focus_list = true;
                }
            }
            if self.git.busy || self.git.saving.is_some() {
                if self.reduced_motion {
                    ui.label("Working…");
                } else {
                    ui.spinner();
                }
            }
        });
        ui.add_space(4.);
    }
    fn git_file_list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        paths: &[PathBuf],
        left: f32,
        body: f32,
    ) {
        // A row outside the virtualized viewport has no current hit geometry.
        self.chrome.hits.remove("git-selected-row");
        ui.allocate_ui_with_layout(
            egui::vec2(left, body),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_height(body);
                ui.set_max_height(body);
                ui.set_max_width(left);
                ui.spacing_mut().item_spacing.y = 0.;
                let start = ui.cursor().min;
                let response = ui.interact(
                    egui::Rect::from_min_size(start, egui::vec2(left, body)),
                    egui::Id::new(LIST),
                    egui::Sense::focusable_noninteractive(),
                );
                if self.git.focus_list {
                    response.request_focus();
                    self.git.focus_list =
                        ui.is_sizing_pass() || !ctx.memory(|m| m.has_focus(egui::Id::new(LIST)));
                    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                }
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Git changed files")
                });
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt(("git-files", self.git.staged))
                    .min_scrolled_height(body)
                    .max_height(body)
                    .auto_shrink([false, false]);
                if std::mem::take(&mut self.git.reveal)
                    && let Some(n) = self
                        .git
                        .path
                        .as_ref()
                        .and_then(|p| paths.iter().position(|x| x == p))
                {
                    scroll =
                        scroll.vertical_scroll_offset((n as f32 * 44. + 22. - body * 0.5).max(0.));
                }
                let output = scroll.show_rows(ui, 44., paths.len().max(1), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.;
                    if paths.is_empty() {
                        ui.label(if self.git.snapshot.is_none(){
"Open a project inside an existing Git repository. Nothing is initialized automatically."}
else if self.git.staged{
"No staged files"}
else{
"Working tree and editor clean"}
);
                    }
                    for n in range.filter(|n| *n < paths.len()) {
                        let path = &paths[n];
                        let s = self.git.snapshot.as_ref().unwrap();
                        let i = s.entry_index(path);
                        let e = i.map(|i| &s.entries[i]);
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let dirty = self.dirty_path(path);
                        let status = e
                            .map(|e| format!("{}{}", e.index, e.worktree))
                            .unwrap_or_else(|| "—".into());
                        let label =
                            format!("{status}  {name}{}", if dirty { " · unsaved" } else { "" });
                        let (rect, _) = ui
                            .allocate_exact_size(egui::vec2(left - 14., 44.), egui::Sense::hover());
                        let r = ui.interact(
                            rect,
                            ui.id().with(("git-file", path)),
                            egui::Sense::click(),
                        );
                        if let Some(i) = i {
                            self.chrome.hits.insert(format!("git-file-{i}"), rect);
                        }
                        self.chrome
                            .hits
                            .insert(format!("git-buffer-{}", name), rect);
                        r.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                true,
                                self.git.path.as_ref() == Some(path),
                                label.clone(),
                            )
                        });
                        if self.git.path.as_ref() == Some(path) {
                            ui.painter().rect_filled(rect, 12, m::PRIMARY_CONTAINER);
                            if ctx.memory(|m| m.has_focus(egui::Id::new(LIST))) {
                                ui.painter().rect_stroke(
                                    rect.shrink(1.),
                                    12,
                                    egui::Stroke::new(2_f32, m::PRIMARY),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            self.chrome.hits.insert("git-selected-row".into(), rect);
                        }
                        m::label_clipped(
                            ui,
                            egui::Rect::from_min_size(
                                rect.min + egui::vec2(7., 2.),
                                egui::vec2(rect.width() - 14., 22.),
                            ),
                            &label,
                            13.,
                            if self.git.stale { m::MUTED } else { m::TEXT },
                        );
                        let parent = path
                            .strip_prefix(&s.root)
                            .unwrap_or(path)
                            .parent()
                            .unwrap_or(std::path::Path::new(""));
                        m::label_clipped(
                            ui,
                            egui::Rect::from_min_size(
                                rect.min + egui::vec2(7., 24.),
                                egui::vec2(rect.width() - 14., 16.),
                            ),
                            &parent.to_string_lossy(),
                            10.,
                            m::MUTED,
                        );
                        let tip = format!(
                            "{}{}",
                            path.display(),
                            e.and_then(|e| e.original.as_ref())
                                .map(|p| format!("\nRenamed from {}", p.display()))
                                .unwrap_or_default()
                        );
                        if r.clicked() && !self.git.mutating && self.git.saving.is_none() {
                            let double =
                                self.git_row_double_click(ctx, path, r.interact_pointer_pos());
                            self.ime_cancel_external();
                            if let Some(i) = i {
                                self.git_select(i);
                            } else {
                                self.git_set_path(path.clone());
                            }
                            ctx.memory_mut(|m| m.request_focus(egui::Id::new(LIST)));
                            if double {
                                self.git_open_selected(ctx);
                            }
                        }
                        m::passive_tooltip(&r, tip);
                    }
                });
                self.chrome
                    .hits
                    .insert("git-list-viewport".into(), output.inner_rect);
            },
        );
    }
    fn git_row_double_click(
        &mut self,
        ctx: &egui::Context,
        path: &std::path::Path,
        pos: Option<egui::Pos2>,
    ) -> bool {
        let Some(pos) = pos.filter(|_| ctx.input(|i| i.modifiers.is_none())) else {
            self.git.row_click = None;
            return false;
        };
        let now = ctx.input(|i| i.time);
        let (delay, distance) = ctx.options(|o| {
            (
                o.input_options.max_double_click_delay,
                o.input_options.max_click_dist,
            )
        });
        let previous = self.git.row_click.replace((path.to_owned(), pos, now));
        // egui's pointer click count is global and time-only: a quick click on
        // Refresh/another row can be reported as a double click here. Own the
        // path/position/time pair instead, including genuine rapid row doubles.
        previous.is_some_and(|(old, at, when)| {
            old == path && now > when && now - when <= delay && at.distance(pos) <= distance
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();

        let ctx = egui::Context::default();

        m::install(&ctx);

        let a = Workbench::new(d.path().into(), ctx, None, false);

        (d, a)
    }

    fn frame(a: &mut Workbench, events: Vec<egui::Event>) {
        let ctx = a.context.clone();

        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760., 560.),
                )),
                events,
                ..Default::default()
            },
            |ctx| a.git_dialog(ctx),
        );
    }

    #[test]
    fn minimum_git_footer_survives_long_diagnostics() {
        let (_d, mut a) = app();

        a.git.open = true;

        a.git.error = "long diagnostics\n".repeat(500);

        a.git.details = true;

        for _ in 0..3 {
            frame(&mut a, vec![]);
        }

        for k in [
            "git-stage",
            "git-unstage",
            "git-message",
            "git-commit",
            "git-close",
        ] {
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760., 560.))
                    .contains_rect(a.chrome.hits[k]),
                "{k} unreachable {:?}",
                a.chrome.hits[k]
            );
        }
    }

    #[test]
    fn escape_restores_actual_initiator_and_draft() {
        let (_d, mut a) = app();

        let id = egui::Id::new("actual-git-button");

        a.git.open = true;

        a.git.return_focus = Some(id);

        a.git.message = "draft kept".into();

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

        assert!(!a.git.open);

        assert_eq!(a.git.message, "draft kept");

        assert!(a.context.memory(|m| m.has_focus(id)));
    }

    #[test]
    fn busy_operation_does_not_close_or_discard_message() {
        let (_d, mut a) = app();

        a.git.open = true;

        a.git.busy = true;

        a.git.mutating = true;

        a.git.message = "draft kept".into();

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

        assert!(a.git.open);

        assert_eq!(a.git.message, "draft kept");
    }

    #[test]
    fn actual_project_button_escape_returns_focus_and_enter_reopens() {
        let (_d, mut a) = app();

        fn shell(a: &mut Workbench, events: Vec<egui::Event>) {
            let ctx = a.context.clone();

            let _ = ctx.run(
                egui::RawInput {
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let b = a.chrome.button(
                            ui,
                            "git-open",
                            "Git changes",
                            Icon::Git,
                            Kind::Tonal,
                            !a.editor_blocked(),
                            180.,
                            true,
                        );

                        if b.clicked() {
                            a.show_git(Some(b.id));
                        }
                    });

                    a.git_dialog(ctx);

                    a.git_step(ctx);
                },
            );
        }

        fn press(k: Key) -> egui::Event {
            egui::Event::Key {
                key: k,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }

        shell(&mut a, vec![]);

        let p = a.chrome.hits["git-open"].center();

        for pressed in [true, false] {
            shell(
                &mut a,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }

        assert!(a.git.open);

        let end = std::time::Instant::now() + Duration::from_secs(5);

        while a.git.busy {
            shell(&mut a, vec![]);

            assert!(std::time::Instant::now() < end);

            std::thread::sleep(Duration::from_millis(5));
        }

        let id = a.git.return_focus.unwrap();

        shell(&mut a, vec![press(Key::Escape)]);

        assert!(!a.git.open);

        assert!(
            a.context.memory(|m| m.has_focus(id)),
            "Escape lost the actual Project button focus"
        );

        shell(&mut a, vec![press(Key::Enter)]);

        assert!(
            a.git.open,
            "Enter must immediately reopen the initiating button"
        );
    }
    #[test]
    fn full_shell_resized_git_escape_returns_to_project_button_before_next_enter() {
        let (_d, mut a) = repo_app();
        a.git.open = false;
        fn shell(a: &mut Workbench, size: egui::Vec2, events: Vec<egui::Event>) {
            let ctx = a.context.clone();
            let _ = ctx.run(
                egui::RawInput {
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    a.git_step(ctx);
                    a.semantic_shortcuts(ctx);
                    a.editor_shortcuts(ctx);
                    a.debug_shortcuts(ctx);
                    a.draw_shell(ctx);
                    a.finish_editor_frame(ctx);
                },
            );
        }
        let large = egui::vec2(912., 1048.);
        let small = egui::vec2(760., 560.);
        for _ in 0..3 {
            shell(&mut a, large, vec![]);
        }
        let p = a.chrome.hits["git-open"].center();
        for pressed in [true, false] {
            shell(
                &mut a,
                large,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(a.git.open);
        wait_git(&mut a);
        a.git.error = "Long review details".repeat(20);
        a.git.details = true;
        for _ in 0..3 {
            shell(&mut a, small, vec![]);
        }
        a.git_set_path(a.root.join("Review.txt"));
        wait_git(&mut a);
        for _ in 0..3 {
            shell(&mut a, small, vec![]);
        }
        a.git_refresh();
        wait_git(&mut a);
        for _ in 0..3 {
            shell(&mut a, small, vec![]);
        }
        let source = a.doc.content_hash();
        let id = a.git.return_focus.unwrap();
        let released = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        shell(
            &mut a,
            small,
            vec![key_event(Key::Escape), released(Key::Escape)],
        );
        assert!(!a.git.open);
        assert!(
            a.context.memory(|m| m.has_focus(id)),
            "full shell lost originating Git button"
        );
        for _ in 0..8 {
            shell(&mut a, small, vec![]);
        }
        assert!(
            a.context.memory(|m| m.has_focus(id)),
            "initiating focus must survive idle frames before Enter"
        );
        shell(
            &mut a,
            small,
            vec![key_event(Key::Enter), released(Key::Enter)],
        );
        assert!(
            !a.git.open_pending,
            "opening Enter must not also open the previously selected file"
        );
        wait_git(&mut a);
        assert!(
            a.git.open,
            "full-shell Enter after Escape must reopen, not go to the source"
        );
        assert_eq!(a.doc.content_hash(), source);
    }

    #[test]
    fn enter_does_not_create_commit() {
        let (_d, mut a) = app();

        a.git.open = true;

        a.git.message = "draft".into();

        for _ in 0..3 {
            frame(&mut a, vec![]);
        }

        frame(
            &mut a,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            }],
        );

        assert_eq!(a.git.mutations, 0);

        assert!(!a.git.busy);
    }
    fn repo_app() -> (tempfile::TempDir, Workbench) {
        let (d, mut a) = app();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "Wex UX Fixture"],
            vec!["config", "user.email", "fixture@localhost"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "core.hooksPath", ".git/hooks"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(d.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        for (n, t) in [
            ("Main.kt", "val main=1\n"),
            ("Park.kt", "val park=2\n"),
            ("Review.txt", "before\n"),
            ("0-new.txt", "initial row\n"),
        ] {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "baseline"]] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(d.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(d.path().join("Review.txt"), "after\n").unwrap();
        a.doc = Document::open(d.path().join("Main.kt")).unwrap();
        a.doc.defer_syntax();
        a.doc.edit(0..0, "// dirty main\n").unwrap();
        let mut parked = Document::open(d.path().join("Park.kt")).unwrap();
        parked.defer_syntax();
        parked.edit(0..0, "// dirty park\n").unwrap();
        a.parked
            .insert(d.path().join("Park.kt"), (parked, Default::default()));
        a.tabs = vec![d.path().join("Main.kt"), d.path().join("Park.kt")];
        a.git.snapshot = Some(Arc::new(
            vcs::refresh(d.path(), &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        ));
        a.git.open = true;
        a.git.message = "draft kept".into();
        a.git.focus_list = true;
        (d, a)
    }
    fn key_event(k: Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn wait_git(a: &mut Workbench) {
        let end = Instant::now() + Duration::from_secs(10);
        while a.git.busy || a.git.saving.is_some() || a.git.save_done.is_some() {
            a.document_save_updates();
            let ctx = a.context.clone();
            a.git_step(&ctx);
            assert!(Instant::now() < end, "{} / {}", a.git.note, a.git.error);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn buffer_only_rows_exist_before_any_disk_write() {
        let (d, mut a) = repo_app();
        let paths = a.git_row_paths();
        assert!(
            paths.contains(&d.path().join("Main.kt")) && paths.contains(&d.path().join("Park.kt"))
        );
        a.git_set_path(d.path().join("Main.kt"));
        assert!(a.git.selected.is_none());
        assert!(!a.git_dirty_selected().is_empty());
        assert!(!a.save_ui.saver.worker_started());
        assert_eq!(
            std::fs::read_to_string(d.path().join("Main.kt")).unwrap(),
            "val main=1\n"
        );
    }
    #[test]
    fn selected_explicit_save_keeps_other_buffers_draft_and_undo_then_reviews_only() {
        let (d, mut a) = repo_app();
        a.git_set_path(d.path().join("Main.kt"));
        a.git_save_selected();
        assert!(a.git.saving.is_some());
        wait_git(&mut a);
        assert_eq!(a.git.saved, 1);
        assert_eq!(a.git.mutations, 0);
        assert!(!a.doc.dirty());
        assert!(a.parked[&d.path().join("Park.kt")].0.dirty());
        assert_eq!(
            std::fs::read_to_string(d.path().join("Park.kt")).unwrap(),
            "val park=2\n"
        );
        assert!(a.git.diff.contains("+// dirty main"));
        assert_eq!(a.git.message, "draft kept");
        assert!(a.doc.undo().unwrap());
        assert!(a.doc.dirty());
        assert_eq!(a.doc.rope().to_string(), "val main=1\n");
        assert!(a.doc.redo().unwrap());
        assert!(!a.doc.dirty());
    }
    #[test]
    fn selected_save_reveals_the_same_file_after_buffer_row_moves_into_git_status() {
        let (d, mut a) = repo_app();
        for n in 0..40 {
            std::fs::write(d.path().join(format!("List{n:02}.txt")), "untracked\n").unwrap();
        }
        a.git.snapshot = Some(Arc::new(
            vcs::refresh(d.path(), &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        ));
        let main = d.path().join("Main.kt");
        a.git_set_path(main.clone());
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        assert!(
            a.chrome.hits["git-list-viewport"].contains_rect(a.chrome.hits["git-selected-row"])
        );
        a.git_save_selected();
        wait_git(&mut a);
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        assert_eq!(a.git.path, Some(main));
        assert_eq!(a.git.saved, 1);
        assert!(a.git.diff.contains("+// dirty main"));
        let selected = a
            .chrome
            .hits
            .get("git-selected-row")
            .expect("saved file must remain visible");
        assert!(a.chrome.hits["git-list-viewport"].contains_rect(*selected));
    }
    #[test]
    fn middle_selected_row_is_whole_in_minimum_expanded_error_viewport() {
        let (d, mut a) = repo_app();
        for n in 0..40 {
            std::fs::write(d.path().join(format!("List{n:02}.txt")), "untracked\n").unwrap();
        }
        a.git.snapshot = Some(Arc::new(
            vcs::refresh(d.path(), &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        ));
        a.git_set_path(d.path().join("Main.kt"));
        a.git.error = "Keep selected file visible while reading error details".into();
        a.git.details = true;
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        let view = a.chrome.hits["git-list-viewport"];
        let selected = a.chrome.hits["git-selected-row"];
        assert_eq!(view.height(), 60.);
        assert_eq!(selected.height(), 44.);
        assert!(
            view.contains_rect(selected),
            "selected {selected:?} outside {view:?}"
        );
    }
    #[test]
    fn rapid_cross_widget_click_selects_only_and_same_row_double_click_opens() {
        let (d, mut a) = repo_app();
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        let click = |a: &mut Workbench, name: &str| {
            let p = a.chrome.hits[name].center();
            frame(
                a,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        };
        click(&mut a, "git-refresh");
        assert!(a.git.busy);
        wait_git(&mut a);
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        click(&mut a, "git-buffer-Main.kt");
        assert!(
            !a.git.open_pending,
            "first row click after another widget must only select"
        );
        wait_git(&mut a);
        assert!(a.git.open && a.git.opened == 0);
        frame(&mut a, vec![]);
        click(&mut a, "git-buffer-Review.txt");
        assert!(
            !a.git.open_pending,
            "rapid click on a different row must only select"
        );
        wait_git(&mut a);
        assert!(a.git.open && a.git.opened == 0);
        assert_eq!(a.git.path, Some(d.path().join("Review.txt")));
        frame(&mut a, vec![]);
        click(&mut a, "git-buffer-Review.txt");
        wait_git(&mut a);
        assert!(!a.git.open);
        assert_eq!(a.git.opened, 1);
    }
    #[test]
    fn shrinking_viewport_keeps_previously_visible_selection_and_message_focus() {
        let (d, mut a) = repo_app();
        for n in 0..40 {
            std::fs::write(d.path().join(format!("List{n:02}.txt")), "untracked\n").unwrap();
        }
        a.git.snapshot = Some(Arc::new(
            vcs::refresh(d.path(), &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        ));
        a.git_set_path(d.path().join("Park.kt"));
        let draw = |a: &mut Workbench, size: egui::Vec2| {
            let ctx = a.context.clone();
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ctx| a.git_dialog(ctx),
            );
        };
        for _ in 0..3 {
            draw(&mut a, egui::vec2(912., 1048.));
        }
        assert!(
            a.chrome.hits["git-list-viewport"].contains_rect(a.chrome.hits["git-selected-row"])
        );
        a.context
            .memory_mut(|m| m.request_focus(egui::Id::new("git-message")));
        a.git.error = "Selected file must stay visible during minimum error review".into();
        a.git.details = true;
        for _ in 0..3 {
            draw(&mut a, egui::vec2(760., 560.));
        }
        let selected = a
            .chrome
            .hits
            .get("git-selected-row")
            .expect("previously visible row lost after resize");
        assert!(a.chrome.hits["git-list-viewport"].contains_rect(*selected));
        assert_eq!(selected.height(), 44.);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("git-message")))
        );
    }
    #[test]
    fn external_save_conflict_keeps_buffer_message_and_stale_review() {
        let (d, mut a) = repo_app();
        a.git_set_path(d.path().join("Main.kt"));
        std::fs::write(d.path().join("Main.kt"), "external\n").unwrap();
        let hash = a.doc.content_hash();
        a.git_save_selected();
        wait_git(&mut a);
        assert!(!a.git.error.is_empty() && a.git.stale);
        assert_eq!(a.git.saved, 0);
        assert_eq!(a.git.message, "draft kept");
        assert_eq!(a.doc.content_hash(), hash);
        assert!(a.doc.dirty());
        assert_eq!(
            std::fs::read_to_string(d.path().join("Main.kt")).unwrap(),
            "external\n"
        );
    }
    #[test]
    fn opening_dirty_parked_file_never_saves_or_resets_view_and_history() {
        let (d, mut a) = repo_app();
        let path = d.path().join("Park.kt");
        let hash = a.parked[&path].0.content_hash();
        a.git_set_path(path.clone());
        a.git_open_selected(&a.context.clone());
        assert!(!a.git.open && a.editor_ui.focus_editor);
        assert_eq!(a.doc.path, Some(path));
        assert_eq!(a.doc.content_hash(), hash);
        assert!(a.doc.dirty());
        assert!(a.doc.undo().unwrap());
        assert_eq!(a.doc.rope().to_string(), "val park=2\n");
        assert!(!a.save_ui.saver.worker_started());
        assert!(a.parked[&d.path().join("Main.kt")].0.dirty());
    }
    #[test]
    fn list_keyboard_end_home_and_message_ownership_do_not_move_source() {
        let (d, mut a) = repo_app();
        let hash = a.doc.content_hash();
        frame(&mut a, vec![]);
        frame(&mut a, vec![key_event(Key::End)]);
        wait_git(&mut a);
        assert_eq!(a.git.path, Some(d.path().join("Park.kt")));
        assert!(a.context.memory(|m| m.has_focus(egui::Id::new(LIST))));
        frame(&mut a, vec![key_event(Key::F6)]);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("git-message")))
        );
        let path = a.git.path.clone();
        frame(&mut a, vec![key_event(Key::ArrowUp), key_event(Key::Enter)]);
        assert!(a.git.open);
        assert_eq!(a.git.path, path);
        assert_eq!(a.doc.content_hash(), hash);
        frame(&mut a, vec![key_event(Key::F6)]);
        frame(&mut a, vec![key_event(Key::Home)]);
        wait_git(&mut a);
        assert_eq!(a.git.path, Some(d.path().join("Review.txt")));
    }
    #[test]
    fn background_review_keeps_path_and_message_focus_when_indices_change() {
        let (d, mut a) = repo_app();
        a.git_set_path(d.path().join("Review.txt"));
        wait_git(&mut a);
        frame(&mut a, vec![]);
        a.context
            .memory_mut(|m| m.request_focus(egui::Id::new("git-message")));
        std::fs::write(d.path().join("0-new.txt"), "new row\n").unwrap();
        std::fs::write(d.path().join("Review.txt"), "external after\n").unwrap();
        a.git_observe(false);
        wait_git(&mut a);
        frame(&mut a, vec![]);
        assert_eq!(a.git.path, Some(d.path().join("Review.txt")));
        assert!(a.git.diff.contains("+external after"));
        assert_eq!(a.git.message, "draft kept");
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("git-message")))
        );
        assert_eq!(a.git.observed_changes, 1);
    }
    #[test]
    fn open_availability_matches_save_build_and_modal_feedback() {
        let (_d, mut a) = repo_app();
        a.git.open = false;
        assert!(a.git_available());
        a.building = true;
        assert!(!a.git_available());
        a.show_git(None);
        assert!(!a.git.open);
        assert!(a.status.contains("Finish"));
        a.building = false;
        a.git.open = true;
        assert!(!a.git_available());
    }
    #[test]
    fn cancelling_read_clears_competing_navigation_intents() {
        let (_d, mut a) = repo_app();
        a.git.busy = true;
        a.git.reload_pending = true;
        a.git.open_pending = true;
        a.git_close(&a.context.clone());
        assert!(a.git.close_pending);
        assert!(!a.git.reload_pending && !a.git.open_pending);
    }
    #[test]
    fn same_frame_navigation_keeps_end_then_up_order() {
        let (d, mut a) = repo_app();
        frame(&mut a, vec![]);
        frame(&mut a, vec![key_event(Key::End), key_event(Key::ArrowUp)]);
        wait_git(&mut a);
        assert_eq!(a.git.path, Some(d.path().join("Main.kt")));
    }
    #[test]
    fn open_following_input_waits_for_exact_navigation_and_never_routes_elsewhere() {
        let (d, mut a) = repo_app();
        let path = d.path().join("Park.kt");
        a.git_set_path(path.clone());
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![key_event(Key::Enter), egui::Event::Text("FOLLOWING".into())],
        );
        assert!(!a.git.open);
        let mut input = egui::RawInput::default();
        a.git_handoff_input(&mut input);
        assert!(
            input
                .events
                .iter()
                .any(|e| matches!(e,egui::Event::Text(s)if s=="FOLLOWING"))
        );
        assert!(a.git.handoff.is_none());
        a.git.handoff = Some((
            path,
            a.open_ui.navigation.wrapping_sub(1),
            vec![egui::Event::Text("STALE".into())],
        ));
        let mut wrong = egui::RawInput::default();
        a.git_handoff_input(&mut wrong);
        assert!(wrong.events.is_empty());
    }
    #[test]
    fn explicit_save_during_read_is_queued_for_original_selected_paths() {
        let (d, mut a) = repo_app();
        a.git_set_path(d.path().join("Main.kt"));
        a.git_observe(false);
        assert!(a.git.busy);
        a.git_save_selected();
        assert!(a.git.saving.is_some());
        wait_git(&mut a);
        assert_eq!(a.git.saved, 1);
        assert_eq!(a.git.mutations, 0);
        assert!(!a.doc.dirty());
        assert!(a.parked[&d.path().join("Park.kt")].0.dirty());
    }
    #[test]
    fn stop_queued_save_before_writer_starts_keeps_disk_and_draft() {
        let (d, mut a) = repo_app();
        a.git_set_path(d.path().join("Main.kt"));
        a.git_observe(false);
        a.git_save_selected();
        a.git_close(&a.context.clone());
        wait_git(&mut a);
        assert!(!a.git.open);
        assert!(!a.save_ui.saver.worker_started());
        assert!(a.doc.dirty());
        assert_eq!(a.git.message, "draft kept");
        assert_eq!(
            std::fs::read_to_string(d.path().join("Main.kt")).unwrap(),
            "val main=1\n"
        );
    }
    fn branch_app() -> (tempfile::TempDir, Workbench) {
        let (d, mut a) = repo_app();
        assert!(
            std::process::Command::new("git")
                .current_dir(d.path())
                .args(["branch", "feature/猫"])
                .status()
                .unwrap()
                .success()
        );
        let c = Arc::new(
            vcs::branches::read(d.path(), &std::sync::atomic::AtomicBool::new(false)).unwrap(),
        );
        a.git.branch_ui.open = true;
        a.git_branch_received(c);
        a.git.branch_ui.selected = Some("feature/猫".into());
        (d, a)
    }
    #[test]
    fn branch_minimum_confirm_and_long_error_keep_all_controls_reachable() {
        let (_d, mut a) = branch_app();
        a.git.branch_ui.confirm = true;
        a.git.error = "long branch error\n".repeat(500);
        a.git.details = true;
        a.git.branch_ui.name = "feature/".to_owned() + &"long".repeat(1000);
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760., 560.));
        for key in [
            "git-branch-filter",
            "git-branch-name",
            "git-branch-create",
            "git-branch-switch",
            "git-branch-refresh",
            "git-branch-back",
        ] {
            assert!(
                screen.contains_rect(a.chrome.hits[key]),
                "{key} outside {:?}",
                a.chrome.hits[key]
            );
        }
    }
    #[test]
    fn branch_switch_rechecks_dirty_active_and_parked_without_saving() {
        let (d, mut a) = branch_app();
        let c = a.git.branch_ui.catalog.clone().unwrap();
        let n = c.items.iter().position(|b| b.name == "feature/猫").unwrap();
        let disk = std::fs::read(d.path().join("Main.kt")).unwrap();
        let hash = a.doc.content_hash();
        a.git_branch_apply(vcs::branches::Action::Switch(n));
        assert!(!a.git.busy);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(std::fs::read(d.path().join("Main.kt")).unwrap(), disk);
        assert!(a.git.note.contains("Save or Undo"));
        a.doc.undo().unwrap();
        assert!(!a.doc.dirty());
        a.git_branch_apply(vcs::branches::Action::Switch(n));
        assert!(!a.git.busy);
        assert!(a.parked.values().any(|(d, _)| d.dirty()));
        assert!(!a.save_ui.saver.worker_started());
    }
    #[test]
    fn branch_create_keeps_unsaved_tabs_and_draft_then_back_keeps_focus() {
        let (d, mut a) = branch_app();
        let hash = a.doc.content_hash();
        a.git_branch_apply(vcs::branches::Action::Create("new-local".into()));
        wait_git(&mut a);
        assert!(a.git.error.is_empty(), "{}", a.git.error);
        assert_eq!(a.git.message, "draft kept");
        assert_eq!(a.doc.content_hash(), hash);
        assert!(a.doc.dirty());
        assert!(
            a.git
                .branch_ui
                .catalog
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|b| b.name == "new-local" && !b.current)
        );
        frame(&mut a, vec![key_event(Key::Escape)]);
        assert!(!a.git.branch_ui.open && a.git.open);
        wait_git(&mut a);
        assert_eq!(a.git.message, "draft kept");
        assert!(
            std::fs::read_to_string(d.path().join("Main.kt"))
                .unwrap()
                .starts_with("val main")
        );
    }
    #[test]
    fn branch_f6_list_arrows_and_enter_review_never_mutate_or_write_source() {
        let (_d, mut a) = branch_app();
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        frame(&mut a, vec![key_event(Key::F6)]);
        frame(&mut a, vec![key_event(Key::Home)]);
        frame(&mut a, vec![key_event(Key::Enter)]);
        assert!(a.git.branch_ui.confirm);
        assert!(!a.git.busy);
        assert_eq!(a.git.mutations, 0);
        assert_eq!(a.git.message, "draft kept");
        assert!(a.doc.dirty());
        frame(&mut a, vec![key_event(Key::F6)]);
        frame(&mut a, vec![]);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("git-branch-filter")))
        );
    }
    #[test]
    fn branch_ime_candidate_escape_and_commit_enter_do_not_leave_or_mutate() {
        let (_d, mut a) = branch_app();
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        a.context
            .memory_mut(|m| m.request_focus(egui::Id::new("git-branch-name")));
        frame(
            &mut a,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit("候補".into()))],
        );
        frame(&mut a, vec![key_event(Key::Escape)]);
        assert!(a.git.branch_ui.open);
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("猫".into())),
                key_event(Key::Enter),
            ],
        );
        assert!(a.git.branch_ui.open && !a.git.busy);
        assert_eq!(a.git.mutations, 0);
        assert_eq!(a.git.message, "draft kept");
    }
    #[test]
    fn branch_header_click_then_escape_returns_to_actual_branch_button() {
        let (_d, mut a) = branch_app();
        a.git.branch_ui.open = false;
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        let pos = a.chrome.hits["git-branches"].center();
        for pressed in [true, false] {
            frame(
                &mut a,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(a.git.branch_ui.open);
        let id = a.git.branch_ui.return_focus.unwrap();
        wait_git(&mut a);
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        frame(&mut a, vec![key_event(Key::Escape)]);
        wait_git(&mut a);
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        assert!(
            a.context.memory(|m| m.has_focus(id)),
            "{:?}",
            a.context.memory(|m| m.focused())
        );
        assert!(!a.git.branch_ui.open && a.git.open);
    }
    #[test]
    fn branch_small_catalog_shrinks_empty_space_instead_of_stretching_the_modal() {
        let (_d, mut a) = branch_app();
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        assert!((a.chrome.hits["git-branch-list-viewport"].height() - 68.).abs() < 0.1);
        assert!(a.chrome.hits["git-branch-back"].bottom() < 520.);
    }
}
