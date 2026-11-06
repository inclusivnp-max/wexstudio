//! Virtualized editor surface, shaped hit testing, expressive find and disk review.
use super::*;
use egui::{Align2, Pos2, Rect, Sense, Stroke, Vec2};
use std::{collections::BTreeMap, ops::Range, sync::Arc, time::Instant};
use wexandroidstudio::{
    editing::{self, line_length},
    editor_jobs::{DiskState, EditorEvent, EditorServices},
    editor_search::{SearchOptions, SearchResults},
    material::{self as m, ButtonKind as Kind, Icon},
};
pub(super) const ROW: f32 = 23.;
pub(super) const GUTTER: f32 = 58.;
#[derive(Default)]
pub(super) struct FindPanel {
    pub open: bool,
    pub expanded: bool,
    pub options: SearchOptions,
    pub replacement: String,
    pub results: SearchResults,
    submitted: Option<(u64, u64, SearchOptions)>,
    id: u64,
    revision: u64,
    epoch: u64,
    pending: bool,
    pub replacing: Option<u64>,
    pub current: Option<usize>,
    pub focus: bool,
    pub handoff: Vec<egui::Event>,
    pub handoff_ready: bool,
    captured: Option<Range<usize>>,
    changed: Option<Instant>,
}
impl FindPanel {
    pub fn submitted_for_handoff(&mut self) {
        self.submitted = None;
        self.results = Default::default();
        self.changed = Some(Instant::now());
    }
}
#[derive(Clone)]
enum DragUnit {
    Character,
    Word(Range<usize>),
    Line(Range<usize>),
}
struct Drag {
    unit: DragUnit,
    snapshot: wexandroidstudio::editor::RenderSnapshot,
    epoch: u64,
}
pub(super) struct Geometry {
    pub viewport: Rect,
    pub origin: Pos2,
    pub rows: BTreeMap<usize, (Pos2, Arc<egui::Galley>)>,
    pub assets: Vec<Arc<long_lines::Prepared>>,
    pub ime: Option<Rect>,
}
impl Default for Geometry {
    fn default() -> Self {
        Self {
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::ZERO),
            origin: Pos2::ZERO,
            rows: BTreeMap::new(),
            assets: Vec::new(),
            ime: None,
        }
    }
}
pub(super) struct EditorUi {
    pub frame_events: Option<std::collections::VecDeque<pointer_input::Ordered>>,
    pub frame_handled: Option<u64>,
    pub frame_editor_focus: bool,
    pub leased_pointer: Option<egui::PointerState>,
    pub long_lines: long_lines::LongLines,
    pub deferred_input: Option<long_lines::DeferredInput>,
    pub pointer_metrics: pointer_input::Metrics,
    pub pointer_layer: Option<egui::LayerId>,
    pub viewport_profile: viewport_smoke::Profile,
    pub text_mesh: wexandroidstudio::editor_mesh::Cache,
    pub input_id: Option<egui::Id>,
    pub find: FindPanel,
    pub services: EditorServices,
    pub conflicts: BTreeMap<PathBuf, DiskState>,
    pub pending_reload: Option<PathBuf>,
    pub(super) signature: Vec<(PathBuf, Option<blake3::Hash>)>,
    pub(super) generation: u64,
    pub epoch: u64,
    pub(super) focus_editor: bool,
    drag: Option<Drag>,
    last_click: Option<(Instant, Pos2, u8)>,
    pub geometry: Geometry,
    pub hits: BTreeMap<String, Rect>,
    pub rendered_rows: usize,
}
impl EditorUi {
    pub fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    pub fn new(root: PathBuf, ctx: egui::Context) -> Self {
        Self::with_inventory(root, ctx, true)
    }
    pub fn without_inventory(root: PathBuf, ctx: egui::Context) -> Self {
        Self::with_inventory(root, ctx, false)
    }
    fn with_inventory(root: PathBuf, ctx: egui::Context, inventory: bool) -> Self {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let services = if inventory {
            EditorServices::new(root, wake)
        } else {
            EditorServices::inert()
        };
        Self {
            frame_events: None,
            frame_handled: None,
            frame_editor_focus: false,
            leased_pointer: None,
            long_lines: Default::default(),
            deferred_input: None,
            pointer_metrics: Default::default(),
            pointer_layer: None,
            text_mesh: Default::default(),
            viewport_profile: Default::default(),
            input_id: None,
            find: Default::default(),
            services,
            conflicts: Default::default(),
            pending_reload: None,
            signature: vec![],
            generation: 0,
            epoch: 0,
            focus_editor: false,
            drag: None,
            last_click: None,
            geometry: Geometry::default(),
            hits: Default::default(),
            rendered_rows: 0,
        }
    }
}
impl Workbench {
    pub(super) fn did_edit(&mut self) {
        self.debug.bp_launch = None;
        if self.preview.open {
            self.preview.invalidate();
        }
        if self.debug.connection.is_some() {
            self.debug.source_changed = true;
            self.debug.execution = None;
        }
        self.last_edit = Instant::now();
        self.diagnostics.clear();
        self.completions.clear();
        self.scroll_to_cursor = true;
        self.edit.clamp(&self.doc);
    }
    pub(super) fn editor_switched(&mut self) {
        self.doc.defer_syntax();
        self.ime_document_changed();
        self.editor_ui.epoch += 1;
        self.editor_ui.services.cancel_search();
        self.editor_ui.find.submitted = None;
        self.editor_ui.find.results = Default::default();
        self.editor_ui.find.current = None;
        self.editor_ui.find.replacing = None;
        self.editor_ui.find.options.within = None;
        self.editor_ui.find.captured = None;
        self.cancel_deferred_input();
        self.editor_ui.focus_editor = !self.editor_ui.find.open;
        self.edit.clamp(&self.doc);
    }
    pub(super) fn sync_watches(&mut self) {
        let mut paths = self
            .parked
            .iter()
            .map(|(p, (d, _))| (p.clone(), d.disk_hash()))
            .collect::<Vec<_>>();
        if let Some(p) = self.doc.path.clone() {
            paths.push((p, self.doc.disk_hash()));
        }
        paths.sort_by(|a, b| a.0.cmp(&b.0));
        if paths != self.editor_ui.signature {
            self.editor_ui.signature = paths.clone();
            self.editor_ui.generation = self.editor_ui.services.watch(paths);
        }
    }
    pub(super) fn editor_updates(&mut self, ctx: &egui::Context) {
        self.sync_watches();
        for _ in 0..64 {
            let Some(event) = self.editor_ui.services.poll() else {
                break;
            };
            match event {
                EditorEvent::Tree { started, inventory } => {
                    if started >= self.file_ui.changed {
                        if inventory.error.is_none() {
                            self.files = inventory.sources();
                            self.file_ui.inventory = inventory;
                        } else {
                            self.file_ui.inventory.error = inventory.error;
                        }
                    }
                }
                EditorEvent::Disk {
                    generation,
                    path,
                    state,
                } => {
                    if self.save_ui.contains(&path) || generation != self.editor_ui.generation {
                        continue;
                    }
                    let active = self.doc.path.as_ref() == Some(&path);
                    let Some((dirty, expected)) = (if active {
                        Some((
                            self.doc.dirty() || self.edit.composition.is_some(),
                            self.doc.disk_hash(),
                        ))
                    } else {
                        self.parked
                            .get(&path)
                            .map(|(d, _)| (d.dirty(), d.disk_hash()))
                    }) else {
                        continue;
                    };
                    if let DiskState::Present(disk) = &state
                        && disk.disk_hash() == expected
                    {
                        self.editor_ui.conflicts.remove(&path);
                        continue;
                    }
                    if !dirty && let DiskState::Present(next) = state {
                        if self.debug.connection.is_some()
                            && self
                                .debug
                                .built_sources
                                .get(&path)
                                .is_some_and(|h| next.content_hash().to_hex().as_str() != h)
                        {
                            self.debug.source_changed = true;
                            self.debug.execution = None;
                        }
                        if active {
                            self.edit.rebase(&self.doc, &next);
                            self.doc = *next;
                            self.doc.defer_syntax();
                            self.editor_ui.epoch += 1;
                            self.editor_ui.find.submitted = None;
                            self.diagnostics.clear();
                            self.completions.clear();
                            self.last_sent = None;
                            self.send_snapshot();
                        } else if let Some((doc, edit)) = self.parked.get_mut(&path) {
                            edit.rebase(doc, &next);
                            *doc = *next;
                        }
                        self.editor_ui.conflicts.remove(&path);
                        self.status = format!(
                            "Reloaded {} · changed outside Wex",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                        self.sync_watches();
                    } else {
                        let missing = matches!(state, DiskState::Missing);
                        self.editor_ui.conflicts.insert(path.clone(), state);
                        if missing
                            && path
                                .extension()
                                .is_some_and(|e| e == "kt" || e == "kts" || e == "java")
                        {
                            self.cancel_semantic_open();
                            self.refactor.id = self
                                .refactor
                                .id
                                .checked_add(1)
                                .expect("semantic request epoch exhausted");
                            self.refactor.busy = false;
                            self.refactor.plan = None;
                            self.refactor.capture = None;
                            self.refactor.origin = None;
                            self.refactor.usages.clear();
                            if self.refactor.dialog {
                                self.refactor.error =
                                    "Source removed on disk · request a new semantic review".into();
                            }
                            self.completion_pending = false;
                            self.completions.clear();
                            if active {
                                self.diagnostic_snapshot = None;
                                self.diagnostics.clear();
                            }
                            self.last_sent = None;
                            self.send_snapshot();
                        }
                        self.status = format!(
                            "External change · {} · review before saving",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
                EditorEvent::Found {
                    id,
                    epoch,
                    revision,
                    result,
                } => {
                    if id != self.editor_ui.find.id
                        || epoch != self.editor_ui.epoch
                        || revision != self.doc.revision
                        || self
                            .editor_ui
                            .find
                            .submitted
                            .as_ref()
                            .is_none_or(|(e, r, o)| {
                                *e != epoch || *r != revision || o != &self.editor_ui.find.options
                            })
                    {
                        continue;
                    }
                    let f = &mut self.editor_ui.find;
                    f.pending = false;
                    f.revision = revision;
                    f.epoch = epoch;
                    f.results = result;
                    f.current = if f.results.matches.is_empty() {
                        None
                    } else {
                        Some(
                            f.results
                                .matches
                                .partition_point(|r| r.start < self.edit.cursor)
                                % f.results.matches.len(),
                        )
                    };
                    if let Some(index) = f.current
                        && self.edit.composition.is_none()
                        && ctx.memory(|m| {
                            m.has_focus(egui::Id::new("find-query"))
                                || m.has_focus(egui::Id::new("replace-query"))
                        })
                    {
                        let r = f.results.matches[index].clone();
                        self.edit.select(r.start, r.end);
                        self.scroll_to_cursor = true;
                    }
                }
                EditorEvent::Replaced {
                    id,
                    epoch,
                    revision,
                    result,
                } => {
                    if self.editor_ui.find.replacing != Some(id) {
                        continue;
                    }
                    self.editor_ui.find.replacing = None;
                    if epoch != self.editor_ui.epoch || revision != self.doc.revision {
                        self.status = "Document changed · replacement result discarded".into();
                        continue;
                    }
                    match result {
                        Ok((mut next, edit, delta, count)) => {
                            // Save/Keep mine can advance the disk baseline while a
                            // text-only job runs without changing the text revision.
                            next.acknowledge_disk(Some(&self.doc));
                            self.doc = *next;
                            self.doc.defer_syntax();
                            self.edit = edit;
                            if let Some(scope) = &mut self.editor_ui.find.options.within {
                                scope.end =
                                    (scope.end as isize + delta).max(scope.start as isize) as usize;
                            }
                            self.did_edit();
                            self.editor_ui.find.submitted = None;
                            self.status = format!(
                                "Replaced {count} match{} · one undo step",
                                if count == 1 { "" } else { "es" }
                            );
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
        }
        let f = &mut self.editor_ui.find;
        if f.open && f.replacing.is_none() {
            let signature = (self.editor_ui.epoch, self.doc.revision, f.options.clone());
            if f.submitted.as_ref() != Some(&signature) {
                if f.changed
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(120))
                    && self.last_edit.elapsed() > Duration::from_millis(120)
                {
                    f.id = self.editor_ui.services.search(
                        self.editor_ui.epoch,
                        &self.doc,
                        f.options.clone(),
                    );
                    f.submitted = Some(signature);
                    f.pending = true;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(120));
                }
            }
        }
    }
    pub(super) fn start_find(&mut self, replace: bool) {
        let range = self.edit.range();
        let selected = if range.end - range.start < 1024 {
            self.edit.selected_text(&self.doc)
        } else {
            String::new()
        };
        self.edit.cancel_composition();
        self.terminal.focus = false;
        let f = &mut self.editor_ui.find;
        self.editor_ui.focus_editor = false;
        f.handoff_ready = true;
        self.editor_ui.frame_editor_focus = false;
        f.open = true;
        f.expanded |= replace;
        f.focus = true;
        f.captured = if range.is_empty() { None } else { Some(range) };
        if !selected.is_empty() && !selected.contains(['\r', '\n']) {
            f.options.query = selected;
        }
        f.submitted = None;
        f.changed = None;
        self.context
            .memory_mut(|m| m.request_focus(egui::Id::new("find-query")));
        self.context.request_repaint();
    }
    pub(super) fn close_find(&mut self) {
        self.editor_ui.find.open = false;
        self.editor_ui.find.handoff.clear();
        self.editor_ui.find.focus = false;
        self.editor_ui.focus_editor = true;
        self.editor_ui.services.cancel_search();
        self.editor_ui.find.pending = false;
        self.editor_ui.find.replacing = None;
    }
    pub(super) fn valid_find(&self) -> bool {
        let f = &self.editor_ui.find;
        f.epoch == self.editor_ui.epoch
            && f.revision == self.doc.revision
            && !f.pending
            && f.submitted.as_ref().is_some_and(|(e, r, o)| {
                *e == self.editor_ui.epoch && *r == self.doc.revision && o == &f.options
            })
            && f.results.error.is_none()
            && !f.results.matches.is_empty()
    }
    pub(super) fn find_next(&mut self, back: bool) {
        if !self.valid_find() {
            return;
        }
        let f = &mut self.editor_ui.find;
        let n = f.results.matches.len();
        let index = if let Some(i) = f.current {
            if back { (i + n - 1) % n } else { (i + 1) % n }
        } else if back {
            n - 1
        } else {
            0
        };
        f.current = Some(index);
        let r = f.results.matches[index].clone();
        self.edit.select(r.start, r.end);
        self.scroll_to_cursor = true;
    }
    pub(super) fn replace_found(&mut self, all: bool) {
        if !self.valid_find() || self.editor_ui.find.replacing.is_some() {
            return;
        }
        let f = &self.editor_ui.find;
        if all && f.results.truncated {
            self.status = "Refine the query before Replace All · more than 50,000 matches".into();
            return;
        }
        let only = if all {
            None
        } else {
            f.current.and_then(|i| f.results.matches.get(i).cloned())
        };
        self.doc.set_view(self.edit.anchor, self.edit.cursor);
        match self.editor_ui.services.replace(
            self.editor_ui.epoch,
            &self.doc,
            &self.edit,
            f.options.clone(),
            f.replacement.clone(),
            only,
        ) {
            Ok(id) => self.editor_ui.find.replacing = Some(id),
            Err(e) => self.status = e.to_string(),
        }
    }
    pub(super) fn editor_blocked(&self) -> bool {
        self.file_ui.open
            || self.git.open
            || self.replace_blocked()
            || self.refactor.dialog
            || (self.projects.open || self.chrome.settings || self.variant_open)
            || self.chrome.device_open
            || self.pending_close.is_some()
            || self.close_ui.open
            || self.editor_ui.pending_reload.is_some()
    }
    pub(super) fn editor_shortcuts(&mut self, ctx: &egui::Context) {
        if self.editor_blocked() || self.doc.path.is_none() {
            return;
        }
        if self.open_ui.pending.is_some()
            && !self.editor_ui.find.open
            && self.completions.is_empty()
            && self.edit.composition.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.cancel_document_open();
            self.status = "File opening cancelled · current tab retained".into();
            return;
        }
        let command_key = |ctx: &egui::Context, key: Key| {
            ctx.input_mut(|input| {
                let mut found = false;
                input.events.retain(|event| {
                    let matched = matches!(
                        event,
                        egui::Event::Key { key: k, pressed: true, modifiers, .. }
                            if *k == key
                                && (modifiers.matches_exact(egui::Modifiers::COMMAND)
                                    || modifiers.matches_exact(egui::Modifiers::CTRL)
                                    || (modifiers.ctrl && modifiers.command
                                        && !modifiers.shift && !modifiers.alt))
                    );
                    found |= matched;
                    !matched
                });
                found
            })
        };
        if command_key(ctx, Key::F) {
            self.start_find(false);
        }
        if command_key(ctx, Key::H) {
            self.start_find(true);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, Key::F3)) {
            self.find_next(true);
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F3)) {
            self.find_next(false);
        }
        let find_focus = ctx.memory(|m| {
            m.has_focus(egui::Id::new("find-query")) || m.has_focus(egui::Id::new("replace-query"))
        });
        if find_focus {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, Key::Enter)) {
                self.find_next(true);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
                if ctx.memory(|m| m.has_focus(egui::Id::new("replace-query"))) {
                    self.replace_found(false);
                } else {
                    self.find_next(false);
                }
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                self.close_find();
            }
        }
    }
    pub(super) fn consume_exact_key(
        ctx: &egui::Context,
        pattern: egui::Modifiers,
        key: Key,
    ) -> bool {
        ctx.input_mut(|input| {
            let mut found=false;
            input.events.retain(|event| {
                let matched=matches!(event,egui::Event::Key{key:k,modifiers,pressed:true,..} if *k==key&&modifiers.matches_exact(pattern));
                found|=matched;!matched
            });found
        })
    }
    pub(super) fn cancel_deferred_input(&mut self) {
        if self.editor_ui.deferred_input.take().is_some() {
            self.editor_ui.pointer_metrics.cancelled += 1;
            self.status = "Deferred editor input cancelled · view changed".into();
        }
        self.editor_ui.drag = None;
    }
    fn resume_terminal_focus(
        &mut self,
        events: &mut std::collections::VecDeque<pointer_input::Ordered>,
    ) -> bool {
        let next = events.iter().position(|e| match e {
            pointer_input::Ordered::Terminal(egui::Event::PointerButton {
                pressed: true, ..
            }) => true,
            pointer_input::Ordered::Raw(e) | pointer_input::Ordered::Terminal(e) => {
                super::input_route::terminal_key(e) || super::input_route::project_key(e)
            }
            _ => false,
        });
        if let Some(i) = next {
            events.drain(..i);
            self.cancel_deferred_input();
            self.editor_ui.pointer_metrics.cancelled += 1;
            self.status = "Unapplied editor input cancelled · terminal focus".into();
            true
        } else {
            false
        }
    }
    fn defer_input(&mut self, events: std::collections::VecDeque<pointer_input::Ordered>) {
        self.editor_ui.pointer_metrics.deferred += 1;
        self.editor_ui.pointer_metrics.last_deferred_revision = self.doc.revision;
        self.editor_ui.pointer_metrics.last_deferred_order = events
            .iter()
            .take(8)
            .map(|e| match e {
                pointer_input::Ordered::Pointer(pointer_input::Pointer::Press { .. }) => "press",
                pointer_input::Ordered::Pointer(pointer_input::Pointer::Move(_)) => "move",
                pointer_input::Ordered::Pointer(pointer_input::Pointer::Release(_)) => "release",
                pointer_input::Ordered::Raw(egui::Event::Paste(_)) => "paste",
                _ => "raw",
            })
            .collect();
        self.editor_ui.deferred_input = Some(long_lines::DeferredInput {
            epoch: self.editor_ui.epoch,
            view: (self.edit.anchor, self.edit.cursor),
            snapshot: self.doc.render_snapshot(),
            events,
        });
        self.context
            .request_repaint_after(Duration::from_millis(16));
    }
    fn pointer_input(&mut self, ctx: &egui::Context, event: &pointer_input::Pointer) -> bool {
        use pointer_input::{Point, Pointer};
        if self
            .editor_ui
            .drag
            .as_ref()
            .is_some_and(|d| d.epoch != self.editor_ui.epoch || !d.snapshot.matches(&self.doc))
        {
            self.editor_ui.drag = None;
        }
        let point = match event {
            Pointer::Press { point, .. } | Pointer::Move(point) | Pointer::Release(point) => *point,
        };
        let rope = self.doc.rope().clone();
        let line = point.line.min(rope.len_lines() - 1);
        let hit = |app: &mut Self, point: Point| -> Option<usize> {
            let line = point.line.min(rope.len_lines() - 1);
            let (g, p) = app.line_layout(ctx, &rope, line, None)?;
            let col = app
                .line_hit(&g, p.as_ref(), egui::vec2(point.x, ROW / 2.))
                .index
                .min(line_length(&rope, line));
            Some(editing::snap(&rope, rope.line_to_char(line) + col))
        };
        match event {
            Pointer::Press { shift, when, .. } => {
                self.terminal.focus = false;
                self.editor_ui.focus_editor = true;
                self.editor_ui.frame_editor_focus = true;
                self.edit.cancel_composition();
                if point.x < 22. - GUTTER {
                    if let Some(path) = self.doc.path.clone() {
                        self.toggle_breakpoint(path, line as u32 + 1);
                    }
                    return true;
                }
                let count = match self.editor_ui.last_click {
                    Some((t, p, n))
                        if when.saturating_duration_since(t) < Duration::from_millis(400)
                            && p.distance(point.screen) < 6. =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                let unit = if point.x < -8. || count == 3 {
                    self.edit.select_line(&rope, line);
                    DragUnit::Line(self.edit.range())
                } else {
                    let Some(pos) = hit(self, point) else {
                        return false;
                    };
                    if count == 2 {
                        self.edit.select_word(&rope, pos);
                        DragUnit::Word(self.edit.range())
                    } else {
                        self.edit.move_to(&rope, pos, *shift);
                        DragUnit::Character
                    }
                };
                self.editor_ui.last_click = Some((*when, point.screen, count));
                self.editor_ui.pointer_metrics.presses += 1;
                self.editor_ui.drag = Some(Drag {
                    unit,
                    snapshot: self.doc.render_snapshot(),
                    epoch: self.editor_ui.epoch,
                });
            }
            Pointer::Move(_) | Pointer::Release(_) => {
                let Some(unit) = self.editor_ui.drag.as_ref().map(|d| d.unit.clone()) else {
                    return true;
                };
                match unit {
                    DragUnit::Character => {
                        let Some(pos) = hit(self, point) else {
                            return false;
                        };
                        self.edit.move_to(&rope, pos, true);
                    }
                    DragUnit::Word(base) => {
                        let Some(pos) = hit(self, point) else {
                            return false;
                        };
                        let r = editing::word_range(&rope, pos);
                        if pos < base.start {
                            self.edit.select(base.end, r.start);
                        } else {
                            self.edit.select(base.start, r.end);
                        }
                    }
                    DragUnit::Line(base) => {
                        let start = rope.line_to_char(line);
                        let end = if line + 1 < rope.len_lines() {
                            rope.line_to_char(line + 1)
                        } else {
                            rope.len_chars()
                        };
                        if start < base.start {
                            self.edit.select(base.end, start);
                        } else {
                            self.edit.select(base.start, end);
                        }
                    }
                }
                if matches!(event, Pointer::Release(_)) {
                    self.editor_ui.pointer_metrics.releases += 1;
                    self.editor_ui.pointer_metrics.last_selection =
                        Some((self.edit.anchor, self.edit.cursor));
                    self.editor_ui.drag = None;
                }
            }
        }
        self.doc.set_view(self.edit.anchor, self.edit.cursor);
        true
    }
    pub(super) fn input(&mut self, ctx: &egui::Context) {
        self.doc.defer_syntax();
        let mut events = if let Some(events) = self.editor_ui.frame_events.take() {
            events
        } else if let Some(layer) = self.editor_ui.pointer_layer {
            pointer_input::capture(
                ctx,
                self.editor_ui.geometry.origin,
                self.editor_ui.geometry.viewport,
                layer,
            )
        } else {
            std::collections::VecDeque::from(ctx.input(|i| {
                i.events
                    .iter()
                    .cloned()
                    .map(pointer_input::Ordered::Raw)
                    .collect::<Vec<_>>()
            }))
        };
        if ctx.input(|i| i.pointer.primary_down())
            && (self.editor_ui.drag.is_some()
                || self.editor_ui.deferred_input.as_ref().is_some_and(|d| {
                    d.events.iter().any(|e| {
                        matches!(
                            e,
                            pointer_input::Ordered::Pointer(pointer_input::Pointer::Press { .. })
                        )
                    })
                }))
            && let Some(p) = self
                .editor_ui
                .leased_pointer
                .as_ref()
                .and_then(|p| p.interact_pos())
                .or_else(|| ctx.pointer_interact_pos())
        {
            let origin = self.editor_ui.geometry.origin;
            pointer_input::push(
                &mut events,
                pointer_input::Ordered::Pointer(pointer_input::Pointer::Move(
                    pointer_input::Point {
                        line: ((p.y - origin.y) / ROW).floor().max(0.) as usize,
                        x: p.x - origin.x - GUTTER,
                        screen: p,
                    },
                )),
            );
        }
        let escape = events.iter().any(|e| {
            matches!(
                e,
                pointer_input::Ordered::Raw(egui::Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                })
            )
        });
        let cancel=escape||ctx.input(|i|i.events.iter().any(|e|matches!(e,egui::Event::Key {key:Key::Escape,pressed:true,..})||matches!(e,egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed:true,..}if !self.editor_ui.geometry.viewport.contains(*pos))));
        if cancel {
            self.cancel_deferred_input();
        }
        if let Some(mut deferred) = self.editor_ui.deferred_input.take() {
            if deferred.epoch == self.editor_ui.epoch
                && deferred.snapshot.matches(&self.doc)
                && deferred.view == (self.edit.anchor, self.edit.cursor)
            {
                while let Some(e) = events.pop_front() {
                    pointer_input::push(&mut deferred.events, e);
                }
                events = deferred.events;
                self.editor_ui.pointer_metrics.resumed += 1;
            } else {
                self.editor_ui.pointer_metrics.cancelled += 1;
                self.editor_ui.drag = None;
                self.status = "Deferred editor input cancelled · document/view changed".into();
            }
        }
        let ime_segments = super::ime::segments(&events);
        let mut ime_segment = 0;
        while let Some(ordered) = events.pop_front() {
            if self.format.open {
                let mut rest = std::iter::once(ordered)
                    .chain(events)
                    .filter_map(|e| match e {
                        pointer_input::Ordered::Raw(e) => Some(e),
                        pointer_input::Ordered::Ime(_, e) => Some(egui::Event::Ime(e)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                ctx.input_mut(|i| i.events.append(&mut rest));
                break;
            }
            if super::ime::barrier(&ordered) {
                ime_segment += 1;
            }
            let (ime_frame, terminal_ime_frame) = ime_segments[ime_segment];
            let event = match ordered {
                pointer_input::Ordered::CancelIme(target) => {
                    self.ime_clear(&target);
                    continue;
                }
                pointer_input::Ordered::Ime(tag, ime) => {
                    if !self.ime_deliver(&tag, &ime) {
                        continue;
                    }
                    if matches!(tag.target, super::ime::Target::Terminal(_)) {
                        self.terminal_event(ctx, egui::Event::Ime(ime), terminal_ime_frame);
                        continue;
                    }
                    egui::Event::Ime(ime)
                }
                pointer_input::Ordered::Pointer(pointer) => {
                    if !self.pointer_input(ctx, &pointer) {
                        events.push_front(pointer_input::Ordered::Pointer(pointer));
                        if self.resume_terminal_focus(&mut events) {
                            continue;
                        }
                        self.defer_input(events);
                        break;
                    }
                    continue;
                }
                pointer_input::Ordered::Raw(event) => event,
                pointer_input::Ordered::Terminal(event) => {
                    self.terminal_event(ctx, event, terminal_ime_frame);
                    continue;
                }
            };
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = &event
                && matches!(
                    key,
                    Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown
                )
                && !modifiers.command
                && !ime_frame
                && self.edit.composition.is_none()
            {
                let lines = if matches!(key, Key::PageUp | Key::PageDown) {
                    (self.editor_ui.geometry.viewport.height() / ROW).max(1.) as isize
                } else {
                    1
                };
                if !self.vertical(
                    ctx,
                    if matches!(key, Key::ArrowUp | Key::PageUp) {
                        -lines
                    } else {
                        lines
                    },
                    modifiers.shift,
                ) {
                    events.push_front(pointer_input::Ordered::Raw(event));
                    if self.resume_terminal_focus(&mut events) {
                        continue;
                    }
                    self.defer_input(events);
                    break;
                }
                continue;
            }

            let history_timer = (self.history_smoke.is_some()
                || self.syntax_smoke.is_some()
                || self.viewport_smoke.is_some())
            .then_some(())
            .and_then(|_| {
                let action = match &event {
                    egui::Event::Paste(_) => "Paste",
                    egui::Event::Key {
                        key: Key::Tab,
                        pressed: true,
                        ..
                    } => "Indent",
                    egui::Event::Ime(egui::ImeEvent::Commit(_)) => "IME",
                    egui::Event::Key {
                        key: Key::Z,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift => "Undo",
                    egui::Event::Key {
                        key: Key::Y,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.command => "Redo",
                    _ => return None,
                };
                Some((Instant::now(), action))
            });
            let before = self.doc.revision;
            let result: Option<anyhow::Result<()>> = match event {
                egui::Event::Copy => {
                    self.edit.cancel_composition();
                    if self.edit.selected() {
                        ctx.copy_text(self.edit.selected_text(&self.doc));
                    }
                    None
                }
                egui::Event::Cut => {
                    self.edit.cancel_composition();
                    if self.edit.selected() {
                        ctx.copy_text(self.edit.selected_text(&self.doc));
                        Some(self.edit.replace(&mut self.doc, ""))
                    } else {
                        None
                    }
                }
                egui::Event::Paste(text) => {
                    self.edit.cancel_composition();
                    Some(self.edit.replace(&mut self.doc, &text))
                }
                egui::Event::Text(text)
                    if !ime_frame
                        && self.edit.composition.is_none()
                        && text != "\n"
                        && text != "\r" =>
                {
                    Some(self.edit.replace(&mut self.doc, &text))
                }
                egui::Event::Ime(ime) => {
                    match ime {
                        egui::ImeEvent::Enabled => self.edit.enable_ime(),
                        egui::ImeEvent::Preedit(text) => self.edit.preedit(text),
                        egui::ImeEvent::Commit(text) => {
                            let r = self.edit.commit(&mut self.doc, &text);
                            if let Err(e) = r {
                                self.status = e.to_string();
                            }
                        }
                        egui::ImeEvent::Disabled => self.edit.cancel_composition(),
                    }
                    None
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if self.editor_command(ctx, key, modifiers) {
                        None
                    } else if key == Key::F12
                        && (modifiers.matches_exact(egui::Modifiers::NONE)
                            || modifiers.matches_exact(egui::Modifiers::SHIFT))
                    {
                        if modifiers.shift {
                            self.project_action(language::ProjectOperation::References);
                        } else {
                            self.request_language(true);
                        }
                        None
                    } else if key == Key::F6 && modifiers.matches_exact(egui::Modifiers::SHIFT) {
                        self.project_action(language::ProjectOperation::PrepareRename);
                        None
                    } else if key == Key::Space && modifiers.matches_exact(egui::Modifiers::COMMAND)
                    {
                        self.request_language(false);
                        None
                    } else if key == Key::Escape {
                        if self.edit.composition.is_some() {
                            self.edit.cancel_composition();
                        } else if !self.completions.is_empty() {
                            self.completions.clear();
                        } else if self.editor_ui.find.open {
                            self.close_find();
                        } else {
                            self.edit.collapse(self.edit.cursor);
                        }
                        None
                    } else if modifiers.command {
                        match key {
                            Key::A => {
                                self.edit.select(0, self.doc.rope().len_chars());
                                None
                            }
                            Key::Z | Key::Y => {
                                self.edit.cancel_composition();
                                if self
                                    .project_replace_history_key(key == Key::Y || modifiers.shift)
                                {
                                    None
                                } else {
                                    Some(
                                        self.edit
                                            .undo(&mut self.doc, key == Key::Y || modifiers.shift)
                                            .map(|_| ()),
                                    )
                                }
                            }
                            Key::ArrowLeft | Key::ArrowRight => {
                                self.edit.horizontal(
                                    self.doc.rope(),
                                    key == Key::ArrowRight,
                                    true,
                                    modifiers.shift,
                                );
                                self.scroll_to_cursor = true;
                                None
                            }
                            Key::Home | Key::End => {
                                let p = if key == Key::Home {
                                    0
                                } else {
                                    self.doc.rope().len_chars()
                                };
                                self.edit.move_to(self.doc.rope(), p, modifiers.shift);
                                self.scroll_to_cursor = true;
                                None
                            }
                            Key::Backspace | Key::Delete => {
                                Some(self.edit.delete(&mut self.doc, key == Key::Delete, true))
                            }
                            _ => None,
                        }
                    } else if ime_frame || self.edit.composition.is_some() {
                        None
                    } else {
                        match key {
                            Key::ArrowLeft | Key::ArrowRight => {
                                self.edit.horizontal(
                                    self.doc.rope(),
                                    key == Key::ArrowRight,
                                    false,
                                    modifiers.shift,
                                );
                                self.scroll_to_cursor = true;
                                None
                            }
                            Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown => {
                                let lines = if matches!(key, Key::PageUp | Key::PageDown) {
                                    (self.editor_ui.geometry.viewport.height() / ROW).max(1.)
                                        as isize
                                } else {
                                    1
                                };
                                let _ = self.vertical(
                                    ctx,
                                    if matches!(key, Key::ArrowUp | Key::PageUp) {
                                        -lines
                                    } else {
                                        lines
                                    },
                                    modifiers.shift,
                                );
                                None
                            }
                            Key::Home | Key::End => {
                                let line = self.doc.rope().char_to_line(self.edit.cursor);
                                let start = self.doc.rope().line_to_char(line);
                                let pos = if key == Key::Home {
                                    start
                                } else {
                                    start + line_length(self.doc.rope(), line)
                                };
                                self.edit.move_to(self.doc.rope(), pos, modifiers.shift);
                                self.scroll_to_cursor = true;
                                None
                            }
                            Key::Backspace | Key::Delete => {
                                Some(self.edit.delete(&mut self.doc, key == Key::Delete, false))
                            }
                            Key::Enter => {
                                let line = self.doc.rope().char_to_line(self.edit.range().start);
                                let indent = self
                                    .doc
                                    .rope()
                                    .line(line)
                                    .chars()
                                    .take_while(|c| matches!(c, ' ' | '\t'))
                                    .collect::<String>();
                                Some(self.edit.replace(&mut self.doc, &format!("\n{indent}")))
                            }
                            Key::Tab => Some(self.edit.indent(&mut self.doc, modifiers.shift)),
                            _ => None,
                        }
                    }
                }
                _ => None,
            };
            if let Some(Err(e)) = result {
                self.status = e.to_string();
            }
            if self.doc.revision != before {
                self.did_edit();
            }
            if let Some((start, action)) = history_timer {
                let elapsed = start.elapsed().as_micros();
                let value = serde_json::json!({"action":action,"elapsed_us":elapsed,"revision_before":before,"revision_after":self.doc.revision,"version":self.doc.version,"dirty":self.doc.dirty(),"tree_end_byte":self.doc.tree().map(|t|t.root_node().end_byte()),"syntax_pending_at_input":self.doc.tree().is_none(),"rope_bytes":self.doc.rope().len_bytes()});
                if let Some(s) = &mut self.history_smoke {
                    s.actions.push(value.clone());
                }
                if let Some(s) = &mut self.viewport_smoke {
                    s.actions.push(value.clone());
                }
                if let Some(s) = &mut self.syntax_smoke {
                    s.actions.push(value);
                }
            }
        }
        self.doc.set_view(self.edit.anchor, self.edit.cursor);
    }
    fn vertical(&mut self, ctx: &egui::Context, delta: isize, extend: bool) -> bool {
        let rope = self.doc.rope().clone();
        let line = rope.char_to_line(self.edit.cursor);
        let start = rope.line_to_char(line);
        let Some((g, _)) = self.line_layout(ctx, &rope, line, None) else {
            return false;
        };
        let x = self.edit.preferred_x.unwrap_or_else(|| {
            g.pos_from_cursor(egui::text::CCursor::new(self.edit.cursor - start))
                .left()
        });
        let target = (line as isize + delta).clamp(0, rope.len_lines() as isize - 1) as usize;
        let Some((g, p)) = self.line_layout(ctx, &rope, target, None) else {
            return false;
        };
        let col = self
            .line_hit(&g, p.as_ref(), egui::vec2(x, ROW / 2.))
            .index
            .min(line_length(&rope, target));
        let at = rope.line_to_char(target) + col;
        self.edit.move_to(&rope, at, extend);
        self.edit.preferred_x = Some(x);
        self.scroll_to_cursor = true;
        true
    }
    pub(super) fn editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let viewport_timer = self.viewport_smoke.as_ref().map(|_| Instant::now());
        if viewport_timer.is_some() {
            self.editor_ui.viewport_profile = Default::default();
        }
        self.editor_ui
            .text_mesh
            .begin_frame(ctx.cumulative_frame_nr());
        let id = ui.make_persistent_id("rope-editor");
        self.editor_ui.input_id = Some(id);
        let viewport = ui.available_rect_before_wrap();
        self.chrome.hits_editor(viewport);
        let response = ui
            .interact(viewport, id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Text);
        if self.editor_ui.focus_editor {
            response.request_focus();
            self.editor_ui.focus_editor = false;
        }
        let blocked = self.editor_blocked();
        self.editor_ui.pointer_layer = Some(ui.layer_id());
        let presses = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => Some(*pos),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let accepted = if self.editor_ui.frame_handled == Some(ctx.cumulative_frame_nr()) {
            self.editor_ui.frame_editor_focus
        } else {
            presses.into_iter().last().is_some_and(|p| {
                self.editor_ui.geometry.viewport.contains(p)
                    && ctx.layer_id_at(p) == Some(ui.layer_id())
            })
        };
        let down = self.editor_ui.leased_pointer.as_ref().map_or_else(
            || ctx.input(|i| i.pointer.primary_down()),
            |p| p.primary_down(),
        );
        if accepted && !blocked {
            response.request_focus();
        }
        let focus = ui.memory(|m| m.has_focus(id)) && !blocked && ctx.input(|i| i.focused);
        if focus {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            if self.editor_ui.frame_handled != Some(ctx.cumulative_frame_nr()) {
                self.input(ctx);
            }
        } else {
            self.edit.cancel_composition();
            self.cancel_deferred_input();
        }
        if !ctx.input(|i| i.focused) {
            self.edit.cancel_composition();
            self.cancel_deferred_input();
        }
        if self.editor_ui.drag.is_some()
            && down
            && let Some(p) = ctx.pointer_interact_pos()
        {
            let dy = if p.y < viewport.top() {
                p.y - viewport.top()
            } else if p.y > viewport.bottom() {
                p.y - viewport.bottom()
            } else {
                0.
            };
            let dx = if p.x < viewport.left() {
                p.x - viewport.left()
            } else if p.x > viewport.right() {
                p.x - viewport.right()
            } else {
                0.
            };
            if dy != 0. || dx != 0. {
                let dt = ctx.input(|i| i.stable_dt).min(0.05);
                self.edit.scroll_y = (self.edit.scroll_y + dy * dt * 10.).max(0.);
                self.edit.scroll_x = (self.edit.scroll_x + dx * dt * 10.).max(0.);
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        let mut display = self.doc.rope().clone();
        let comp = self.edit.composition.clone();
        if let Some(c) = &comp {
            display.remove(c.range.clone());
            display.insert(c.range.start, &c.text);
        }
        self.begin_line_layouts(ctx, comp.as_ref());
        let mut caret = comp
            .as_ref()
            .map(|c| c.range.start + c.text.chars().count())
            .unwrap_or(self.edit.cursor)
            .min(display.len_chars());
        let mut caret_line = display.char_to_line(caret);
        if self.scroll_to_cursor {
            let y = caret_line as f32 * ROW;
            if y < self.edit.scroll_y {
                self.edit.scroll_y = y;
            } else if y + ROW > self.edit.scroll_y + viewport.height() {
                self.edit.scroll_y = (y + ROW - viewport.height()).max(0.);
            }
            if let Some((g, _)) = self.line_layout(ctx, &display, caret_line, comp.as_ref()) {
                let x = GUTTER
                    + g.pos_from_cursor(egui::text::CCursor::new(
                        caret - display.line_to_char(caret_line),
                    ))
                    .left();
                if x < self.edit.scroll_x + GUTTER {
                    self.edit.scroll_x = (x - GUTTER).max(0.);
                } else if x + 12. > self.edit.scroll_x + viewport.width() {
                    self.edit.scroll_x = (x + 12. - viewport.width()).max(0.);
                }
                self.scroll_to_cursor = false;
            }
        }
        self.edit.scroll_y = self
            .edit
            .scroll_y
            .min((display.len_lines() as f32 * ROW - viewport.height()).max(0.));
        let scroll = egui::ScrollArea::both()
            .id_salt(("code-viewport", self.doc.path.clone()))
            .animated(false)
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..Default::default()
            })
            .auto_shrink([false, false])
            .scroll_offset(egui::vec2(self.edit.scroll_x, self.edit.scroll_y));
        self.editor_ui.geometry.rows.clear();
        self.editor_ui.geometry.assets.clear();
        self.editor_ui.geometry.ime = None;
        self.editor_ui.rendered_rows = 0;
        ui.spacing_mut().item_spacing.y = 0.;
        let output = scroll.show_viewport(ui, |ui, visible| {
            let origin = ui.cursor().min;
            self.editor_ui.geometry.origin = origin;
            let first = (visible.top() / ROW).floor().max(0.) as usize;
            let end = ((visible.bottom() / ROW).ceil() as usize + 1).min(display.len_lines());
            let mut width = viewport.width();
            let mut pending_rows = false;
            if comp.is_none() {
                caret = self.edit.cursor.min(display.len_chars());
                caret_line = display.char_to_line(caret);
            }
            for line in first..end {
                self.editor_ui.rendered_rows += 1;
                let start = display.line_to_char(line);
                let raw = display.line(line);
                let mut text_len = raw.len_chars();
                while text_len > 0 && matches!(raw.char(text_len - 1), '\r' | '\n') {
                    text_len -= 1;
                }
                let text_end = start + text_len;
                let pos = origin + egui::vec2(GUTTER, line as f32 * ROW);
                let timer = viewport_timer.map(|_| Instant::now());
                let Some((galley, prepared)) = self.line_layout(ctx, &display, line, comp.as_ref())
                else {
                    pending_rows = true;
                    if let Some(error) = self.editor_ui.long_lines.error(line).map(str::to_owned) {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(
                            Rect::from_min_size(pos, egui::vec2(viewport.width() - GUTTER, ROW)),
                        ));
                        child.horizontal(|ui| {
                            ui.label(egui::RichText::new(error).color(m::ERROR));
                            if self
                                .chrome
                                .button(
                                    ui,
                                    &format!("long-line-retry-{line}"),
                                    "Retry",
                                    Icon::Refresh,
                                    Kind::Tonal,
                                    true,
                                    74.,
                                    self.reduced_motion,
                                )
                                .clicked()
                            {
                                self.editor_ui.long_lines.retry(line);
                                ctx.request_repaint();
                            }
                        });
                    }
                    if self.editor_ui.long_lines.error(line).is_none() {
                        ui.painter().text(
                            pos,
                            Align2::LEFT_TOP,
                            if self.editor_ui.deferred_input.is_some() {
                                "Preparing text · input queued…"
                            } else {
                                "Preparing long line…"
                            },
                            FontId::monospace(13.),
                            m::MUTED,
                        );
                    }
                    ui.painter().text(
                        origin + egui::vec2(42., line as f32 * ROW + ROW / 2.),
                        Align2::RIGHT_CENTER,
                        (line + 1).to_string(),
                        FontId::monospace(12.),
                        m::MUTED,
                    );
                    continue;
                };
                if let Some(t) = timer {
                    self.editor_ui.viewport_profile.font_layout_us += t.elapsed().as_micros();
                    self.editor_ui.viewport_profile.source_vertices += galley.num_vertices;
                    self.editor_ui.viewport_profile.rows += 1;
                }
                width = width.max(GUTTER + galley.size().x + 16.);
                let row = Rect::from_min_size(
                    origin + egui::vec2(0., line as f32 * ROW),
                    egui::vec2(visible.width().max(width), ROW),
                );
                if line == caret_line {
                    ui.painter().rect_filled(row, 5, m::CONTAINER);
                }
                if let Some(path) = self.doc.path.as_ref() {
                    let gutter = Rect::from_center_size(
                        origin + egui::vec2(12., line as f32 * ROW + ROW / 2.),
                        egui::vec2(20., ROW),
                    );
                    self.chrome
                        .hits
                        .insert(format!("debug-gutter-{}", line + 1), gutter);
                    let executing = self
                        .debug
                        .execution
                        .as_ref()
                        .is_some_and(|(p, l)| p == path && *l == line as u32 + 1);
                    if executing {
                        ui.painter().rect_filled(
                            row,
                            5,
                            m::mix(m::CONTAINER, m::PRIMARY_CONTAINER, 0.55),
                        );
                    }
                    if self.breakpoint_marker_current(path)
                        && self
                            .debug
                            .breakpoints
                            .get(path)
                            .is_some_and(|b| b.contains(&(line as u32 + 1)))
                    {
                        let verified = self.debug.verified.get(path).is_some_and(|b| {
                            b.iter().any(|b| b.line == line as u32 + 1 && b.verified)
                        });
                        let center = gutter.center();
                        if (verified || self.debug.connection.is_none())
                            && !self.debug.bp_service.pending()
                        {
                            ui.painter().circle_filled(center, 5., m::ERROR);
                        } else {
                            ui.painter()
                                .circle_stroke(center, 5., Stroke::new(1.5_f32, m::ERROR));
                        }
                    }
                    if executing {
                        m::icon(
                            ui,
                            Rect::from_center_size(
                                gutter.center() + egui::vec2(12., 0.),
                                egui::vec2(12., 18.),
                            ),
                            Icon::Play,
                            m::PRIMARY,
                        );
                    }
                }

                if self.editor_ui.find.open && comp.is_none() && self.valid_find() {
                    let matches = &self.editor_ui.find.results.matches;
                    let index = matches.partition_point(|r| r.end < start);
                    for (i, r) in matches.iter().enumerate().skip(index) {
                        if r.start > text_end {
                            break;
                        }
                        paint_range(
                            ui,
                            &galley,
                            pos,
                            start,
                            text_len,
                            r,
                            if self.editor_ui.find.current == Some(i) {
                                m::mix(m::PRIMARY_CONTAINER, m::PRIMARY, 0.2)
                            } else {
                                m::mix(m::LOW, m::WARNING, 0.25)
                            },
                        );
                    }
                }
                if comp.is_none() && self.edit.selected() {
                    paint_range(
                        ui,
                        &galley,
                        pos,
                        start,
                        text_len,
                        &self.edit.range(),
                        m::PRIMARY_CONTAINER,
                    );
                }
                ui.painter().text(
                    origin + egui::vec2(42., line as f32 * ROW + ROW / 2.),
                    Align2::RIGHT_CENTER,
                    (line + 1).to_string(),
                    FontId::monospace(12.),
                    m::MUTED,
                );
                if prepared.is_none()
                    && galley.num_vertices <= 4096
                    && ui
                        .clip_rect()
                        .contains_rect(galley.mesh_bounds.translate(pos.to_vec2()))
                {
                    if viewport_timer.is_some() {
                        self.editor_ui.viewport_profile.submitted_vertices += galley.num_vertices;
                    }
                    ui.painter().galley(pos, galley.clone(), m::TEXT);
                } else {
                    let (mesh, stats) = if let Some(p) = &prepared {
                        p.layout.mesh(pos, ui.clip_rect(), m::TEXT, p.texture.id())
                    } else {
                        let texture = ui.fonts(|f| f.font_image_size());
                        self.editor_ui.text_mesh.mesh(
                            &galley,
                            pos,
                            ui.clip_rect(),
                            texture,
                            m::TEXT,
                        )
                    };
                    if viewport_timer.is_some() {
                        self.editor_ui.viewport_profile.submitted_vertices +=
                            stats.submitted_vertices;
                        self.editor_ui.viewport_profile.candidate_glyphs += stats.candidate_glyphs;
                        self.editor_ui.viewport_profile.indexed_glyphs +=
                            stats.newly_indexed_glyphs;
                    }
                    ui.painter().add(egui::Shape::mesh(mesh));
                }
                if self
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Some(1) && d.range.start.line as usize == line)
                    && comp.is_none()
                {
                    ui.painter().line_segment(
                        [
                            pos + egui::vec2(0., ROW - 2.),
                            pos + egui::vec2(galley.size().x, ROW - 2.),
                        ],
                        Stroke::new(1.0_f32, m::ERROR),
                    );
                }
                if line == caret_line && focus {
                    let cursor = galley.pos_from_cursor(egui::text::CCursor::new(caret - start));
                    let r = cursor.translate(pos.to_vec2());
                    let blink = self.reduced_motion
                        || self.last_edit.elapsed() < Duration::from_millis(600)
                        || ctx.input(|i| i.time) % 1. < 0.55;
                    if blink {
                        ui.painter().line_segment(
                            [r.left_top(), r.left_bottom()],
                            Stroke::new(1.5_f32, m::PRIMARY),
                        );
                    }
                    let ime = egui::output::IMEOutput {
                        rect: viewport,
                        cursor_rect: r,
                    };
                    ui.output_mut(|o| o.ime = Some(ime));
                    self.editor_ui.geometry.ime = Some(r);
                    ctx.request_repaint_after(Duration::from_millis(300));
                }
                if let Some(p) = prepared {
                    self.editor_ui.geometry.assets.push(p);
                }
                self.editor_ui.geometry.rows.insert(line, (pos, galley));
            }
            if pending_rows {
                width = width.max(self.edit.scroll_x + viewport.width());
            }
            ui.set_min_size(egui::vec2(width, display.len_lines() as f32 * ROW));
        });
        self.edit.scroll_x = output.state.offset.x;
        self.edit.scroll_y = output.state.offset.y;
        self.editor_ui.geometry.viewport = output.inner_rect;
        if response.hovered() {
            ui.output_mut(|o| o.mutable_text_under_cursor = true);
        }
        if !down && self.editor_ui.drag.is_some() && self.editor_ui.deferred_input.is_none() {
            self.editor_ui.drag = None;
        }
        response.context_menu(|ui| {
            for (key, label, icon, enabled, command) in [
                (
                    "edit-cut",
                    "Cut · Ctrl+X",
                    Icon::Close,
                    self.edit.selected(),
                    egui::ViewportCommand::RequestCut,
                ),
                (
                    "edit-copy",
                    "Copy · Ctrl+C",
                    Icon::File,
                    self.edit.selected(),
                    egui::ViewportCommand::RequestCopy,
                ),
                (
                    "edit-paste",
                    "Paste · Ctrl+V",
                    Icon::Save,
                    true,
                    egui::ViewportCommand::RequestPaste,
                ),
            ] {
                if self
                    .chrome
                    .button(
                        ui,
                        key,
                        label,
                        icon,
                        Kind::Ghost,
                        enabled,
                        210.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    self.editor_ui.focus_editor = true;
                    ctx.send_viewport_cmd(command);
                    ui.close();
                }
            }
            if self
                .chrome
                .button(
                    ui,
                    "edit-all",
                    "Select all · Ctrl+A",
                    Icon::Code,
                    Kind::Ghost,
                    true,
                    210.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.edit.select(0, self.doc.rope().len_chars());
                self.editor_ui.focus_editor = true;
                ui.close();
            }
            if self
                .chrome
                .button(
                    ui,
                    "edit-find",
                    "Find / replace · Ctrl+F",
                    Icon::Search,
                    Kind::Ghost,
                    true,
                    210.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.start_find(false);
                ui.close();
            }
        });
        self.doc.set_view(self.edit.anchor, self.edit.cursor);
        if let Some(t) = viewport_timer {
            self.editor_ui.viewport_profile.editor_us = t.elapsed().as_micros();
        }
    }
    pub(super) fn editor_breadcrumb(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 252.).max(1.);
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, 40.), Sense::hover());
            let path = self
                .doc
                .path
                .as_ref()
                .map(|p| {
                    p.strip_prefix(&self.root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_else(|| "Open a source file to start".into());
            m::label_clipped(ui, r, &path.replace('/', " / "), 11., m::MUTED);
            if self
                .chrome
                .button(
                    ui,
                    "editor-search",
                    "",
                    Icon::Search,
                    if self.editor_ui.find.open {
                        Kind::Tonal
                    } else {
                        Kind::Ghost
                    },
                    self.doc.path.is_some(),
                    40.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Find · Ctrl+F / Replace · Ctrl+H")
                .clicked()
            {
                if self.editor_ui.find.open {
                    self.close_find();
                } else {
                    self.start_find(false);
                }
            }
            if self.chrome.button(ui,"format-code","Format",Icon::Code,Kind::Tonal,self.format_available()&&!self.editor_blocked(),92.,self.reduced_motion).on_hover_text_passive("Format Kotlin (K2) or Java (google-java-format/AOSP) · Ctrl+Alt+L · preview, no Save.").clicked(){Self::consume_exact_key(ctx,egui::Modifiers::NONE,Key::Enter);Self::consume_exact_key(ctx,egui::Modifiers::NONE,Key::Space);self.start_formatting();}
            for (key, label, operation) in [
                (
                    "editor-usages",
                    "Find usages · Shift+F12",
                    language::ProjectOperation::References,
                ),
                (
                    "editor-rename",
                    "Rename symbol · Shift+F6",
                    language::ProjectOperation::PrepareRename,
                ),
            ] {
                if self
                    .chrome
                    .button(
                        ui,
                        key,
                        "",
                        if key == "editor-usages" {
                            Icon::Search
                        } else {
                            Icon::Code
                        },
                        Kind::Ghost,
                        self.language.is_some() && !self.refactor.busy,
                        40.,
                        self.reduced_motion,
                    )
                    .on_hover_text_passive(label)
                    .clicked()
                {
                    self.project_action(operation);
                }
            }
        });
        ui.add_space(4.);
    }
    pub(super) fn find_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let desired = if self.editor_ui.find.expanded {
            156.
        } else {
            110.
        };
        let h = self
            .chrome
            .spatial(
                ctx,
                "find-height",
                if self.editor_ui.find.open {
                    desired
                } else {
                    0.
                },
                0.,
                self.reduced_motion,
            )
            .clamp(0., desired + 6.);
        if h < 1. {
            return;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::hover());
        ui.painter().rect_filled(rect, 16, m::CONTAINER);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            rect.min + egui::vec2(8., 8.),
            egui::vec2(rect.width() - 16., desired - 16.),
        )));
        child.set_clip_rect(rect.shrink(2.));
        child.add_enabled_ui(self.editor_ui.find.open, |ui| {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 192.).max(60.);
                let handed=!self.editor_ui.find.handoff.is_empty()&&self.editor_ui.find.handoff_ready;
                let saved=if handed {let events=std::mem::take(&mut self.editor_ui.find.handoff);Some(ctx.input_mut(|i|{let old=std::mem::replace(&mut i.events,events);let pointer=std::mem::take(&mut i.pointer);(old,pointer)}))}else{None};
                let should_focus = self.editor_ui.find.focus || handed;
                let r = ui.add_sized([width, 40.], egui::TextEdit::singleline(&mut self.editor_ui.find.options.query)
                    .id(egui::Id::new("find-query")).hint_text("Find in file…").margin(egui::vec2(10., 10.)));
                if should_focus {
                    r.request_focus();
                    // Keep the admission flag until egui has committed focus;
                    // the next frame may be the first one that exposes the ID.
                    self.editor_ui.find.focus = true;
                }
                if let Some((events,pointer))=saved{ctx.input_mut(|i|{i.events=events;i.pointer=pointer;});}
                self.editor_ui.hits.insert("find-query".into(), r.rect);
                if r.changed() {
                    self.editor_ui.find.changed = Some(Instant::now());
                    self.editor_ui.find.results = Default::default();
                }
                let valid = self.valid_find();
                if self.chrome.button(ui, "find-previous", "", Icon::Collapse, Kind::Ghost, valid, 40., self.reduced_motion)
                    .on_hover_text_passive("Previous · Shift+F3").clicked() { self.find_next(true); }
                if self.chrome.button(ui, "find-next", "", Icon::Expand, Kind::Ghost, valid, 40., self.reduced_motion)
                    .on_hover_text_passive("Next · F3 / Enter").clicked() { self.find_next(false); }
                if self.chrome.button(ui, "find-expand", "", Icon::Refresh, Kind::Ghost, true, 40., self.reduced_motion)
                    .on_hover_text_passive("Show / hide replacement · Ctrl+H").clicked() {
                    self.editor_ui.find.expanded = !self.editor_ui.find.expanded;
                }
                if self.chrome.button(ui, "find-close", "", Icon::Close, Kind::Ghost, true, 40., self.reduced_motion)
                    .on_hover_text_passive("Close · Escape").clicked() { self.close_find(); }
            });
            ui.horizontal(|ui| {
                for (key, label, width, active) in [
                    ("find-case", "Aa", 46., self.editor_ui.find.options.case_sensitive),
                    ("find-word", "Word", 64., self.editor_ui.find.options.whole_word),
                    ("find-regex", ".*", 46., self.editor_ui.find.options.regex),
                    ("find-scope", "Selection", 96., self.editor_ui.find.options.within.is_some()),
                ] {
                    let enabled = key != "find-scope" || self.editor_ui.find.captured.is_some() || active;
                    let response = m::button(ui, &mut self.chrome.motion, m::Button {
                        key, label, icon: None, kind: if active { Kind::Tonal } else { Kind::Ghost }, enabled, width,
                    }, self.reduced_motion);
                    self.chrome.hits.insert(key.into(), response.rect);
                    if response.on_hover_text_passive(match key {
                        "find-case" => "Match case",
                        "find-word" => "Whole words (Unicode)",
                        "find-regex" => "Regular expression",
                        _ => "Search only the original selection",
                    }).clicked() {
                        match key {
                            "find-case" => self.editor_ui.find.options.case_sensitive = !active,
                            "find-word" => self.editor_ui.find.options.whole_word = !active,
                            "find-regex" => self.editor_ui.find.options.regex = !active,
                            _ => self.editor_ui.find.options.within = if active { None } else { self.editor_ui.find.captured.clone() },
                        }
                        self.editor_ui.find.changed = Some(Instant::now());
                        self.editor_ui.find.results = Default::default();
                    }
                }
                let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.), 40.), Sense::hover());
                let f = &self.editor_ui.find;
                let label = if f.pending || f.replacing.is_some() {
                    "Searching…".into()
                } else if f.results.error.is_some() {
                    "Invalid regex".into()
                } else {
                    format!("{} / {}{}", f.current.map(|i| i + 1).unwrap_or(0), f.results.matches.len(), if f.results.truncated { "+" } else { "" })
                };
                m::label_clipped(ui, r, &label, 11., if f.results.error.is_some() { m::ERROR } else { m::MUTED });
                if let Some(e) = &f.results.error {
                    ui.interact(r, ui.id().with("find-error"), Sense::hover()).on_hover_text_passive(e);
                }
            });
            if self.editor_ui.find.expanded {
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 188.).max(60.);
                    let r = ui.add_sized([width, 40.], egui::TextEdit::singleline(&mut self.editor_ui.find.replacement)
                        .id(egui::Id::new("replace-query")).hint_text("Replace with…").margin(egui::vec2(10., 10.)));
                    self.editor_ui.hits.insert("replace-query".into(), r.rect);
                    r.on_hover_text_passive("Regex replacement: $1, ${name}, $$, \\n and \\t. Literal mode keeps text unchanged.");
                    let enabled = self.valid_find() && self.editor_ui.find.replacing.is_none();
                    if self.chrome.button(ui, "replace-one", "Replace", Icon::Refresh, Kind::Tonal, enabled, 108., self.reduced_motion).clicked() {
                        self.replace_found(false);
                    }
                    if self.chrome.button(ui, "replace-all", "All", Icon::Refresh, Kind::Filled, enabled && !self.editor_ui.find.results.truncated, 64., self.reduced_motion)
                        .on_hover_text_passive("Replace all · one undo step").clicked() { self.replace_found(true); }
                });
            }
        });
        ui.add_space(6.);
    }
    pub(super) fn external_banner(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(path) = self.doc.path.clone() else {
            return;
        };
        let Some(state) = self.editor_ui.conflicts.get(&path) else {
            return;
        };
        let (title, detail, present, missing) = match state {
            DiskState::Present(_) => (
                "Changed outside Wex",
                "Your buffer is preserved. Review the disk version before saving.".to_string(),
                true,
                false,
            ),
            DiskState::Missing => (
                "File removed or renamed",
                "Your buffer is preserved. Recreate it only when you choose Save.".into(),
                false,
                true,
            ),
            DiskState::Error(e) => ("Disk read needs attention", e.clone(), false, false),
        };
        egui::Frame::new().fill(m::mix(m::CONTAINER,m::ERROR,0.08)).corner_radius(16).inner_margin(12).show(ui,|ui|{
            ui.label(RichText::new(title).size(16.).color(m::ERROR));if ctx.content_rect().height()>=700. {ui.label(RichText::new(detail).size(12.).color(m::MUTED));}
            ui.horizontal_wrapped(|ui|{
                if self.chrome.button(ui,"disk-reload","Reload disk",Icon::Refresh,Kind::Outline,present,126.,self.reduced_motion).clicked(){if self.doc.dirty(){self.editor_ui.pending_reload=Some(path.clone());}else{self.reload_external(path.clone());}}
                if self.chrome.button(ui,"disk-keep",if missing{"Recreate on Save"}else{"Keep mine"},Icon::Save,Kind::Tonal,present||missing,if missing{174.}else{126.},self.reduced_motion).on_hover_text_passive("Keep local text; Save checks the observed disk state again before writing.").clicked(){self.keep_external(path.clone());}
                if self.chrome.button(ui,"disk-retry","",Icon::Refresh,Kind::Ghost,true,40.,self.reduced_motion).on_hover_text_passive("Retry disk read").clicked(){self.editor_ui.generation=self.editor_ui.services.watch(self.editor_ui.signature.clone());}
            });
        });
        ui.add_space(6.);
    }
    pub(super) fn keep_external(&mut self, path: PathBuf) {
        if self.save_ui.contains(&path) {
            self.status = "Save pending · review disk after completion".into();
            return;
        }
        let Some(state) = self.editor_ui.conflicts.remove(&path) else {
            return;
        };
        let disk = match &state {
            DiskState::Present(d) => Some(d.as_ref()),
            DiskState::Missing => None,
            DiskState::Error(_) => {
                self.editor_ui.conflicts.insert(path, state);
                return;
            }
        };
        if self.doc.path.as_ref() == Some(&path) {
            self.doc.acknowledge_disk(disk);
        } else if let Some((doc, _)) = self.parked.get_mut(&path) {
            doc.acknowledge_disk(disk);
        }
        self.status = "Local text kept · Save rechecks the observed disk version".into();
        self.sync_watches();
    }
    pub(super) fn reload_external(&mut self, path: PathBuf) {
        if self.save_ui.contains(&path) {
            self.status = "Save pending · review disk after completion".into();
            return;
        }
        let Some(state) = self.editor_ui.conflicts.remove(&path) else {
            return;
        };
        let DiskState::Present(next) = state else {
            self.editor_ui.conflicts.insert(path, state);
            return;
        };
        if self.doc.path.as_ref() == Some(&path) {
            self.edit.rebase(&self.doc, &next);
            self.doc = *next;
            self.doc.defer_syntax();
            self.editor_ui.epoch += 1;
            self.editor_ui.find.submitted = None;
            self.last_sent = None;
            self.diagnostics.clear();
            self.completions.clear();
            self.send_snapshot();
        } else if let Some((doc, edit)) = self.parked.get_mut(&path) {
            edit.rebase(doc, &next);
            *doc = *next;
        }
        self.editor_ui.pending_reload = None;
        self.status = "Reloaded disk version · local edits discarded by choice".into();
        self.sync_watches();
    }
    pub(super) fn reload_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = self.editor_ui.pending_reload.clone() else {
            return;
        };
        egui::Modal::new(egui::Id::new("reload-disk"))
            .frame(m::card().fill(m::HIGH).inner_margin(24))
            .show(ctx, |ui| {
                ui.set_width(370.);
                ui.heading("Reload from disk?");
                ui.add_space(12.);
                ui.label("This discards local edits. The disk version stays unchanged.");
                ui.add_space(20.);
                ui.horizontal(|ui| {
                    if self
                        .chrome
                        .button(
                            ui,
                            "reload-cancel",
                            "Cancel",
                            Icon::Close,
                            Kind::Ghost,
                            true,
                            110.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.editor_ui.pending_reload = None;
                    }
                    if self
                        .chrome
                        .button(
                            ui,
                            "reload-confirm",
                            "Reload",
                            Icon::Refresh,
                            Kind::Danger,
                            true,
                            126.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.reload_external(path);
                    }
                });
            });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.editor_ui.pending_reload = None;
        }
    }
}
fn paint_range(
    ui: &egui::Ui,
    g: &egui::Galley,
    pos: Pos2,
    line_start: usize,
    len: usize,
    r: &Range<usize>,
    color: Color32,
) {
    if r.end < line_start || r.start > line_start + len {
        return;
    }
    let a = r.start.saturating_sub(line_start).min(len);
    let b = r.end.saturating_sub(line_start).min(len);
    if a == b && r.start != r.end && r.end <= line_start {
        return;
    }
    let x = g.pos_from_cursor(egui::text::CCursor::new(a)).left();
    let mut end = g.pos_from_cursor(egui::text::CCursor::new(b)).left();
    if r.end > line_start + len {
        end += 8.;
    }
    if end - x < 2. {
        end = x + 2.;
    }
    ui.painter().rect_filled(
        Rect::from_min_size(pos + egui::vec2(x, 0.), egui::vec2(end - x, ROW)),
        2,
        color,
    );
}
pub(super) fn append_syntax(
    job: &mut egui::text::LayoutJob,
    doc: &wexandroidstudio::editor::RenderSnapshot,
    byte_start: usize,
    text: &str,
) {
    let spans = doc.leaf_spans(byte_start..byte_start + text.len());
    let mut at = 0;
    for (span, kind) in spans {
        let a = span.start.saturating_sub(byte_start).min(text.len());
        let b = span.end.saturating_sub(byte_start).min(text.len());
        if a < at || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        job.append(&text[at..a], 0., format(m::TEXT));
        job.append(&text[a..b], 0., format(token_color(&kind)));
        at = b;
    }
    job.append(&text[at..], 0., format(m::TEXT));
}
pub(super) fn append_display(
    job: &mut egui::text::LayoutJob,
    doc: &wexandroidstudio::editor::RenderSnapshot,
    display: &ropey::Rope,
    range: Range<usize>,
    composition: Option<&editing::Composition>,
) {
    if let Some(c) = composition {
        let inserted = c.text.chars().count();
        let finish = c.range.start + inserted;
        let mut at = range.start;
        while at < range.end {
            let (end, original) = if at < c.range.start {
                (range.end.min(c.range.start), Some(at))
            } else if at < finish {
                (range.end.min(finish), None)
            } else {
                (
                    range.end,
                    Some(at - inserted + (c.range.end - c.range.start)),
                )
            };
            let text = display.slice(at..end).to_string();
            if let Some(original) = original {
                append_syntax(job, doc, doc.rope().char_to_byte(original), &text);
            } else {
                let mut f = format(m::ON_CONTAINER);
                f.background = m::PRIMARY_CONTAINER;
                f.underline = Stroke::new(1.5_f32, m::PRIMARY);
                job.append(&text, 0., f);
            }
            at = end;
        }
    } else {
        append_syntax(
            job,
            doc,
            doc.rope().char_to_byte(range.start),
            &display.slice(range).to_string(),
        );
    }
    if job.text.is_empty() {
        job.append(" ", 0., format(m::TEXT));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(text: &str) -> (tempfile::TempDir, Workbench) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Main.kt"), text).unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let mut app = Workbench::new(dir.path().to_owned(), ctx, None, false);
        app.editor_ui.focus_editor = true;
        (dir, app)
    }
    fn frame(a: &mut Workbench, events: Vec<egui::Event>) -> egui::FullOutput {
        frame_focus(a, events, true)
    }
    fn frame_focus(a: &mut Workbench, events: Vec<egui::Event>, focused: bool) -> egui::FullOutput {
        let ctx = a.context.clone();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 600.))),
                focused,
                events,
                ..Default::default()
            },
            |ctx| {
                a.editor_updates(ctx);
                egui::CentralPanel::default().show(ctx, |ui| a.editor(ui, ctx));
            },
        )
    }
    fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn command_key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    #[test]
    fn same_frame_paste_move_then_f12_and_completion_use_new_rope_snapshot_and_clock() {
        for key in [Key::F12, Key::Space] {
            let (_d, mut a) = app("val answer=7\n");
            frame(&mut a, vec![]);
            let document = language::SnapshotSender::default();
            let mut snapshots = document.subscribe();
            let (requests, mut commands) = tokio::sync::mpsc::channel(8);
            let (_tx, events) = tokio::sync::mpsc::channel(8);
            let task = a.runtime.spawn(std::future::pending());
            a.language = Some(Connection {
                document,
                requests,
                events,
                task,
            });
            let key_mods = if key == Key::Space {
                egui::Modifiers::COMMAND
            } else {
                egui::Modifiers::NONE
            };
            frame(
                &mut a,
                vec![
                    egui::Event::Paste("// typed 👋\n".into()),
                    command_key(Key::End, egui::Modifiers::NONE),
                    command_key(key, key_mods),
                ],
            );
            let request = commands.try_recv().unwrap();
            let (path, version, line, character) = match request {
                Request::Definition {
                    path,
                    version,
                    line,
                    character,
                    ..
                }
                | Request::Complete {
                    path,
                    version,
                    line,
                    character,
                } => (path, version, line, character),
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(line, 1);
            assert_eq!(character, 12);
            assert_eq!(Some(&path), a.doc.path.as_ref());
            assert_eq!(version, a.doc.version);
            let snapshot = a.runtime.block_on(snapshots.latest()).unwrap().unwrap();
            assert_eq!(snapshot.version, version);
            assert_eq!(snapshot.text, "// typed 👋\nval answer=7\n");
            assert!(commands.try_recv().is_err());
        }
    }
    #[test]
    fn focused_semantic_hotkey_follows_edits_instead_of_global_preconsumption() {
        let (_d, mut a) = app("val answer=7\n");
        frame(&mut a, vec![]);
        let document = language::SnapshotSender::default();
        let (requests, mut commands) = tokio::sync::mpsc::channel(8);
        let (_tx, events) = tokio::sync::mpsc::channel(8);
        let task = a.runtime.spawn(std::future::pending());
        a.language = Some(Connection {
            document,
            requests,
            events,
            task,
        });
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 600.))),
                events: vec![
                    egui::Event::Paste("// typed\n".into()),
                    command_key(Key::F12, egui::Modifiers::SHIFT),
                ],
                ..Default::default()
            },
            |ctx| {
                a.semantic_shortcuts(ctx);
                egui::CentralPanel::default().show(ctx, |ui| a.editor(ui, ctx));
            },
        );
        let request = commands.try_recv().unwrap();
        match request {
            Request::Project {
                version,
                line,
                buffers,
                operation: language::ProjectOperation::References,
                ..
            } => {
                assert_eq!(line, 1);
                assert_eq!(version, a.doc.version);
                assert_eq!(buffers[0].text, "// typed\nval answer=7\n");
                assert_eq!(buffers[0].version, version);
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn ordered_shift_f6_rename_is_not_stolen_by_plain_f6_debug_pause() {
        let (_d, mut a) = app("val answer=7\n");
        frame(&mut a, vec![]);
        let document = language::SnapshotSender::default();
        let (requests, mut commands) = tokio::sync::mpsc::channel(8);
        let (_tx, events) = tokio::sync::mpsc::channel(8);
        let task = a.runtime.spawn(std::future::pending());
        a.language = Some(Connection {
            document,
            requests,
            events,
            task,
        });
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 600.))),
                events: vec![
                    egui::Event::Paste("// typed\n".into()),
                    command_key(Key::F6, egui::Modifiers::SHIFT),
                ],
                ..Default::default()
            },
            |ctx| {
                a.semantic_shortcuts(ctx);
                a.debug_shortcuts(ctx);
                egui::CentralPanel::default().show(ctx, |ui| a.editor(ui, ctx));
            },
        );
        let request = commands
            .try_recv()
            .expect("Shift+F6 must reach rename, not Debug Pause");
        match request {
            Request::Project {
                version,
                line,
                operation: language::ProjectOperation::PrepareRename,
                ..
            } => {
                assert_eq!(line, 1);
                assert_eq!(version, a.doc.version);
                assert!(a.refactor.dialog);
            }
            other => panic!("unexpected {other:?}"),
        };
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn mouse_drag_selects_unicode_by_galley_positions() {
        let (_dir, mut a) = app("val привет = 1\nsecond\n");
        frame(&mut a, vec![]);
        let (pos, g) = a.editor_ui.geometry.rows[&0].clone();
        let from = pos
            + egui::vec2(
                g.pos_from_cursor(egui::text::CCursor::new(4)).left() + 0.5,
                10.,
            );
        let to = pos
            + egui::vec2(
                g.pos_from_cursor(egui::text::CCursor::new(10)).left() + 0.5,
                10.,
            );
        frame(
            &mut a,
            vec![egui::Event::PointerMoved(from), pointer(from, true)],
        );
        assert!(a.editor_ui.drag.is_some());
        assert_eq!(a.edit.cursor, 4);
        frame(&mut a, vec![egui::Event::PointerMoved(to)]);
        assert_eq!(a.edit.selected_text(&a.doc), "привет");
        frame(&mut a, vec![pointer(to, false)]);
        assert_eq!(a.edit.selected_text(&a.doc), "привет");
    }
    #[test]
    fn copy_cut_paste_and_undo_use_editor_events() {
        let (_dir, mut a) = app("привет world\n");
        frame(&mut a, vec![]);
        a.edit.select(0, 6);
        let out = frame(&mut a, vec![egui::Event::Copy]);
        assert!(
            out.platform_output
                .commands
                .iter()
                .any(|c| matches!(c,egui::OutputCommand::CopyText(t)if t=="привет"))
        );
        frame(&mut a, vec![egui::Event::Cut]);
        assert_eq!(a.doc.rope().to_string(), " world\n");
        frame(&mut a, vec![egui::Event::Paste("世界".into())]);
        assert_eq!(a.doc.rope().to_string(), "世界 world\n");
    }
    #[test]
    fn ime_is_transient_and_exports_candidate_rect() {
        let (_dir, mut a) = app("val x = hello\n");
        frame(&mut a, vec![]);
        a.edit.select(8, 13);
        let revision = a.doc.revision;
        let out = frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("日本".into())),
            ],
        );
        assert_eq!(a.doc.revision, revision);
        assert_eq!(a.doc.rope().to_string(), "val x = hello\n");
        assert!(out.platform_output.ime.is_some());
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("日本語".into())),
                egui::Event::Text("日本語".into()),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), "val x = 日本語\n");
        assert_eq!(a.doc.revision, revision + 1);
    }
    #[test]
    fn ten_thousand_lines_paint_only_the_viewport() {
        let (_dir, mut a) = app(&(0..10000)
            .map(|n| format!("val item{n} = {n}\n"))
            .collect::<String>());
        frame(&mut a, vec![]);
        assert!(a.editor_ui.rendered_rows < 40);
        a.edit.collapse(a.doc.rope().len_chars());
        a.scroll_to_cursor = true;
        frame(&mut a, vec![]);
        assert!(a.editor_ui.rendered_rows < 40);
        assert!(a.edit.scroll_y > 10000.);
    }
    fn shell_frame(a: &mut Workbench, events: Vec<egui::Event>, width: f32) {
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 560.))),
                events,
                ..Default::default()
            },
            |ctx| a.draw_shell(ctx),
        );
    }
    #[test]
    fn external_reload_confirmation_cancel_accept_and_keep_guard() {
        let (_dir, mut a) = app("original");
        a.reduced_motion = true;
        let path = a.doc.path.clone().unwrap();
        a.apply_edit(0, 8, "local");
        std::fs::write(&path, "remote").unwrap();
        a.editor_ui.conflicts.insert(
            path.clone(),
            DiskState::Present(Box::new(Document::open(&path).unwrap())),
        );
        a.editor_ui.pending_reload = Some(path.clone());
        for _ in 0..4 {
            shell_frame(&mut a, vec![], 920.);
        }
        let pos = a.chrome.hits["reload-cancel"].center();
        shell_frame(
            &mut a,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
            920.,
        );
        shell_frame(&mut a, vec![pointer(pos, false)], 920.);
        assert!(a.editor_ui.pending_reload.is_none());
        assert_eq!(a.doc.rope(), "local");
        a.keep_external(path.clone());
        std::fs::write(&path, "newer").unwrap();
        a.save_document();
        assert!(!a.wait_for_save());
        assert!(a.doc.dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer");
        a.editor_ui.conflicts.insert(
            path.clone(),
            DiskState::Present(Box::new(Document::open(&path).unwrap())),
        );
        a.editor_ui.pending_reload = Some(path.clone());
        for _ in 0..4 {
            shell_frame(&mut a, vec![], 920.);
        }
        let pos = a.chrome.hits["reload-confirm"].center();
        shell_frame(
            &mut a,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
            920.,
        );
        shell_frame(&mut a, vec![pointer(pos, false)], 920.);
        assert!(a.editor_ui.pending_reload.is_none());
        assert_eq!(a.doc.rope(), "newer");
        assert!(!a.doc.dirty());
    }
    #[test]
    fn expanded_find_controls_fit_small_and_wide_windows() {
        for width in [760., 1000., 1440.] {
            let (_dir, mut a) = app("alpha beta");
            a.reduced_motion = true;
            a.start_find(true);
            for _ in 0..4 {
                shell_frame(&mut a, vec![], width);
            }
            let screen = a.context.content_rect();
            for key in [
                "find-previous",
                "find-next",
                "find-close",
                "find-case",
                "find-word",
                "find-regex",
                "find-scope",
                "replace-one",
                "replace-all",
            ] {
                assert!(
                    screen.contains_rect(a.chrome.hits[key]),
                    "{key} clipped at {width}"
                );
            }
            assert!(
                a.editor_ui.geometry.viewport.height() > 60.,
                "code area at {width}"
            );
        }
    }
    #[test]
    fn async_replace_discards_results_after_a_new_edit_or_tab_epoch() {
        for switch_epoch in [false, true] {
            let (_dir, mut a) = app("alpha alpha");
            let id = a
                .editor_ui
                .services
                .replace(
                    a.editor_ui.epoch,
                    &a.doc,
                    &a.edit,
                    SearchOptions {
                        query: "alpha".into(),
                        ..Default::default()
                    },
                    "replaced".into(),
                    None,
                )
                .unwrap();
            a.editor_ui.find.replacing = Some(id);
            if switch_epoch {
                a.editor_ui.epoch += 1;
            } else {
                a.apply_edit(0, 0, "local ");
            }
            let expected = a.doc.rope().clone();
            let deadline = Instant::now() + Duration::from_secs(3);
            while a.editor_ui.find.replacing.is_some() {
                let ctx = a.context.clone();
                a.editor_updates(&ctx);
                assert!(Instant::now() < deadline, "replacement worker timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(a.doc.rope(), &expected);
            assert!(a.status.contains("discarded"));
        }
    }
    #[test]
    fn query_change_invalidates_results_before_debounce_submits_a_new_job() {
        let (_dir, mut a) = app("alpha beta");
        let options = SearchOptions {
            query: "alpha".into(),
            ..Default::default()
        };
        a.editor_ui.find.id =
            a.editor_ui
                .services
                .search(a.editor_ui.epoch, &a.doc, options.clone());
        a.editor_ui.find.submitted = Some((a.editor_ui.epoch, a.doc.revision, options));
        a.editor_ui.find.options.query = "beta".into();
        // Keep the panel closed to leave the previous job as the only worker output.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(10));
            let ctx = a.context.clone();
            a.editor_updates(&ctx);
        }
        assert!(a.editor_ui.find.results.matches.is_empty());
        assert!(!a.valid_find());
    }
    #[test]
    fn async_replace_preserves_a_save_that_advanced_the_disk_baseline() {
        let (_dir, mut a) = app("alpha alpha");
        a.apply_edit(0, 0, "// local\n");
        let id = a
            .editor_ui
            .services
            .replace(
                a.editor_ui.epoch,
                &a.doc,
                &a.edit,
                SearchOptions {
                    query: "alpha".into(),
                    ..Default::default()
                },
                "beta".into(),
                None,
            )
            .unwrap();
        a.editor_ui.find.replacing = Some(id);
        a.doc.save().unwrap();
        let baseline = a.doc.disk_hash();
        let deadline = Instant::now() + Duration::from_secs(3);
        while a.editor_ui.find.replacing.is_some() {
            let ctx = a.context.clone();
            a.editor_updates(&ctx);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.doc.disk_hash(), baseline);
        assert_eq!(a.doc.rope(), "// local\nbeta beta");
        a.doc.save().unwrap();
        assert!(!a.doc.dirty());
    }
    #[test]
    fn long_target_vertical_wait_preserves_following_paste_event_order() {
        let text = format!("// {}\nval tail = 7\n", "x".repeat(50000));
        let (_dir, mut a) = app(&text);
        let tail = a.doc.rope().line_to_char(1);
        a.edit.collapse(tail + 3);
        frame(
            &mut a,
            vec![
                command_key(Key::ArrowUp, egui::Modifiers::NONE),
                egui::Event::Paste("Q".into()),
            ],
        );
        assert!(a.editor_ui.deferred_input.is_some());
        assert_eq!(a.doc.rope().to_string(), text);
        let until = Instant::now() + Duration::from_secs(10);
        while a.editor_ui.deferred_input.is_some() {
            frame(&mut a, vec![]);
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(a.doc.rope().slice(..4).to_string(), "// Q");
        assert_eq!(a.edit.cursor, 4);
        a.doc.undo().unwrap();
        assert_eq!(a.doc.rope().to_string(), text);
    }
    #[test]
    fn long_target_deferred_input_is_never_replayed_into_replacement_document() {
        let text = format!("// {}\nval tail = 7\n", "x".repeat(50000));
        let (_dir, mut a) = app(&text);
        let tail = a.doc.rope().line_to_char(1);
        a.edit.collapse(tail + 3);
        frame(
            &mut a,
            vec![
                command_key(Key::ArrowUp, egui::Modifiers::NONE),
                egui::Event::Paste("OLD".into()),
            ],
        );
        assert!(a.editor_ui.deferred_input.is_some());
        a.doc = Document::new("fresh").unwrap();
        a.edit.collapse(0);
        frame(&mut a, vec![egui::Event::Paste("N".into())]);
        assert_eq!(a.doc.rope().to_string(), "Nfresh");
        assert!(a.editor_ui.deferred_input.is_none());
    }
    #[test]
    fn quick_drag_release_then_paste_in_one_frame_obeys_native_event_order() {
        let (_dir, mut a) = app("// abcdefgh\n");
        frame(&mut a, vec![]);
        let (pos, g) = a.editor_ui.geometry.rows.get(&0).unwrap();
        let start = *pos
            + egui::vec2(
                g.pos_from_cursor(egui::text::CCursor::new(3)).left() + 0.1,
                ROW / 2.,
            );
        let end = *pos
            + egui::vec2(
                g.pos_from_cursor(egui::text::CCursor::new(7)).left() + 0.1,
                ROW / 2.,
            );
        frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(start),
                pointer(start, true),
                egui::Event::PointerMoved(end),
                pointer(end, false),
                egui::Event::Paste("Q".into()),
            ],
        );
        assert_eq!(
            a.doc.rope().to_string(),
            "// Qefgh\n",
            "drag/release must select before subsequent Paste in the same native input frame"
        );
    }
    fn settled(a: &mut Workbench) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            frame(a, vec![]);
            if a.editor_ui.geometry.rows.contains_key(&0) {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn column(a: &Workbench, line: usize, col: usize) -> Pos2 {
        let (p, g) = a.editor_ui.geometry.rows.get(&line).unwrap();
        *p + egui::vec2(
            g.pos_from_cursor(egui::text::CCursor::new(col)).left() + 0.1,
            ROW / 2.,
        )
    }
    fn deferred_drag(a: &mut Workbench, shift: bool) {
        settled(a);
        let start = column(a, 0, 3);
        let end = column(a, 0, 7);
        let at = a.doc.rope().len_chars() - 1;
        a.doc.edit(at..at, "X").unwrap();
        let mut press = pointer(start, true);
        if let egui::Event::PointerButton { modifiers, .. } = &mut press {
            modifiers.shift = shift;
        }
        frame(
            a,
            vec![
                egui::Event::PointerMoved(start),
                press,
                egui::Event::PointerMoved(end),
                pointer(end, false),
                egui::Event::Paste("Q".into()),
            ],
        );
        assert!(a.editor_ui.deferred_input.is_some());
        assert_eq!(a.doc.rope().slice(..11).to_string(), "// abcdefgh");
    }
    fn drain_input(a: &mut Workbench) {
        let until = Instant::now() + Duration::from_secs(10);
        while a.editor_ui.deferred_input.is_some() {
            frame(a, vec![]);
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn pending_long_drag_release_paste_replays_in_order_and_undo_restores_selection() {
        let text = format!("// abcdefgh{}\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        deferred_drag(&mut a, false);
        drain_input(&mut a);
        assert_eq!(a.doc.rope().slice(..8).to_string(), "// Qefgh");
        assert_eq!(a.editor_ui.pointer_metrics.last_selection, Some((3, 7)));
        assert!(
            a.editor_ui.pointer_metrics.deferred > 0 && a.editor_ui.pointer_metrics.resumed > 0
        );
        a.edit.undo(&mut a.doc, false).unwrap();
        assert_eq!(a.edit.range(), 3..7);
        a.edit.undo(&mut a.doc, false).unwrap();
        assert_eq!(a.doc.rope().to_string(), text);
    }
    #[test]
    fn shift_pending_drag_uses_press_modifiers_not_frame_end_modifiers() {
        let text = format!("// abcdefgh{}\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        settled(&mut a);
        a.edit.select(0, 2);
        deferred_drag(&mut a, true);
        drain_input(&mut a);
        assert_eq!(a.editor_ui.pointer_metrics.last_selection, Some((0, 7)));
        assert_eq!(a.doc.rope().slice(..5).to_string(), "Qefgh");
    }
    #[test]
    fn pending_gesture_epoch_switch_cancels_even_if_exact_text_identity_returns() {
        let text = format!("// abcdefgh{}\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        deferred_drag(&mut a, false);
        let captured = a.doc.render_snapshot();
        a.editor_switched();
        assert!(captured.matches(&a.doc));
        frame(&mut a, vec![]);
        assert!(a.editor_ui.deferred_input.is_none());
        assert_eq!(a.doc.rope().slice(..11).to_string(), "// abcdefgh");
        assert!(a.editor_ui.pointer_metrics.cancelled > 0);
    }
    #[test]
    fn focus_loss_cancels_pending_drag_and_paste_without_ghost_write() {
        let text = format!("// abcdefgh{}\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        deferred_drag(&mut a, false);
        frame_focus(&mut a, vec![], false);
        assert!(a.editor_ui.deferred_input.is_none());
        a.editor_ui.focus_editor = true;
        frame(&mut a, vec![egui::Event::Paste("N".into())]);
        assert!(a.doc.rope().to_string().starts_with("N// abcdefgh"));
        assert!(!a.doc.rope().to_string().starts_with("// Q"));
    }
    #[test]
    fn double_click_word_drag_backwards_then_copy_uses_completed_selection() {
        let (_d, mut a) = app("alpha beta\nsecond\n");
        settled(&mut a);
        let start = column(&a, 0, 8);
        let end = column(&a, 0, 2);
        frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(start),
                pointer(start, true),
                pointer(start, false),
            ],
        );
        let output = frame(
            &mut a,
            vec![
                pointer(start, true),
                egui::Event::PointerMoved(end),
                pointer(end, false),
                egui::Event::Copy,
            ],
        );
        assert_eq!(a.edit.anchor, 10);
        assert_eq!(a.edit.cursor, 0);
        assert!(
            output
                .platform_output
                .commands
                .iter()
                .any(|c| matches!(c,egui::OutputCommand::CopyText(s)if s=="alpha beta"))
        );
    }
    #[test]
    fn third_click_line_drag_does_not_wait_for_pending_glyph_geometry() {
        let text = format!("// abcdefgh{}\nsecond\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        settled(&mut a);
        let start = column(&a, 0, 5);
        let end = column(&a, 1, 3);
        for _ in 0..2 {
            frame(
                &mut a,
                vec![
                    egui::Event::PointerMoved(start),
                    pointer(start, true),
                    pointer(start, false),
                ],
            );
        }
        let at = a.doc.rope().len_chars() - 1;
        a.doc.edit(at..at, "X").unwrap();
        frame(
            &mut a,
            vec![
                pointer(start, true),
                egui::Event::PointerMoved(end),
                pointer(end, false),
            ],
        );
        assert!(a.editor_ui.deferred_input.is_none());
        assert_eq!(a.edit.range(), 0..a.doc.rope().len_chars());
    }
    #[test]
    fn entering_editor_then_paste_references_uses_new_clock_and_buffer() {
        let (_d, mut a) = app("val answer=7\n");
        frame(&mut a, vec![]);
        let p = column(&a, 0, 0);
        let document = language::SnapshotSender::default();
        let (requests, mut commands) = tokio::sync::mpsc::channel(8);
        let (_tx, events) = tokio::sync::mpsc::channel(8);
        let task = a.runtime.spawn(std::future::pending());
        a.language = Some(Connection {
            document,
            requests,
            events,
            task,
        });
        let ctx = a.context.clone();
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("foreign")));
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 600.))),
                events: vec![
                    egui::Event::PointerMoved(p),
                    pointer(p, true),
                    pointer(p, false),
                    egui::Event::Paste("// after focus\n".into()),
                    command_key(Key::F12, egui::Modifiers::SHIFT),
                ],
                ..Default::default()
            },
            |ctx| {
                a.semantic_shortcuts(ctx);
                egui::CentralPanel::default().show(ctx, |ui| a.editor(ui, ctx));
            },
        );
        let Request::Project {
            version,
            line,
            buffers,
            ..
        } = commands.try_recv().unwrap()
        else {
            panic!("project request required")
        };
        assert_eq!(
            buffers[0].text, "// after focus\nval answer=7\n",
            "semantic fallback must not capture before the incoming editor click/Paste"
        );
        assert_eq!(version, a.doc.version);
        assert_eq!(line, 1);
        assert!(commands.try_recv().is_err());
    }
    fn ctrl() -> egui::Modifiers {
        egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        }
    }
    fn route_frame(a: &mut Workbench, events: Vec<egui::Event>) {
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 600.))),
                events,
                ..Default::default()
            },
            |ctx| {
                a.editor_updates(ctx);
                a.semantic_shortcuts(ctx);
                a.editor_shortcuts(ctx);
                a.debug_shortcuts(ctx);
                // Production global fallback must see no already-owned CtrlS.
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, Key::S)) {
                    a.save_document();
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    a.find_bar(ui, ctx);
                    a.editor(ui, ctx);
                });
                a.finish_editor_frame(ctx);
                a.document_save_updates();
            },
        );
    }
    fn finish_save(a: &mut Workbench) {
        let end = Instant::now() + Duration::from_secs(5);
        while a.save_ui.pending.is_some() {
            a.document_save_updates();
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn paste_then_save_in_one_frame_writes_new_text_not_previous_snapshot() {
        let (_d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        let p = a.doc.path.clone().unwrap();
        route_frame(
            &mut a,
            vec![
                egui::Event::Paste("// saved\n".into()),
                command_key(Key::S, ctrl()),
            ],
        );
        finish_save(&mut a);
        assert_eq!(std::fs::read_to_string(p).unwrap(), "// saved\nval x=1\n");
        assert!(!a.doc.dirty());
    }
    #[test]
    fn save_before_later_typing_does_not_autosave_later_edit() {
        let (_d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        let p = a.doc.path.clone().unwrap();
        route_frame(
            &mut a,
            vec![command_key(Key::S, ctrl()), egui::Event::Text("x".into())],
        );
        finish_save(&mut a);
        assert_eq!(std::fs::read_to_string(p).unwrap(), "val x=1\n");
        assert_eq!(a.doc.rope().to_string(), "xval x=1\n");
        assert!(a.doc.dirty());
    }
    #[test]
    fn paste_then_f9_sets_breakpoint_at_new_cursor_line() {
        let (_d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        route_frame(
            &mut a,
            vec![
                egui::Event::Paste("// typed\n".into()),
                command_key(Key::F9, egui::Modifiers::NONE),
            ],
        );
        super::super::breakpoint_ui::settle(&mut a);
        assert!(a.debug.breakpoints[a.doc.path.as_ref().unwrap()].contains(&2));
        assert!(!a.debug.breakpoints[a.doc.path.as_ref().unwrap()].contains(&1));
    }
    #[test]
    fn find_handoff_after_code_paste_routes_query_without_editing_source() {
        let (_d, mut a) = app("val x=1\n");
        a.reduced_motion = true;
        frame(&mut a, vec![]);
        route_frame(
            &mut a,
            vec![
                egui::Event::Paste("// code\n".into()),
                command_key(Key::F, ctrl()),
                egui::Event::Text("needle".into()),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), "// code\nval x=1\n");
        assert_eq!(a.editor_ui.find.options.query, "needle");
        route_frame(&mut a, vec![]);
        assert!(
            a.context
                .memory(|m| m.has_focus(egui::Id::new("find-query")))
        );
    }
    #[test]
    fn animated_find_keeps_same_frame_query_until_field_is_drawn() {
        let (_d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        route_frame(
            &mut a,
            vec![
                command_key(Key::F, ctrl()),
                egui::Event::Text("needle".into()),
            ],
        );
        let end = Instant::now() + Duration::from_secs(3);
        while a.editor_ui.find.options.query != "needle" {
            route_frame(&mut a, vec![]);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        assert!(a.editor_ui.find.handoff.is_empty());
    }
    #[test]
    fn incoming_editor_click_then_save_does_not_leak_paste_to_find() {
        let (_d, mut a) = app("val x=1\n");
        a.reduced_motion = true;
        frame(&mut a, vec![]);
        a.start_find(false);
        route_frame(&mut a, vec![egui::Event::Text("query".into())]);
        let p = column(&a, 0, 0);
        let path = a.doc.path.clone().unwrap();
        route_frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(p),
                pointer(p, true),
                pointer(p, false),
                egui::Event::Paste("// code\n".into()),
                command_key(Key::S, ctrl()),
            ],
        );
        finish_save(&mut a);
        assert_eq!(a.editor_ui.find.options.query, "query");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "// code\nval x=1\n");
    }
    #[test]
    fn save_all_captures_dirty_active_and_parked_buffers_after_preceding_input() {
        let (d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        let b = d.path().join("B.kt");
        std::fs::write(&b, "val b=2\n").unwrap();
        let mut doc = Document::open(&b).unwrap();
        doc.defer_syntax();
        doc.edit(0..0, "// parked\n").unwrap();
        let b = doc.path.clone().unwrap();
        a.parked.insert(b.clone(), (doc, EditState::default()));
        a.tabs.push(b.clone());
        route_frame(
            &mut a,
            vec![
                egui::Event::Paste("// active\n".into()),
                command_key(Key::S, ctrl() | egui::Modifiers::SHIFT),
            ],
        );
        finish_save(&mut a);
        assert!(
            std::fs::read_to_string(a.doc.path.as_ref().unwrap())
                .unwrap()
                .starts_with("// active\n")
        );
        assert!(
            std::fs::read_to_string(b)
                .unwrap()
                .starts_with("// parked\n")
        );
        assert!(!a.doc.dirty());
    }
    #[test]
    fn completed_save_preserves_clock_of_already_queued_definition() {
        let (_d, mut a) = app("val x=1\n");
        frame(&mut a, vec![]);
        let (requests, mut commands) = tokio::sync::mpsc::channel(8);
        let (_tx, events) = tokio::sync::mpsc::channel(8);
        a.language = Some(Connection {
            document: language::SnapshotSender::default(),
            requests,
            events,
            task: a.runtime.spawn(std::future::pending()),
        });
        route_frame(
            &mut a,
            vec![
                egui::Event::Paste("// saved\n".into()),
                command_key(Key::S, ctrl()),
                command_key(Key::F12, egui::Modifiers::NONE),
            ],
        );
        let Request::Definition {
            version,
            navigation,
            path,
            ..
        } = commands.try_recv().unwrap()
        else {
            panic!("definition required")
        };
        let sent = a.last_sent.clone();
        finish_save(&mut a);
        assert_eq!(
            a.doc.version, version,
            "baseline-only receipt must not advance the source clock"
        );
        assert_eq!(a.last_sent, sent);
        assert!(a.definition_is_current(navigation, &path, version));
        assert!(matches!(commands.try_recv(), Ok(Request::Saved)));
        assert!(commands.try_recv().is_err());
        assert!(!a.doc.dirty());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "// saved\nval x=1\n"
        );
    }
    #[test]
    fn explicit_terminal_focus_cancels_unapplied_long_row_intent_without_ghost_paste() {
        let text = format!("// {}\nval tail = 7\n", "x".repeat(50000));
        let (_d, mut a) = app(&text);
        let tail = a.doc.rope().line_to_char(1);
        a.edit.collapse(tail + 3);
        route_frame(
            &mut a,
            vec![
                command_key(Key::ArrowUp, egui::Modifiers::NONE),
                egui::Event::Paste("GHOST".into()),
                command_key(Key::F12, egui::Modifiers::ALT),
                egui::Event::Text("printf 'FOCUS_OK\\n'".into()),
                command_key(Key::Enter, egui::Modifiers::NONE),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), text);
        assert!(a.editor_ui.deferred_input.is_none());
        assert!(a.editor_ui.pointer_metrics.cancelled > 0);
        assert_eq!(a.terminal.sessions.len(), 1);
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let f = a.terminal.sessions[0].handle.snapshot().unwrap();
            if f.text().lines().any(|l| l == "FOCUS_OK") {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        let cleanup = a.terminal.sessions[0].handle.cleanup();
        a.terminal.sessions[0].handle.stop();
        while !cleanup.done() {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
