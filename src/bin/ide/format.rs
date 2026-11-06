//! Explicit backend formatting is an explicit reviewed, unsaved, one-Undo source transaction.
use super::*;
use std::sync::Arc;
use wexandroidstudio::{
    editor::RenderSnapshot,
    formatting::{Backend, Review},
    material::{self as m, ButtonKind as Kind, Icon},
};

#[derive(Default)]
pub(super) struct FormatUi {
    pub open: bool,
    pub busy: bool,
    pub applying: bool,
    pub id: u64,
    pub review: Option<Arc<Review>>,
    pub error: String,
    pub pending: Option<(u64, Result<Prepared, String>)>,
    focus_cancel: bool,
    guard: Option<Guard>,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
}

struct Guard {
    path: PathBuf,
    text: RenderSnapshot,
    version: i32,
    revision: u64,
    disk_hash: Option<blake3::Hash>,
    model: Option<(u64, u64)>,
}
pub(super) struct Prepared {
    doc: Document,
    anchor: usize,
    cursor: usize,
}
pub(super) fn shortcut(key: Key, m: egui::Modifiers) -> bool {
    key == Key::L
        && (m.matches_exact(egui::Modifiers::COMMAND | egui::Modifiers::ALT)
            || m.matches_exact(egui::Modifiers::CTRL | egui::Modifiers::ALT))
}
impl Workbench {
    pub(super) fn format_available(&self) -> bool {
        self.doc.path.as_ref().is_some_and(|p| {
            Backend::for_path(p).is_ok() && self.semantic_source_eligible(p, &self.doc)
        }) && self
            .language
            .as_ref()
            .is_some_and(|c| !c.task.is_finished())
            && (!self.native_language
                || self
                    .model_status
                    .as_ref()
                    .is_some_and(|s| s.phase == language::ModelPhase::Applied))
            && self.open_ui.pending.is_none()
            && !self.refactor.busy
            && self.debug.connection.is_none()
            && !self.building
            && !self.running
            && self.save_ui.pending.is_none()
    }
    fn format_current(&self) -> bool {
        self.format.guard.as_ref().is_some_and(|g| {
            self.doc.path.as_ref() == Some(&g.path)
                && g.text.matches(&self.doc)
                && self.doc.version == g.version
                && self.doc.revision == g.revision
                && self.doc.disk_hash() == g.disk_hash
                && (!self.native_language
                    || self.model_status.as_ref().is_some_and(|m| {
                        m.phase == language::ModelPhase::Applied
                            && g.model == Some((m.revision, m.attempt))
                    }))
        })
    }
    pub(super) fn start_formatting(&mut self) {
        if self.format.busy || self.format.applying || (!self.format.open && self.editor_blocked())
        {
            return;
        }
        if !self.format_available() {
            self.status =
                "Formatting needs ready Kotlin/Java analysis · stop Debug/build first".into();
            return;
        }
        if self.edit.composition.is_some() {
            self.status = "Finish text composition before formatting".into();
            return;
        }
        let clocks = self
            .parked
            .values()
            .map(|(d, _)| d.version)
            .fold(self.snapshot_clock.max(self.doc.version), i32::max);
        if i32::try_from(self.parked.len() + 1)
            .ok()
            .and_then(|n| clocks.checked_add(n))
            .is_none()
        {
            self.status = "Source version exhausted · reopen before formatting".into();
            return;
        }
        self.ime_cancel_external();
        self.cancel_deferred_input();
        self.send_snapshot();
        let Some(path) = self.doc.path.clone() else {
            return;
        };
        let range = if self.edit.selected() {
            let r = self.edit.range();
            let Ok((al, ac)) = self.doc.lsp_position(r.start) else {
                return;
            };
            let Ok((bl, bc)) = self.doc.lsp_position(r.end) else {
                return;
            };
            Some(kotlin::Range {
                start: kotlin::Position {
                    line: al,
                    character: ac,
                },
                end: kotlin::Position {
                    line: bl,
                    character: bc,
                },
            })
        } else {
            None
        };
        let Some(next) = self.format.id.checked_add(1) else {
            self.status = "Formatting request counter exhausted · reopen workspace".into();
            return;
        };
        self.format.id = next;
        let id = self.format.id;
        let (cancel, rx) = tokio::sync::watch::channel(false);
        self.format.focus_cancel = !self.format.open;
        self.format.open = true;
        self.format.busy = true;
        self.format.error.clear();
        self.format.cancel = Some(cancel);
        self.format.guard = Some(Guard {
            path: path.clone(),
            text: self.doc.render_snapshot(),
            version: self.doc.version,
            revision: self.doc.revision,
            disk_hash: self.doc.disk_hash(),
            model: self.model_status.as_ref().map(|m| (m.revision, m.attempt)),
        });
        self.completions.clear();
        let request = Request::Formatting {
            id,
            path,
            version: self.doc.version,
            range,
            cancel: rx,
        };
        if self
            .language
            .as_ref()
            .is_none_or(|c| c.requests.try_send(request).is_err())
        {
            self.format.busy = false;
            self.format.error = "Analysis request queue is unavailable; Retry".into();
        }
        self.status = format!(
            "{} · preparing preview · buffers remain unchanged",
            Backend::for_path(&self.format.guard.as_ref().unwrap().path)
                .map(|b| b.label())
                .unwrap_or("Formatter")
        );
    }
    pub(super) fn format_reply(&mut self, id: u64, result: Result<Box<Review>, String>) {
        if !self.format.open || id != self.format.id {
            if let Ok(r) = result {
                self.runtime.spawn_blocking(move || drop(r));
            }
            return;
        }
        self.format.busy = false;
        if !self.format_current() {
            if let Ok(r) = result {
                self.runtime.spawn_blocking(move || drop(r));
            }
            self.format.error = "Source or model changed · request a new preview".into();
            return;
        }
        match result {
            Ok(r) => {
                if self.doc.disk_hash() != Some(r.capture.disk_hash) {
                    self.runtime.spawn_blocking(move || drop(r));
                    self.format.error =
                        "Disk source changed · resolve external changes then Retry".into();
                    return;
                }
                self.retire_format_review();
                self.status = if r.unchanged {
                    format!("{} · already formatted", r.backend.label())
                } else {
                    format!(
                        "{} · {} edits ready for review",
                        r.backend.label(),
                        r.edits.len()
                    )
                };
                self.format.review = Some(Arc::from(r));
            }
            Err(e) => {
                self.format.error = e.chars().take(4096).collect();
                self.status = "Formatting stopped · Retry or Cancel".into();
            }
        }
    }
    pub(super) fn retire_format_review(&mut self) {
        if let Some(r) = self.format.review.take() {
            self.runtime.spawn_blocking(move || drop(r));
        }
    }
    pub(super) fn cancel_formatting(&mut self) {
        if let Some(c) = self.format.cancel.take() {
            c.send_replace(true);
        }
        self.format.id = self.format.id.saturating_add(1);
        self.format.open = false;
        self.format.busy = false;
        self.format.applying = false;
        self.retire_format_review();
        self.format.guard = None;
        self.format.error.clear();
        self.editor_ui.focus_editor = true;
        self.status = "Formatting closed · source buffers unchanged".into();
    }
    fn apply_formatting(&mut self) {
        if self.format.busy || self.format.applying || !self.format_current() {
            return;
        }
        let Some(review) = self.format.review.clone().filter(|r| !r.unchanged) else {
            return;
        };
        let before = match self.doc.fork() {
            Ok(d) => d,
            Err(e) => {
                self.format.error = e.to_string();
                return;
            }
        };
        let anchor = self.edit.anchor;
        let cursor = self.edit.cursor;
        let id = self.format.id;
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.format.applying = true;
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Prepared> {
                let doc = review.apply(&before)?;
                let low = anchor.min(cursor);
                let a = review.map_position(anchor, anchor != low || anchor == cursor);
                let c = review.map_position(cursor, cursor != low || anchor == cursor);
                Ok(Prepared {
                    doc,
                    anchor: a,
                    cursor: c,
                })
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::FormattingApplied(id, Box::new(result)))
            })
            .await;
            ctx.request_repaint();
        });
    }
    pub(super) fn formatting_update(&mut self, id: u64, result: Result<Prepared, String>) {
        if self.format.open && id == self.format.id && self.format.pending.is_none() {
            self.format.pending = Some((id, result));
        } else {
            self.runtime.spawn_blocking(move || drop(result));
        }
    }
    pub(super) fn formatting_step(&mut self) {
        if let Some((id, r)) = self.format.pending.take() {
            self.formatting_applied(id, r);
        }
    }
    pub(super) fn formatting_applied(&mut self, id: u64, result: Result<Prepared, String>) {
        if !self.format.open || id != self.format.id {
            self.runtime.spawn_blocking(move || drop(result));
            return;
        }
        self.format.applying = false;
        if !self.format_current() {
            self.format.error =
                "Source history/model changed during Apply · no edits applied".into();
            return;
        }
        match result {
            Ok(p) => {
                self.doc = p.doc;
                self.edit.select(p.anchor, p.cursor);
                self.edit.clamp(&self.doc);
                self.format.open = false;
                self.retire_format_review();
                self.format.guard = None;
                self.format.cancel = None;
                self.editor_ui.focus_editor = true;
                self.did_edit();
                self.sync_watches();
                self.status = "Formatting applied · unsaved · Ctrl+Z undoes the batch".into();
            }
            Err(e) => {
                self.format.error = e.chars().take(4096).collect();
                self.status = "Formatting stopped · source unchanged · Retry or Cancel".into();
            }
        }
    }
    pub(super) fn formatting_dialog(&mut self, ctx: &egui::Context) {
        if !self.format.open {
            return;
        }
        let current = self.format_current();
        if self.format.busy && self.language.as_ref().is_none_or(|c| c.task.is_finished()) {
            self.format.busy = false;
            self.format.error = "Analysis connection ended; reopen analysis then Retry".into();
        }
        let review = self.format.review.clone();
        let busy = self.format.busy || self.format.applying;
        let valid = current && !busy && self.format.error.is_empty();
        let can_apply = valid && review.as_ref().is_some_and(|r| !r.unchanged);
        let retry_allowed = !busy && self.format_available();
        let mut cancel = false;
        let mut retry = false;
        let mut apply = false;
        let ime = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))));
        if !ime && Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::Escape) {
            self.cancel_formatting();
            return;
        }
        if !ime
            && can_apply
            && (Self::consume_exact_key(ctx, egui::Modifiers::COMMAND, Key::Enter)
                || Self::consume_exact_key(ctx, egui::Modifiers::CTRL, Key::Enter))
        {
            apply = true;
        }
        let backend = review
            .as_ref()
            .map(|r| r.backend)
            .or_else(|| {
                self.doc
                    .path
                    .as_ref()
                    .and_then(|p| Backend::for_path(p).ok())
            })
            .unwrap_or(Backend::K2);
        egui::Modal::new(egui::Id::new("format-review")).frame(m::card().fill(m::HIGH).inner_margin(20)).show(ctx,|ui| {
            ui.set_width((ctx.content_rect().width()-64.).clamp(280.,920.));
            ui.heading(if review.as_ref().is_some_and(|r|r.requested.is_some()){"Format selection"}else if backend==Backend::GoogleJava{"Format Java code"}else{"Format Kotlin code"});
            let name=self.doc.path.as_ref().and_then(|p|p.file_name()).map(|s|s.to_string_lossy()).unwrap_or_default();ui.add(egui::Label::new(egui::RichText::new(name).color(m::MUTED)).truncate());
            ui.label(egui::RichText::new(backend.label()).small().color(m::MUTED));
            let body=(ctx.content_rect().height()-220.).clamp(100.,620.);
            egui::ScrollArea::vertical().id_salt("format-body").max_height(body).show(ui,|ui|{
            if busy {ui.horizontal(|ui|{if !self.reduced_motion{ui.spinner();}ui.label(if self.format.applying{"Preparing the unsaved edit…"}else{"The formatter is preparing a preview…"});});}
            if !current{ui.colored_label(m::ERROR,"Preview is outdated · Retry reads the current source/model");}
            if !self.format.error.is_empty(){egui::ScrollArea::vertical().id_salt("format-errors").max_height(72.).show(ui,|ui|{ui.colored_label(m::ERROR,&self.format.error);});}
            if let Some(r)=&review {
                ui.label(if r.unchanged{"Already formatted · no edits to apply".into()}else{format!("{} edits · current buffer only · no Save",r.edits.len())});
                if self.doc.path.as_ref().is_some_and(|p|self.debug.breakpoints.get(p).is_some_and(|b|!b.is_empty())){ui.label("Breakpoints follow surviving code · deleted code requires review in Debug.");}
                if r.outside_selection_changed{ui.label("The formatter also adjusted surrounding text; all returned changes are included below.");}
                if r.abbreviated{ui.label("Long preview lines are abbreviated at400 characters.");}
                if r.crlf.0!=r.crlf.1{ui.label(format!("CRLF line endings: {} → {}",r.crlf.0,r.crlf.1));}
                ui.label(egui::RichText::new("− Before    /    + After · line numbers may shift").small().color(m::MUTED));
                let height=(body-100.).clamp(60.,400.);
                egui::ScrollArea::both().id_salt("format-preview").max_height(height).show_rows(ui,44.,r.rows.len(),|ui,rows| {
                    ui.spacing_mut().item_spacing.y=0.;ui.set_min_width((r.preview_columns as f32*12.).max(ui.available_width()));
                    for n in rows {
                        let row=&r.rows[n];let width=ui.available_width().max(600.);let(rect,_)=ui.allocate_exact_size(egui::vec2(width,44.),egui::Sense::hover());
                        let before=format!("− {:>5}  {}",row.before_line.map(|n|n.to_string()).unwrap_or_default(),row.before);
                        let after=format!("+ {:>5}  {}",row.after_line.map(|n|n.to_string()).unwrap_or_default(),row.after);
                        let p=ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));p.text(rect.min+egui::vec2(4.,4.),egui::Align2::LEFT_TOP,before,egui::FontId::monospace(11.),if row.changed{m::ERROR}else{m::MUTED});p.text(rect.min+egui::vec2(4.,22.),egui::Align2::LEFT_TOP,after,egui::FontId::monospace(11.),if row.changed{m::PRIMARY}else{m::MUTED});
                    }
                });
            }
            });
            ui.add_space(12.);
            ui.horizontal(|ui| {
                let b=self.chrome.button(ui,"format-cancel","Cancel · Esc",Icon::Close,Kind::Ghost,true,132.,self.reduced_motion);if self.format.focus_cancel{b.request_focus();self.format.focus_cancel=false;}
                if b.clicked()&&!ime{cancel=true;}
                if self.chrome.button(ui,"format-retry","Retry",Icon::Refresh,Kind::Tonal,retry_allowed,96.,self.reduced_motion).clicked()&&!ime{retry=true;}
                if self.chrome.button(ui,"format-apply","Apply · Ctrl+Enter",Icon::Check,Kind::Filled,can_apply,188.,self.reduced_motion).clicked()&&!ime{apply=true;}
            });
        });
        if cancel {
            self.cancel_formatting();
        } else if retry {
            self.start_formatting();
        } else if apply {
            self.apply_formatting();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wexandroidstudio::formatting::Capture;
    fn app() -> (tempfile::TempDir, Workbench, Arc<Review>) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("Main.kt");
        std::fs::write(&p, "val x=1\n").unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let mut a = Workbench::new(d.path().to_owned(), ctx, None, false);
        let r=Capture::read(d.path(),&p,a.doc.rope().to_string(),a.doc.version).unwrap().review(&serde_json::json!([{"range":{"start":{"line":0,"character":5},"end":{"line":0,"character":5}},"newText":" "}]),None).unwrap();
        a.format.open = true;
        a.format.id = 7;
        a.format.guard = Some(Guard {
            path: p,
            text: a.doc.render_snapshot(),
            version: a.doc.version,
            revision: a.doc.revision,
            disk_hash: a.doc.disk_hash(),
            model: None,
        });
        let r = Arc::new(r);
        a.format.review = Some(r.clone());
        (d, a, r)
    }
    #[test]
    fn cancel_wins_over_queued_apply_and_late_response() {
        let (_d, mut a, r) = app();
        let before = a.doc.rope().to_string();
        let doc = r.apply(&a.doc).unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        a.format.cancel = Some(tx);
        a.formatting_update(
            7,
            Ok(Prepared {
                doc,
                anchor: 0,
                cursor: 0,
            }),
        );
        a.cancel_formatting();
        a.formatting_step();
        a.format_reply(7, Ok(Box::new((*r).clone())));
        assert!(*rx.borrow());
        assert_eq!(a.doc.rope().to_string(), before);
        assert!(!a.format.open);
    }
    #[test]
    fn newer_text_and_model_disable_review() {
        let (_d, mut a, r) = app();
        a.doc.edit(0..0, "// changed\n").unwrap();
        assert!(!a.format_current());
        a.format_reply(7, Ok(Box::new((*r).clone())));
        assert!(!a.format.error.is_empty());
        assert!(a.doc.rope().to_string().starts_with("// changed"));
        let (_d, mut a, r) = app();
        a.native_language = true;
        a.model_status = Some(language::ModelStatus {
            revision: 1,
            attempt: 1,
            phase: language::ModelPhase::Applied,
            detail: String::new(),
            retained: false,
        });
        a.format.guard.as_mut().unwrap().model = Some((1, 1));
        assert!(a.format_current());
        a.model_status.as_mut().unwrap().phase = language::ModelPhase::Warming;
        assert!(!a.format_current());
        a.format_reply(7, Ok(Box::new((*r).clone())));
        assert!(!a.format.error.is_empty());
    }
    #[test]
    fn actual_apply_worker_external_writer_error_preserves_source() {
        let (_d, mut a, _r) = app();
        let before = a.doc.rope().to_string();
        std::fs::write(a.doc.path.as_ref().unwrap(), "val x=2\n").unwrap();
        a.apply_formatting();
        let end = std::time::Instant::now() + Duration::from_secs(8);
        while a.format.applying {
            assert!(std::time::Instant::now() < end);
            while let Ok(update) = a.rx.try_recv() {
                if let Update::FormattingApplied(id, r) = update {
                    a.formatting_update(id, *r);
                }
            }
            a.formatting_step();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!a.format.error.is_empty());
        assert_eq!(a.doc.rope().to_string(), before);
    }
    #[test]
    fn minimum_error_and_preview_keep_footer_inside_viewport() {
        let (_d, mut a, _r) = app();
        a.format.error = "Long source failure ".repeat(200);
        let mut review = (**a.format.review.as_ref().unwrap()).clone();
        review.abbreviated = true;
        review.outside_selection_changed = true;
        review.requested = Some(kotlin::Range {
            start: kotlin::Position {
                line: 0,
                character: 0,
            },
            end: kotlin::Position {
                line: 0,
                character: 5,
            },
        });
        a.format.review = Some(Arc::new(review));
        let ctx = a.context.clone();
        for _ in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760., 560.),
                    )),
                    ..Default::default()
                },
                |ctx| a.formatting_dialog(ctx),
            );
        }
        for name in ["format-cancel", "format-retry", "format-apply"] {
            let hit = a.chrome.hits[name];
            assert!(ctx.content_rect().contains_rect(hit), "{name}: {hit:?}");
            assert!(hit.height() >= 36.);
        }
    }
    #[test]
    fn resized_modal_footer_is_inside_actual_settled_layout_in_both_modes() {
        for reduced in [false, true] {
            let (_d, mut a, _r) = app();
            a.reduced_motion = reduced;
            let ctx = a.context.clone();
            for size in [egui::vec2(1400., 900.), egui::vec2(760., 560.)] {
                for _ in 0..4 {
                    let _ = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ctx| a.formatting_dialog(ctx),
                    );
                }
                for name in ["format-cancel", "format-retry", "format-apply"] {
                    assert!(
                        ctx.content_rect().contains_rect(a.chrome.hits[name]),
                        "reduced={reduced} size={size:?} {name}={:?}",
                        a.chrome.hits[name]
                    );
                }
            }
        }
    }
    #[test]
    fn shortcut_is_exact_and_ime_commit_enter_does_not_apply() {
        assert!(shortcut(
            Key::L,
            egui::Modifiers::CTRL | egui::Modifiers::ALT
        ));
        assert!(!shortcut(
            Key::L,
            egui::Modifiers::CTRL | egui::Modifiers::ALT | egui::Modifiers::SHIFT
        ));
        let (_d, mut a, _) = app();
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760., 560.),
                )),
                events: vec![
                    egui::Event::Ime(egui::ImeEvent::Commit("x".into())),
                    egui::Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::CTRL,
                    },
                ],
                ..Default::default()
            },
            |ctx| a.formatting_dialog(ctx),
        );
        assert!(a.format.open);
        assert!(!a.format.applying);
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
    }
}
