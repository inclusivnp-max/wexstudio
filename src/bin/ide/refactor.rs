//! Project semantic actions: actual K2 requests, reviewed multi-buffer transactions.
use super::*;
use anyhow::Context;
use std::collections::BTreeMap;
use wexandroidstudio::{
    language::{ProjectOperation, ProjectResponse},
    material::{self as m, ButtonKind as Kind, Icon},
    workspace_edit::{Capture, Plan},
};
#[derive(Default)]
pub(super) struct RefactorUi {
    pub id: u64,
    pub busy: bool,
    pub dialog: bool,
    pub name: String,
    pub error: String,
    pub origin: Option<(PathBuf, u32, u32)>,
    pub capture: Option<Capture>,
    pub plan: Option<Plan>,
    pub usages: Vec<(PathBuf, kotlin::Range, String)>,
    undo: Option<Vec<(PathBuf, Document, blake3::Hash)>>,
}
pub(super) struct Prepared {
    capture: Capture,
    docs: Vec<(PathBuf, Document, Document)>,
}
impl Workbench {
    fn project_rope_buffers(&self) -> Vec<language::RopeBuffer> {
        self.doc
            .path
            .iter()
            .filter(|p| self.semantic_source_eligible(p, &self.doc))
            .map(|p| language::RopeBuffer {
                path: p.clone(),
                text: self.doc.rope().clone(),
                version: self.doc.version,
            })
            .chain(
                self.parked
                    .iter()
                    .filter(|(p, (d, _))| self.semantic_source_eligible(p, d))
                    .map(|(p, (d, _))| language::RopeBuffer {
                        path: p.clone(),
                        text: d.rope().clone(),
                        version: d.version,
                    }),
            )
            .collect()
    }
    #[cfg(test)]
    fn project_buffers(&self) -> Vec<wexandroidstudio::workspace_edit::Buffer> {
        self.project_rope_buffers()
            .iter()
            .map(|b| wexandroidstudio::workspace_edit::Buffer {
                path: b.path.clone(),
                text: b.text.to_string(),
                version: b.version,
            })
            .collect()
    }
    fn matches_capture(&self, capture: &Capture) -> bool {
        self.doc
            .path
            .iter()
            .map(|p| (p, &self.doc))
            .chain(self.parked.iter().map(|(p, (d, _))| (p, d)))
            .all(|(p, d)| {
                capture
                    .sources
                    .get(p)
                    .is_none_or(|s| s.hash == d.content_hash())
            })
    }
    pub(super) fn project_action(&mut self, operation: ProjectOperation) {
        if self.replace_blocked()
            || self.git.open
            || !self
                .doc
                .path
                .as_ref()
                .is_some_and(|p| self.semantic_source_eligible(p, &self.doc))
        {
            return;
        }
        if self.native_language
            && !self
                .model_status
                .as_ref()
                .is_some_and(|s| s.phase == language::ModelPhase::Applied)
        {
            self.status =
                "Project model is updating/stale · retry semantic action after refresh".into();
            return;
        }
        if self.open_ui.pending.is_some() || self.refactor.busy || self.building || self.running {
            return;
        }
        if !matches!(operation, ProjectOperation::References) && self.debug.connection.is_some() {
            self.status = "Stop Debug before renaming sources".into();
            return;
        }
        self.send_snapshot();
        let Some(path) = self.doc.path.clone() else {
            return;
        };
        let Ok((line, character)) = self.doc.lsp_position(self.edit.cursor) else {
            return;
        };
        let (path, line, character) = if matches!(operation, ProjectOperation::Rename(_)) {
            let Some(origin) = self.refactor.origin.clone() else {
                return;
            };
            origin
        } else {
            (path, line, character)
        };
        if self.doc.path.as_ref() != Some(&path) {
            self.refactor.error = "Return to the original source to preview rename".into();
            return;
        }
        let buffers = self.project_rope_buffers();
        self.refactor.id += 1;
        let id = self.refactor.id;
        if matches!(operation, ProjectOperation::PrepareRename) {
            self.refactor.dialog = true;
            self.refactor.name.clear();
            self.refactor.plan = None;
            self.refactor.origin = Some((path.clone(), line, character));
            self.completions.clear();
        }
        self.refactor.error.clear();
        self.refactor.busy = true;
        let request = Request::Project {
            id,
            operation,
            path,
            version: self.doc.version,
            line,
            character,
            buffers,
        };
        if self
            .language
            .as_ref()
            .is_none_or(|c| c.requests.try_send(request).is_err())
        {
            self.refactor.busy = false;
            self.refactor.error = "K2 is not ready; retry after analysis starts".into();
            self.status = self.refactor.error.clone();
        }
    }
    pub(super) fn project_reply(&mut self, id: u64, result: Result<ProjectResponse, String>) {
        if id != self.refactor.id {
            return;
        }
        self.refactor.busy = false;
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                self.refactor.error = e.clone();
                self.status = e;
                return;
            }
        };
        if !self.matches_capture(&response.capture) {
            self.refactor.error = "Open buffers changed; request analysis again".into();
            self.status = self.refactor.error.clone();
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            match response.operation {
                ProjectOperation::PrepareRename => {
                    anyhow::ensure!(
                        !response.value.is_null(),
                        "No renameable symbol at the caret"
                    );
                    let placeholder = response.value["placeholder"].as_str().map(str::to_owned);
                    let range = response.value.get("range").unwrap_or(&response.value);
                    self.refactor.name = if let Some(p) = placeholder {
                        p
                    } else {
                        let r: kotlin::Range = serde_json::from_value(range.clone())?;
                        let a = self.doc.char_at_lsp(r.start.line, r.start.character)?;
                        let b = self.doc.char_at_lsp(r.end.line, r.end.character)?;
                        self.doc.rope().slice(a..b).to_string()
                    };
                    self.refactor.capture = Some(response.capture);
                    self.context
                        .memory_mut(|m| m.request_focus(egui::Id::new("rename-name")));
                }
                ProjectOperation::Rename(_) => {
                    self.refactor.plan =
                        Some(response.plan.context("missing validated rename plan")?);
                    self.refactor.capture = Some(response.capture);
                }
                ProjectOperation::References => {
                    self.refactor.usages.clear();
                    let values = if response.value.is_null() {
                        vec![]
                    } else {
                        response
                            .value
                            .as_array()
                            .context("invalid references result")?
                            .clone()
                    };
                    anyhow::ensure!(values.len() <= 100_000, "references exceed 100000");
                    for value in values {
                        let path = kotlin::uri_path(
                            value["uri"].as_str().context("missing reference URI")?,
                        )?;
                        let range: kotlin::Range = serde_json::from_value(value["range"].clone())?;
                        let snippet = response
                            .capture
                            .sources
                            .get(&path)
                            .and_then(|s| s.text.lines().nth(range.start.line as usize))
                            .unwrap_or("")
                            .trim()
                            .to_owned();
                        self.refactor.usages.push((path, range, snippet));
                    }
                    self.refactor
                        .usages
                        .sort_by_key(|(p, r, _)| (p.clone(), r.start.line, r.start.character));
                    self.refactor
                        .usages
                        .dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
                    self.refactor.capture = Some(response.capture);
                    self.tab = 4;
                    self.chrome.dock_open = true;
                    self.status = format!("K2 · {} symbol usages", self.refactor.usages.len());
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.refactor.error = format!("{e:#}");
            self.status = self.refactor.error.clone();
        }
    }
    fn apply_refactor(&mut self) {
        let (Some(capture), Some(plan)) =
            (self.refactor.capture.clone(), self.refactor.plan.clone())
        else {
            return;
        };
        if self.refactor.busy {
            return;
        }
        if !self.matches_capture(&capture) {
            self.refactor.error = "Buffers changed; preview again".into();
            return;
        }
        let mut open = BTreeMap::new();
        let result = (|| -> anyhow::Result<()> {
            for f in &plan.files {
                let d = if self.doc.path.as_ref() == Some(&f.path) {
                    Some(&self.doc)
                } else {
                    self.parked.get(&f.path).map(|(d, _)| d)
                };
                if let Some(d) = d {
                    open.insert(f.path.clone(), d.fork()?);
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.refactor.error = e.to_string();
            return;
        }
        self.refactor.busy = true;
        let id = self.refactor.id;
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Prepared> {
                capture.verify_disk()?;
                let mut docs = Vec::new();
                for f in plan.files {
                    let before = match open.remove(&f.path) {
                        Some(d) => d,
                        None => Document::open(&f.path)?,
                    };
                    let after = f.apply(&before)?;
                    docs.push((f.path, before, after));
                }
                capture.verify_disk()?;
                Ok(Prepared { capture, docs })
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::RefactorApplied(id, Box::new(result)))
            })
            .await;
            ctx.request_repaint();
        });
    }
    pub(super) fn refactor_applied(&mut self, id: u64, result: Result<Prepared, String>) {
        if id != self.refactor.id {
            return;
        }
        self.refactor.busy = false;
        let prepared = match result {
            Ok(p) => p,
            Err(e) => {
                self.refactor.error = e.clone();
                self.status = e;
                return;
            }
        };
        if !self.matches_capture(&prepared.capture) {
            self.refactor.error = "Open buffers changed during apply; no edits applied".into();
            return;
        }
        for (path, before, _) in &prepared.docs {
            let current = if self.doc.path.as_ref() == Some(path) {
                Some(&self.doc)
            } else {
                self.parked.get(path).map(|(d, _)| d)
            };
            if current.is_some_and(|d| {
                d.revision != before.revision || d.disk_hash() != before.disk_hash()
            }) {
                self.refactor.error =
                    "Buffer history or disk baseline changed during apply; no edits applied".into();
                return;
            }
        }
        let mut undo = Vec::new();
        for (path, before, after) in prepared.docs {
            undo.push((path.clone(), before, after.content_hash()));
            if self.doc.path.as_ref() == Some(&path) {
                self.doc = after;
                self.edit.clamp(&self.doc);
            } else {
                let edit = self
                    .parked
                    .remove(&path)
                    .map(|(_, e)| e)
                    .unwrap_or_default();
                self.parked.insert(path.clone(), (after, edit));
                if !self.tabs.contains(&path) {
                    self.tabs.push(path);
                }
            }
        }
        self.status = format!(
            "Renamed in {} files · unsaved · Undo rename available",
            undo.len()
        );
        self.refactor.undo = Some(undo);
        self.refactor.dialog = false;
        self.refactor.plan = None;
        self.refactor.usages.clear();
        self.did_edit();
        self.sync_watches();
        self.last_sent = None;
        self.send_snapshot();
    }
    fn undo_refactor(&mut self) {
        let Some(undo) = self.refactor.undo.as_ref() else {
            return;
        };
        let result: anyhow::Result<Vec<(PathBuf, Document)>> = undo
            .iter()
            .map(|(p, before, hash)| {
                let current = if self.doc.path.as_ref() == Some(p) {
                    Some(&self.doc)
                } else {
                    self.parked.get(p).map(|(d, _)| d)
                }
                .context("A renamed tab was closed; project undo is no longer available")?;
                anyhow::ensure!(
                    current.content_hash() == *hash,
                    "Renamed buffer changed; use its normal undo history"
                );
                let mut d = before.fork()?;
                d.acknowledge_disk(Some(current));
                d.revision = current
                    .revision
                    .checked_add(1)
                    .context("document revision exhausted")?;
                d.version = current
                    .version
                    .checked_add(1)
                    .context("document version exhausted")?;
                Ok((p.clone(), d))
            })
            .collect();
        match result {
            Ok(docs) => {
                for (p, d) in docs {
                    if self.doc.path.as_ref() == Some(&p) {
                        self.doc = d;
                        self.edit.clamp(&self.doc);
                    } else if let Some((old, e)) = self.parked.get_mut(&p) {
                        *old = d;
                        e.clamp(old);
                    }
                }
                self.refactor.undo = None;
                self.did_edit();
                self.last_sent = None;
                self.send_snapshot();
                self.sync_watches();
                self.status = "Project rename undone · buffers restored".into();
            }
            Err(e) => self.status = e.to_string(),
        }
    }
    pub(super) fn semantic_shortcuts(&mut self, ctx: &egui::Context) {
        self.dispatch_editor_frame(ctx);
        // A focused editor dispatches these in event order, after preceding edits/moves.
        if self
            .editor_ui
            .input_id
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
        {
            return;
        }

        if (self.file_ui.open || self.projects.open || self.chrome.settings || self.variant_open)
            || self.chrome.device_open
            || self.pending_close.is_some()
            || self.editor_ui.pending_reload.is_some()
            || self.refactor.dialog
        {
            return;
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::COMMAND | egui::Modifiers::ALT, Key::L)
            || Self::consume_exact_key(ctx, egui::Modifiers::CTRL | egui::Modifiers::ALT, Key::L)
        {
            self.start_formatting();
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::SHIFT, Key::F6) {
            self.project_action(ProjectOperation::PrepareRename);
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::SHIFT, Key::F12) {
            self.project_action(ProjectOperation::References);
        }
    }
    pub(super) fn usages_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} usages · K2", self.refactor.usages.len()))
                    .color(m::PRIMARY),
            );
            if self
                .chrome
                .button(
                    ui,
                    "usages-refresh",
                    "Find usages",
                    Icon::Search,
                    Kind::Tonal,
                    !self.refactor.busy,
                    136.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.project_action(ProjectOperation::References);
            }
            if self
                .chrome
                .button(
                    ui,
                    "rename-undo",
                    "Undo rename",
                    Icon::Refresh,
                    Kind::Ghost,
                    self.refactor.undo.is_some(),
                    142.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.undo_refactor();
            }
        });
        if self.refactor.busy {
            ui.label("K2 is resolving symbols…");
        }
        if !self.refactor.error.is_empty() {
            ui.colored_label(m::ERROR, &self.refactor.error);
        }
        let mut selected = None;
        egui::ScrollArea::both()
            .id_salt("semantic-usages")
            .show_rows(ui, 48., self.refactor.usages.len(), |ui, range| {
                for i in range {
                    let (p, r, text) = &self.refactor.usages[i];
                    let label = format!(
                        "{}:{}:{}",
                        p.strip_prefix(&self.root).unwrap_or(p).display(),
                        r.start.line + 1,
                        r.start.character + 1
                    );
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width().max(100.), 44.),
                        egui::Sense::click(),
                    );
                    let hover = self.chrome.motion.value(
                        ui.ctx(),
                        format!("usage-hover-{i}"),
                        if response.hovered() || response.has_focus() {
                            1.
                        } else {
                            0.
                        },
                        0.,
                        m::MotionToken::Effects,
                        self.reduced_motion,
                    );
                    ui.painter().rect_filled(
                        rect,
                        12,
                        m::mix(m::CONTAINER, m::PRIMARY_CONTAINER, hover * 0.55),
                    );
                    m::label_clipped(
                        ui,
                        egui::Rect::from_min_size(
                            rect.min + egui::vec2(10., 2.),
                            egui::vec2(rect.width() - 20., 18.),
                        ),
                        &label,
                        11.,
                        m::PRIMARY,
                    );
                    ui.painter().with_clip_rect(rect.shrink(8.)).text(
                        rect.min + egui::vec2(10., 24.),
                        egui::Align2::LEFT_CENTER,
                        text,
                        FontId::monospace(12.),
                        m::TEXT,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label)
                    });
                    response
                        .clone()
                        .on_hover_text_passive(format!("{label}\n{text}"));
                    self.chrome.hits.insert(format!("usage-{i}"), rect);
                    if response.clicked() {
                        selected = Some(i);
                    }
                }
            });
        if let Some(i) = selected {
            let (p, r, _) = self.refactor.usages[i].clone();
            let expected = self
                .refactor
                .capture
                .as_ref()
                .and_then(|c| c.sources.get(&p))
                .map(|s| s.hash);
            self.open_at(p, open_ui::Intent::Usage(r, expected));
        }
    }
    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context) {
        if !self.refactor.dialog {
            return;
        }
        let mut cancel = false;
        let mut preview = false;
        let mut apply = false;
        egui::Modal::new(egui::Id::new("semantic-rename"))
            .frame(m::card().fill(m::HIGH).inner_margin(24))
            .show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 80.).clamp(280., 620.));
                ui.heading("Rename symbol");
                ui.label(
                    RichText::new("K2 semantic refactoring · review before applying")
                        .color(m::MUTED),
                );
                ui.add_space(12.);
                let response = ui.add_enabled(
                    !self.refactor.busy,
                    egui::TextEdit::singleline(&mut self.refactor.name)
                        .id(egui::Id::new("rename-name"))
                        .desired_width(f32::INFINITY),
                );
                self.chrome.hits.insert("rename-name".into(), response.rect);
                if response.changed() {
                    self.refactor.plan = None;
                }
                if self.refactor.busy {
                    ui.label("Resolving project symbols…");
                }
                if !self.refactor.error.is_empty() {
                    ui.colored_label(m::ERROR, &self.refactor.error);
                }
                if let Some(plan) = &self.refactor.plan {
                    ui.label(format!(
                        "{} text edits in {} files · buffers only, disk unchanged",
                        plan.edit_count,
                        plan.files.len()
                    ));
                    egui::ScrollArea::vertical()
                        .id_salt("rename-preview")
                        .max_height((ctx.content_rect().height() * 0.38).min(320.))
                        .show(ui, |ui| {
                            for f in &plan.files {
                                ui.label(
                                    RichText::new(
                                        f.path
                                            .strip_prefix(&self.root)
                                            .unwrap_or(&f.path)
                                            .display()
                                            .to_string(),
                                    )
                                    .color(m::PRIMARY),
                                );
                                let before = f.before.text.lines().collect::<Vec<_>>();
                                let after = f.after.lines().collect::<Vec<_>>();
                                for row in (0..before.len().max(after.len()))
                                    .filter(|i| before.get(*i) != after.get(*i))
                                    .take(500)
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} − {}",
                                            row + 1,
                                            before.get(row).unwrap_or(&"")
                                        ))
                                        .monospace()
                                        .color(m::ERROR),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "{} + {}",
                                            row + 1,
                                            after.get(row).unwrap_or(&"")
                                        ))
                                        .monospace()
                                        .color(m::SUCCESS),
                                    );
                                }
                            }
                        });
                }
                ui.add_space(16.);
                ui.horizontal_wrapped(|ui| {
                    preview = self
                        .chrome
                        .button(
                            ui,
                            "rename-preview",
                            "Preview",
                            Icon::Search,
                            Kind::Tonal,
                            !self.refactor.busy && !self.refactor.name.trim().is_empty(),
                            110.,
                            self.reduced_motion,
                        )
                        .clicked();
                    apply = self
                        .chrome
                        .button(
                            ui,
                            "rename-apply",
                            "Apply",
                            Icon::Check,
                            Kind::Filled,
                            !self.refactor.busy && self.refactor.plan.is_some(),
                            100.,
                            self.reduced_motion,
                        )
                        .clicked();
                    cancel = self
                        .chrome
                        .button(
                            ui,
                            "rename-cancel",
                            "Cancel",
                            Icon::Close,
                            Kind::Ghost,
                            true,
                            100.,
                            self.reduced_motion,
                        )
                        .clicked();
                });
            });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.refactor.id += 1;
            self.refactor.dialog = false;
            self.refactor.busy = false;
            self.refactor.plan = None;
        } else if preview {
            self.project_action(ProjectOperation::Rename(self.refactor.name.clone()));
        } else if apply {
            self.apply_refactor();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "fun greet() = 7\n").unwrap();
        std::fs::write(d.path().join("B.kt"), "val x = greet()\n").unwrap();
        let c = egui::Context::default();
        m::install(&c);
        let a = Workbench::new(d.path().to_owned(), c, None, false);
        (d, a)
    }
    fn prepared(a: &Workbench) -> Prepared {
        let capture = Capture::read(&a.root, &a.project_buffers()).unwrap();
        let ap = a.root.join("A.kt");
        let bp = a.root.join("B.kt");
        let edit = |start: u32, end: u32| json!({"range":{"start":{"line":0,"character":start},"end":{"line":0,"character":end}},"newText":"welcome"});
        let plan=capture.plan(&json!({"changes":{kotlin::file_uri(&ap).unwrap():[edit(4,9)],kotlin::file_uri(&bp).unwrap():[edit(8,13)]}})).unwrap();
        let docs = plan
            .files
            .iter()
            .map(|f| {
                let d = Document::open(&f.path).unwrap();
                let next = f.apply(&d).unwrap();
                (f.path.clone(), d, next)
            })
            .collect();
        Prepared { capture, docs }
    }
    #[test]
    fn parked_edits_reach_workspace_snapshot_without_churning_unchanged_tabs() {
        let (_d, mut a) = app();
        let document = language::SnapshotSender::default();
        let mut snapshots = document.subscribe();
        let (requests, _) = tokio::sync::mpsc::channel(8);
        let (_, events) = tokio::sync::mpsc::channel(8);
        let task = a.runtime.spawn(std::future::pending());
        a.language = Some(Connection {
            document,
            requests,
            events,
            task,
        });
        let original = a.doc.path.clone().unwrap();
        a.send_snapshot();
        a.open_file_wait(a.root.join("B.kt"));
        let old = a
            .runtime
            .block_on(snapshots.latest())
            .unwrap()
            .as_ref()
            .unwrap()
            .others
            .iter()
            .find(|b| b.path == original)
            .unwrap()
            .version;
        a.doc.edit(0..0, "// active\n").unwrap();
        a.send_snapshot();
        assert_eq!(
            a.runtime
                .block_on(snapshots.latest())
                .unwrap()
                .as_ref()
                .unwrap()
                .others
                .iter()
                .find(|b| b.path == original)
                .unwrap()
                .version,
            old
        );
        a.parked
            .get_mut(&original)
            .unwrap()
            .0
            .edit(0..0, "// parked\n")
            .unwrap();
        a.send_snapshot();
        let latest = a.runtime.block_on(snapshots.latest()).unwrap();
        let parked = latest
            .as_ref()
            .unwrap()
            .others
            .iter()
            .find(|b| b.path == original)
            .unwrap();
        assert!(parked.version > old && parked.text.starts_with("// parked"));
    }
    #[test]
    fn multi_file_commit_and_project_undo_preserve_disk_and_all_buffers() {
        let (_d, mut a) = app();
        let p = prepared(&a);
        a.refactor_applied(0, Ok(p));
        assert!(a.doc.dirty());
        assert!(a.parked.values().all(|(d, _)| d.dirty()));
        assert!(a.doc.rope().to_string().contains("welcome"));
        let revision = a.doc.revision;
        a.undo_refactor();
        assert!(a.doc.revision > revision);
        assert!(a.doc.rope().to_string().contains("greet"));
        assert!(!a.doc.dirty());
        assert!(a.parked.values().all(|(d, _)| !d.dirty()));
        assert_eq!(
            std::fs::read_to_string(a.root.join("B.kt")).unwrap(),
            "val x = greet()\n"
        );
    }
    #[test]
    fn changed_buffer_rejects_whole_prepared_transaction() {
        let (_d, mut a) = app();
        let p = prepared(&a);
        a.doc.edit(0..0, "// edit\n").unwrap();
        let before = a.doc.rope().to_string();
        a.refactor_applied(0, Ok(p));
        assert_eq!(a.doc.rope().to_string(), before);
        assert!(a.parked.is_empty());
        assert!(a.refactor.error.contains("changed"));
    }
    #[test]
    fn project_undo_refuses_partial_revert_after_one_file_was_edited() {
        let (_d, mut a) = app();
        let p = prepared(&a);
        a.refactor_applied(0, Ok(p));
        a.doc.edit(0..0, "//new\n").unwrap();
        a.undo_refactor();
        assert!(
            a.parked
                .values()
                .all(|(d, _)| d.rope().to_string().contains("welcome"))
        );
        assert!(a.status.contains("changed"));
    }
    #[test]
    fn project_undo_after_save_retains_current_disk_baseline() {
        let (_d, mut a) = app();
        let p = prepared(&a);
        a.refactor_applied(0, Ok(p));
        assert!(a.save_all_documents());
        a.undo_refactor();
        assert!(a.doc.dirty());
        assert!(a.save_all_documents());
        assert_eq!(
            std::fs::read_to_string(a.root.join("A.kt")).unwrap(),
            "fun greet() = 7\n"
        );
    }
    #[test]
    fn cancelled_request_cannot_reopen_a_preview_or_clear_new_busy_state() {
        let (_d, mut a) = app();
        a.refactor.id = 3;
        a.refactor.busy = true;
        a.project_reply(2, Err("old failure".into()));
        assert!(a.refactor.busy);
        assert!(a.refactor.error.is_empty());
    }
    #[test]
    fn rename_modal_and_fifth_dock_segment_fit_responsive_workspace() {
        for width in [760., 1000., 1440.] {
            let (_d, mut a) = app();
            a.refactor.dialog = true;
            a.refactor.name = "welcome".into();
            a.tab = 4;
            let ctx = a.context.clone();
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 560.),
                        )),
                        ..Default::default()
                    },
                    |ctx| a.draw_shell(ctx),
                );
            }
            for key in [
                "editor-usages",
                "editor-rename",
                "dock-4",
                "rename-name",
                "rename-preview",
                "rename-apply",
                "rename-cancel",
            ] {
                assert!(
                    ctx.content_rect().contains_rect(a.chrome.hits[key]),
                    "{key} clipped at{width}"
                );
            }
        }
    }
    #[test]
    fn project_overlay_omits_clean_missing_sources_but_keeps_an_intentional_dirty_draft() {
        let (d, mut a) = app();
        let path = d.path().join("Missing.kt");
        std::fs::write(&path, "class Missing\n").unwrap();
        let doc = Document::open(path.clone()).unwrap();
        a.parked.insert(path.clone(), (doc, Default::default()));
        std::fs::remove_file(&path).unwrap();
        a.editor_ui.conflicts.insert(
            path.clone(),
            wexandroidstudio::editor_jobs::DiskState::Missing,
        );
        assert!(a.project_buffers().iter().all(|b| b.path != path));
        a.parked
            .get_mut(&path)
            .unwrap()
            .0
            .edit(0..0, "// draft\n")
            .unwrap();
        assert!(a.project_buffers().iter().any(|b| b.path == path));
        assert!(!path.exists());
    }
}
