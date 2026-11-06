//! Shared expressive chrome. All actions use the same component and motion tokens.
use super::*;
use egui::{Align2, Rect, Sense, Ui, Vec2};
use std::collections::{BTreeMap, BTreeSet};
use wexandroidstudio::material::{self as m, ButtonKind as Kind, Icon, MotionToken as Token};

pub(super) struct Chrome {
    pub(super) motion: m::Motion,
    pub device_open: bool,
    device_search: String,
    pub settings: bool,
    explorer_open: bool,
    pub dock_open: bool,
    last_tab: usize,
    search: String,
    pub(super) reveal_tab: bool,
    pub(super) collapsed: BTreeSet<PathBuf>,
    pub(super) hits: BTreeMap<String, Rect>,
    pub smoke: Option<UiSmoke>,
}
impl Default for Chrome {
    fn default() -> Self {
        Self {
            motion: Default::default(),
            device_open: false,
            device_search: String::new(),
            settings: false,
            explorer_open: true,
            dock_open: true,
            last_tab: 0,
            search: String::new(),
            reveal_tab: false,
            collapsed: Default::default(),
            hits: Default::default(),
            smoke: None,
        }
    }
}
impl Chrome {
    pub(super) fn hits_editor(&mut self, rect: Rect) {
        self.hits.insert("editor".into(), rect);
    }
    // This adapter mirrors the shared button specification and records live hitboxes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn button(
        &mut self,
        ui: &mut Ui,
        key: &str,
        label: &str,
        icon: Icon,
        kind: Kind,
        enabled: bool,
        width: f32,
        reduced: bool,
    ) -> egui::Response {
        let r = m::button(
            ui,
            &mut self.motion,
            m::Button {
                key,
                label,
                icon: Some(icon),
                kind,
                enabled,
                width,
            },
            reduced,
        );
        self.hits.insert(key.into(), r.rect);
        r
    }
    pub(super) fn spatial(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        target: f32,
        initial: f32,
        reduced: bool,
    ) -> f32 {
        self.motion
            .value(ctx, key, target, initial, Token::DefaultSpatial, reduced)
    }
}
impl Workbench {
    pub(super) fn dirty_path(&self, path: &PathBuf) -> bool {
        if self.doc.path.as_ref() == Some(path) {
            self.doc.dirty()
        } else {
            self.parked.get(path).is_some_and(|(d, _)| d.dirty())
        }
    }
    pub(super) fn close_tab(&mut self, path: PathBuf, discard: bool) {
        if self.save_ui.pending.is_some() {
            self.status = "Save pending · keep tabs open until it completes".into();
            return;
        }
        if self.dirty_path(&path) && !discard {
            self.pending_close = Some(path);
            return;
        }
        if self.doc.path.as_ref() == Some(&path) {
            if let Some(next) = self.tabs.iter().find(|p| **p != path).cloned() {
                self.open_file(next);
            } else {
                self.doc = Document::new("").unwrap();
                self.edit.collapse(0);
                self.editor_switched();
                self.last_sent = None;
                self.diagnostics.clear();
                self.completions.clear();
            }
        }
        self.tabs.retain(|p| *p != path);
        self.parked.remove(&path);
        self.pending_close = None;
        self.editor_ui.conflicts.remove(&path);
    }
    pub(super) fn draw_shell(&mut self, ctx: &egui::Context) {
        if self.welcome {
            self.draw_welcome(ctx);
            self.file_dialog(ctx);
            self.projects_dialog(ctx);
            return;
        }
        self.chrome.motion.prune(ctx.cumulative_frame_nr());
        if self.chrome.last_tab != self.tab {
            self.chrome.dock_open = true;
            self.chrome.last_tab = self.tab;
        }
        let reduced = self.reduced_motion;
        if reduced {
            self.chrome.motion.settle_all();
        }
        let narrow = ctx.content_rect().width() < 1100.;
        egui::TopBottomPanel::top("app-bar")
            .exact_height(76.)
            .frame(
                egui::Frame::new()
                    .fill(m::SURFACE)
                    .inner_margin(egui::Margin::symmetric(18, 16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (logo, response) = ui.allocate_exact_size(Vec2::splat(40.), Sense::click());
                    self.chrome.hits.insert("projects".into(), logo);
                    if response
                        .on_hover_text_passive("New / Open project")
                        .clicked()
                    {
                        self.show_projects();
                    }
                    ui.painter().rect_filled(logo, 14, m::PRIMARY_CONTAINER);
                    ui.painter().text(
                        logo.center(),
                        Align2::CENTER_CENTER,
                        "W",
                        FontId::proportional(26.),
                        m::ON_CONTAINER,
                    );
                    let actions_width = if narrow { 464. } else { 654. };
                    let width = (ui.available_width() - actions_width - 8.).max(80.);
                    let (title, _) = ui.allocate_exact_size(egui::vec2(width, 40.), Sense::hover());
                    self.variant_title(ui, title);
                    m::label_clipped(
                        ui,
                        Rect::from_min_size(title.min, egui::vec2(width, 20.)),
                        "Wex Studio",
                        20.,
                        m::TEXT,
                    );
                    m::label_clipped(
                        ui,
                        Rect::from_min_size(
                            title.min + egui::vec2(0., 22.),
                            egui::vec2(width, 16.),
                        ),
                        &format!(
                            "{}  /  {} · {}{}",
                            self.root.file_name().unwrap_or_default().to_string_lossy(),
                            self.app_module,
                            self.variant,
                            if self.compose_enabled {
                                " · Compose"
                            } else {
                                ""
                            }
                        ),
                        11.,
                        m::MUTED,
                    );
                    let model = self
                        .devices
                        .iter()
                        .find(|d| Some(&d.serial) == self.selected.as_ref())
                        .map(|d| d.model.as_deref().unwrap_or("Android"))
                        .unwrap_or("Select device");
                    let r = self.chrome.button(
                        ui,
                        "device",
                        model,
                        Icon::Phone,
                        if self.chrome.device_open {
                            Kind::Tonal
                        } else {
                            Kind::Outline
                        },
                        !self.running
                            && !self.building
                            && self.debug.connection.is_none()
                            && !self.debug.busy,
                        if narrow { 168. } else { 224. },
                        reduced,
                    );
                    if r.clicked() {
                        self.chrome.device_open = !self.chrome.device_open;
                    }
                    r.on_hover_text_passive(
                        self.selected
                            .as_deref()
                            .unwrap_or("Choose an Android device"),
                    );
                    if self
                        .chrome
                        .button(
                            ui,
                            "save",
                            if narrow { "" } else { "Save" },
                            Icon::Save,
                            Kind::Ghost,
                            self.doc.path.is_some(),
                            if narrow { 40. } else { 78. },
                            reduced,
                        )
                        .on_hover_text_passive("Save · Ctrl+S")
                        .clicked()
                    {
                        self.save_document();
                    }
                    if self
                        .chrome
                        .button(
                            ui,
                            "build",
                            if narrow { "" } else { "Build" },
                            Icon::Build,
                            Kind::Tonal,
                            self.save_ui.pending.is_none()
                                && !self.running
                                && !self.building
                                && self.debug.connection.is_none()
                                && !self.debug.busy,
                            if narrow { 40. } else { 88. },
                            reduced,
                        )
                        .on_hover_text_passive("Build APK · Ctrl+B")
                        .clicked()
                    {
                        self.build();
                    }
                    let can_run = self.save_ui.pending.is_none()
                        && !self.running
                        && !self.building
                        && self.debug.connection.is_none()
                        && !self.debug.busy
                        && self.selected.as_ref().is_some_and(|s| {
                            self.devices.iter().any(|d| &d.serial == s && d.usable())
                        });
                    if self
                        .chrome
                        .button(
                            ui,
                            "run",
                            if self.running { "Busy" } else { "Run" },
                            Icon::Play,
                            Kind::Filled,
                            can_run,
                            88.,
                            reduced,
                        )
                        .on_hover_text_passive("Build → install → launch · Shift+F10")
                        .clicked()
                    {
                        self.run();
                    }
                    if self
                        .chrome
                        .button(
                            ui,
                            "debug",
                            if narrow { "" } else { "Debug" },
                            Icon::Debug,
                            if self.debug.paused {
                                Kind::Filled
                            } else {
                                Kind::Tonal
                            },
                            (can_run && self.variant == "debug") || self.debug.connection.is_some(),
                            if narrow { 40. } else { 88. },
                            reduced,
                        )
                        .on_hover_text_passive("Build → install → debug over JDWP · F5")
                        .clicked()
                    {
                        if self.debug.connection.is_some() {
                            self.tab = 3;
                            self.chrome.dock_open = true;
                        } else {
                            self.start_debug();
                        }
                    }
                    if self
                        .chrome
                        .button(
                            ui,
                            "settings",
                            "",
                            Icon::More,
                            Kind::Ghost,
                            true,
                            40.,
                            reduced,
                        )
                        .on_hover_text_passive("Workspace & appearance")
                        .clicked()
                    {
                        self.chrome.settings = true;
                    }
                });
            });
        let model_retry = self
            .model_status
            .as_ref()
            .is_some_and(|s| s.phase == language::ModelPhase::Failed);
        egui::TopBottomPanel::bottom("status-bar")
            .exact_height(if model_retry { 48. } else { 34. })
            .frame(
                egui::Frame::new()
                    .fill(m::SURFACE)
                    .inner_margin(egui::Margin::symmetric(18, 2)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let w = ui.available_width();
                    let height = if model_retry { 40. } else { 28. };
                    let (r, _) = ui
                        .allocate_exact_size(egui::vec2((w * 0.5).max(1.), height), Sense::hover());
                    m::label_clipped(ui, r, &self.status, 11., m::MUTED);
                    ui.interact(r, ui.id().with("status-tip"), Sense::hover())
                        .on_hover_text_passive(&self.status);
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(
                            (w * 0.5 - 8. - if model_retry { 108. } else { 0. }).max(1.),
                            height,
                        ),
                        Sense::hover(),
                    );
                    m::label_clipped(
                        ui,
                        r,
                        &format!(
                            "{}  {} · Ln {}, Col {} · UTF-8",
                            self.model_brief(),
                            self.language_status,
                            self.doc.rope().char_to_line(self.edit.cursor) + 1,
                            self.edit.cursor
                                - self
                                    .doc
                                    .rope()
                                    .line_to_char(self.doc.rope().char_to_line(self.edit.cursor))
                                + 1
                        ),
                        11.,
                        m::MUTED,
                    );
                    ui.interact(r, ui.id().with("language-tip"), Sense::hover())
                        .on_hover_text_passive(self.model_tip());
                    if model_retry
                        && self
                            .chrome
                            .button(
                                ui,
                                "model-retry",
                                "Retry",
                                Icon::Refresh,
                                Kind::Tonal,
                                true,
                                100.,
                                self.reduced_motion,
                            )
                            .clicked()
                    {
                        self.retry_model();
                    }
                });
            });
        egui::SidePanel::left("navigation")
            .exact_width(78.)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(m::SURFACE)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let rail_compact = ui.available_height() < 550.;
                    if rail_compact {
                        ui.spacing_mut().item_spacing.y = 2.;
                    }
                    let selected = if self.psearch.open {
                        "nav-search"
                    } else if self.preview.open {
                        "nav-preview"
                    } else if self.chrome.dock_open && self.tab == 1 {
                        "nav-build"
                    } else if self.chrome.dock_open && self.tab == 2 {
                        "nav-problems"
                    } else if self.chrome.dock_open && self.tab == 3 {
                        "nav-debug"
                    } else {
                        "project"
                    };
                    if let Some(r) = self.chrome.hits.get(selected).copied() {
                        let y = self
                            .chrome
                            .spatial(ctx, "rail-y", r.top(), r.top(), reduced);
                        ui.painter().rect_filled(
                            Rect::from_min_size(egui::pos2(r.left(), y), r.size()),
                            20,
                            m::PRIMARY_CONTAINER,
                        );
                    }
                    for (key, label, icon, active) in [
                        (
                            "project",
                            "Project",
                            Icon::Folder,
                            self.chrome.explorer_open,
                        ),
                        (
                            "nav-build",
                            "Build",
                            Icon::Build,
                            self.chrome.dock_open && self.tab == 1,
                        ),
                        (
                            "nav-problems",
                            "Problems",
                            Icon::Problems,
                            self.chrome.dock_open && self.tab == 2,
                        ),
                        ("nav-preview", "Preview", Icon::Phone, self.preview.open),
                        ("nav-search", "Search", Icon::Search, self.psearch.open),
                        (
                            "nav-debug",
                            "Debug",
                            Icon::Debug,
                            self.chrome.dock_open && self.tab == 3,
                        ),
                    ] {
                        if self
                            .chrome
                            .button(ui, key, "", icon, Kind::Ghost, true, 56., reduced)
                            .on_hover_text_passive(label)
                            .clicked()
                        {
                            match key {
                                "project" => {
                                    if self.psearch.open {
                                        self.close_project_search();
                                        self.chrome.explorer_open = true;
                                    } else {
                                        self.chrome.explorer_open = !self.chrome.explorer_open;
                                    }
                                }
                                "nav-search" => {
                                    if self.psearch.open {
                                        self.close_project_search();
                                    } else {
                                        self.show_project_search();
                                    }
                                }
                                "nav-build" => {
                                    self.tab = 1;
                                    self.chrome.dock_open = true;
                                }
                                "nav-preview" => self.toggle_preview(),
                                "nav-debug" => {
                                    self.tab = 3;
                                    self.chrome.dock_open = true;
                                }
                                _ => {
                                    self.tab = 2;
                                    self.chrome.dock_open = true;
                                }
                            }
                        }
                        ui.label(RichText::new(label).size(10.).color(if active {
                            m::PRIMARY
                        } else {
                            m::MUTED
                        }));
                        ui.add_space(if rail_compact { 0. } else { 12. });
                    }
                    ui.add_space((ui.available_height() - 78.).max(0.));
                    if self
                        .chrome
                        .button(
                            ui,
                            "preferences",
                            "",
                            Icon::Settings,
                            Kind::Ghost,
                            true,
                            56.,
                            reduced,
                        )
                        .on_hover_text_passive("Preferences")
                        .clicked()
                    {
                        self.chrome.settings = true;
                    }
                    ui.label(RichText::new("Settings").size(10.).color(m::MUTED));
                });
            });
        let target = if self.psearch.open {
            if narrow { 300. } else { 344. }
        } else if self.chrome.explorer_open {
            if narrow { 222. } else { 268. }
        } else {
            0.
        };
        let explorer = self
            .chrome
            .spatial(ctx, "explorer-width", target, target, reduced)
            .clamp(0., 350.);
        if explorer > 1. {
            egui::SidePanel::left("explorer")
                .exact_width(explorer)
                .resizable(false)
                .frame(egui::Frame::new().fill(m::SURFACE))
                .show(ctx, |ui| {
                    let clip = ui.max_rect();
                    let mut child =
                        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                            clip.min,
                            egui::vec2(
                                if self.psearch.open {
                                    if narrow { 300. } else { 344. }
                                } else if narrow {
                                    222.
                                } else {
                                    268.
                                },
                                clip.height(),
                            ),
                        )));
                    child.set_clip_rect(clip);
                    child.add_enabled_ui(self.chrome.explorer_open || self.psearch.open, |ui| {
                        if self.psearch.open {
                            self.project_search_panel(ui, ctx);
                        } else {
                            self.explorer(ui);
                        }
                    });
                });
        }
        let target = if self.chrome.dock_open {
            let reserve = if self.editor_ui.find.open {
                if self.editor_ui.find.expanded {
                    156.
                } else {
                    110.
                }
            } else {
                0.
            };
            let reserve = reserve
                + if self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| self.editor_ui.conflicts.contains_key(p))
                {
                    90.
                } else {
                    0.
                };
            (ctx.content_rect().height()
                * if matches!(self.tab, 3 | 5) {
                    0.40
                } else {
                    0.29
                })
            .clamp(
                190.,
                if matches!(self.tab, 3 | 5) {
                    380.
                } else {
                    300.
                },
            )
            .min((ctx.content_rect().height() - 324. - reserve).max(60.))
        } else {
            60.
        };
        let dock = self
            .chrome
            .spatial(ctx, "dock-height", target, target, reduced)
            .clamp(60., 400.);
        egui::TopBottomPanel::bottom("dock")
            .exact_height(dock)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(m::SURFACE)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 16,
                        top: 8,
                        bottom: 0,
                    }),
            )
            .show(ctx, |ui| {
                m::card()
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        self.console(ui, ctx);
                    });
            });
        self.preview_panel(ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(m::SURFACE)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 16,
                        top: 0,
                        bottom: 0,
                    }),
            )
            .show(ctx, |ui| {
                m::card()
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        self.file_tabs(ui, ctx);
                        ui.add_space(6.);
                        self.editor_breadcrumb(ui, ctx);
                        self.find_bar(ui, ctx);
                        self.external_banner(ui, ctx);
                        if self.doc.path.is_some() {
                            self.editor(ui, ctx);
                        } else {
                            empty(
                                ui,
                                Icon::Code,
                                "Your workspace, ready",
                                "Choose a Kotlin or Java file in Project.",
                            );
                        }
                    });
            });
        self.device_menu(ctx);
        self.dialogs(ctx);
        self.reload_dialog(ctx);
        self.rename_dialog(ctx);
        self.formatting_dialog(ctx);
        self.project_replace_dialog(ctx);
        self.git_dialog(ctx);
        if !self.completions.is_empty() {
            let mut selected = None;
            egui::Window::new("K2 suggestions")
                .id(egui::Id::new("k2-completion"))
                .default_pos([420., 220.])
                .resizable(false)
                .collapsible(false)
                .frame(m::card().fill(m::HIGH))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(300.)
                        .show(ui, |ui| {
                            for (i, item) in self.completions.iter().take(30).enumerate() {
                                if self
                                    .chrome
                                    .button(
                                        ui,
                                        &format!("completion-{i}"),
                                        &item.label,
                                        Icon::Code,
                                        Kind::Ghost,
                                        true,
                                        280.,
                                        reduced,
                                    )
                                    .on_hover_text_passive(item.detail.as_deref().unwrap_or(""))
                                    .clicked()
                                {
                                    selected = Some(i);
                                }
                            }
                        });
                });
            if let Some(i) = selected {
                self.accept_completion(i);
            }
        }
    }
    fn explorer(&mut self, ui: &mut Ui) {
        let reduced = self.reduced_motion;
        m::card()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_height((ui.available_height() - 24.).max(0.));
                ui.horizontal(|ui| {
                    ui.heading("Project");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self
                            .chrome
                            .button(
                                ui,
                                "collapse-project",
                                "",
                                Icon::Collapse,
                                Kind::Ghost,
                                true,
                                40.,
                                reduced,
                            )
                            .on_hover_text_passive("Hide project")
                            .clicked()
                        {
                            self.chrome.explorer_open = false;
                        }
                    });
                });
                ui.add_space(8.);
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut self.chrome.search)
                        .hint_text("Find a file…")
                        .desired_width(ui.available_width())
                        .margin(egui::vec2(12., 10.)),
                );
                self.chrome.hits.insert("file-filter".into(), filter.rect);
                ui.add_space(6.);
                self.file_toolbar(ui);
                let git_button = self
                    .chrome
                    .button(
                        ui,
                        "git-open",
                        "Git changes",
                        Icon::Git,
                        Kind::Tonal,
                        self.git_available(),
                        ui.available_width(),
                        self.reduced_motion,
                    )
                    .on_hover_text_passive("Review disk changes and staged files · Ctrl+Shift+G");
                if git_button.clicked() {
                    if !git_button.clicked_by(egui::PointerButton::Primary) {
                        // A keyboard activation belongs to this button, not the
                        // newly focused list. Keep later events in their order.
                        ui.ctx().input_mut(|i| {
                            if let Some(at) = i.events.iter().position(|event| matches!(event,
                                egui::Event::Key {key:Key::Enter|Key::Space,pressed:true,modifiers,..}
                                if modifiers.matches_exact(egui::Modifiers::NONE))) {
                                i.events.remove(at);
                            }
                        });
                    }
                    self.show_git(Some(git_button.id));
                }
                ui.add_space(6.);
                let query = self.chrome.search.to_lowercase();
                let mut rows: Vec<(PathBuf, String, usize, bool)> = Vec::new();
                let mut seen = BTreeSet::new();
                let entries = if self.file_ui.inventory.entries.is_empty() {
                    self.files
                        .iter()
                        .map(|p| project::ExplorerEntry {
                            path: p.clone(),
                            directory: false,
                        })
                        .collect()
                } else {
                    self.file_ui.inventory.entries.clone()
                };
                for entry in entries {
                    let file = &entry.path;
                    let rel = file.strip_prefix(&self.root).unwrap_or(file);
                    if !query.is_empty() && !rel.to_string_lossy().to_lowercase().contains(&query) {
                        continue;
                    }
                    let parts: Vec<_> = rel.components().collect();
                    let mut folder = self.root.clone();
                    let mut hidden = false;
                    for (depth, part) in
                        parts.iter().take(parts.len().saturating_sub(1)).enumerate()
                    {
                        folder.push(part.as_os_str());
                        if seen.insert(folder.clone()) {
                            rows.push((
                                folder.clone(),
                                part.as_os_str().to_string_lossy().to_string(),
                                depth,
                                true,
                            ));
                        }
                        if query.is_empty() && self.chrome.collapsed.contains(&folder) {
                            hidden = true;
                            break;
                        }
                    }
                    if !hidden && seen.insert(file.clone()) {
                        rows.push((
                            file.clone(),
                            file.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            parts.len().saturating_sub(1),
                            entry.directory,
                        ));
                    }
                }
                if self.file_ui.inventory.partial {
                    ui.label(
                        RichText::new("Partial tree · 10,000 entries / depth 20").color(m::WARNING),
                    );
                }
                if let Some(e) = &self.file_ui.inventory.error {
                    ui.label(RichText::new(e).small().color(m::WARNING));
                }
                // Compact intermediate package folders, retaining module/source-set boundaries.
                let structural = ["src", "main", "java", "kotlin", "res", "assets"];
                let mut at = 0;
                while at + 1 < rows.len() {
                    let depth = rows[at].2;
                    if rows[at].3
                        && rows[at + 1].3
                        && rows[at + 1].2 == depth + 1
                        && depth > 0
                        && !structural.contains(&rows[at].1.as_str())
                        && !structural.contains(&rows[at + 1].1.as_str())
                    {
                        let next = rows.remove(at + 1);
                        rows[at].0 = next.0;
                        rows[at].1.push('.');
                        rows[at].1.push_str(&next.1);
                        for r in rows.iter_mut().skip(at + 1) {
                            if r.2 <= depth {
                                break;
                            }
                            r.2 -= 1;
                        }
                    } else {
                        at += 1;
                    }
                }
                if rows.is_empty() {
                    ui.label(RichText::new("No matching files").color(m::MUTED));
                }
                let mut open = None;
                ui.spacing_mut().item_spacing.y = 0.;
                egui::ScrollArea::vertical()
                    .id_salt("file-tree")
                    .auto_shrink([false, false])
                    .show_rows(ui, 34., rows.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.;
                        for i in range {
                            let (path, label, depth, folder) = &rows[i];
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 34.),
                                Sense::hover(),
                            );
                            // A file owns its gesture even when unrelated prefix
                            // widgets change their automatic IDs between frames.
                            let r = ui.interact(rect, egui::Id::new(("explorer-entry", path)), Sense::click());
                            let active = self.doc.path.as_ref() == Some(path);
                            let t = self
                                .chrome
                                .motion
                                .value(
                                    ui.ctx(),
                                    format!("file:{}", path.display()),
                                    if active { 1. } else { 0. },
                                    if active { 1. } else { 0. },
                                    Token::Effects,
                                    reduced,
                                )
                                .clamp(0., 1.);
                            if active || r.hovered() || t > 0.001 {
                                ui.painter().rect_filled(
                                    rect,
                                    10,
                                    m::mix(
                                        if r.hovered() { m::HIGH } else { m::LOW },
                                        m::PRIMARY_CONTAINER,
                                        t,
                                    ),
                                );
                            }
                            let indent = (*depth as f32 * 12.).min(64.);
                            let ir = Rect::from_center_size(
                                egui::pos2(rect.left() + 14. + indent, rect.center().y),
                                Vec2::splat(18.),
                            );
                            wexandroidstudio::file_icons::paint(
                                ui,
                                ir,
                                path,
                                *folder,
                                !self.chrome.collapsed.contains(path),
                                true,
                            );
                            m::label_clipped(
                                ui,
                                Rect::from_min_max(
                                    egui::pos2(ir.right() + 8., rect.top()),
                                    rect.right_bottom() - egui::vec2(14., 0.),
                                ),
                                label,
                                12.,
                                if active { m::ON_CONTAINER } else { m::TEXT },
                            );
                            if *folder {
                                let expanded = !self.chrome.collapsed.contains(path);
                                let t = self.chrome.spatial(
                                    ui.ctx(),
                                    &format!("folder:{}", path.display()),
                                    if expanded { 1. } else { 0. },
                                    if expanded { 1. } else { 0. },
                                    reduced,
                                );
                                let angle = t * std::f32::consts::FRAC_PI_2;
                                let center = rect.right_center() - egui::vec2(7., 0.);
                                let rotate = |v: Vec2| {
                                    center
                                        + egui::vec2(
                                            v.x * angle.cos() - v.y * angle.sin(),
                                            v.x * angle.sin() + v.y * angle.cos(),
                                        )
                                };
                                ui.painter().add(egui::Shape::line(
                                    vec![
                                        rotate(egui::vec2(-2., -4.)),
                                        rotate(egui::vec2(2., 0.)),
                                        rotate(egui::vec2(-2., 4.)),
                                    ],
                                    egui::Stroke::new(1.5_f32, m::MUTED),
                                ));
                            }
                            if self.dirty_path(path) || self.editor_ui.conflicts.contains_key(path)
                            {
                                ui.painter().circle_filled(
                                    rect.right_center() - egui::vec2(7., 0.),
                                    2.5,
                                    if self.editor_ui.conflicts.contains_key(path) {
                                        m::ERROR
                                    } else {
                                        m::PRIMARY
                                    },
                                );
                            }
                            let mut action = None;
                            r.context_menu(|ui| {
                                for (label, kind) in [
                                    ("New file here", files_ui::Mode::New),
                                    ("New folder here", files_ui::Mode::Folder),
                                    ("Rename / Move…", files_ui::Mode::Move),
                                    ("Move to trash…", files_ui::Mode::Trash),
                                ] {
                                    if ui.button(label).clicked() {
                                        action = Some(kind);
                                        ui.close();
                                    }
                                }
                            });
                            if let Some(mode) = action {
                                self.show_file_dialog(mode, Some((path.clone(), *folder)));
                            }
                            if r.clicked() {
                                self.file_ui.selected = Some((path.clone(), *folder));
                                if *folder {
                                    if !self.chrome.collapsed.remove(path) {
                                        self.chrome.collapsed.insert(path.clone());
                                    }
                                } else {
                                    open = Some(path.clone());
                                }
                            }
                            if !folder {
                                self.chrome.hits.insert(
                                    format!(
                                        "file-{}",
                                        path.file_name().unwrap_or_default().to_string_lossy()
                                    ),
                                    rect,
                                );
                            }
                            m::passive_tooltip(&r, path.to_string_lossy());
                        }
                    });
                if let Some(path) = open {
                    self.open_file(path);
                }
            });
    }
    fn file_tabs(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        self.syntax_bar(ui);
        self.document_save_bar(ui);
        self.document_open_bar(ui);
        let mut open = None;
        let mut close = None;
        let mut active = None;
        let base = ui.cursor().min;
        let reveal = std::mem::take(&mut self.chrome.reveal_tab);
        egui::ScrollArea::horizontal()
            .animated(false)
            .id_salt("document-tabs")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.;
                    for path in self.tabs.clone() {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let selected = self.doc.path.as_ref() == Some(&path);
                        let (r, _) = ui.allocate_exact_size(egui::vec2(170., 42.), Sense::hover());
                        if selected {
                            ui.painter().rect_filled(r, 14, m::CONTAINER);
                            active = Some(r);
                            if reveal {
                                ui.scroll_to_rect(r, Some(egui::Align::Center));
                            }
                        }
                        let select = ui.interact(
                            Rect::from_min_max(r.min, r.right_bottom() - egui::vec2(30., 0.)),
                            ui.id().with(&path),
                            Sense::click(),
                        );
                        select.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                true,
                                selected,
                                name.as_ref(),
                            )
                        });
                        wexandroidstudio::file_icons::paint(
                            ui,
                            Rect::from_center_size(
                                r.left_center() + egui::vec2(16., 0.),
                                Vec2::splat(18.),
                            ),
                            &path,
                            false,
                            false,
                            true,
                        );
                        m::label_clipped(
                            ui,
                            Rect::from_min_max(
                                r.min + egui::vec2(32., 0.),
                                r.right_bottom() - egui::vec2(40., 0.),
                            ),
                            &name,
                            12.,
                            if selected { m::TEXT } else { m::MUTED },
                        );
                        if self.dirty_path(&path) || self.editor_ui.conflicts.contains_key(&path) {
                            ui.painter().circle_filled(
                                r.right_center() - egui::vec2(36., 0.),
                                2.5,
                                if self.editor_ui.conflicts.contains_key(&path) {
                                    m::ERROR
                                } else {
                                    m::PRIMARY
                                },
                            );
                        }
                        let cr = Rect::from_center_size(
                            r.right_center() - egui::vec2(15., 0.),
                            egui::vec2(28., 32.),
                        );
                        let c = ui.interact(cr, ui.id().with(("close", &path)), Sense::click());
                        if c.hovered() {
                            ui.painter().rect_filled(cr, 10, m::HIGH);
                        }
                        m::icon(ui, cr.shrink(6.), Icon::Close, m::MUTED);
                        self.chrome
                            .hits
                            .insert(format!("tab-{}", name), select.rect);
                        self.chrome.hits.insert(format!("close-{}", name), cr);
                        if select.clicked() {
                            open = Some(path.clone());
                        }
                        if c.clicked() {
                            close = Some(path.clone());
                        }
                        select.on_hover_text_passive(path.to_string_lossy());
                        c.on_hover_text_passive("Close tab");
                    }
                    if let Some(r) = active {
                        let x = self.chrome.spatial(
                            ctx,
                            "tab-x",
                            r.left() - base.x,
                            r.left() - base.x,
                            self.reduced_motion,
                        );
                        let w = self.chrome.spatial(
                            ctx,
                            "tab-width",
                            r.width(),
                            r.width(),
                            self.reduced_motion,
                        );
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                egui::pos2(base.x + x, r.bottom() - 3.),
                                egui::vec2(w, 3.),
                            ),
                            2,
                            m::PRIMARY,
                        );
                    }
                });
            });
        if let Some(p) = open {
            self.open_file(p);
        }
        if let Some(p) = close {
            self.close_tab(p, false);
        }
    }
    fn console(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let reduced = self.reduced_motion;
        ui.set_min_width(ui.available_width());
        ui.set_min_height((ui.available_height() - 16.).max(0.));
        ui.horizontal(|ui| {
            let cell = ((ui.available_width() - 56.) / 6.).clamp(52., 102.);
            let (segment, _) = ui.allocate_exact_size(egui::vec2(cell * 6., 40.), Sense::hover());
            ui.painter().rect_filled(segment, 20, m::CONTAINER);
            let x = self.chrome.spatial(
                ctx,
                "dock-tab-x",
                self.tab as f32 * cell,
                self.tab as f32 * cell,
                reduced,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(
                    segment.min + egui::vec2(x + 3., 3.),
                    egui::vec2(cell - 6., 34.),
                ),
                17,
                m::PRIMARY_CONTAINER,
            );
            for (i, label) in ["Logcat", "Build", "Problems", "Debug", "Usages", "Terminal"]
                .iter()
                .enumerate()
            {
                let r = Rect::from_min_size(
                    segment.min + egui::vec2(i as f32 * cell, 0.),
                    egui::vec2(cell, 40.),
                );
                let response = ui.interact(r, ui.id().with(("dock-tab", i)), Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        self.tab == i,
                        *label,
                    )
                });
                let title = if i == 2 && !self.diagnostics.is_empty() {
                    format!("{label} · {}", self.diagnostics.len())
                } else {
                    label.to_string()
                };
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    title,
                    FontId::proportional(12.),
                    if self.tab == i {
                        m::ON_CONTAINER
                    } else {
                        m::MUTED
                    },
                );
                self.chrome.hits.insert(format!("dock-{i}"), r);
                if response.clicked() {
                    self.tab = i;
                    self.chrome.dock_open = true;
                    if i == 5 {
                        if self.terminal.sessions.is_empty() {
                            self.new_terminal();
                        } else {
                            self.terminal.focus = true;
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self
                    .chrome
                    .button(
                        ui,
                        "toggle-dock",
                        "",
                        if self.chrome.dock_open {
                            Icon::Expand
                        } else {
                            Icon::Collapse
                        },
                        Kind::Ghost,
                        true,
                        40.,
                        reduced,
                    )
                    .on_hover_text_passive("Toggle output panel")
                    .clicked()
                {
                    self.chrome.dock_open = !self.chrome.dock_open;
                }
                if self.building || self.running {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(32., 40.), Sense::hover());
                    m::loading(ui, r, reduced);
                }
            });
        });
        if !self.chrome.dock_open || ui.available_height() < 75. {
            return;
        }
        ui.add_space(6.);
        if self.tab == 5 {
            self.terminal_panel(ui, ctx);
            return;
        }
        if self.tab == 4 {
            self.usages_panel(ui);
            return;
        }
        if self.tab == 3 {
            self.debug_panel(ui);
            return;
        }
        if self.tab == 0 {
            ui.horizontal(|ui| {
                if self
                    .chrome
                    .button(
                        ui,
                        "log-connect",
                        "",
                        Icon::Play,
                        Kind::Tonal,
                        self.selected.is_some() && !self.running,
                        40.,
                        reduced,
                    )
                    .on_hover_text_passive("Connect Logcat")
                    .clicked()
                {
                    self.start_logs();
                }
                if self
                    .chrome
                    .button(
                        ui,
                        "log-stop",
                        "",
                        Icon::Pause,
                        Kind::Ghost,
                        self.log_task.is_some(),
                        40.,
                        reduced,
                    )
                    .on_hover_text_passive("Stop Logcat stream")
                    .clicked()
                {
                    self.stop_logs();
                }
                if self
                    .chrome
                    .button(
                        ui,
                        "log-clear",
                        "",
                        Icon::Close,
                        Kind::Ghost,
                        true,
                        40.,
                        reduced,
                    )
                    .on_hover_text_passive("Clear logs")
                    .clicked()
                {
                    self.ring.clear();
                }
                let filter_response = ui.add_sized(
                    [(ui.available_width() - 8.).max(40.), 36.],
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter Logcat · regular expression")
                        .margin(egui::vec2(12., 8.)),
                );
                self.chrome
                    .hits
                    .insert("logcat-filter".into(), filter_response.rect);
                if filter_response.changed() {
                    match regex::Regex::new(&self.filter) {
                        Ok(r) => {
                            self.compiled_filter.expression = Some(r);
                            self.filter_error = None;
                        }
                        Err(e) => self.filter_error = Some(e.to_string()),
                    }
                }
            });
            if let Some(e) = &self.filter_error {
                ui.colored_label(m::ERROR, "Invalid expression")
                    .on_hover_text_passive(e);
            }
            ui.spacing_mut().item_spacing.y = 0.;
            let entries = self
                .ring
                .entries()
                .filter(|e| self.compiled_filter.matches(e))
                .collect::<Vec<_>>();
            if entries.is_empty() {
                empty(
                    ui,
                    Icon::Log,
                    if self.log_task.is_some() {
                        "Listening to your device"
                    } else {
                        "Follow your app in real time"
                    },
                    if self.log_task.is_some() {
                        "New matching messages will appear here."
                    } else {
                        "Run an app or connect Logcat to start streaming."
                    },
                );
            } else {
                egui::ScrollArea::both()
                    .id_salt("log-rows")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show_rows(ui, 22., entries.len(), |ui, range| {
                        for i in range {
                            let e = entries[i];
                            let text = format!(
                                "{}.{:03}  {:>6}  {}  {:<20}  {}",
                                e.seconds,
                                e.nanos / 1_000_000,
                                e.pid,
                                e.level(),
                                e.tag,
                                e.message.replace('\n', " ↵ ")
                            );
                            line_row(
                                ui,
                                &text,
                                if e.priority >= 6 {
                                    m::ERROR
                                } else if e.priority == 5 {
                                    m::WARNING
                                } else {
                                    m::TEXT
                                },
                            );
                        }
                    });
            }
        } else if self.tab == 1 {
            self.compile_cache_panel(ui);
            ui.spacing_mut().item_spacing.y = 0.;
            if self.output.is_empty() {
                empty(
                    ui,
                    Icon::Build,
                    "From source to APK",
                    "Wex native build · Kotlin / Java / resources / DEX / signing",
                );
            } else {
                egui::ScrollArea::both()
                    .id_salt("build-rows")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show_rows(ui, 22., self.output.len(), |ui, range| {
                        for i in range {
                            line_row(ui, &self.output[i].replace('\n', " ↵ "), m::TEXT);
                        }
                    });
            }
        } else if self.diagnostics.is_empty() {
            empty(
                ui,
                Icon::Check,
                "No reported problems",
                &self.language_status,
            );
        } else {
            let mut jump = None;
            egui::ScrollArea::vertical()
                .id_salt("diagnostic-rows")
                .show(ui, |ui| {
                    for d in &self.diagnostics {
                        let r = egui::Frame::new()
                            .fill(m::CONTAINER)
                            .corner_radius(12)
                            .inner_margin(12)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    RichText::new(format!(
                                        "{}:{}  {}",
                                        d.range.start.line + 1,
                                        d.range.start.character + 1,
                                        d.message
                                    ))
                                    .color(
                                        if d.severity == Some(1) {
                                            m::ERROR
                                        } else {
                                            m::WARNING
                                        },
                                    ),
                                );
                            })
                            .response;
                        if ui
                            .interact(
                                r.rect,
                                ui.id().with((d.range.start.line, d.range.start.character)),
                                Sense::click(),
                            )
                            .clicked()
                        {
                            jump = Some(d.range.start.clone());
                        }
                    }
                });
            if let Some(p) = jump {
                self.edit
                    .collapse(self.doc.char_at_lsp(p.line, p.character).unwrap_or(0));
                self.scroll_to_cursor = true;
            }
        }
    }
    fn device_menu(&mut self, ctx: &egui::Context) {
        let reduced = self.reduced_motion;
        let desired = (144. + self.devices.len() as f32 * 74.).clamp(220., 480.);
        let h = self
            .chrome
            .spatial(
                ctx,
                "device-height",
                if self.chrome.device_open { desired } else { 0. },
                0.,
                reduced,
            )
            .clamp(0., desired + 12.);
        if h < 1. {
            return;
        }
        let anchor = self
            .chrome
            .hits
            .get("device")
            .copied()
            .unwrap_or(Rect::NOTHING);
        let screen = ctx.content_rect();
        let width = 360_f32.min(screen.width() - 32.);
        let pos = egui::pos2(
            anchor.right().min(screen.right() - 16.) - width,
            anchor.bottom() + 8.,
        );
        let mut chosen = None;
        let response = egui::Area::new(egui::Id::new("device-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let (visible, _) = ui.allocate_exact_size(
                    egui::vec2(width, h.min(screen.bottom() - pos.y - 16.)),
                    Sense::hover(),
                );
                ui.painter().rect_filled(visible, 24, m::HIGH);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    visible.min + egui::vec2(16., 12.),
                    egui::vec2(width - 32., desired - 24.),
                )));
                child.set_clip_rect(visible.shrink(4.));
                child.add_enabled_ui(self.chrome.device_open, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Run on device");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self
                                .chrome
                                .button(
                                    ui,
                                    "device-refresh",
                                    "",
                                    Icon::Refresh,
                                    Kind::Ghost,
                                    true,
                                    40.,
                                    reduced,
                                )
                                .on_hover_text_passive("Refresh connected devices")
                                .clicked()
                            {
                                self.refresh_devices();
                            }
                        });
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.chrome.device_search)
                            .hint_text("Find model or serial…")
                            .desired_width(width - 40.)
                            .margin(egui::vec2(12., 10.)),
                    );
                    ui.add_space(8.);
                    let search = self.chrome.device_search.to_lowercase();
                    let mut count = 0;
                    egui::ScrollArea::vertical()
                        .max_height(desired - 140.)
                        .show(ui, |ui| {
                            for d in &self.devices {
                                if !format!(
                                    "{} {}",
                                    d.model.as_deref().unwrap_or("Android"),
                                    d.serial
                                )
                                .to_lowercase()
                                .contains(&search)
                                {
                                    continue;
                                }
                                count += 1;
                                let selected = self.selected.as_ref() == Some(&d.serial);
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(width - 40., 68.),
                                    Sense::hover(),
                                );
                                let response = ui.interact(
                                    r,
                                    ui.id().with(&d.serial),
                                    if d.usable() {
                                        Sense::click()
                                    } else {
                                        Sense::hover()
                                    },
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        d.usable(),
                                        selected,
                                        format!(
                                            "{} · {} · {}",
                                            d.model.as_deref().unwrap_or("Android"),
                                            d.serial,
                                            d.state
                                        ),
                                    )
                                });
                                if selected || response.hovered() {
                                    ui.painter().rect_filled(
                                        r,
                                        16,
                                        if selected {
                                            m::PRIMARY_CONTAINER
                                        } else {
                                            m::CONTAINER
                                        },
                                    );
                                }
                                m::icon(
                                    ui,
                                    Rect::from_center_size(
                                        r.left_center() + egui::vec2(22., 0.),
                                        Vec2::splat(24.),
                                    ),
                                    Icon::Phone,
                                    if d.usable() { m::PRIMARY } else { m::MUTED },
                                );
                                m::label_clipped(
                                    ui,
                                    Rect::from_min_size(
                                        r.min + egui::vec2(44., 8.),
                                        egui::vec2(r.width() - 78., 24.),
                                    ),
                                    d.model.as_deref().unwrap_or("Android"),
                                    14.,
                                    if d.usable() { m::TEXT } else { m::MUTED },
                                );
                                m::label_clipped(
                                    ui,
                                    Rect::from_min_size(
                                        r.min + egui::vec2(44., 34.),
                                        egui::vec2(r.width() - 48., 22.),
                                    ),
                                    &format!("{} · {}", d.serial, d.state),
                                    11.,
                                    m::MUTED,
                                );
                                if selected {
                                    m::icon(
                                        ui,
                                        Rect::from_center_size(
                                            r.right_center() - egui::vec2(20., 8.),
                                            Vec2::splat(18.),
                                        ),
                                        Icon::Check,
                                        m::PRIMARY,
                                    );
                                }
                                self.chrome.hits.insert(format!("serial-{}", d.serial), r);
                                response
                                    .clone()
                                    .on_hover_text_passive(format!("{} · {}", d.serial, d.state));
                                if response.clicked() && d.usable() {
                                    chosen = Some(d.serial.clone());
                                }
                            }
                        });
                    if count == 0 {
                        ui.add_space(8.);
                        ui.label("No matching devices");
                        ui.label(
                            RichText::new("Connect USB with debugging enabled, then refresh.")
                                .small()
                                .color(m::MUTED),
                        );
                    }
                });
            });
        if let Some(serial) = chosen {
            self.select_device(serial);
            self.chrome.device_open = false;
        }
        if self.chrome.device_open
            && (ctx.input(|i| i.key_pressed(Key::Escape))
                || ctx.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer.interact_pos().is_some_and(|p| {
                            !response.response.rect.contains(p) && !anchor.contains(p)
                        })
                }))
        {
            self.chrome.device_open = false;
        }
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        self.window_close_dialog(ctx);
        self.breakpoint_review_dialog(ctx);
        self.file_dialog(ctx);
        self.projects_dialog(ctx);
        self.variant_dialog(ctx);
        if self.chrome.settings {
            egui::Modal::new(egui::Id::new("preferences"))
                .frame(m::card().fill(m::HIGH).inner_margin(24))
                .show(ctx, |ui| {
                    ui.set_width(380.);
                    ui.heading("Make it yours");
                    ui.label(RichText::new("Workspace & appearance").color(m::MUTED));
                    ui.add_space(20.);
                    ui.label(RichText::new("Material 3 Expressive").size(18.));
                    ui.label("Tonal surfaces, rounded shapes and responsive motion.");
                    ui.add_space(12.);
                    let reduced = self.reduced_motion;
                    let r = m::switch(
                        ui,
                        &mut self.chrome.motion,
                        "reduce-motion",
                        &mut self.reduced_motion,
                        "Reduce motion",
                        reduced,
                    );
                    if self.reduced_motion {
                        self.chrome.motion.settle_all();
                    }
                    self.chrome.hits.insert("reduce-motion".into(), r.rect);
                    ui.label(
                        RichText::new("Instant transitions · no spring or loading animation")
                            .small()
                            .color(m::MUTED),
                    );
                    ui.add_space(20.);
                    ui.horizontal(|ui| {
                        if self
                            .chrome
                            .button(
                                ui,
                                "k2-connect",
                                "Connect K2",
                                Icon::Code,
                                Kind::Tonal,
                                true,
                                156.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.connect_language(false);
                        }
                        if self
                            .chrome
                            .button(
                                ui,
                                "reload-model",
                                "Reload model",
                                Icon::Refresh,
                                Kind::Outline,
                                true,
                                172.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.connect_language(true);
                        }
                    });
                    ui.add_space(20.);
                    if self
                        .chrome
                        .button(
                            ui,
                            "settings-projects",
                            "New / Open project",
                            Icon::Folder,
                            Kind::Tonal,
                            true,
                            240.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.show_projects();
                    }
                    ui.add_space(12.);
                    self.model_card(ui);
                    ui.add_space(12.);
                    ui.horizontal(|ui| {
                        m::badge(ui, "RUST / WGPU", m::PRIMARY);
                        m::badge(ui, "WEX NATIVE", m::SUCCESS);
                    });
                    ui.add_space(20.);
                    if self
                        .chrome
                        .button(
                            ui,
                            "settings-done",
                            "Done",
                            Icon::Check,
                            Kind::Filled,
                            true,
                            110.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.chrome.settings = false;
                    }
                });
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.chrome.settings = false;
            }
        }
        if let Some(path) = self.pending_close.clone() {
            egui::Modal::new(egui::Id::new("save-changes"))
                .frame(m::card().fill(m::HIGH).inner_margin(24))
                .show(ctx, |ui| {
                    ui.set_width(390.);
                    ui.heading("Keep your changes?");
                    ui.add_space(12.);
                    ui.label(format!(
                        "{} has unsaved edits.",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    ui.add_space(20.);
                    ui.horizontal(|ui| {
                        if self
                            .chrome
                            .button(
                                ui,
                                "close-cancel",
                                "Cancel",
                                Icon::Close,
                                Kind::Ghost,
                                true,
                                106.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.pending_close = None;
                        }
                        if self
                            .chrome
                            .button(
                                ui,
                                "close-discard",
                                "Discard",
                                Icon::Close,
                                Kind::Danger,
                                true,
                                116.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.close_tab(path.clone(), true);
                        }
                        if self
                            .chrome
                            .button(
                                ui,
                                "close-save",
                                "Save",
                                Icon::Save,
                                Kind::Filled,
                                true,
                                106.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.request_save_paths(
                                vec![path.clone()],
                                save_ui::After::Close(path.clone()),
                            );
                        }
                    });
                });
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.pending_close = None;
            }
        }
    }
}
fn line_row(ui: &mut Ui, text: &str, color: Color32) {
    ui.spacing_mut().item_spacing.y = 0.;
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), FontId::monospace(12.), color);
    let (r, _) = ui.allocate_exact_size(
        egui::vec2(galley.size().x.max(ui.available_width()), 22.),
        Sense::hover(),
    );
    ui.painter().galley(
        r.left_top() + egui::vec2(0., (22. - galley.size().y) * 0.5),
        galley,
        color,
    );
}
fn empty(ui: &mut Ui, icon: Icon, title: &str, detail: &str) {
    let (r, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ui.available_height().max(0.)),
        Sense::hover(),
    );
    let painter = ui.painter().with_clip_rect(r);
    let title = painter.layout(
        title.into(),
        FontId::proportional(17.),
        m::TEXT,
        (r.width() - 24.).max(1.),
    );
    let detail = painter.layout(
        detail.into(),
        FontId::proportional(12.),
        m::MUTED,
        (r.width() - 24.).max(1.),
    );
    let mut height = title.size().y + 8. + detail.size().y;
    let show_icon = r.height() >= height + 62.;
    if show_icon {
        height += 40.;
    }
    let mut y = r.top() + ((r.height() - height) * 0.5).max(0.);
    if show_icon {
        m::icon(
            ui,
            Rect::from_center_size(egui::pos2(r.center().x, y + 13.), Vec2::splat(26.)),
            icon,
            m::PRIMARY,
        );
        y += 40.;
    }
    let title_h = title.size().y;
    painter.galley(
        egui::pos2(r.center().x - title.size().x / 2., y),
        title,
        m::TEXT,
    );
    painter.galley(
        egui::pos2(r.center().x - detail.size().x / 2., y + title_h + 8.),
        detail,
        m::MUTED,
    );
}

/// Native-window regression driver. Pointer events pass through eframe's raw input
/// hook, so hit testing, focus, popup layers and painting are exercised together.
pub(super) struct UiSmoke {
    dir: PathBuf,
    start: std::time::Instant,
    step: usize,
    release: Option<egui::Pos2>,
    release_at: std::time::Instant,
    next: std::time::Instant,
    awaiting: Option<String>,
    deferred_click: Option<(String, u8)>,
    shots: Vec<serde_json::Value>,
    checks: Vec<String>,
    samples: Vec<serde_json::Value>,
    original: Option<(PathBuf, String)>,
}
impl UiSmoke {
    pub(super) fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            start: std::time::Instant::now(),
            step: 0,
            release: None,
            release_at: std::time::Instant::now(),
            next: std::time::Instant::now() + Duration::from_secs(3),
            awaiting: None,
            deferred_click: None,
            shots: vec![],
            checks: vec![],
            samples: vec![],
            original: None,
        }
    }
}
impl Workbench {
    pub(super) fn ui_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.chrome.smoke.take() else {
            return;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(90),
            "UI smoke timed out at {}",
            s.step
        );
        // Wait for genuine native focus, and keep pointer presses alive across
        // more than one winit frame (fast GPU frames must not shorten a click).
        if !input.focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.chrome.smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if let Some(pos) = s.release
            && std::time::Instant::now() < s.release_at
        {
            input.events.push(egui::Event::PointerMoved(pos));
            self.chrome.smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if let Some(pos) = s.release.take() {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
            self.chrome.smoke = Some(s);
            ctx.request_repaint();
            return;
        }
        if s.awaiting.is_none()
            && let Some((key, hovered)) = s.deferred_click.take()
        {
            let pos = self
                .chrome
                .hits
                .get(&key)
                .unwrap_or_else(|| panic!("missing live widget {key}"))
                .center();
            input.events.push(egui::Event::PointerMoved(pos));
            if hovered < 2 {
                s.deferred_click = Some((key, hovered + 1));
            } else {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                s.release = Some(pos);
                s.release_at = std::time::Instant::now() + Duration::from_millis(80);
            }
            self.chrome.smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if std::time::Instant::now() < s.next || s.awaiting.is_some() {
            self.chrome.smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        let mut click: Option<String> = None;
        let mut shot: Option<&str> = None;
        let mut hold = false;
        match s.step {
            0 => {
                s.original = Some((self.doc.path.clone().unwrap(), self.doc.rope().to_string()));
                shot = Some("01-workspace");
            }
            1 => {
                click = Some("device".into());
            }
            2 => {
                assert!(self.chrome.device_open);
                shot = Some("02-devices");
                s.checks.push(
                    "Device chooser opens in native window; model/serial rows visible".into(),
                );
            }
            3 => {
                if let Some(serial) = &self.selected {
                    click = Some(format!("serial-{serial}"));
                    s.checks.push(format!(
                        "Device row selects explicit serial {serial}; chooser closes"
                    ));
                } else {
                    input.events.push(egui::Event::Key {
                        key: Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    });
                }
            }
            4 => {
                assert!(!self.chrome.device_open);
                click = Some("dock-1".into());
            }
            5 => {
                click = Some("dock-2".into());
            }
            6 => {
                click = Some("dock-0".into());
            }
            7 => {
                assert_eq!(self.tab, 0);
                s.checks
                    .push("Dock segment can retarget before settling".into());
                click = Some("project".into());
            }
            8 => {
                click = Some("project".into());
            }
            9 => {
                assert!(self.chrome.explorer_open);
                s.checks
                    .push("Explorer collapse reverses mid-flight without reset".into());
                click = Some("file-Greeter.kt".into());
            }
            10 => {
                if self.doc.path.as_ref().unwrap().ends_with("Greeter.kt") {
                    s.checks
                        .push("Project opens second source as a real document tab".into());
                    click = Some("tab-MainActivity.kt".into());
                } else {
                    hold = true;
                }
            }
            11 => {
                assert!(self.doc.path.as_ref().unwrap().ends_with("MainActivity.kt"));
                let r = *self.chrome.hits.get("editor").expect("editor rect");
                input.events.push(egui::Event::PointerMoved(r.center()));
                input.events.push(egui::Event::PointerButton {
                    pos: r.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                s.release = Some(r.center());
                s.release_at = std::time::Instant::now() + Duration::from_millis(80);
            }
            12 => {
                input
                    .events
                    .push(egui::Event::Text("// UI smoke unsaved\n".into()));
            }
            13 => {
                assert!(self.doc.dirty());
                click = Some("tab-Greeter.kt".into());
            }
            14 => {
                assert!(self.parked.values().any(|(d, _)| d.dirty()));
                click = Some("tab-MainActivity.kt".into());
            }
            15 => {
                assert!(self.doc.dirty());
                assert!(self.doc.rope().to_string().contains("// UI smoke unsaved"));
                s.checks
                    .push("Dirty Rope buffer and cursor survive tab switching".into());
                click = Some("close-MainActivity.kt".into());
            }
            16 => {
                assert!(self.pending_close.is_some());
                shot = Some("03-save-dialog");
            }
            17 => {
                click = Some("close-cancel".into());
            }
            18 => {
                assert!(self.pending_close.is_none() && self.doc.dirty());
                click = Some("close-MainActivity.kt".into());
            }
            19 => {
                click = Some("close-discard".into());
            }
            20 => {
                assert!(self.pending_close.is_none());
                assert!(!self.tabs.iter().any(|p| p.ends_with("MainActivity.kt")));
                s.checks.push(
                    "Close dirty tab: Cancel preserves; Discard removes only in-memory edits"
                        .into(),
                );
                click = Some("file-MainActivity.kt".into());
            }
            21 => {
                let (p, text) = s.original.as_ref().unwrap();
                assert_eq!(&std::fs::read_to_string(p).unwrap(), text);
                assert_eq!(&self.doc.rope().to_string(), text);
                shot = Some("04-tabs");
            }
            22 => {
                click = Some("settings".into());
            }
            23 => {
                assert!(self.chrome.settings);
                shot = Some("05-preferences");
            }
            24 => {
                click = Some("reduce-motion".into());
            }
            25 => {
                assert!(self.reduced_motion);
                click = Some("settings-done".into());
            }
            26 => {
                click = Some("project".into());
            }
            27 => {
                assert!(!self.chrome.explorer_open);
                assert!(
                    self.chrome
                        .motion
                        .snapshot()
                        .iter()
                        .all(|(_, v, vel, t)| (v - t).abs() < 0.01 && vel.abs() < 0.01)
                );
                s.checks.push(
                    "Reduced motion snaps positions and velocities; no animated busy indicator"
                        .into(),
                );
                shot = Some("06-reduced-motion");
            }
            28 => {
                click = Some("project".into());
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            29 => {
                shot = Some("07-wide");
            }
            30 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(800., 700.)));
            }
            31 => {
                shot = Some("08-compact");
            }
            32 => {
                let screen = ctx.content_rect();
                for key in ["device", "save", "build", "run", "settings"] {
                    let r = self.chrome.hits[key];
                    assert!(
                        screen.contains_rect(r),
                        "clipped essential control {key}: {r:?} screen={screen:?}"
                    );
                }
                s.checks.push(
                    "Every essential toolbar action stays inside actual compact viewport".into(),
                );
                click = Some("toggle-dock".into());
            }
            33 => {
                assert!(!self.chrome.dock_open);
                click = Some("toggle-dock".into());
            }
            34 => {
                assert!(self.chrome.dock_open);
                s.checks
                    .push("Output panel collapses and reopens through its live button".into());
                let interrupted = self.chrome.motion.interrupted_keys();
                for key in ["explorer-width", "dock-tab-x"] {
                    assert!(
                        interrupted.iter().any(|(k, n)| k == key && *n > 0),
                        "not interrupted: {key}"
                    );
                }
                assert!(self.chrome.motion.interruptions > 0);
                let report = serde_json::json!({"result":"GUI_EXPRESSIVE_OK","renderer":"wgpu","input":"eframe raw_input_hook pointer/key events","checks":s.checks,"screenshots":s.shots,"interruptions":self.chrome.motion.interruptions,"interrupted_keys":interrupted,"motion_samples":s.samples,"disk_source_unchanged":true,"device_serial":self.selected,"deployment_invoked":false});
                std::fs::write(
                    s.dir.join("live-window.json"),
                    serde_json::to_vec_pretty(&report).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "GUI_EXPRESSIVE_OK: 8 screenshots; tabs/dirty-close/device/dock/interruption/reduced-motion/responsive controls; disk unchanged; no deployment"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        if let Some(key) = click {
            s.deferred_click = Some((key, 0));
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.samples
            .push(serde_json::json!({"step":s.step,"values":self.chrome.motion.snapshot()}));
        if !hold {
            s.step += 1;
        }
        // Rapid reversals use actual frames, not an artificial dt or a reset spring.
        s.next = std::time::Instant::now()
            + Duration::from_millis(if [5, 6, 8].contains(&s.step) {
                35
            } else if [29, 31].contains(&s.step) {
                900
            } else {
                240
            });
        self.chrome.smoke = Some(s);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub(super) fn ui_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.chrome.smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let path = s.dir.join(format!("{name}.png"));
                    let pixels = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        &path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    s.shots.push(serde_json::json!({"path":path,"width":image.size[0],"height":image.size[1]}));
                    s.awaiting = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, Workbench) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.kt"), "class A\n").unwrap();
        std::fs::write(dir.path().join("B.kt"), "class B\n").unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let a = Workbench::new(dir.path().canonicalize().unwrap(), ctx, None, false);
        (dir, a)
    }
    #[test]
    fn parked_tabs_keep_rope_cursor_and_undo() {
        let (_d, mut a) = app();
        let path = a.doc.path.clone().unwrap();
        a.apply_edit(0, 0, "// first\n");
        let cursor = a.edit.cursor;
        let next = a.files.iter().find(|p| **p != path).unwrap().clone();
        assert!(a.open_file_wait(next));
        assert!(a.parked[&path].0.dirty());
        assert!(a.open_file_wait(path));
        assert_eq!(a.edit.cursor, cursor);
        assert_eq!(a.tabs.len(), 2);
        a.doc.undo().unwrap();
        assert!(!a.doc.dirty());
    }
    #[test]
    fn close_dirty_requires_decision_and_save_preserves_disk() {
        let (_d, mut a) = app();
        let path = a.doc.path.clone().unwrap();
        a.apply_edit(0, 0, "// change\n");
        a.close_tab(path.clone(), false);
        assert_eq!(a.pending_close, Some(path.clone()));
        assert_eq!(a.tabs.len(), 1);
        a.pending_close = None;
        assert!(a.doc.dirty());
        assert!(a.save_path(&path));
        a.close_tab(path.clone(), true);
        assert!(a.tabs.is_empty());
        assert!(a.doc.path.is_none());
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .starts_with("// change")
        );
    }
    #[test]
    fn discard_does_not_write_and_save_all_includes_parked() {
        let (_d, mut a) = app();
        let p = a.doc.path.clone().unwrap();
        let original = std::fs::read_to_string(&p).unwrap();
        a.apply_edit(0, 0, "// discard\n");
        a.close_tab(p.clone(), true);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
        a.open_file_wait(p.clone());
        a.apply_edit(0, 0, "// kept\n");
        let next = a.files.iter().find(|f| **f != p).unwrap().clone();
        a.open_file_wait(next.clone());
        a.apply_edit(0, 0, "// second\n");
        assert!(a.save_all_documents());
        assert!(std::fs::read_to_string(p).unwrap().starts_with("// kept"));
        assert!(
            std::fs::read_to_string(next)
                .unwrap()
                .starts_with("// second")
        );
    }
    #[test]
    fn save_all_stops_on_external_change() {
        let (_d, mut a) = app();
        let path = a.doc.path.clone().unwrap();
        a.apply_edit(0, 0, "// dirty\n");
        std::fs::write(path, "external edit\n").unwrap();
        assert!(!a.save_all_documents());
        assert!(a.doc.dirty());
    }
    #[test]
    fn responsive_toolbar_and_dock_fit_small_and_wide_layouts() {
        for (width, height) in [
            (760., 560.),
            (760., 800.),
            (1000., 560.),
            (1000., 800.),
            (1440., 800.),
        ] {
            let (_d, mut a) = app();
            a.reduced_motion = true;
            let ctx = a.context.clone();
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, height),
                        )),
                        ..Default::default()
                    },
                    |ctx| a.draw_shell(ctx),
                );
            }
            for key in [
                "device",
                "save",
                "build",
                "run",
                "settings",
                "toggle-dock",
                "dock-2",
                "preferences",
            ] {
                assert!(
                    ctx.content_rect().contains_rect(a.chrome.hits[key]),
                    "{key} clipped at {width}: {:?}",
                    a.chrome.hits[key]
                );
            }
        }
    }
    #[test]
    fn new_active_tab_is_revealed_after_horizontal_overflow() {
        let (dir, mut a) = app();
        a.reduced_motion = true;
        let mut last = PathBuf::new();
        for i in 0..12 {
            let path = dir.path().join(format!("File{i}.kt"));
            std::fs::write(&path, "class Example\n").unwrap();
            a.open_file_wait(path.clone());
            last = path;
        }
        let ctx = a.context.clone();
        for _ in 0..4 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760., 560.),
                    )),
                    ..Default::default()
                },
                |ctx| a.draw_shell(ctx),
            );
        }
        let key = format!("tab-{}", last.file_name().unwrap().to_string_lossy());
        assert!(
            ctx.content_rect().contains_rect(a.chrome.hits[&key]),
            "active tab should scroll into viewport"
        );
    }
    #[test]
    fn k2_snapshot_versions_do_not_regress_when_switches_coalesce() {
        let (_dir, mut a) = app();
        let document = language::SnapshotSender::default();
        let mut snapshots = document.subscribe();
        let (requests, _commands) = tokio::sync::mpsc::channel(8);
        let (_events, events) = tokio::sync::mpsc::channel(8);
        let task = a.runtime.spawn(std::future::pending());
        a.language = Some(Connection {
            document,
            requests,
            events,
            task,
        });
        a.send_snapshot();
        let first = a
            .runtime
            .block_on(snapshots.latest())
            .unwrap()
            .as_ref()
            .unwrap()
            .version;
        a.apply_edit(0, 0, "// dirty\n");
        a.send_snapshot();
        let original = a.doc.path.clone().unwrap();
        let edited = a
            .runtime
            .block_on(snapshots.latest())
            .unwrap()
            .as_ref()
            .unwrap()
            .version;
        assert!(edited > first);
        let next = a.files.iter().find(|p| **p != original).unwrap().clone();
        a.open_file_wait(next);
        a.open_file_wait(original.clone());
        let back = a
            .runtime
            .block_on(snapshots.latest())
            .unwrap()
            .as_ref()
            .unwrap()
            .version;
        assert!(back > edited);
        assert!(
            a.runtime
                .block_on(snapshots.latest())
                .unwrap()
                .as_ref()
                .unwrap()
                .text
                .starts_with("// dirty")
        );
        a.close_tab(original.clone(), true);
        a.open_file_wait(original);
        let latest = a.runtime.block_on(snapshots.latest()).unwrap();
        assert!(latest.as_ref().unwrap().version > back);
        assert!(!latest.as_ref().unwrap().text.starts_with("// dirty"));
    }
    #[test]
    fn popup_text_does_not_edit_source() {
        let (_d, mut a) = app();
        a.chrome.device_open = true;
        let before = a.doc.rope().to_string();
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Text("search".into())],
                ..Default::default()
            },
            |ctx| a.draw_shell(ctx),
        );
        assert_eq!(a.doc.rope().to_string(), before);
    }
    #[test]
    fn explorer_file_click_identity_survives_unrelated_prefix_widget_ids() {
        let (d, mut a) = app();
        let ctx = a.context.clone();
        let frame = |a: &mut Workbench, prefix: usize, events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760., 560.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.set_max_width(250.);
                        ui.skip_ahead_auto_ids(prefix);
                        a.explorer(ui);
                    });
                },
            );
        };
        for _ in 0..3 {
            frame(&mut a, 0, vec![]);
        }
        let pos = a.chrome.hits["file-B.kt"].center();
        for (pressed, prefix) in [(true, 0), (false, 1)] {
            frame(
                &mut a,
                prefix,
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
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while a.open_ui.pending.is_some() && std::time::Instant::now() < until {
            a.document_open_updates();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            a.doc.path,
            Some(d.path().join("B.kt")),
            "same displayed file and unchanged geometry must keep its press/release identity"
        );
    }
    #[test]
    fn explorer_path_tooltip_does_not_eat_first_click_on_neighbor_file() {
        let (d, mut a) = app();
        let ctx = a.context.clone();
        ctx.style_mut(|s| {
            s.interaction.tooltip_delay = 0.;
            s.interaction.tooltip_grace_time = 0.;
            s.interaction.show_tooltips_only_when_still = false;
        });
        let mut time = 0.;
        let mut frame = |a: &mut Workbench, events: Vec<egui::Event>| {
            time += 0.1;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760., 560.),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.set_max_width(250.);
                        a.explorer(ui);
                    });
                },
            );
        };
        for _ in 0..3 {
            frame(&mut a, vec![]);
        }
        let hover = a.chrome.hits["file-A.kt"].center();
        frame(&mut a, vec![egui::Event::PointerMoved(hover)]);
        for _ in 0..5 {
            frame(&mut a, vec![]);
        }
        let pos = a.chrome.hits["file-B.kt"].center();
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
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while a.open_ui.pending.is_some() && std::time::Instant::now() < until {
            a.document_open_updates();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            a.doc.path,
            Some(d.path().join("B.kt")),
            "a plain path tooltip must be click-through to the first neighboring file click"
        );
    }
}
