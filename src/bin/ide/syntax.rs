//! Exact-state syntax readiness, never an old tree projected onto newer bytes.
use super::*;
use std::sync::Arc;
use wexandroidstudio::{
    editor::SyntaxSnapshot,
    material::{self as m, ButtonKind as Kind, Icon},
    syntax_jobs::{SyntaxParser, Ticket},
};
pub(super) struct Pending {
    pub snapshot: SyntaxSnapshot,
    pub ticket: Ticket,
}
#[derive(Default)]
pub(super) struct SyntaxUi {
    pub parser: SyntaxParser,
    pub pending: Option<Pending>,
    failed: Option<SyntaxSnapshot>,
    pub error: Option<String>,
    pub last_parse_us: Option<u128>,
}
impl Workbench {
    pub(super) fn document_syntax_step(&mut self) {
        self.doc.defer_syntax();
        for _ in 0..8 {
            let Some(event) = self.syntax_ui.parser.poll() else {
                break;
            };
            if self
                .syntax_ui
                .pending
                .as_ref()
                .is_none_or(|p| p.ticket.id != event.id)
                || !event.snapshot.matches(&self.doc)
            {
                continue;
            }
            self.syntax_ui.pending = None;
            match event.result {
                Ok(time) => {
                    self.syntax_ui.last_parse_us = Some(time.as_micros());
                    self.syntax_ui.failed = None;
                    self.syntax_ui.error = None;
                }
                Err(e) => {
                    self.syntax_ui.failed = Some(event.snapshot);
                    self.syntax_ui.error = Some(e);
                }
            }
        }
        let Some(snapshot) = self.doc.syntax_snapshot() else {
            if self.syntax_ui.pending.take().is_some() {
                self.syntax_ui.parser.cancel();
            }
            self.syntax_ui.failed = None;
            self.syntax_ui.error = None;
            return;
        };
        if self
            .syntax_ui
            .pending
            .as_ref()
            .is_some_and(|p| p.snapshot.matches(&self.doc))
            || self
                .syntax_ui
                .failed
                .as_ref()
                .is_some_and(|p| p.matches(&self.doc))
        {
            return;
        }
        self.syntax_ui.failed = None;
        self.syntax_ui.error = None;
        let ctx = self.context.clone();
        match self
            .syntax_ui
            .parser
            .request(snapshot.clone(), Arc::new(move || ctx.request_repaint()))
        {
            Ok(ticket) => self.syntax_ui.pending = Some(Pending { snapshot, ticket }),
            Err(e) => {
                self.syntax_ui.failed = Some(snapshot);
                self.syntax_ui.error = Some(format!("{e:#}"));
            }
        }
    }
    pub(super) fn syntax_bar(&mut self, ui: &mut egui::Ui) {
        if self.doc.tree().is_some() {
            return;
        }
        let mut retry = false;
        m::card().fill(m::CONTAINER).inner_margin(6).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(error) = &self.syntax_ui.error {
                    ui.add_sized(
                        [(ui.available_width() - 90.).max(60.), 22.],
                        egui::Label::new(RichText::new(format!("Syntax: {error}")).color(m::ERROR))
                            .truncate(),
                    )
                    .on_hover_text_passive(error);
                    retry = self
                        .chrome
                        .button(
                            ui,
                            "syntax-retry",
                            "Retry",
                            Icon::Refresh,
                            Kind::Outline,
                            true,
                            80.,
                            self.reduced_motion,
                        )
                        .clicked();
                } else {
                    if !self.reduced_motion {
                        ui.add(egui::Spinner::new().size(16.).color(m::PRIMARY));
                    }
                    ui.label(
                        RichText::new("Updating syntax · text and selection remain available")
                            .size(12.)
                            .color(m::MUTED),
                    );
                }
            });
        });
        ui.add_space(4.);
        if retry {
            self.syntax_ui.failed = None;
            self.syntax_ui.error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::Ordering, time::Instant};
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "val x=1\n").unwrap();
        std::fs::write(d.path().join("B.kt"), "val b=7\n").unwrap();
        let a = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        (d, a)
    }
    fn ready(a: &mut Workbench) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            a.document_syntax_step();
            if a.doc.tree().is_some() && a.syntax_ui.pending.is_none() {
                return;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn queued_old_result_cannot_replace_newer_text_view_or_clock() {
        let (_d, mut a) = app();
        assert!(!a.syntax_ui.parser.worker_started());
        a.apply_edit(6, 7, "2");
        a.document_syntax_step();
        let done = a.syntax_ui.pending.as_ref().unwrap().ticket.done.clone();
        let end = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
        a.apply_edit(6, 7, "3");
        let clocks = (a.doc.version, a.doc.revision);
        let view = (a.edit.anchor, a.edit.cursor);
        assert!(a.doc.tree().is_none());
        ready(&mut a);
        assert_eq!(a.doc.rope(), "val x=3\n");
        assert_eq!((a.doc.version, a.doc.revision), clocks);
        assert_eq!((a.edit.anchor, a.edit.cursor), view);
    }
    #[test]
    fn tab_switch_and_undo_use_exact_cached_state_not_late_cursor_or_tree() {
        let (_d, mut a) = app();
        a.apply_edit(0, 0, "// pending\n");
        a.document_syntax_step();
        let old = a.doc.path.clone().unwrap();
        let next = a.root.join("B.kt");
        a.open_file_wait(next.clone());
        ready(&mut a);
        assert_eq!(a.doc.path, Some(next));
        assert_eq!(a.doc.rope(), "val b=7\n");
        a.open_file_wait(old);
        a.doc.undo().unwrap();
        assert!(a.doc.tree().is_some());
        ready(&mut a);
        assert!(!a.doc.dirty());
        a.doc.redo().unwrap();
        ready(&mut a);
        assert_eq!(a.doc.rope(), "// pending\nval x=1\n");
    }
    #[test]
    fn ready_project_and_projectless_documents_do_not_start_a_parser() {
        let (_d, mut a) = app();
        ready(&mut a);
        assert!(!a.syntax_ui.parser.worker_started());
        let mut welcome =
            Workbench::with_mode(a.root.clone(), egui::Context::default(), None, false, true);
        welcome.document_syntax_step();
        assert!(welcome.welcome);
        assert!(!welcome.syntax_ui.parser.worker_started());
    }
}
