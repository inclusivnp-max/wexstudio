//! Explicit preference/recovery review. Rendering never reads project or journal files.
use super::*;
use std::collections::BTreeMap;
use wexandroidstudio::{
    breakpoint_store::{Action, ReviewMode as Mode, ReviewSide as Side},
    material::{self as m, ButtonKind as Kind, Icon},
};
#[derive(Default)]
pub(super) struct Ui {
    pub open: bool,
    // The model can arrive after this frame's paint. Old control rectangles are
    // not evidence that the newly received review has appeared on screen.
    pub painted: Option<u64>,
    id: Option<u64>,
    mode: Mode,
    choices: BTreeMap<u64, Side>,
    applying: Option<u64>,
    recovery: Option<String>,
    merge_count: usize,
}
impl Ui {
    pub(super) fn result_summary(
        &self,
        view: &wexandroidstudio::breakpoint_store::Review,
    ) -> (usize, usize) {
        match self.mode {
            Mode::Local => (view.local_count, 0),
            Mode::Disk => (view.disk_count, 0),
            Mode::Merge => (
                self.merge_count,
                view.conflicts.saturating_sub(self.choices.len()),
            ),
        }
    }
    fn choose(&mut self, row: &wexandroidstudio::breakpoint_store::Difference, side: Side) {
        let before = row
            .selected(Mode::Merge, self.choices.get(&row.id).copied())
            .flatten()
            .is_some() as usize;
        self.choices.insert(row.id, side);
        let after = row.selected(Mode::Merge, Some(side)).flatten().is_some() as usize;
        self.merge_count = self.merge_count - before + after;
    }

    pub fn received(&mut self, report: &wexandroidstudio::breakpoint_store::Report) {
        if let Some(outcome) = &report.review_outcome
            && self.applying == Some(outcome.id)
        {
            self.applying = None;
            if outcome.error.is_none() {
                self.open = false;
            }
        }

        if self.id != report.review.as_ref().map(|r| r.id) {
            self.painted = None;
            self.id = report.review.as_ref().map(|r| r.id);
            self.mode = Mode::Merge;
            self.choices.clear();
            self.merge_count = report
                .review
                .as_ref()
                .map_or(0, |r| r.merge_unconflicted_count);
        }
    }
}
impl Workbench {
    pub(super) fn begin_breakpoint_review(&mut self, recovery: Option<String>) {
        if self.running
            || self.building
            || self.debug.connection.is_some()
            || self.save_ui.pending.is_some()
            || self.file_ui.open
            || self.git.open
            || self.projects.open
            || self.chrome.settings
            || self.replace_blocked()
        {
            return;
        }
        self.debug.bp_launch = None;
        if self.breakpoint_request(Some(Action::Review(recovery.clone()))) {
            self.debug.bp_review = Ui {
                open: true,
                recovery,
                ..Default::default()
            };
        }
    }
    pub(super) fn breakpoint_review_dialog(&mut self, ctx: &egui::Context) {
        if !self.debug.bp_review.open {
            return;
        }
        let pending = self.debug.bp_service.pending();
        let applying = self.debug.bp_review.applying.is_some();
        let view = self.debug.bp_report.as_ref().and_then(|r| r.review.clone());
        let error = self.debug.bp_report.as_ref().and_then(|r| r.error.clone());
        let mut cancel = false;
        let mut apply = false;
        let mut refresh = false;
        let response=egui::Modal::new(egui::Id::new("breakpoint-review")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-112.).clamp(320.,560.));
            ui.heading("Review breakpoint changes");
            ui.label(RichText::new("Source files and editor Undo remain unchanged").color(m::MUTED));
            if let Some(view)=&view {
                ui.label(format!("Local {} · Disk {} · Conflicts {}",view.local_count,view.disk_count,view.conflicts));
                if view.recovery.is_some(){ui.colored_label(m::PRIMARY,"Recovery draft · changes were not saved at the previous close");}
                ui.horizontal_wrapped(|ui|{
                    for(mode,label,key)in[(Mode::Merge,"Merge","bp-mode-merge"),(Mode::Local,"Keep only local","bp-mode-local"),(Mode::Disk,"Use only disk","bp-mode-disk")]{
                        let active=self.debug.bp_review.mode==mode;
                        if self.chrome.button(ui,key,label,Icon::Code,if active{Kind::Filled}else{Kind::Tonal},!pending,150.,self.reduced_motion).clicked(){self.debug.bp_review.mode=mode;}
                    }
                });
                let(count,unresolved)=self.debug.bp_review.result_summary(view);
                ui.label(RichText::new(if unresolved==0{format!("Result: {count} markers")}else{format!("Selected: {count} markers · choose {unresolved} conflicts")}).color(m::PRIMARY));
                ui.small(match self.debug.bp_review.mode{Mode::Merge=>"Unchanged sides merge automatically. Choose a side for every conflicting identity.",Mode::Local=>"Replace the reviewed disk markers with only this local snapshot (including local removals).",Mode::Disk=>"Discard the reviewed local marker changes and keep only the disk snapshot."});
                let height=(ctx.content_rect().height()-358.).clamp(68.,320.);
                egui::ScrollArea::vertical().id_salt("breakpoint-review-rows").max_height(height).show_rows(ui,152.,view.differences.len(),|ui,range|{
                    for row in &view.differences[range]{
                        ui.push_id(row.id,|ui|{
                            ui.label(RichText::new(format!("Marker {}{}",row.id,if row.conflict{" · conflict"}else{" · mergeable"})).color(if row.conflict{m::ERROR}else{m::PRIMARY}));
                            for(label,entry)in[("Base",&row.base),("Local",&row.local),("Disk",&row.disk)] {
                                let text=entry.as_ref().map(|e|format!("{label}: {}:{}{}",e.path.strip_prefix(&self.root).unwrap_or(&e.path).display(),e.marker.line,if e.marker.span.is_none(){" · unresolved"}else{""})).unwrap_or_else(||format!("{label}: removed / absent"));
                                ui.add(egui::Label::new(RichText::new(&text).small()).truncate()).on_hover_text_passive(text);
                            }
                            let result=match row.selected(self.debug.bp_review.mode,self.debug.bp_review.choices.get(&row.id).copied()){
                                None=>"Result: choose Local or Disk".into(),Some(None)=>"Result: remove marker".into(),Some(Some(e))=>format!("Result: keep {}:{}",e.path.strip_prefix(&self.root).unwrap_or(&e.path).display(),e.marker.line),
                            };
                            ui.add(egui::Label::new(RichText::new(&result).small().color(m::PRIMARY)).truncate()).on_hover_text_passive(result);
                            if row.conflict && self.debug.bp_review.mode==Mode::Merge {
                                ui.horizontal(|ui|{for(side,label)in[(Side::Local,"Local"),(Side::Disk,"Disk")]{
                                    let chosen=self.debug.bp_review.choices.get(&row.id)==Some(&side);
                                    if self.chrome.button(ui,&format!("bp-choice-{}-{side:?}",row.id),label,Icon::Code,if chosen{Kind::Filled}else{Kind::Tonal},!pending,100.,self.reduced_motion).clicked(){self.debug.bp_review.choose(row,side);}
                                }});
                            }else{ui.add_space(34.);}
                        });
                    }
                });
            }else{ui.label(if pending{"Reading the exact disk snapshot…"}else{"Review expired or could not be prepared. Retry after repairing the reported input."});}
            if let Some(error)=&error{ui.add(egui::Label::new(RichText::new(error).small().color(m::ERROR)).truncate()).on_hover_text_passive(error);}
            ui.separator();
            ui.horizontal_wrapped(|ui|{
                if self.chrome.button(ui,"bp-review-cancel","Cancel",Icon::Close,Kind::Ghost,!applying,100.,self.reduced_motion).clicked(){cancel=true;}
                if self.chrome.button(ui,"bp-review-refresh","Read again",Icon::Refresh,Kind::Tonal,!pending,140.,self.reduced_motion).clicked(){refresh=true;}
                let ready=view.as_ref().is_some_and(|v|self.debug.bp_review.mode!=Mode::Merge||self.debug.bp_review.choices.len()==v.conflicts);
                if self.chrome.button(ui,"bp-review-apply",if applying{"Saving…"}else{"Apply markers"},Icon::Save,Kind::Filled,ready&&!pending,166.,self.reduced_motion).clicked(){apply=true;}
            });
        });
        self.debug.bp_review.painted = view.as_ref().map(|v| v.id);
        let escape = response.is_top_modal
            && !response.any_popup_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if (cancel || escape) && !applying {
            self.debug.bp_review.open = false;
            self.editor_ui.focus_editor = true;
            self.breakpoint_request(Some(Action::CancelReview));
        } else if refresh {
            let recovery = view
                .as_ref()
                .and_then(|r| r.recovery.clone())
                .or_else(|| self.debug.bp_review.recovery.clone());
            self.breakpoint_request(Some(Action::Review(recovery)));
        } else if apply
            && let Some(view) = view
            && self.breakpoint_request(Some(Action::ApplyReview(
                view.id,
                self.debug.bp_review.mode,
                self.debug.bp_review.choices.clone(),
            )))
        {
            self.debug.bp_review.applying = Some(view.id);
        }

        if pending {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_count_tracks_explicit_deletion_and_repeated_choices_without_row_scans() {
        use wexandroidstudio::breakpoint_store::{Difference, Review, StoredMarker};
        let entry = wexandroidstudio::breakpoint_store::Entry {
            path: PathBuf::from("/fixture/A.kt"),
            marker: StoredMarker {
                id: 1,
                line: 1,
                source_hash: None,
                span: None,
                reason: None,
            },
        };
        let row = Difference {
            id: 1,
            base: Some(entry.clone()),
            local: None,
            disk: Some(wexandroidstudio::breakpoint_store::Entry {
                marker: StoredMarker {
                    line: 2,
                    ..entry.marker.clone()
                },
                ..entry
            }),
            conflict: true,
        };
        let view = Review {
            id: 1,
            differences: vec![row.clone()],
            local_count: 0,
            disk_count: 1,
            conflicts: 1,
            merge_unconflicted_count: 0,
            recovery: None,
        };
        let mut ui = Ui::default();
        assert_eq!(ui.result_summary(&view), (0, 1));
        ui.choose(&row, Side::Disk);
        assert_eq!(ui.result_summary(&view), (1, 0));
        ui.choose(&row, Side::Disk);
        assert_eq!(ui.result_summary(&view), (1, 0));
        ui.choose(&row, Side::Local);
        assert_eq!(ui.result_summary(&view), (0, 0));
        ui.mode = Mode::Disk;
        assert_eq!(ui.result_summary(&view), (1, 0));
        ui.mode = Mode::Local;
        assert_eq!(ui.result_summary(&view), (0, 0));
    }
    #[test]
    fn preview_cancel_and_apply_preserve_dirty_source_undo_and_block_deployment() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("A.kt");
        let original = "target();\n";
        std::fs::write(&path, original).unwrap();
        let ctx = egui::Context::default();
        let mut app = Workbench::new(root.path().to_owned(), ctx, None, false);
        app.breakpoint_request(Some(Action::Toggle(
            path.clone(),
            1,
            app.doc.tracking_capture(),
        )));
        eprintln!(
            "REVIEW_TEST_STAGE_1 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        app.doc.edit(0..0, "// dirty\n").unwrap();
        eprintln!(
            "REVIEW_TEST_STAGE_2 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        let text = app.doc.rope().to_string();
        let id = app.doc.text_id().clone();
        let version = app.doc.version;
        std::fs::write(
            root.path().join(".wex/breakpoints.json"),
            "{\"schema\":2,\"files\":{}}",
        )
        .unwrap();
        let bytes = std::fs::read(root.path().join(".wex/breakpoints.json")).unwrap();
        app.begin_breakpoint_review(None);
        eprintln!(
            "REVIEW_TEST_STAGE_3 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        assert!(app.debug.bp_review.open);
        let review = app
            .debug
            .bp_report
            .as_ref()
            .unwrap()
            .review
            .clone()
            .unwrap();
        app.build();
        app.run();
        app.start_debug();
        app.start_preview();
        assert!(!app.building && !app.running && !app.debug.busy && !app.preview.busy);
        assert!(app.save_ui.pending.is_none());
        assert_eq!(app.doc.rope().to_string(), text);
        assert_eq!(app.doc.text_id(), &id);
        assert_eq!(app.doc.version, version);
        assert_eq!(
            std::fs::read(root.path().join(".wex/breakpoints.json")).unwrap(),
            bytes
        );
        app.debug.bp_review.open = false;
        app.breakpoint_request(Some(Action::CancelReview));
        eprintln!(
            "REVIEW_TEST_STAGE_4 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        assert_eq!(
            std::fs::read(root.path().join(".wex/breakpoints.json")).unwrap(),
            bytes
        );
        app.begin_breakpoint_review(None);
        eprintln!(
            "REVIEW_TEST_STAGE_5 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        let next = app
            .debug
            .bp_report
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
            .id;
        assert!(next > review.id);
        app.breakpoint_request(Some(Action::ApplyReview(next, Mode::Disk, BTreeMap::new())));
        app.debug.bp_review.applying = Some(next);
        app.breakpoint_request(Some(Action::Refresh));
        eprintln!(
            "REVIEW_TEST_STAGE_6 requested={} delivered={}",
            app.debug.bp_service.requested, app.debug.bp_service.delivered
        );
        breakpoint_ui::settle(&mut app);
        assert!(!app.debug.bp_review.open);
        assert!(app.debug.bp_report.as_ref().unwrap().rows.is_empty());
        assert_eq!(app.doc.text_id(), &id);
        assert!(app.doc.undo().unwrap());
        assert_eq!(app.doc.rope().to_string(), original);
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
