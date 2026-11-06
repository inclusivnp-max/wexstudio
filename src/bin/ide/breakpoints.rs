//! Source anchors, preference errors and reviewed unresolved markers share M3 controls.
use super::*;
use std::{collections::BTreeSet, sync::Arc};
use wexandroidstudio::{
    breakpoint_store::Action,
    material::{self as m, ButtonKind as Kind, Icon},
    text_tracking::TextId,
};
type Signature = Vec<(PathBuf, TextId, Option<TextId>, Option<blake3::Hash>)>;
pub(super) struct Launch {
    ticket: u64,
    signature: Signature,
    serial: String,
    module: String,
    variant: String,
    navigation: u64,
}
impl Workbench {
    fn open_breakpoint_source(&mut self, row: &wexandroidstudio::breakpoint_store::Row) {
        let report = self.debug.bp_report.as_ref();
        let doc = if self.doc.path.as_ref() == Some(&row.path) {
            Some(&self.doc)
        } else {
            self.parked.get(&row.path).map(|(d, _)| d)
        };
        let exact = doc
            .zip(report.and_then(|r| r.documents.get(&row.path)))
            .is_some_and(|(d, id)| d.text_id() == id);
        let pos = kotlin::Position {
            line: row.line.saturating_sub(1),
            character: 0,
        };
        if row.resolved && exact {
            self.open_at(row.path.clone(), open_ui::Intent::Definition(pos));
        } else if row.resolved
            && let Some(hash) = report
                .and_then(|r| r.sources.get(&row.path))
                .and_then(|h| blake3::Hash::from_hex(h).ok())
        {
            self.open_at(
                row.path.clone(),
                open_ui::Intent::Usage(
                    kotlin::Range {
                        start: pos.clone(),
                        end: pos,
                    },
                    Some(hash),
                ),
            );
        } else {
            self.open_file(row.path.clone());
        }
    }
    pub(super) fn breakpoint_signature(&self) -> Signature {
        let mut result = self
            .parked
            .iter()
            .map(|(p, (d, _))| {
                (
                    p.clone(),
                    d.text_id().clone(),
                    d.saved_text_id().cloned(),
                    d.disk_hash(),
                )
            })
            .collect::<Vec<_>>();
        if let Some(p) = &self.doc.path {
            result.push((
                p.clone(),
                self.doc.text_id().clone(),
                self.doc.saved_text_id().cloned(),
                self.doc.disk_hash(),
            ));
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }
    pub(super) fn breakpoint_request(&mut self, action: Option<Action>) -> bool {
        if self.welcome || self.boot.pending {
            return false;
        }
        let signature = self.breakpoint_signature();
        if action.is_none()
            && (!self.debug.bp_service.active()
                || !self.debug.bp_service.signature_changed(&signature))
        {
            return false;
        }
        let mut docs = self
            .parked
            .iter()
            .map(|(p, (d, _))| (p.clone(), d.tracking_capture()))
            .collect::<Vec<_>>();
        if let Some(p) = &self.doc.path {
            docs.push((p.clone(), self.doc.tracking_capture()));
        }
        let ctx = self.context.clone();
        match self.debug.bp_service.submit(
            docs,
            signature,
            action,
            Arc::new(move || ctx.request_repaint()),
        ) {
            Ok(submitted) => {
                if submitted {
                    self.context.request_repaint();
                }
                submitted
            }
            Err(e) => {
                self.status = format!("Breakpoints: {e:#}");
                false
            }
        }
    }
    pub(super) fn breakpoint_validate_for_debug(&mut self, serial: String) {
        if self.breakpoint_request(Some(Action::Validate)) {
            self.debug.bp_launch = Some(Launch {
                ticket: self.debug.bp_service.requested,
                signature: self.breakpoint_signature(),
                serial,
                module: self.app_module.clone(),
                variant: self.variant.clone(),
                navigation: self.open_ui.navigation,
            });
            self.debug.reason = "Checking saved breakpoint sources…".into();
            self.status = self.debug.reason.clone();
        }
    }
    pub(super) fn breakpoint_step(&mut self) {
        let signature = self.breakpoint_signature();
        if self.debug.bp_launch.as_ref().is_some_and(|p| {
            p.signature != signature
                || Some(&p.serial) != self.selected.as_ref()
                || p.module != self.app_module
                || p.variant != self.variant
                || p.navigation != self.open_ui.navigation
        }) {
            self.debug.bp_launch = None;
            self.status = "Debug cancelled · source/device/variant changed".into();
        }
        self.breakpoint_request(None);
        let Some(mut report) = self.debug.bp_service.poll() else {
            return;
        };
        self.close_ui.received(&report);
        let was_open = self.debug.bp_review.open;
        self.debug.bp_review.received(&report);
        if was_open && !self.debug.bp_review.open {
            self.editor_ui.focus_editor = true;
        }
        let paths = self
            .debug
            .breakpoints
            .keys()
            .chain(report.lines.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let changed = paths
            .into_iter()
            .filter(|p| self.debug.breakpoints.get(p) != report.lines.get(p))
            .collect::<Vec<_>>();
        self.debug.breakpoints = std::mem::take(&mut report.lines);
        for p in changed {
            self.debug.breakpoint_changed(&p);
            self.sync_breakpoints(p);
        }
        let launch = self.debug.bp_launch.take();
        if let Some(launch) = launch {
            if launch.ticket == report.ticket
                && launch.signature == signature
                && report.validated
                && self.save_ui.pending.is_none()
                && !self.doc.dirty()
                && self.parked.values().all(|(d, _)| !d.dirty())
                && self.selected.as_ref() == Some(&launch.serial)
                && self
                    .devices
                    .iter()
                    .any(|d| d.serial == launch.serial && d.usable())
                && !self.running
                && !self.building
                && self.debug.connection.is_none()
                && !self.file_ui.open
                && !self.replace_blocked()
                && !self.git.open
            {
                let sources = report.sources.clone();
                self.install_breakpoint_report(report);
                self.launch_debug_anchors(launch.serial, sources);
                return;
            }
            self.status = report.error.clone().unwrap_or_else(|| {
                "Review unresolved breakpoints before Debug · no app launched".into()
            });
        } else if let Some(e) = &report.error {
            self.status = e.clone();
        }
        self.install_breakpoint_report(report);
    }
    fn install_breakpoint_report(&mut self, report: wexandroidstudio::breakpoint_store::Report) {
        if let Some(old) = self.debug.bp_report.replace(report) {
            self.runtime.spawn_blocking(move || drop(old));
        }
    }
    pub(super) fn breakpoint_marker_current(&self, path: &std::path::Path) -> bool {
        self.debug
            .bp_report
            .as_ref()
            .and_then(|r| r.documents.get(path))
            .is_some_and(|id| id == self.doc.text_id())
    }
    pub(super) fn breakpoint_panel(&mut self, ui: &mut egui::Ui) {
        let pending = self.debug.bp_service.pending();
        let error = self.debug.bp_report.as_ref().and_then(|r| r.error.clone());
        let rows = self
            .debug
            .bp_report
            .as_ref()
            .map(|r| r.rows.clone())
            .unwrap_or_default();
        let recoveries = self
            .debug
            .bp_report
            .as_ref()
            .map(|r| r.recoveries.clone())
            .unwrap_or_default();
        if self.debug.connection.is_some()
            && rows.is_empty()
            && error.is_none()
            && !pending
            && recoveries.is_empty()
        {
            return;
        }
        if self.debug.connection.is_some() {
            ui.horizontal(|ui| {
                ui.small(format!(
                    "{} source markers · stop Debug to review",
                    rows.len()
                ));
                if let Some(e) = &error {
                    ui.add(egui::Label::new(RichText::new(e).color(m::ERROR)).truncate())
                        .on_hover_text_passive(e);
                }
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("Breakpoints · {}", rows.len()))
                    .strong()
                    .color(m::PRIMARY),
            );
            if pending {
                ui.label("Updating source anchors…");
            }
            if self.debug.bp_launch.is_some()
                && self
                    .chrome
                    .button(
                        ui,
                        "bp-cancel-debug",
                        "Cancel Debug",
                        Icon::Close,
                        Kind::Tonal,
                        true,
                        152.,
                        self.reduced_motion,
                    )
                    .clicked()
            {
                self.debug.bp_launch = None;
                self.status = "Debug cancelled · source markers retained".into();
            }
            if self
                .chrome
                .button(
                    ui,
                    "bp-refresh",
                    "Check sources",
                    Icon::Refresh,
                    Kind::Ghost,
                    !pending,
                    152.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.breakpoint_request(Some(Action::Refresh));
            }
        });
        if !recoveries.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("bp-recovery-list")
                .max_height(84.)
                .show_rows(ui, 40., recoveries.len(), |ui, range| {
                    for draft in &recoveries[range] {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(format!(
                                    "Recovery · {} markers{}",
                                    draft.markers,
                                    if draft.issue.is_some() {
                                        " · error"
                                    } else {
                                        ""
                                    }
                                ))
                                .truncate(),
                            )
                            .on_hover_text_passive(draft.issue.as_deref().unwrap_or(&draft.name));
                            if self
                                .chrome
                                .button(
                                    ui,
                                    &format!("bp-recover-{}", draft.name),
                                    "Review draft",
                                    Icon::Refresh,
                                    Kind::Tonal,
                                    !pending && draft.issue.is_none(),
                                    156.,
                                    self.reduced_motion,
                                )
                                .clicked()
                            {
                                self.begin_breakpoint_review(Some(draft.name.clone()));
                            }
                        });
                    }
                });
        }
        if let Some(e) = error {
            ui.add(egui::Label::new(RichText::new(&e).color(m::ERROR)).truncate())
                .on_hover_text_passive(&e);
            ui.horizontal_wrapped(|ui| {
                if self
                    .chrome
                    .button(
                        ui,
                        "bp-retry",
                        "Retry",
                        Icon::Refresh,
                        Kind::Tonal,
                        !pending,
                        100.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    self.breakpoint_request(Some(Action::Retry));
                }
                if self
                    .chrome
                    .button(
                        ui,
                        "bp-review",
                        "Review changes",
                        Icon::Code,
                        Kind::Tonal,
                        !pending,
                        156.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    self.begin_breakpoint_review(None);
                }
            });
        }
        let enabled = !self.running
            && !self.building
            && self.debug.connection.is_none()
            && self.save_ui.pending.is_none()
            && !self.file_ui.open
            && !self.git.open
            && !self.replace_blocked();
        egui::ScrollArea::vertical().id_salt("source-breakpoints").max_height(152.).show_rows(ui,94.,rows.len(),|ui,range|{
            for row in &rows[range]{
                let name=row.path.strip_prefix(&self.root).unwrap_or(&row.path).display().to_string();
                ui.add(egui::Label::new(RichText::new(format!("{}  {name}:{}",if row.resolved{"●"}else{"○"},row.line)).color(if row.resolved{m::ERROR}else{m::MUTED})).truncate()).on_hover_text_passive(row.path.display().to_string());
                ui.add(egui::Label::new(row.issue.as_deref().unwrap_or("Mapped to current code · source text is unchanged")).truncate()).on_hover_text_passive(row.issue.as_deref().unwrap_or("Marker changes do not edit or save source text"));
                ui.horizontal(|ui|{
                    let current=self.doc.path.as_ref()==Some(&row.path);
                    if self.chrome.button(ui,&format!("bp-open-{}",row.id),if row.resolved{"Go"}else{"Open"},Icon::Code,Kind::Ghost,enabled,88.,self.reduced_motion).on_hover_text_passive(if row.resolved{"Go to the source-bound marker"}else{"Open the file without guessing a deleted code location"}).clicked(){self.open_breakpoint_source(row);}
                    if self.chrome.button(ui,&format!("bp-rebind-{}",row.id),"Use caret line",Icon::Code,Kind::Tonal,enabled&&current&&!pending,148.,self.reduced_motion).on_hover_text_passive("Explicitly rebind this marker to the current code line; source text is unchanged").clicked(){
                        self.breakpoint_request(Some(Action::Rebind(row.path.clone(),row.id,self.doc.rope().char_to_line(self.edit.cursor)as u32+1,self.doc.tracking_capture())));
                    }
                    if self.chrome.button(ui,&format!("bp-remove-{}",row.id),"Remove",Icon::Close,Kind::Ghost,enabled&&!pending,96.,self.reduced_motion).clicked(){self.breakpoint_request(Some(Action::Remove(row.path.clone(),row.id)));}
                });
            }
        });
        ui.separator();
    }
}
#[cfg(test)]
pub(super) fn settle(app: &mut Workbench) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        app.breakpoint_step();
        if !app.debug.bp_service.pending() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "breakpoints worker: {}",
            app.status
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Workbench, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Main.kt");
        std::fs::write(&path, "fun f() {\n    println(42)\n}\n").unwrap();
        let app = Workbench::new(dir.path().to_owned(), egui::Context::default(), None, false);
        (dir, app, path)
    }
    #[test]
    fn actual_ui_marker_follows_edit_undo_and_redo_and_hides_stale_projection() {
        let (_dir, mut a, p) = fixture();
        a.toggle_breakpoint(p.clone(), 2);
        settle(&mut a);
        assert!(a.debug.breakpoints[&p].contains(&2));
        assert!(a.breakpoint_marker_current(&p));
        a.doc.edit(0..0, "// 😀\n").unwrap();
        a.did_edit();
        assert!(!a.breakpoint_marker_current(&p));
        settle(&mut a);
        assert_eq!(a.debug.breakpoints[&p], BTreeSet::from([3]));
        a.doc.undo().unwrap();
        a.did_edit();
        settle(&mut a);
        assert_eq!(a.debug.breakpoints[&p], BTreeSet::from([2]));
        a.doc.redo().unwrap();
        a.did_edit();
        settle(&mut a);
        assert_eq!(a.debug.breakpoints[&p], BTreeSet::from([3]));
    }
    #[test]
    fn actual_ui_removed_marker_requires_review_and_rebind_is_not_a_text_edit() {
        let (_dir, mut a, p) = fixture();
        a.toggle_breakpoint(p.clone(), 2);
        settle(&mut a);
        let start = a.doc.rope().line_to_char(1);
        let end = a.doc.rope().line_to_char(2);
        a.doc.edit(start..end, "").unwrap();
        a.did_edit();
        settle(&mut a);
        assert!(!a.debug.breakpoints.contains_key(&p));
        let row = &a.debug.bp_report.as_ref().unwrap().rows[0];
        assert!(!row.resolved && row.issue.as_ref().unwrap().contains("deleted"));
        let id = row.id;
        let before = a.doc.text_id().clone();
        a.breakpoint_request(Some(Action::Rebind(
            p.clone(),
            id,
            1,
            a.doc.tracking_capture(),
        )));
        settle(&mut a);
        assert!(a.debug.breakpoints[&p].contains(&1));
        assert_eq!(a.doc.text_id(), &before);
        a.doc.undo().unwrap();
        a.did_edit();
        settle(&mut a);
        assert!(a.debug.breakpoints[&p].contains(&1));
    }
    #[test]
    fn actual_ui_reconciles_parked_dirty_buffers_and_saved_baselines() {
        let (dir, mut a, p) = fixture();
        a.toggle_breakpoint(p.clone(), 2);
        settle(&mut a);
        let other = dir.path().join("Other.kt");
        std::fs::write(&other, "fun other() {}\n").unwrap();
        let mut d = Document::open(&other).unwrap();
        a.breakpoint_request(Some(Action::Toggle(other.clone(), 1, d.tracking_capture())));
        settle(&mut a);
        d.edit(0..0, "// parked\n").unwrap();
        a.parked.insert(other.clone(), (d, EditState::default()));
        settle(&mut a);
        assert!(a.debug.breakpoints[&other].contains(&2));
        assert!(a.debug.breakpoints[&p].contains(&2));
        a.parked.get_mut(&other).unwrap().0.save().unwrap();
        settle(&mut a);
        let stored: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(".wex/breakpoints.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored["files"][other.to_str().unwrap()][0]["line"], 2);
    }
    #[test]
    fn source_edit_cancels_pending_debug_validation_without_starting_tools_or_device() {
        let (_dir, mut a, p) = fixture();
        a.toggle_breakpoint(p, 2);
        settle(&mut a);
        a.selected = Some("EXACT_TEST_SERIAL".into());
        a.breakpoint_validate_for_debug("EXACT_TEST_SERIAL".into());
        assert!(a.debug.bp_launch.is_some());
        a.doc.edit(0..0, "// later\n").unwrap();
        a.did_edit();
        settle(&mut a);
        assert!(a.debug.bp_launch.is_none() && !a.running && a.run_task.is_none());
    }
    #[test]
    fn accepting_same_hash_save_still_changes_the_saved_revision_signature() {
        let (_dir, mut a, p) = fixture();
        let original = a.doc.rope().to_string();
        a.doc.edit(0..0, "// transient\n").unwrap();
        a.doc.edit(0..13, "").unwrap();
        assert_eq!(a.doc.rope().to_string(), original);
        let signature = a.breakpoint_signature();
        let hash = a.doc.disk_hash();
        let receipt = a.doc.save_snapshot().unwrap().write(|| false).unwrap();
        a.doc.accept_saved(&receipt).unwrap();
        assert_eq!(a.doc.disk_hash(), hash);
        assert_ne!(a.breakpoint_signature(), signature);
        a.toggle_breakpoint(p.clone(), 2);
        settle(&mut a);
        assert!(a.debug.breakpoints[&p].contains(&2));
    }
}
