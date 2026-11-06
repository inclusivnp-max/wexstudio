//! Project scan/read/Rope/Tree-sitter and preferences run off the GPU thread.
//! A dropped/cancelled generation cannot commit, start K2/devices, or record Recent.
use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use wexandroidstudio::material::{self as m, ButtonKind as Kind, Icon};
pub(super) struct Loaded {
    pub files: Vec<PathBuf>,
    pub doc: Document,
    pub tree: project::ExplorerInventory,
    pub variant: String,
    pub module: String,
    pub compose: bool,
    pub native: bool,
    pub language: bool,
    pub breakpoints: wexandroidstudio::breakpoint_store::Loaded,
}
pub(super) struct Job {
    rx: mpsc::Receiver<Result<Loaded, String>>,
    cancel: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    pub started: Instant,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Job {
    pub fn new(root: PathBuf, ctx: egui::Context) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = cancel.clone();
        let finished = done.clone();
        let thread = std::thread::Builder::new()
            .name("wex-project-open".into())
            .spawn(move || {
                let result =
                    load(&root, || stop.load(Ordering::Relaxed)).map_err(|e| format!("{e:#}"));
                if !stop.load(Ordering::Relaxed) {
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
                finished.store(true, Ordering::Release);
            })
            .expect("project open worker");
        Self {
            rx,
            cancel,
            done,
            started: Instant::now(),
            thread: Some(thread),
        }
    }
    fn poll(&mut self) -> Option<Result<Loaded, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Project opener stopped · retry".into()))
            }
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = self.thread.take().unwrap().join();
        }
    }
}
fn load(root: &std::path::Path, cancel: impl Fn() -> bool) -> anyhow::Result<Loaded> {
    anyhow::ensure!(!cancel(), "Project open cancelled");
    let native = root.join("wex.json").is_file();
    let config_bytes = if native {
        Some(std::fs::read(root.join("wex.json"))?)
    } else {
        None
    };
    let cfg = if native {
        Some(wexandroidstudio::native::Config::load(root)?)
    } else {
        None
    };
    if let Some(bytes) = &config_bytes {
        anyhow::ensure!(
            std::fs::read(root.join("wex.json"))? == *bytes,
            "Project configuration changed while opening · retry"
        );
    }
    let tree = project::explorer_entries_until(root, &cancel);
    anyhow::ensure!(!cancel(), "Project open cancelled");
    if let Some(e) = &tree.error {
        anyhow::bail!("Project scan: {e}");
    }
    let files = tree.sources();
    let path = files
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "kt"))
        .or_else(|| {
            files
                .iter()
                .find(|p| p.extension().is_some_and(|e| e == "java"))
        })
        .or_else(|| files.first());
    let doc = if let Some(p) = path {
        Document::open_cancellable(p, &cancel)?
    } else {
        Document::new("// Create or open a source file from the project panel.\n")?
    };
    anyhow::ensure!(!cancel(), "Project open cancelled");
    if let Some(bytes) = &config_bytes {
        anyhow::ensure!(
            std::fs::read(root.join("wex.json"))? == *bytes,
            "Project configuration changed while opening · retry"
        );
    }
    let breakpoints = wexandroidstudio::breakpoint_store::load(root);
    Ok(Loaded {
        files,
        doc,
        tree,
        variant: variants::load(root),
        module: cfg
            .as_ref()
            .map(|c| format!(":{}", c.build.application_module))
            .unwrap_or_else(|| ":app".into()),
        compose: cfg.as_ref().is_some_and(|c| c.build.compose.is_some()),
        native,
        language: (native
            || root.join("settings.gradle.kts").is_file()
            || root.join("settings.gradle").is_file())
            && kotlin::default_server().is_file(),
        breakpoints,
    })
}
#[derive(Default)]
pub(super) struct Boot {
    pub job: Option<Job>,
    pub error: Option<String>,
    pub pending: bool,
    pub loaded_ms: Option<u128>,
    pub configure: bool,
}
impl Workbench {
    pub(super) fn for_launch(
        root: PathBuf,
        ctx: egui::Context,
        screenshot: Option<PathBuf>,
        semantic: bool,
        welcome: bool,
    ) -> Self {
        let mut app = Self::with_mode(root, ctx, screenshot, semantic, true);
        if !welcome {
            app.welcome = false;
            app.boot.pending = true;
            app.boot.configure = true;
            app.boot.job = Some(Job::new(app.root.clone(), app.context.clone()));
            app.status = "Opening project on a worker…".into();
        }
        app
    }
    pub(super) fn boot_poll(&mut self) {
        let Some(job) = self.boot.job.as_mut() else {
            return;
        };
        let Some(result) = job.poll() else {
            return;
        };
        let elapsed = job.started.elapsed().as_millis();
        self.boot.job = None;
        match result {
            Err(e) => {
                self.boot.error = Some(e);
            }
            Ok(data) => {
                self.startup_trace.mark("project_loaded");
                self.boot.pending = false;
                self.boot.error = None;
                self.boot.loaded_ms = Some(elapsed);
                self.files = data.files;
                self.doc = data.doc;
                self.tabs = self.doc.path.iter().cloned().collect();
                self.file_ui.inventory = data.tree;
                self.variant = data.variant;
                self.app_module = data.module;
                self.compose_enabled = data.compose;
                self.native_language = data.native;
                self.debug = debug_ui::DebugUi::with_preferences(&self.root, data.breakpoints);
                self.editor_ui = edit_ui::EditorUi::new(self.root.clone(), self.context.clone());
                self.status = "Project loaded · Rust / wgpu".into();
                self.editor_switched();
                self.sync_watches();
                self.refresh_devices();
                if data.language {
                    self.connect_language(false);
                }
                if data.native {
                    self.projects
                        .history
                        .submit(project_recent::Action::Record(self.root.clone()));
                }
                if self.boot.configure {
                    self.boot.configure = false;
                    self.configure_smokes();
                }
            }
        }
    }
    pub(super) fn boot_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(m::SURFACE).inner_margin(28))
            .show(ctx, |ui| {
                ui.add_space((ui.available_height() * 0.23).max(24.));
                ui.vertical_centered(|ui| {
                    ui.heading(RichText::new("Wex Studio").size(40.));
                    ui.add_space(16.);
                    ui.label(RichText::new(self.root.to_string_lossy()).color(m::MUTED));
                    ui.add_space(16.);
                    if let Some(error) = &self.boot.error {
                        ui.label(RichText::new(error).color(m::ERROR));
                        ui.add_space(12.);
                        if self
                            .chrome
                            .button(
                                ui,
                                "boot-retry",
                                "Retry opening",
                                Icon::Refresh,
                                Kind::Filled,
                                true,
                                190.,
                                self.reduced_motion,
                            )
                            .clicked()
                        {
                            self.boot.error = None;
                            self.boot.job = Some(Job::new(self.root.clone(), ctx.clone()));
                        }
                    } else {
                        ui.label("Opening files and preparing the editor…");
                        ui.add_space(10.);
                        if self.reduced_motion {
                            ui.label("Opening…");
                        } else {
                            ui.add(egui::Spinner::new().color(m::PRIMARY));
                        }
                        ui.label(
                            RichText::new("Source scanning and parsing stay off the UI thread.")
                                .small()
                                .color(m::MUTED),
                        );
                    }
                    ui.add_space(16.);
                    if self
                        .chrome
                        .button(
                            ui,
                            "boot-cancel",
                            "Back to welcome",
                            Icon::Close,
                            Kind::Tonal,
                            true,
                            210.,
                            self.reduced_motion,
                        )
                        .clicked()
                    {
                        self.cancel_boot();
                    }
                });
            });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.cancel_boot();
        }
        ctx.request_repaint_after(Duration::from_millis(30));
    }
    pub(super) fn cancel_boot(&mut self) {
        self.boot.job = None;
        self.boot.pending = false;
        self.boot.configure = false;
        self.boot.error = None;
        self.welcome = true;
        self.semantic_smoke = false;
        self.projects.history.ensure_loaded();
        self.status = "Project opening cancelled".into();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_is_inert_until_real_worker_commits() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "val a=1\n").unwrap();
        let mut app = Workbench::for_launch(
            d.path().to_owned(),
            egui::Context::default(),
            None,
            false,
            false,
        );
        app.boot.configure = false;
        assert!(
            app.boot.pending
                && app.doc.path.is_none()
                && app.language.is_none()
                && app.devices.is_empty()
        );
        assert_eq!(app.editor_ui.services.active_workers(), 0);
        let end = Instant::now() + Duration::from_secs(5);
        while app.boot.pending {
            assert!(Instant::now() < end);
            app.boot_poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.doc.rope().to_string(), "val a=1\n");
        assert_eq!(app.editor_ui.services.active_workers(), 2);
    }
    #[test]
    fn queued_completed_candidate_never_commits_after_cancel() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.kt"), "val a=1\n").unwrap();
        let mut app = Workbench::for_launch(
            d.path().to_owned(),
            egui::Context::default(),
            None,
            false,
            false,
        );
        app.boot.configure = false;
        let done = app.boot.job.as_ref().unwrap().done.clone();
        let end = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::Acquire) {
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        app.cancel_boot();
        app.boot_poll();
        assert!(
            app.welcome
                && !app.boot.pending
                && app.doc.path.is_none()
                && app.language.is_none()
                && app.devices.is_empty()
        );
    }
    #[test]
    fn invalid_project_retains_error_and_cancel_drops_late_candidate() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("wex.json"), "bad").unwrap();
        let mut app = Workbench::for_launch(
            d.path().to_owned(),
            egui::Context::default(),
            None,
            false,
            false,
        );
        app.boot.configure = false;
        let end = Instant::now() + Duration::from_secs(5);
        while app.boot.error.is_none() {
            assert!(Instant::now() < end);
            app.boot_poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.boot.pending && app.doc.path.is_none() && app.language.is_none());
        app.cancel_boot();
        app.boot_poll();
        assert!(
            app.welcome && !app.boot.pending && app.doc.path.is_none() && app.devices.is_empty()
        );
    }
}
