//! Projectless native welcome: no source discovery, project preferences, device or K2 startup.
use super::*;
use wexandroidstudio::material::{self as m, ButtonKind as Kind, Icon};
pub(super) struct Startup {
    dir: PathBuf,
    started: std::time::Instant,
    first_update: Option<u128>,
    first_presented: Option<u128>,
    frames: u32,
    requested: bool,
}
impl Startup {
    pub fn new(dir: PathBuf, started: std::time::Instant) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            started,
            first_update: None,
            first_presented: None,
            frames: 0,
            requested: false,
        }
    }
}
impl Workbench {
    pub(super) fn draw_welcome(&mut self, ctx: &egui::Context) {
        self.chrome.motion.prune(ctx.cumulative_frame_nr());
        if self.reduced_motion {
            self.chrome.motion.settle_all();
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(m::SURFACE).inner_margin(24))
            .show(ctx, |ui| {
                ui.add_space(((ui.available_height() - 400.) / 2.).max(24.));
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(76., 76.), egui::Sense::hover());
                    ui.painter().rect_filled(r, 24, m::PRIMARY_CONTAINER);
                    ui.painter().text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        "W",
                        FontId::proportional(44.),
                        m::ON_CONTAINER,
                    );
                    ui.add_space(18.);
                    ui.label(RichText::new("Wex Studio").size(40.));
                    ui.label(
                        RichText::new("Make your next Android app")
                            .size(20.)
                            .color(m::MUTED),
                    );
                    ui.add_space(24.);
                    ui.horizontal(|ui| {
                        ui.add_space(((ui.available_width() - 556.) / 2.).max(0.));
                        for (mode, key, label) in [
                            (0, "welcome-new", "New project"),
                            (1, "welcome-open", "Open project"),
                            (2, "welcome-recent", "Recent projects"),
                        ] {
                            if self
                                .chrome
                                .button(
                                    ui,
                                    key,
                                    label,
                                    Icon::Folder,
                                    if mode == 0 { Kind::Filled } else { Kind::Tonal },
                                    true,
                                    180.,
                                    self.reduced_motion,
                                )
                                .clicked()
                            {
                                self.show_projects();
                                self.projects.existing_mode = mode == 1;
                                self.projects.recent_mode = mode == 2;
                            }
                        }
                    });
                    ui.add_space(24.);
                    ui.label(
                        RichText::new("Rust · Native GPU · wex.json · No Gradle").color(m::MUTED),
                    );
                    ui.add_space(8.);
                    ui.label(
                        RichText::new(
                            "Code analysis and Android tools start after you open a project.",
                        )
                        .small()
                        .color(m::MUTED),
                    );
                    if self.projects.history.busy {
                        ui.label(
                            RichText::new("Loading recent projects…")
                                .small()
                                .color(m::MUTED),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{} recent projects",
                                self.projects.history.records.len()
                            ))
                            .small()
                            .color(m::MUTED),
                        );
                    }
                    if let Some(error) = &self.projects.history.error {
                        ui.label(RichText::new(error).color(m::WARNING));
                    }
                });
            });
    }
    pub(super) fn startup_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.startup_smoke else {
            return;
        };
        s.first_update
            .get_or_insert_with(|| s.started.elapsed().as_millis());
        s.frames += 1;
        assert!(
            s.started.elapsed() < Duration::from_secs(40),
            "startup screenshot timeout"
        );
        if s.frames >= 3
            && !s.requested
            && (s.first_presented.is_none() || s.started.elapsed() >= Duration::from_millis(1300))
        {
            s.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                let path = s.dir.join(if s.first_presented.is_none() {
                    "welcome-first.png"
                } else {
                    "welcome.png"
                });
                let bytes = image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>();
                image::save_buffer(
                    &path,
                    &bytes,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                if s.first_presented.is_none() {
                    s.first_presented = Some(s.started.elapsed().as_millis());
                    s.requested = false;
                    return;
                }
                assert!(
                    self.welcome
                        && self.files.is_empty()
                        && self.doc.path.is_none()
                        && self.language.is_none()
                        && !self.native_language
                        && self.devices.is_empty()
                );
                assert_eq!(project::discovery_calls(), 0);
                let executable = std::env::current_exe().unwrap();
                let result = serde_json::json!({"result":"NATIVE_WELCOME_STARTUP_OK","long_line_layout":self.editor_ui.long_lines.worker.metrics(),"welcome":true,"files_loaded":self.files.len(),"source_discovery_calls":project::discovery_calls(),"document":self.doc.path,"k2_started":self.language.is_some(),"devices_loaded":self.devices.len(),"first_update_ms":s.first_update,"first_gpu_screenshot_ms":s.first_presented,"inventory_observation_ms":s.started.elapsed().as_millis(),"gpu_screenshot_received_ms":s.started.elapsed().as_millis(),"executable_bytes":std::fs::metadata(&executable).unwrap().len(),"executable":executable,"release_mode":!cfg!(debug_assertions),"panic_abort":cfg!(panic="abort"),"startup_trace":self.startup_trace.json(),"editor_workers":self.editor_ui.services.active_workers(),"document_open_worker":self.open_ui.opener.worker_started(),"document_save_worker":self.save_ui.saver.worker_started(),"syntax_worker":self.syntax_ui.parser.worker_started(),"document_retirement":wexandroidstudio::document_retirement::snapshot(),"font_fallback_ready":self.fonts.ready,"font_fallback_installed":self.fonts.installed,"renderer":"wgpu","screenshot":path,"width":image.size[0],"height":image.size[1]});
                std::fs::write(
                    s.dir.join("startup.json"),
                    serde_json::to_vec_pretty(&result).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "NATIVE_WELCOME_STARTUP_OK: genuine GPU frame; zero source discovery/documents/K2/devices; first update {}ms; screenshot {}ms",
                    s.first_update.unwrap(),
                    s.started.elapsed().as_millis()
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn welcome_does_not_load_even_a_valid_native_project() {
        let t = tempfile::tempdir().unwrap();
        let root = wexandroidstudio::scaffold::create(t.path(), &Default::default())
            .unwrap()
            .root;
        let app = Workbench::with_mode(root, egui::Context::default(), None, false, true);
        assert!(
            app.files.is_empty()
                && app.doc.path.is_none()
                && app.language.is_none()
                && app.devices.is_empty()
        );
        assert!(!app.native_language && !app.compose_enabled);
        assert_eq!(app.variant, "debug");
        assert!(app.app_module.is_empty());
    }
}
