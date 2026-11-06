//! UI-owned exact-state cache; font shaping, atlas copy and first mesh index stay off GPU.
use super::edit_ui::append_display;
use super::*;
use std::collections::VecDeque;
use std::sync::Arc;
use wexandroidstudio::editing;
use wexandroidstudio::{
    editor::RenderSnapshot,
    editor_layout::{FontConfig, Layout, Worker},
};
pub(super) const LONG_LINE_BYTES: usize = 16 * 1024;
struct Scope {
    snapshot: RenderSnapshot,
    composition: Option<editing::Composition>,
    font: FontConfig,
}
impl Scope {
    fn same(&self, o: &Self) -> bool {
        self.snapshot.same(&o.snapshot)
            && self.font.same(&o.font)
            && match (&self.composition, &o.composition) {
                (None, None) => true,
                (Some(a), Some(b)) => a.range == b.range && a.text == b.text,
                _ => false,
            }
    }
}
pub(super) struct Prepared {
    pub layout: Arc<Layout>,
    pub texture: egui::TextureHandle,
}
struct Entry {
    line: usize,
    ticket: u64,
    used: u64,
    ready: Option<Arc<Prepared>>,
    staged: Option<Arc<Layout>>,
    error: Option<String>,
}
#[derive(Default)]
pub(super) struct LongLines {
    pub worker: Worker,
    scope: Option<Scope>,
    entries: Vec<Entry>,
    clock: u64,
}
impl LongLines {
    fn begin(&mut self, s: Scope) {
        if self.scope.as_ref().is_none_or(|old| !old.same(&s)) {
            self.worker.invalidate();
            self.entries.clear();
            self.scope = Some(s);
        }
        while let Some(done) = self.worker.poll() {
            if done.generation != self.worker.generation() {
                continue;
            }
            if let Some(e) = self.entries.iter_mut().find(|e| e.ticket == done.id) {
                e.staged = Some(done.layout);
            }
        }
    }
}
impl LongLines {
    pub fn error(&self, line: usize) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.line == line)
            .and_then(|e| e.error.as_deref())
    }
    pub fn retry(&mut self, line: usize) {
        if let Some(i) = self.entries.iter().position(|e| e.line == line) {
            let e = self.entries.swap_remove(i);
            self.worker.cancel(e.ticket);
        }
    }
    pub fn pending(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.ready.is_none() && e.error.is_none())
    }
}

pub(super) struct DeferredInput {
    pub epoch: u64,
    pub view: (usize, usize),
    pub snapshot: RenderSnapshot,
    pub events: VecDeque<pointer_input::Ordered>,
}
impl Workbench {
    pub(super) fn begin_line_layouts(
        &mut self,
        ctx: &egui::Context,
        composition: Option<&editing::Composition>,
    ) {
        self.editor_ui.long_lines.begin(Scope {
            snapshot: self.doc.render_snapshot(),
            composition: composition.cloned(),
            font: FontConfig::current(ctx),
        });
    }

    pub(super) fn line_layout(
        &mut self,
        ctx: &egui::Context,
        display: &ropey::Rope,
        line: usize,
        composition: Option<&editing::Composition>,
    ) -> Option<(Arc<egui::Galley>, Option<Arc<Prepared>>)> {
        let raw = display.line(line);
        let mut len = raw.len_chars();
        while len > 0 && matches!(raw.char(len - 1), '\r' | '\n') {
            len -= 1;
        }
        let start = display.line_to_char(line);
        if raw.len_bytes() < LONG_LINE_BYTES {
            let mut job = egui::text::LayoutJob::default();
            append_display(
                &mut job,
                &self.doc.render_snapshot(),
                display,
                start..start + len,
                composition,
            );
            job.wrap.max_width = f32::INFINITY;
            return Some((ctx.fonts_mut(|f| f.layout_job(job)), None));
        }
        let scope = Scope {
            snapshot: self.doc.render_snapshot(),
            composition: composition.cloned(),
            font: FontConfig::current(ctx),
        };
        let cache = &mut self.editor_ui.long_lines;
        cache.begin(scope);
        cache.clock += 1;
        let clock = cache.clock;
        // Upload an immutable worker atlas by Arc, never copying its pixels on UI.
        for e in &mut cache.entries {
            if let Some(layout) = e.staged.take() {
                e.ready = Some(Arc::new(Prepared {
                    texture: layout.upload(
                        ctx,
                        format!("long-row-{}-{}", cache.worker.generation(), e.ticket),
                    ),
                    layout,
                }));
            }
        }
        if let Some(e) = cache.entries.iter_mut().find(|e| e.line == line) {
            e.used = clock;
            if let Some(error) = &e.error {
                self.status = error.clone();
            }
            return e
                .ready
                .as_ref()
                .map(|p| (p.layout.galley().clone(), Some(p.clone())));
        }
        if cache.entries.len() >= 32 {
            let i = cache
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .unwrap()
                .0;
            let e = cache.entries.swap_remove(i);
            cache.worker.cancel(e.ticket);
        }
        let scope = cache.scope.as_ref().unwrap();
        let snapshot = scope.snapshot.clone();
        let display = display.clone();
        let composition = composition.cloned();
        let wake = ctx.clone();
        let make = move || {
            let mut job = egui::text::LayoutJob::default();
            append_display(
                &mut job,
                &snapshot,
                &display,
                start..start + len,
                composition.as_ref(),
            );
            job.wrap.max_width = f32::INFINITY;
            job
        };
        let (id, error) = match cache.worker.request(
            scope.font.clone(),
            make,
            Arc::new(move || wake.request_repaint()),
        ) {
            Ok(id) => (id, None),
            Err(e) => (0, Some(e.to_string())),
        };
        cache.entries.push(Entry {
            line,
            ticket: id,
            used: clock,
            ready: None,
            staged: None,
            error,
        });
        None
    }
    pub(super) fn line_hit(
        &mut self,
        g: &Arc<egui::Galley>,
        p: Option<&Arc<Prepared>>,
        point: egui::Vec2,
    ) -> egui::text::CCursor {
        if let Some(p) = p {
            p.layout.hit(point)
        } else {
            self.editor_ui.text_mesh.cursor_from_pos(g, point)
        }
    }
}
