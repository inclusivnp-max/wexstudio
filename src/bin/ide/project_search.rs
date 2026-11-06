//! M3 Find in Files: persistent unsaved snapshots, cancellable worker and hash-fenced navigation.
use super::*;
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use wexandroidstudio::{
    editor::RenderSnapshot,
    editor_search::SearchOptions,
    material::{self as m, ButtonKind as Kind, Icon},
    project_search::{Overlay, Results, Worker},
};
#[derive(Default)]
pub(super) struct SearchUi {
    pub open: bool,
    pub focus: bool,
    pub handoff: Vec<egui::Event>,
    pub handoff_ready: bool,
    pub options: SearchOptions,
    pub id: u64,
    pub pending: bool,
    pub stale: bool,
    pub note: String,
    pub result: Option<Results>,
    pub rows: Vec<(usize, Option<usize>)>,
    pub selected: Option<usize>,
    pub reveal: Option<usize>,
    pub hits: BTreeMap<String, egui::Rect>,
    pub worker: Worker,
    guards: BTreeMap<PathBuf, RenderSnapshot>,
    changed: Option<Instant>,
    pub navigated: usize,
    pub rejected_navigation: usize,
    pub accepted: usize,
    pub discarded: usize,
}
impl Workbench {
    fn search_buffers(&self) -> BTreeMap<PathBuf, (&Document, bool)> {
        let mut out = self
            .parked
            .iter()
            .map(|(p, (d, _))| (p.clone(), (d, d.dirty())))
            .collect::<BTreeMap<_, _>>();
        if let Some(p) = &self.doc.path {
            out.insert(p.clone(), (&self.doc, self.doc.dirty()));
        }
        out
    }
    fn search_guards_current(&self) -> bool {
        let docs = self.search_buffers();
        docs.len() == self.psearch.guards.len()
            && docs
                .iter()
                .all(|(p, (d, _))| self.psearch.guards.get(p).is_some_and(|s| s.matches(d)))
    }
    pub(super) fn show_project_search(&mut self) {
        if let Some(old) = self.ime.focus(
            super::ime::Target::Widget(Some(egui::Id::new("project-search-query"))),
            false,
        ) {
            self.ime_clear(&old);
        }
        self.cancel_deferred_input();
        self.edit.cancel_composition();
        self.terminal.focus = false;
        self.editor_ui.focus_editor = false;
        self.editor_ui.frame_editor_focus = false;
        self.editor_ui.find.focus = false;
        self.psearch.open = true;
        self.psearch.focus = true;
        self.psearch.handoff_ready = true;
        self.context.request_repaint();
    }
    pub(super) fn close_project_search(&mut self) {
        self.ime_cancel_external();
        self.psearch.open = false;
        self.psearch.focus = false;
        self.psearch.handoff.clear();
        self.cancel_project_search();
        self.editor_ui.focus_editor = true;
    }
    pub(super) fn cancel_project_search(&mut self) {
        self.cancel_project_search_open();
        self.psearch.worker.cancel();
        self.psearch.pending = false;
        self.psearch.stale = true;
        self.psearch.changed = None;
        self.psearch.note = "Cancelled · Refresh to search again".into();
    }
    pub(super) fn invalidate_project_search(&mut self) {
        self.cancel_project_search_open();
        self.psearch.worker.cancel();
        self.psearch.pending = false;
        self.psearch.stale = true;
        self.psearch.changed = Some(Instant::now());
        self.psearch.note = "Changed · refreshing snapshot…".into();
        if self.psearch.options.query.is_empty() {
            self.psearch.result = None;
            self.psearch.rows.clear();
            self.psearch.changed = None;
            self.psearch.note = "Enter text to search the project".into();
        }
    }
    pub(super) fn start_project_search(&mut self) {
        self.psearch.changed = None;
        self.psearch.options.within = None;
        if self.psearch.options.query.is_empty() {
            self.invalidate_project_search();
            return;
        }
        let docs = self.search_buffers();
        let guards = docs
            .iter()
            .map(|(p, (d, _))| (p.clone(), d.render_snapshot()))
            .collect::<BTreeMap<_, _>>();
        let overlays = docs
            .iter()
            .map(|(p, (d, dirty))| {
                (
                    p.clone(),
                    Overlay {
                        text: d.render_snapshot(),
                        dirty: *dirty,
                    },
                )
            })
            .collect();
        let ctx = self.context.clone();
        match self.psearch.worker.request(
            self.root.clone(),
            overlays,
            self.psearch.options.clone(),
            Arc::new(move || ctx.request_repaint()),
        ) {
            Ok(id) => {
                self.psearch.id = id;
                self.psearch.guards = guards;
                self.psearch.pending = true;
                self.psearch.stale = true;
                self.psearch.note = "Searching disk + open buffers…".into();
            }
            Err(e) => {
                self.psearch.pending = false;
                self.psearch.stale = true;
                self.psearch.note = format!("Search error: {e:#}");
            }
        }
    }
    pub(super) fn project_search_step(&mut self, ctx: &egui::Context) {
        if self.psearch.open
            && (self.psearch.pending || (!self.psearch.stale && self.psearch.result.is_some()))
            && !self.search_guards_current()
        {
            self.invalidate_project_search();
        }
        if let Some(done) = self.psearch.worker.poll() {
            if self.psearch.pending && done.id == self.psearch.id && self.search_guards_current() {
                self.psearch.pending = false;
                match done.result {
                    Ok(r) => {
                        self.psearch.note = format!(
                            "{} matches · {} files{}",
                            r.stats.matches,
                            r.files.len(),
                            if r.stats.limited {
                                " · LIMIT reached"
                            } else {
                                ""
                            }
                        );
                        self.psearch.rows = r
                            .files
                            .iter()
                            .enumerate()
                            .flat_map(|(fi, f)| {
                                std::iter::once((fi, None))
                                    .chain((0..f.hits.len()).map(move |mi| (fi, Some(mi))))
                            })
                            .collect();
                        self.psearch.result = Some(r);
                        self.psearch.stale = false;
                        self.psearch.selected = None;
                        self.psearch.accepted += 1;
                    }
                    Err(e) => {
                        self.psearch.stale = true;
                        self.psearch.note = format!("Search error: {e}");
                    }
                }
            } else {
                self.psearch.discarded += 1;
            }
        }
        if self.psearch.open
            && !self.replace_blocked()
            && self
                .psearch
                .changed
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.start_project_search();
        }
        if self.psearch.pending || (self.psearch.changed.is_some() && !self.replace_blocked()) {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
    }
    pub(super) fn search_intent_current(&self, id: u64) -> bool {
        self.psearch.id == id
            && !self.psearch.stale
            && !self.psearch.pending
            && self.search_guards_current()
    }
    /// Called BEFORE activation: stale disk results never change the active document or cursor.
    pub(super) fn search_candidate_current(
        &self,
        id: u64,
        path: &PathBuf,
        hash: blake3::Hash,
        doc: &Document,
    ) -> bool {
        doc.path.as_ref() == Some(path)
            && self.search_intent_current(id)
            && if let Some(guard) = self.psearch.guards.get(path) {
                guard.matches(doc)
            } else {
                doc.content_hash() == hash
            }
    }
    pub(super) fn search_navigation_rejected(&mut self) {
        self.psearch.rejected_navigation += 1;
        self.psearch.note = "Result changed · Refresh before navigating".into();
        self.psearch.stale = true;
        self.psearch.worker.cancel();
        self.psearch.pending = false;
        self.psearch.changed = None;
    }
    pub(super) fn navigate_project_match(&mut self, fi: usize, mi: usize) {
        if !self.search_intent_current(self.psearch.id) {
            self.search_navigation_rejected();
            return;
        }
        let Some(f) = self.psearch.result.as_ref().and_then(|r| r.files.get(fi)) else {
            return;
        };
        let Some(hit) = f.hits.get(mi) else { return };
        let (p, hash, range) = (f.path.clone(), f.hash, hit.range.clone());
        self.open_at(
            p,
            open_ui::Intent::ProjectSearch {
                id: self.psearch.id,
                hash,
                range,
            },
        );
    }
    pub(super) fn search_accept_opened_guard(&mut self, id: u64) {
        if self.psearch.id == id
            && let Some(path) = self.doc.path.clone()
        {
            self.psearch.guards.insert(path, self.doc.render_snapshot());
        }
    }
    pub(super) fn finish_search_navigation(
        &mut self,
        id: u64,
        hash: blake3::Hash,
        range: std::ops::Range<usize>,
    ) {
        let Some(path) = self.doc.path.clone() else {
            return;
        };
        if !self.search_candidate_current(id, &path, hash, &self.doc)
            || range.end > self.doc.rope().len_chars()
        {
            self.search_navigation_rejected();
            return;
        }
        self.psearch.guards.insert(path, self.doc.render_snapshot());
        self.edit.select(range.start, range.end);
        self.scroll_to_cursor = true;
        self.editor_ui.focus_editor = true;
        self.terminal.focus = false;
        self.psearch.focus = false;
        self.psearch.navigated += 1;
        self.status = "Project match · unsaved buffers preserved".into();
    }
    pub(super) fn project_search_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.psearch.hits.clear();
        m::card()
            .inner_margin(12)
            .show(ui, |ui| self.project_search_contents(ui, ctx));
    }
    fn project_search_contents(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Find in Files").size(21.).color(m::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self
                    .chrome
                    .button(
                        ui,
                        "ps-close",
                        "",
                        Icon::Close,
                        Kind::Ghost,
                        true,
                        36.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    self.close_project_search();
                }
            });
        });
        ui.label(
            RichText::new("Project · disk + unsaved tabs")
                .size(11.)
                .color(m::MUTED),
        );
        ui.add_space(8.);
        let handed = !self.psearch.handoff.is_empty() && self.psearch.handoff_ready;
        let saved = if handed {
            let events = std::mem::take(&mut self.psearch.handoff);
            Some(ctx.input_mut(|i| {
                let old = std::mem::replace(&mut i.events, events);
                let pointer = std::mem::take(&mut i.pointer);
                (old, pointer)
            }))
        } else {
            None
        };
        if self.psearch.focus || handed {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("project-search-query")));
            self.psearch.focus = false;
        }
        let enter = ctx.memory(|m| m.has_focus(egui::Id::new("project-search-query")))
            && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Enter);
        let down = ctx.memory(|m| m.has_focus(egui::Id::new("project-search-query")))
            && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::ArrowDown);
        let query = ui.add_sized(
            [ui.available_width(), 40.],
            egui::TextEdit::singleline(&mut self.psearch.options.query)
                .id(egui::Id::new("project-search-query"))
                .char_limit(4096)
                .hint_text("Find in project…  Ctrl+Shift+F")
                .margin(egui::vec2(10., 10.)),
        );
        if let Some((events, pointer)) = saved {
            ctx.input_mut(|i| {
                i.events = events;
                i.pointer = pointer;
            });
        }
        self.psearch.hits.insert("query".into(), query.rect);
        if ui.clip_rect().contains_rect(query.rect) {
            self.psearch.handoff_ready = true;
        }
        if query.has_focus() {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    query.id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                )
            });
        }
        if query.changed() {
            self.invalidate_project_search();
        }
        if enter {
            self.start_project_search();
        }
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            for (label, field) in [
                ("Aa", &mut self.psearch.options.case_sensitive),
                ("Word", &mut self.psearch.options.whole_word),
                (".*", &mut self.psearch.options.regex),
            ] {
                let r = ui.selectable_label(*field, label);
                self.psearch.hits.insert(format!("option-{label}"), r.rect);
                if r.clicked() {
                    *field = !*field;
                    changed = true;
                }
            }
        });
        if changed {
            self.invalidate_project_search();
        }
        ui.horizontal_wrapped(|ui| {
            if self
                .chrome
                .button(
                    ui,
                    "ps-refresh",
                    "Search",
                    Icon::Search,
                    Kind::Filled,
                    !self.psearch.options.query.is_empty(),
                    108.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Refresh disk and all unsaved tabs · Enter")
                .clicked()
            {
                self.start_project_search();
            }
            if self
                .chrome
                .button(
                    ui,
                    "ps-cancel",
                    "Stop",
                    Icon::Stop,
                    Kind::Tonal,
                    self.psearch.pending || self.psearch.changed.is_some(),
                    86.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.cancel_project_search();
            }
            if self.psearch.pending {
                ui.spinner();
            }
        });
        self.project_replace_controls(ui);
        ui.label(
            RichText::new(&self.psearch.note)
                .size(12.)
                .color(if self.psearch.stale {
                    m::MUTED
                } else {
                    m::PRIMARY
                }),
        );
        if let Some(result) = &self.psearch.result {
            let s = &result.stats;
            ui.label(RichText::new(format!("{} searched · {} open · {} skipped",s.files,s.overlays,s.skipped_binary+s.skipped_large+s.skipped_special+s.excluded_overlays)).size(10.).color(m::MUTED)).on_hover_text_passive("UTF-8 text; excludes .git/.wex/.gradle/build/target/node_modules/.verification. Limits: 8 MiB/file, 256 MiB read, 20,000 files, 5,000 matches. No .gitignore rule engine. Refresh for closed-file disk changes.");
            if s.limited {
                ui.colored_label(m::ERROR, "Partial results · refine the query");
            }
            if !s.errors.is_empty() {
                ui.label(
                    RichText::new(format!("{} read errors · hover for details", s.read_errors))
                        .color(m::ERROR),
                )
                .on_hover_text_passive(s.errors.join("\n"));
            }
        }
        let list = egui::Id::new("project-search-results");
        let valid = !self.psearch.stale && !self.psearch.pending;
        if down && valid {
            self.psearch.selected = self.psearch.rows.iter().position(|(_, m)| m.is_some());
            self.psearch.reveal = self.psearch.selected;
            ctx.memory_mut(|m| {
                m.move_focus(egui::FocusDirection::None);
                m.request_focus(list);
            });
        }
        ui.interact(
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 1.)),
            list,
            egui::Sense::focusable_noninteractive(),
        );
        let mut activate = None;
        if ctx.memory(|m| m.has_focus(list)) && valid {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    list,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let direction = if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::ArrowDown) {
                1
            } else if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::ArrowUp) {
                -1
            } else {
                0
            };
            if direction != 0 {
                ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                let indices = self
                    .psearch
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (_, m))| m.map(|_| i))
                    .collect::<Vec<_>>();
                if !indices.is_empty() {
                    let at = self
                        .psearch
                        .selected
                        .and_then(|i| indices.iter().position(|x| *x == i))
                        .unwrap_or(0);
                    let next =
                        (at as isize + direction).clamp(0, indices.len() as isize - 1) as usize;
                    self.psearch.selected = Some(indices[next]);
                    self.psearch.reveal = self.psearch.selected;
                }
            }
            if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Enter) {
                activate = self
                    .psearch
                    .selected
                    .and_then(|i| self.psearch.rows.get(i))
                    .and_then(|(f, m)| m.map(|m| (*f, m)));
            }
        }
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("project-search-scroll")
            .auto_shrink([false, false]);
        if let Some(i) = self.psearch.reveal.take() {
            scroll = scroll.vertical_scroll_offset(i as f32 * (48. + ui.spacing().item_spacing.y));
        }
        scroll.show_rows(ui, 48., self.psearch.rows.len(), |ui, visible| {
            for index in visible {
                let (fi, mi) = self.psearch.rows[index];
                let f = &self.psearch.result.as_ref().unwrap().files[fi];
                let w = ui.available_width();
                let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 48.), egui::Sense::click());
                self.psearch.hits.insert(format!("row-{index}"), rect);
                let active = self.psearch.selected == Some(index);
                let a = self.chrome.spatial(
                    ctx,
                    &format!("ps-row-{index}"),
                    if active {
                        1.
                    } else if r.hovered() {
                        0.4
                    } else {
                        0.
                    },
                    0.,
                    self.reduced_motion,
                );
                ui.painter().rect_filled(
                    rect,
                    12,
                    egui::Color32::from(
                        egui::Rgba::from(m::SURFACE) * (1. - a)
                            + egui::Rgba::from(m::PRIMARY_CONTAINER) * a,
                    ),
                );
                if let Some(mi) = mi {
                    let h = &f.hits[mi];
                    r.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            valid,
                            active,
                            format!("{}:{} {}", h.line, h.column, h.snippet),
                        )
                    });
                    m::label_clipped(
                        ui,
                        egui::Rect::from_min_size(
                            rect.min + egui::vec2(10., 3.),
                            egui::vec2(w - 20., 18.),
                        ),
                        &format!("{}:{}", h.line, h.column),
                        12.,
                        if valid { m::PRIMARY } else { m::MUTED },
                    );
                    m::label_clipped(
                        ui,
                        egui::Rect::from_min_size(
                            rect.min + egui::vec2(10., 23.),
                            egui::vec2(w - 20., 20.),
                        ),
                        &h.snippet,
                        12.,
                        if valid { m::TEXT } else { m::MUTED },
                    );
                    if r.clicked() && valid {
                        self.psearch.selected = Some(index);
                        activate = Some((fi, mi));
                    }
                    r.on_hover_text_passive(format!(
                        "{}:{}:{}\n{}",
                        f.path.display(),
                        h.line,
                        h.column,
                        h.snippet
                    ));
                } else {
                    let title = f
                        .path
                        .strip_prefix(&self.root)
                        .unwrap_or(&f.path)
                        .display()
                        .to_string();
                    let icon_rect = egui::Rect::from_center_size(
                        rect.left_center() + egui::vec2(16., 0.),
                        egui::Vec2::splat(18.),
                    );
                    wexandroidstudio::file_icons::paint(
                        ui, icon_rect, &f.path, false, false, valid,
                    );
                    let label_rect = egui::Rect::from_min_max(
                        rect.min + egui::vec2(32., 14.),
                        rect.max - egui::vec2(8., 14.),
                    );
                    m::label_clipped(
                        ui,
                        label_rect,
                        &format!(
                            "{}{} · {}",
                            if f.unsaved { "● " } else { "" },
                            title,
                            f.hits.len()
                        ),
                        12.,
                        m::MUTED,
                    );
                    r.on_hover_text_passive(format!(
                        "{}{}",
                        f.path.display(),
                        if f.unsaved {
                            "\nUnsaved buffer snapshot"
                        } else {
                            ""
                        }
                    ));
                }
            }
        });
        if let Some((fi, mi)) = activate {
            self.navigate_project_match(fi, mi);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        for (n, t) in [
            ("Main.kt", "val needle = 1\n"),
            ("Park.kt", "val parked = 2\n"),
            ("Closed.txt", "😀 needle\n"),
        ] {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        let ctx = egui::Context::default();
        m::install(&ctx);
        let mut a = Workbench::new(d.path().into(), ctx, None, false);
        a.doc = Document::open(d.path().join("Main.kt")).unwrap();
        a.doc.defer_syntax();
        (d, a)
    }
    fn run(a: &mut Workbench) {
        let ctx = a.context.clone();
        a.project_search_step(&ctx);
        a.document_open_updates();
    }
    fn search(a: &mut Workbench, q: &str) {
        a.psearch.open = true;
        a.psearch.options.query = q.into();
        a.start_project_search();
        let end = Instant::now() + Duration::from_secs(10);
        while a.psearch.pending {
            run(a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
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
                a.project_search_step(ctx);
                a.semantic_shortcuts(ctx);
                a.editor_shortcuts(ctx);
                a.debug_shortcuts(ctx);
                a.draw_shell(ctx);
                a.finish_editor_frame(ctx);
                a.project_search_step(ctx);
                a.ime_finish(ctx);
            },
        );
    }
    fn key(k: Key, m: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }
    #[test]
    fn dirty_active_parked_and_closed_text_navigate_without_save_or_undo_loss() {
        let (d, mut a) = fixture();
        let p = d.path().join("Park.kt");
        let mut doc = Document::open(&p).unwrap();
        doc.defer_syntax();
        doc.edit(0..0, "// parked needle\n").unwrap();
        a.parked.insert(p.clone(), (doc, Default::default()));
        a.tabs.push(p.clone());
        a.doc.edit(0..0, "// active needle\n").unwrap();
        let active = a.doc.render_snapshot();
        search(&mut a, "needle");
        assert_eq!(a.psearch.result.as_ref().unwrap().stats.matches, 4);
        let fi = a
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .position(|f| f.path == p)
            .unwrap();
        a.navigate_project_match(fi, 0);
        assert_eq!(a.doc.path, Some(p.clone()));
        assert_eq!(a.edit.selected_text(&a.doc), "needle");
        assert!(a.doc.dirty());
        assert!(active.matches(&a.parked[&d.path().join("Main.kt")].0));
        assert!(a.doc.undo().unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "val parked = 2\n");
        assert!(a.save_ui.pending.is_none());
    }
    #[test]
    fn closed_file_hash_changed_rejects_before_activation() {
        let (d, mut a) = fixture();
        search(&mut a, "needle");
        let path = a.doc.path.clone();
        let hash = a.doc.content_hash();
        let (fi, _) = a
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .enumerate()
            .find(|(_, f)| f.path.ends_with("Closed.txt"))
            .unwrap();
        std::fs::write(d.path().join("Closed.txt"), "changed needle").unwrap();
        a.navigate_project_match(fi, 0);
        let end = Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(a.doc.path, path);
        assert_eq!(a.doc.content_hash(), hash);
        assert_eq!(a.psearch.rejected_navigation, 1);
        assert!(a.psearch.stale);
    }
    #[test]
    fn closed_file_valid_exact_unicode_character_range_gets_selected() {
        let (_d, mut a) = fixture();
        search(&mut a, "needle");
        let fi = a
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .position(|f| f.path.ends_with("Closed.txt"))
            .unwrap();
        a.navigate_project_match(fi, 0);
        let end = Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(a.doc.path.as_ref().unwrap().ends_with("Closed.txt"));
        assert_eq!(a.edit.range(), 2..8);
        assert_eq!(a.edit.selected_text(&a.doc), "needle");
        assert!(!a.psearch.stale);
        assert_eq!(a.psearch.navigated, 1);
    }
    #[test]
    fn edited_buffer_invalidates_results_and_late_search_completion() {
        let (_d, mut a) = fixture();
        search(&mut a, "needle");
        let path = a.doc.path.clone();
        a.doc.edit(0..0, "// edit\n").unwrap();
        run(&mut a);
        assert!(a.psearch.stale);
        a.navigate_project_match(0, 0);
        assert_eq!(a.doc.path, path);
        assert_eq!(a.psearch.rejected_navigation, 1);
        a.start_project_search();
        a.doc.edit(0..0, "// latest needle\n").unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while a.psearch.worker.active_id() != 0 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(1));
        }
        run(&mut a);
        assert!(a.psearch.stale);
        assert_eq!(a.psearch.accepted, 1);
    }
    #[test]
    fn query_change_fences_async_navigation_and_error_cancel_keep_last_good() {
        let (_d, mut a) = fixture();
        search(&mut a, "needle");
        let origin = a.doc.path.clone();
        let fi = a
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .position(|f| f.path.ends_with("Closed.txt"))
            .unwrap();
        a.navigate_project_match(fi, 0);
        a.psearch.options.query = "[".into();
        a.psearch.options.regex = true;
        a.invalidate_project_search();
        let end = Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(a.doc.path, origin);
        a.start_project_search();
        while a.psearch.pending {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(a.psearch.stale);
        assert!(a.psearch.note.contains("error"));
        assert!(a.psearch.result.is_some());
        a.cancel_project_search();
        assert!(a.psearch.stale);
        assert!(!a.psearch.pending);
    }
    #[test]
    fn incoming_ctrl_shift_f_and_text_in_one_frame_uses_project_query_not_source() {
        let (_d, mut a) = fixture();
        frame(&mut a, vec![]);
        let hash = a.doc.content_hash();
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        frame(
            &mut a,
            vec![
                key(
                    Key::F,
                    egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                ),
                egui::Event::Text("needle".into()),
            ],
        );
        for _ in 0..60 {
            frame(&mut a, vec![]);
            if a.psearch.options.query == "needle" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(a.psearch.open);
        assert_eq!(a.psearch.options.query, "needle");
        assert_eq!(a.doc.content_hash(), hash);
        assert!(!a.editor_ui.find.open);
    }
    #[test]
    fn shortcut_fresh_ime_handoff_stays_in_actual_project_query() {
        let (_d, mut a) = fixture();
        a.reduced_motion = true;
        frame(&mut a, vec![]);
        let hash = a.doc.content_hash();
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        frame(
            &mut a,
            vec![
                key(
                    Key::F,
                    egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                ),
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("日".into())),
            ],
        );
        frame(&mut a, vec![]);
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("日本".into())),
                egui::Event::Ime(egui::ImeEvent::Disabled),
            ],
        );
        assert_eq!(a.psearch.options.query, "日本");
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn project_panel_enter_then_result_keyboard_navigation_is_not_source_input() {
        let (_d, mut a) = fixture();
        a.reduced_motion = true;
        a.show_project_search();
        frame(
            &mut a,
            vec![
                egui::Event::Text("needle".into()),
                key(Key::Enter, egui::Modifiers::NONE),
            ],
        );
        let end = Instant::now() + Duration::from_secs(5);
        while a.psearch.pending {
            frame(&mut a, vec![]);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(a.psearch.options.query, "needle");
        assert!(a.psearch.result.is_some());
        frame(&mut a, vec![key(Key::ArrowDown, egui::Modifiers::NONE)]);
        assert!(a.psearch.selected.is_some());

        frame(&mut a, vec![key(Key::Enter, egui::Modifiers::NONE)]);
        while a.open_ui.pending.is_some() {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(a.edit.selected_text(&a.doc), "needle");
        assert_eq!(a.psearch.navigated, 1);
    }
    #[test]
    fn source_to_existing_project_query_click_keeps_following_text_in_query() {
        let (_d, mut a) = fixture();
        a.reduced_motion = true;
        a.show_project_search();
        frame(&mut a, vec![]);
        let source = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(source));
        let hash = a.doc.content_hash();
        let p = a.psearch.hits["query"].center();
        let click = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(p),
                click(true),
                click(false),
                egui::Event::Text("needle".into()),
            ],
        );
        frame(&mut a, vec![egui::Event::Text("X".into())]);
        assert_eq!(a.psearch.options.query, "needleX");
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn source_composition_cancelled_by_project_search_cannot_ghost_commit_into_query() {
        let (_d, mut a) = fixture();
        a.reduced_motion = true;
        frame(&mut a, vec![]);
        let hash = a.doc.content_hash();
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        frame(
            &mut a,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("候補".into())),
            ],
        );
        frame(
            &mut a,
            vec![
                key(
                    Key::F,
                    egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                ),
                egui::Event::Ime(egui::ImeEvent::Commit("候補".into())),
                egui::Event::Text("needle".into()),
            ],
        );
        frame(&mut a, vec![]);
        assert_eq!(a.psearch.options.query, "needle");
        assert_eq!(a.doc.content_hash(), hash);
    }
    #[test]
    fn closed_result_retargeted_to_identical_external_bytes_is_not_activated() {
        use std::os::unix::fs::symlink;
        let (d, mut a) = fixture();
        let out = tempfile::tempdir().unwrap();
        let external = out.path().join("same.txt");
        std::fs::write(&external, "😀 needle\n").unwrap();
        search(&mut a, "needle");
        let origin = a.doc.path.clone();
        let fi = a
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .position(|f| f.path.ends_with("Closed.txt"))
            .unwrap();
        let path = d.path().join("Closed.txt");
        std::fs::remove_file(&path).unwrap();
        symlink(&external, &path).unwrap();
        a.navigate_project_match(fi, 0);
        let end = Instant::now() + Duration::from_secs(5);
        while a.open_ui.pending.is_some() {
            run(&mut a);
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(a.doc.path, origin);
        assert_eq!(a.psearch.rejected_navigation, 1);
        assert!(!a.parked.contains_key(&external));
    }
}
