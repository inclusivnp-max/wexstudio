//! GPU desktop shell on winit + wgpu. The editor paints only visible Rope lines.
use eframe::egui::{self, Color32, FontId, Key, RichText, TextFormat};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use tokio::io::AsyncReadExt;
use wexandroidstudio::material::PassiveTooltip;
use wexandroidstudio::{
    adb::{AdbClient, Device},
    editing::EditState,
    editor::Document,
    gradle::SyncOptions,
    kotlin::{self, CompletionItem, Diagnostic},
    language::{self, Connection, Event as LanguageEvent, Request, RopeSnapshot},
    logcat::{Decoder, LogEntry, LogFilter, LogRing},
    project,
    run::{self, Deployment, RunEvent, RunOptions, RunReport},
};

#[path = "ide/analysis_smoke.rs"]
mod analysis_smoke;
#[path = "ide/boot.rs"]
mod boot;
#[path = "ide/boot_smoke.rs"]
mod boot_smoke;
#[path = "ide/breakpoint_preferences_smoke.rs"]
mod breakpoint_preferences_smoke;
#[path = "ide/breakpoint_review.rs"]
mod breakpoint_review_ui;
#[path = "ide/breakpoint_smoke.rs"]
mod breakpoint_smoke;
#[path = "ide/breakpoints.rs"]
mod breakpoint_ui;
#[path = "ide/compile_cache_smoke.rs"]
mod compile_cache_smoke;
#[path = "ide/compile_cache_ui.rs"]
mod compile_cache_ui;
#[path = "ide/debug_smoke.rs"]
mod debug_smoke;
#[path = "ide/debug.rs"]
mod debug_ui;
#[path = "ide/edit.rs"]
mod edit_ui;
#[path = "ide/editor_smoke.rs"]
mod editor_smoke;
#[path = "ide/files_smoke.rs"]
mod files_smoke;
#[path = "ide/files.rs"]
mod files_ui;
#[path = "ide/format_smoke.rs"]
mod format_smoke;
#[path = "ide/format.rs"]
mod format_ui;
#[path = "ide/git_branch_model_smoke.rs"]
mod git_branch_model_smoke;
#[path = "ide/git_branches_smoke.rs"]
mod git_branches_smoke;
#[path = "ide/git_flow_smoke.rs"]
mod git_flow_smoke;
#[path = "ide/git_smoke.rs"]
mod git_smoke;
#[path = "ide/git.rs"]
mod git_ui;
#[path = "ide/history_smoke.rs"]
mod history_smoke;
#[path = "ide/ime.rs"]
mod ime;
#[path = "ide/ime_smoke.rs"]
mod ime_smoke;
#[path = "ide/input_route.rs"]
mod input_route;
#[path = "ide/java_format_smoke.rs"]
mod java_format_smoke;
#[path = "ide/long_lines.rs"]
mod long_lines;
#[path = "ide/model_reload.rs"]
mod model_reload;
#[path = "ide/model_reload_smoke.rs"]
mod model_reload_smoke;
#[path = "ide/open_smoke.rs"]
mod open_smoke;
#[path = "ide/open.rs"]
mod open_ui;
#[path = "ide/pointer_input.rs"]
mod pointer_input;
#[path = "ide/preview_smoke.rs"]
mod preview_smoke;
#[path = "ide/preview.rs"]
mod preview_ui;
#[path = "ide/project_browser.rs"]
mod project_browser;
#[path = "ide/project_nav_smoke.rs"]
mod project_nav_smoke;
#[path = "ide/project_recent.rs"]
mod project_recent;
#[path = "ide/project_replace_smoke.rs"]
mod project_replace_smoke;
#[path = "ide/project_replace.rs"]
mod project_replace_ui;
#[path = "ide/project_replace_ux_smoke.rs"]
mod project_replace_ux_smoke;
#[path = "ide/project_search_smoke.rs"]
mod project_search_smoke;
#[path = "ide/project_search.rs"]
mod project_search_ui;
#[path = "ide/project_smoke.rs"]
mod project_smoke;
#[path = "ide/projects.rs"]
mod projects;
#[path = "ide/refactor_smoke.rs"]
mod refactor_smoke;
#[path = "ide/refactor.rs"]
mod refactor_ui;
#[path = "ide/save_smoke.rs"]
mod save_smoke;
#[path = "ide/save.rs"]
mod save_ui;
#[path = "ide/startup.rs"]
mod startup;
#[path = "ide/syntax_smoke.rs"]
mod syntax_smoke;
#[path = "ide/syntax.rs"]
mod syntax_ui;
#[path = "ide/terminal_smoke.rs"]
mod terminal_smoke;
#[path = "ide/terminal.rs"]
mod terminal_ui;
#[path = "ide/ui.rs"]
mod ui;
#[path = "ide/variants.rs"]
mod variants;
#[path = "ide/viewport_smoke.rs"]
mod viewport_smoke;
#[path = "ide/welcome.rs"]
mod welcome;
#[path = "ide/window_close.rs"]
mod window_close;
#[path = "ide/window_close_legacy.rs"]
mod window_close_legacy;
#[path = "ide/window_close_smoke.rs"]
mod window_close_smoke;

enum Update {
    PreviewReady(u64, Result<Box<preview_ui::RenderedPreview>, String>),
    PreviewPhase(u64, String),
    PreviewCatalog(u64, Box<wexandroidstudio::preview_catalog::Catalog>),
    FormattingApplied(u64, Box<Result<format_ui::Prepared, String>>),
    RefactorApplied(u64, Box<Result<refactor_ui::Prepared, String>>),
    Devices(Result<Vec<Device>, String>),
    DebugReady(
        u64,
        Box<Result<wexandroidstudio::debugger::Started, String>>,
    ),
    DebugReply(u64, u64, debug_ui::Reply, Result<serde_json::Value, String>),
    Log(u64, LogEntry),
    LogStatus(u64, String),
    RunReady(Box<Result<Deployment, String>>),
    Status(String),
    BuildLine(String),
    BuildDone(Result<Box<wexandroidstudio::build::BuildReport>, String>),
}
struct Workbench {
    root: PathBuf,
    boot: boot::Boot,
    open_ui: open_ui::OpenUi,
    save_ui: save_ui::SaveUi,
    syntax_ui: syntax_ui::SyntaxUi,
    syntax_smoke: Option<syntax_smoke::Smoke>,
    save_smoke: Option<save_smoke::Smoke>,
    open_smoke: Option<open_smoke::Smoke>,
    history_smoke: Option<history_smoke::Smoke>,
    viewport_smoke: Option<viewport_smoke::Smoke>,
    boot_smoke: Option<boot_smoke::Smoke>,
    startup_trace: startup::Trace,
    fonts: wexandroidstudio::material::FontFallback,
    first_update_seen: bool,
    welcome: bool,
    startup_smoke: Option<welcome::Startup>,
    projects: projects::Projects,
    psearch: project_search_ui::SearchUi,
    preplace: project_replace_ui::ReplaceUi,
    git: git_ui::GitUi,
    git_smoke: Option<git_smoke::Smoke>,
    git_flow_smoke: Option<git_flow_smoke::Smoke>,
    git_branches_smoke: Option<git_branches_smoke::Smoke>,
    git_branch_model_smoke: Option<git_branch_model_smoke::Smoke>,
    project_search_smoke: Option<project_search_smoke::Smoke>,
    project_replace_smoke: Option<project_replace_smoke::Smoke>,
    project_replace_ux_smoke: Option<project_replace_ux_smoke::Smoke>,
    project_smoke: Option<project_smoke::Smoke>,
    project_nav_smoke: Option<project_nav_smoke::Smoke>,
    analysis_smoke: Option<analysis_smoke::AnalysisSmoke>,
    preview: preview_ui::PreviewUi,
    preview_smoke: Option<preview_smoke::PreviewSmoke>,
    format_smoke: Option<format_smoke::Smoke>,
    java_format_smoke: Option<java_format_smoke::Smoke>,
    format: format_ui::FormatUi,
    refactor: refactor_ui::RefactorUi,
    refactor_smoke: Option<refactor_smoke::RefactorSmoke>,
    debug: debug_ui::DebugUi,
    breakpoint_smoke: Option<breakpoint_smoke::Smoke>,
    preference_smoke: Option<breakpoint_preferences_smoke::Smoke>,
    debug_smoke: Option<debug_smoke::DebugSmoke>,
    app_module: String,
    variant: String,
    variant_open: bool,
    last_build: Option<wexandroidstudio::build::BuildReport>,
    run_smoke_variant: Option<String>,
    variant_smoke_click: u8,
    variant_smoke_release: Option<(egui::Pos2, std::time::Instant)>,
    variant_smoke_visited: bool,
    variant_smoke_pending: bool,
    variant_smoke_screenshot: Option<PathBuf>,
    compose_enabled: bool,
    files_smoke: Option<files_smoke::Smoke>,
    file_ui: files_ui::FileUi,
    files: Vec<PathBuf>,
    doc: Document,
    edit: EditState,
    editor_ui: edit_ui::EditorUi,
    editor_smoke: Option<editor_smoke::EditorSmoke>,
    devices: Vec<Device>,
    selected: Option<String>,
    status: String,
    ring: LogRing,
    filter: String,
    filter_error: Option<String>,
    compiled_filter: LogFilter,
    output: std::collections::VecDeque<String>,
    tab: usize,
    terminal: terminal_ui::TerminalUi,
    ime: ime::Ime,
    ime_smoke: Option<ime_smoke::Smoke>,
    terminal_smoke: Option<terminal_smoke::Smoke>,
    building: bool,
    running: bool,
    run_task: Option<tokio::task::JoinHandle<()>>,
    last_run: Option<RunReport>,
    log_generation: u64,
    run_smoke: Option<String>,
    run_smoke_started: bool,
    run_smoke_done: bool,
    run_smoke_click: u8,
    run_smoke_release: Option<(egui::Pos2, std::time::Instant)>,
    chrome: ui::Chrome,
    tabs: Vec<PathBuf>,
    parked: std::collections::BTreeMap<PathBuf, (Document, EditState)>,
    pending_close: Option<PathBuf>,
    close_ui: window_close::Ui,
    legacy_exit: Option<window_close_legacy::Exit>,
    reduced_motion: bool,
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    tx: mpsc::SyncSender<Update>,
    rx: mpsc::Receiver<Update>,
    log_task: Option<tokio::task::JoinHandle<()>>,
    build_task: Option<tokio::task::JoinHandle<()>>,
    context: egui::Context,
    screenshot: Option<PathBuf>,
    rendered_frames: u32,
    language: Option<Connection>,
    language_status: String,
    model_status: Option<language::ModelStatus>,
    model_reload_smoke: Option<model_reload_smoke::Smoke>,
    compile_cache_smoke: Option<compile_cache_smoke::Smoke>,
    native_language: bool,
    diagnostics: Vec<Diagnostic>,
    diagnostic_snapshot: Option<(PathBuf, i32)>,
    completions: Vec<CompletionItem>,
    completion_version: i32,
    completion_pending: bool,
    last_sent: Option<(PathBuf, i32)>,
    snapshot_clock: i32,
    last_buffers: Vec<(PathBuf, i32, u64)>,
    last_edit: std::time::Instant,
    scroll_to_cursor: bool,
    semantic_smoke: bool,
    smoke_step: u8,
    smoke_deadline: std::time::Instant,
    smoke_source: Option<PathBuf>,
    smoke_completion: bool,
    smoke_definition: bool,
    smoke_diagnostic: bool,
}
impl Workbench {
    #[cfg(test)]
    fn new(
        root: PathBuf,
        ctx: egui::Context,
        screenshot: Option<PathBuf>,
        semantic_smoke: bool,
    ) -> Self {
        Self::with_mode(root, ctx, screenshot, semantic_smoke, false)
    }
    fn with_mode(
        root: PathBuf,
        ctx: egui::Context,
        screenshot: Option<PathBuf>,
        semantic_smoke: bool,
        welcome: bool,
    ) -> Self {
        let files = if welcome {
            vec![]
        } else {
            project::source_files(&root)
        };
        let doc = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "kt"))
            .or_else(|| {
                files
                    .iter()
                    .find(|p| p.extension().is_some_and(|e| e == "java"))
            })
            .or_else(|| files.first())
            .and_then(|p| Document::open(p).ok())
            .unwrap_or_else(|| {
                Document::new("// Open a Kotlin file from the project panel.\n").unwrap()
            });
        let (tx, rx) = mpsc::sync_channel(2048);
        let app_module = if welcome {
            String::new()
        } else {
            wexandroidstudio::native::Config::load(&root)
                .map(|c| format!(":{}", c.build.application_module))
                .unwrap_or_else(|_| ":app".into())
        };
        let tabs = doc.path.iter().cloned().collect();
        let editor_ui = if welcome {
            edit_ui::EditorUi::without_inventory(root.clone(), ctx.clone())
        } else {
            edit_ui::EditorUi::new(root.clone(), ctx.clone())
        };
        let debug = if welcome {
            Default::default()
        } else {
            debug_ui::DebugUi::load(&root)
        };
        let variant = if welcome {
            "debug".into()
        } else {
            variants::load(&root)
        };
        let compose_enabled = !welcome
            && wexandroidstudio::native::Config::load(&root)
                .is_ok_and(|c| c.build.compose.is_some());
        let mut app = Self {
            root,
            boot: Default::default(),
            open_ui: Default::default(),
            save_ui: Default::default(),
            syntax_ui: Default::default(),
            syntax_smoke: None,
            save_smoke: None,
            open_smoke: None,
            history_smoke: None,
            viewport_smoke: None,
            boot_smoke: None,
            startup_trace: Default::default(),
            fonts: Default::default(),
            first_update_seen: false,
            welcome,
            startup_smoke: None,
            projects: Default::default(),
            psearch: Default::default(),
            preplace: Default::default(),
            git: Default::default(),
            git_smoke: None,
            git_flow_smoke: None,
            git_branches_smoke: None,
            git_branch_model_smoke: None,
            project_search_smoke: None,
            project_replace_smoke: None,
            project_replace_ux_smoke: None,
            project_smoke: None,
            project_nav_smoke: None,
            analysis_smoke: None,
            preview: Default::default(),
            preview_smoke: None,
            debug,
            breakpoint_smoke: None,
            preference_smoke: None,
            format_smoke: None,
            java_format_smoke: None,
            format: Default::default(),
            refactor: Default::default(),
            refactor_smoke: None,
            debug_smoke: None,
            app_module,
            variant,
            variant_open: false,
            last_build: None,
            run_smoke_variant: None,
            variant_smoke_click: 0,
            variant_smoke_release: None,
            variant_smoke_visited: false,
            variant_smoke_pending: false,
            variant_smoke_screenshot: None,
            compose_enabled,
            files_smoke: None,
            file_ui: Default::default(),
            files,
            doc,
            edit: EditState::default(),
            editor_ui,
            editor_smoke: None,
            devices: vec![],
            selected: None,
            status: if welcome {
                "Choose a project to begin"
            } else {
                "Workspace ready · Rust / wgpu"
            }
            .into(),
            ring: LogRing::new(5 * 1024 * 1024),
            filter: String::new(),
            filter_error: None,
            compiled_filter: LogFilter::default(),
            output: Default::default(),
            tab: 0,
            terminal: Default::default(),
            ime: Default::default(),
            ime_smoke: None,
            terminal_smoke: None,
            building: false,
            running: false,
            run_task: None,
            last_run: None,
            log_generation: 0,
            run_smoke: None,
            run_smoke_started: false,
            run_smoke_done: false,
            run_smoke_click: 0,
            run_smoke_release: None,
            chrome: Default::default(),
            tabs,
            parked: Default::default(),
            pending_close: None,
            close_ui: Default::default(),
            legacy_exit: None,
            reduced_motion: std::env::var_os("WEX_REDUCED_MOTION").is_some(),
            runtime: std::sync::Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap(),
            ),
            tx,
            rx,
            log_task: None,
            build_task: None,
            context: ctx,
            screenshot,
            rendered_frames: 0,
            language: None,
            language_status: "K2 disconnected".into(),
            model_status: None,
            model_reload_smoke: None,
            compile_cache_smoke: None,
            native_language: false,
            diagnostics: vec![],
            diagnostic_snapshot: None,
            completions: vec![],
            completion_version: 0,
            completion_pending: false,
            last_sent: None,
            snapshot_clock: 0,
            last_buffers: vec![],
            last_edit: std::time::Instant::now(),
            scroll_to_cursor: false,
            semantic_smoke,
            smoke_step: 0,
            smoke_deadline: std::time::Instant::now() + Duration::from_secs(120),
            smoke_source: None,
            smoke_completion: false,
            smoke_definition: false,
            smoke_diagnostic: false,
        };
        app.doc.defer_syntax();
        if !welcome {
            app.refresh_devices();
        }
        if !welcome
            && (app.root.join("wex.json").exists()
                || app.root.join("settings.gradle.kts").exists()
                || app.root.join("settings.gradle").exists())
            && kotlin::default_server().is_file()
        {
            app.connect_language(false);
        }
        app
    }
    fn configure_smokes(&mut self) {
        if std::env::args().nth(2).as_deref() == Some("--branch-model-smoke") {
            let deploy = std::env::args().nth(4).as_deref() == Some("--serial");
            if deploy {
                assert_eq!(std::env::args().nth(5).as_deref(), Some("emulator-5580"));
            } else {
                assert_eq!(std::env::args().nth(4).as_deref(), Some("--desktop-only"));
            }
            self.git_branch_model_smoke = Some(git_branch_model_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("Branch model output")),
                self,
                deploy,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--git-branches-smoke") {
            self.git_branches_smoke = Some(git_branches_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("Branch evidence")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--git-flow-smoke") {
            self.git_flow_smoke = Some(git_flow_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("Git flow evidence")),
                self,
            ));
        }

        if std::env::args().nth(2).as_deref() == Some("--git-smoke") {
            self.git_smoke = Some(git_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("Git evidence")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--syntax-smoke") {
            self.syntax_smoke = Some(syntax_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("syntax evidence")),
                self,
            ));
        }

        if std::env::args().nth(2).as_deref() == Some("--save-smoke") {
            self.save_smoke = Some(save_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("save evidence")),
                self,
            ));
        }

        if std::env::args().nth(2).as_deref() == Some("--gesture-smoke") {
            let mut smoke = viewport_smoke::Smoke::new(
                PathBuf::from(std::env::args().nth(3).expect("gesture output")),
                self,
            );
            smoke.cold = Some(Default::default());
            smoke.gesture = Some(Default::default());
            self.viewport_smoke = Some(smoke);
        }
        if std::env::args().nth(2).as_deref() == Some("--layout-smoke") {
            let mut smoke = viewport_smoke::Smoke::new(
                PathBuf::from(std::env::args().nth(3).expect("layout output")),
                self,
            );
            smoke.cold = Some(Default::default());
            self.viewport_smoke = Some(smoke);
        }
        if std::env::args().nth(2).as_deref() == Some("--viewport-smoke") {
            self.viewport_smoke = Some(viewport_smoke::Smoke::new(
                PathBuf::from(std::env::args().nth(3).expect("viewport output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--project-replace-ux-smoke") {
            self.project_replace_ux_smoke = Some(project_replace_ux_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("replacement UX evidence")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--project-replace-smoke") {
            self.project_replace_smoke = Some(project_replace_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("replace output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--project-search-smoke") {
            self.project_search_smoke = Some(project_search_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("search output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--ime-scope-smoke") {
            self.ime_smoke = Some(ime_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("IME evidence")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--terminal-smoke") {
            self.terminal_smoke = Some(terminal_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("terminal output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--input-order-smoke") {
            let mut smoke = history_smoke::Smoke::new(
                PathBuf::from(std::env::args().nth(3).expect("input order output")),
                self,
            );
            smoke.commands = Some(Default::default());
            self.history_smoke = Some(smoke);
        }
        if std::env::args().nth(2).as_deref() == Some("--history-smoke") {
            self.history_smoke = Some(history_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("history evidence")),
                self,
            ));
        }

        if std::env::args().nth(2).as_deref() == Some("--file-open-smoke") {
            self.open_smoke = Some(open_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("open evidence")),
                self,
            ));
        }

        if std::env::args().nth(2).as_deref() == Some("--files-smoke") {
            assert_eq!(std::env::args().nth(4).as_deref(), Some("--serial"));
            assert_eq!(std::env::args().nth(5).as_deref(), Some("emulator-5580"));
            self.files_smoke = Some(files_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("files output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--project-nav-smoke") {
            self.project_nav_smoke = Some(project_nav_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("navigation output")),
                std::env::args().nth(4).as_deref() == Some("--reopen"),
                self,
            ));
        }
        if !self.welcome
            && std::env::args().nth(2).is_none()
            && !self.root.join("wex.json").is_file()
            && !self.root.join("settings.gradle.kts").is_file()
            && !self.root.join("settings.gradle").is_file()
        {
            self.show_projects();
            self.projects.recent_mode = true;
        }
        if matches!(
            std::env::args().nth(2).as_deref(),
            Some("--project-smoke" | "--project-ready-smoke")
        ) {
            self.project_smoke = Some(project_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("project output")),
                std::env::args().nth(2).as_deref() == Some("--project-ready-smoke"),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--refactor-smoke") {
            let dir = PathBuf::from(
                std::env::args_os()
                    .nth(3)
                    .expect("refactor output directory"),
            )
            .canonicalize()
            .expect("existing output directory");
            self.refactor_smoke = Some(refactor_smoke::RefactorSmoke::new(dir, self));
        }
        if std::env::args().nth(2).as_deref() == Some("--ui-smoke") {
            self.chrome.smoke = Some(ui::UiSmoke::new(PathBuf::from(
                std::env::args_os().nth(3).expect("output directory"),
            )));
        }
        if matches!(
            std::env::args().nth(2).as_deref(),
            Some("--preview-smoke" | "--preview-catalog-smoke")
        ) {
            self.preview_smoke = Some(preview_smoke::PreviewSmoke::new(
                PathBuf::from(
                    std::env::args_os()
                        .nth(3)
                        .expect("preview output directory"),
                ),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--compile-cache-smoke") {
            self.compile_cache_smoke = Some(compile_cache_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("cache output directory")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--model-reload-smoke") {
            self.model_reload_smoke = Some(model_reload_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("reload output directory")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--analysis-smoke") {
            self.analysis_smoke = Some(analysis_smoke::AnalysisSmoke::new(
                PathBuf::from(
                    std::env::args_os()
                        .nth(3)
                        .expect("analysis output directory"),
                ),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--editor-smoke") {
            self.editor_smoke = Some(editor_smoke::EditorSmoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("output directory")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--java-format-smoke") {
            self.java_format_smoke = Some(java_format_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("output")),
                self,
            ));
        }
        if matches!(
            std::env::args().nth(2).as_deref(),
            Some("--breakpoint-preferences-smoke" | "--window-close-smoke")
        ) {
            self.preference_smoke = Some(breakpoint_preferences_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("preference proof output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--breakpoints-smoke") {
            self.breakpoint_smoke = Some(breakpoint_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("output")),
                self,
            ));
        }
        if std::env::args().nth(2).as_deref() == Some("--format-smoke") {
            self.format_smoke = Some(format_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("format output")),
                self,
            ));
        }
        if matches!(
            std::env::args().nth(2).as_deref(),
            Some("--debug-smoke" | "--debug-close-smoke")
        ) {
            assert_eq!(std::env::args().nth(4).as_deref(), Some("--serial"));
            self.debug_smoke = Some(debug_smoke::DebugSmoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("debug output directory")),
                std::env::args().nth(5).expect("explicit debug serial"),
                self,
            ));
        }
        if std::env::args().nth(6).as_deref() == Some("--variant") {
            let variant = std::env::args().nth(7).expect("explicit smoke variant");
            assert!(matches!(variant.as_str(), "debug" | "release"));
            self.run_smoke_variant = Some(variant);
        }
        self.run_smoke = if std::env::args().nth(2).as_deref() == Some("--run-smoke") {
            Some(std::env::args().nth(5).expect("explicit Run serial"))
        } else {
            None
        };
        if self.run_smoke.is_some() {
            self.smoke_deadline = std::time::Instant::now() + Duration::from_secs(240);
        }
    }
    fn connect_language(&mut self, force: bool) {
        if self.native_language
            && let Some(connection) = &self.language
            && !connection.task.is_finished()
        {
            if connection
                .requests
                .try_send(Request::Refresh {
                    force,
                    variant: Some(self.variant.clone()),
                })
                .is_err()
            {
                self.status = "Model request queue busy · retry".into();
            }
            return;
        }
        // Loader seeds this flag; genuine ModelReady updates it after config changes.
        self.language.take();
        self.model_status = None;
        self.completion_pending = false;
        self.diagnostics.clear();
        self.diagnostic_snapshot = None;
        self.completions.clear();
        self.last_sent = None;
        self.language_status = if self.root.join("wex.json").is_file() {
            "Android analysis model → K2 starting…"
        } else {
            "Gradle model → K2 starting…"
        }
        .into();
        self.native_language = self.native_language || self.root.join("wex.json").is_file();
        let start = if self.native_language {
            Connection::start_native
        } else {
            Connection::start
        };
        self.language = Some(start(
            &self.runtime,
            self.root.clone(),
            kotlin::default_server(),
            SyncOptions {
                force,
                variant: self.variant.clone(),
                ..Default::default()
            },
        ));
        self.send_snapshot();
    }
    fn send_snapshot(&mut self) {
        let timer = self
            .history_smoke
            .as_ref()
            .map(|_| std::time::Instant::now());
        if self.language.is_some()
            && let Some(snapshot) = self.language_snapshot()
        {
            self.language.as_ref().unwrap().document.send_rope(snapshot);
            if let Some(timer) = timer
                && let Some(smoke) = &mut self.history_smoke
            {
                smoke.capture_us.push(timer.elapsed().as_micros());
            }
        }
    }
    fn semantic_source_eligible(&self, path: &std::path::Path, doc: &Document) -> bool {
        path.extension()
            .is_some_and(|e| e == "kt" || e == "kts" || e == "java")
            && (doc.dirty()
                || !matches!(
                    self.editor_ui.conflicts.get(path),
                    Some(wexandroidstudio::editor_jobs::DiskState::Missing)
                ))
    }
    fn source_event_current(&self, path: &std::path::Path, version: i32) -> bool {
        self.doc.path.as_deref() == Some(path)
            && self.doc.version == version
            && self.semantic_source_eligible(path, &self.doc)
    }
    // Keep parked source overlays even while a config/text tab is active; close removed URIs.
    fn language_snapshot(&mut self) -> Option<Option<RopeSnapshot>> {
        let eligible = self
            .doc
            .path
            .iter()
            .filter(|p| self.semantic_source_eligible(p, &self.doc))
            .cloned()
            .chain(
                self.parked
                    .iter()
                    .filter(|(p, (d, _))| self.semantic_source_eligible(p, d))
                    .map(|(p, _)| p.clone()),
            )
            .collect::<std::collections::BTreeSet<_>>();
        let active = self
            .doc
            .path
            .as_ref()
            .filter(|p| eligible.contains(*p))
            .cloned();
        let path = active
            .clone()
            .or_else(|| self.parked.keys().find(|p| eligible.contains(*p)).cloned());
        let Some(path) = path else {
            let changed = self.last_sent.take().is_some() || !self.last_buffers.is_empty();
            self.last_buffers.clear();
            return changed.then_some(None);
        };
        let version = if active.is_some() {
            self.doc.version
        } else {
            self.parked[&path].0.version
        };
        let identities = self
            .parked
            .iter()
            .filter(|(p, _)| eligible.contains(*p))
            .map(|(p, (d, _))| (p.clone(), d.version, d.revision))
            .collect::<Vec<_>>();
        if self.last_sent.as_ref() == Some(&(path.clone(), version))
            && self.last_buffers == identities
        {
            return None;
        }
        if active.is_some() {
            self.snapshot_clock = self
                .snapshot_clock
                .max(self.doc.version)
                .checked_add(1)
                .expect("LSP version exhausted");
            self.doc.version = self.snapshot_clock;
        }
        for (p, (d, _)) in &mut self.parked {
            if eligible.contains(p)
                && (active.is_none() && *p == path
                    || !self
                        .last_buffers
                        .iter()
                        .any(|(old, v, r)| old == p && *v == d.version && *r == d.revision))
            {
                self.snapshot_clock = self
                    .snapshot_clock
                    .max(d.version)
                    .checked_add(1)
                    .expect("LSP version exhausted");
                d.version = self.snapshot_clock;
            }
        }
        let doc = if active.is_some() {
            &self.doc
        } else {
            &self.parked[&path].0
        };
        let snapshot = RopeSnapshot {
            path: path.clone(),
            text: doc.rope().clone(),
            version: doc.version,
            others: self
                .parked
                .iter()
                .filter(|(p, _)| eligible.contains(*p) && **p != path)
                .map(|(p, (d, _))| language::RopeBuffer {
                    path: p.clone(),
                    text: d.rope().clone(),
                    version: d.version,
                })
                .collect(),
        };
        self.last_sent = Some((path, snapshot.version));
        self.last_buffers = self
            .parked
            .iter()
            .filter(|(p, _)| eligible.contains(*p))
            .map(|(p, (d, _))| (p.clone(), d.version, d.revision))
            .collect();
        Some(Some(snapshot))
    }
    fn request_language(&mut self, definition: bool) {
        if !self
            .doc
            .path
            .as_ref()
            .is_some_and(|p| self.semantic_source_eligible(p, &self.doc))
        {
            self.completion_pending = false;
            self.completions.clear();
            self.status = "K2 needs a current source or an explicitly kept draft".into();
            return;
        }
        if !definition && self.completion_pending {
            return;
        }
        if !definition {
            self.completions.clear();
            self.completion_pending = true;
        }
        if definition {
            self.cancel_document_open();
        }
        self.send_snapshot();
        if let Some(connection) = &self.language
            && let Ok((line, character)) = self.doc.lsp_position(self.edit.cursor)
        {
            let request = if definition {
                self.status = "Finding definition…".into();
                Request::Definition {
                    navigation: self.open_ui.navigation,
                    line,
                    character,
                    path: self.doc.path.clone().unwrap(),
                    version: self.doc.version,
                }
            } else {
                Request::Complete {
                    line,
                    character,
                    path: self.doc.path.clone().unwrap(),
                    version: self.doc.version,
                }
            };
            if connection.requests.try_send(request).is_err() {
                self.language_status = "K2 request queue busy".into();
                self.completion_pending = false;
            }
        }
    }
    fn accept_completion(&mut self, index: usize) {
        self.doc.defer_syntax();
        if self.completion_version != self.doc.version {
            return;
        }
        if let Some(item) = self.completions.get(index).cloned() {
            if item.extra.get("command").is_some_and(|c| {
                c["command"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("jetbrains.kotlin.completion."))
            }) {
                if let Some(c) = &self.language
                    && let Some(path) = self.doc.path.clone()
                {
                    let _ = c.requests.try_send(Request::ApplyCompletion {
                        path,
                        version: self.doc.version,
                        item: Box::new(item),
                    });
                    self.completions.clear();
                }
                return;
            }
            match kotlin::apply_completion(&mut self.doc, self.edit.cursor, &item) {
                Ok(cursor) => {
                    self.edit.collapse(cursor);
                    self.completions.clear();
                    self.last_edit = std::time::Instant::now();
                    self.send_snapshot();
                }
                Err(e) => self.status = format!("Completion: {e:#}"),
            }
        }
    }
    fn save_document(&mut self) {
        if let Some(path) = self.doc.path.clone() {
            self.request_save_paths(vec![path], save_ui::After::None);
        } else {
            self.status = "Document has no path".into();
        }
    }
    fn saved_language(&mut self) {
        self.send_snapshot();
        if let Some(c) = &self.language {
            let _ = c.requests.try_send(Request::Saved);
        }
    }
    fn language_events(&mut self) {
        let mut events = Vec::new();
        if let Some(connection) = &mut self.language {
            while let Ok(e) = connection.events.try_recv() {
                events.push(e);
            }
        }
        for event in events {
            match event {
                LanguageEvent::Model(status) => self.model_update(status),
                LanguageEvent::OutsideModel { path, version } => {
                    if self.source_event_current(&path, version) {
                        self.diagnostics.clear();
                        self.diagnostic_snapshot = None;
                        self.language_status =
                            "Current file is outside the active model · buffer retained".into();
                    }
                }
                LanguageEvent::Formatting { id, result } => self.format_reply(id, result),
                LanguageEvent::Project { id, result } => self.project_reply(id, result),
                LanguageEvent::WorkspaceEdit {
                    path,
                    version,
                    edit,
                    reply,
                } => {
                    let applied = if self.doc.path.as_ref() == Some(&path)
                        && self.doc.version == version
                        && (!self.native_language
                            || self
                                .model_status
                                .as_ref()
                                .is_some_and(|s| s.phase == language::ModelPhase::Applied))
                    {
                        match kotlin::file_uri(&path).and_then(|uri| {
                            kotlin::apply_workspace_edit(
                                &mut self.doc,
                                &uri,
                                &edit,
                                self.edit.cursor,
                            )
                        }) {
                            Ok(cursor) => {
                                self.edit.collapse(cursor);
                                self.last_edit = std::time::Instant::now();
                                self.send_snapshot();
                                true
                            }
                            Err(e) => {
                                self.status = format!("K2 edit: {e:#}");
                                false
                            }
                        }
                    } else {
                        false
                    };
                    let _ = reply.send(applied);
                }
                LanguageEvent::Ready {
                    variant,
                    compose_enabled,
                    cache_hit,
                    modules,
                    server_info,
                } => {
                    if let Some(enabled) = compose_enabled {
                        self.compose_enabled = enabled;
                    }
                    self.language_status = format!(
                        "K2 {} · {} · {} modules · model cache {}",
                        server_info["version"].as_str().unwrap_or("ready"),
                        variant,
                        modules,
                        if cache_hit { "HIT" } else { "MISS" }
                    );
                    if self.semantic_smoke && self.smoke_step == 0 {
                        self.smoke_step = 1;
                    }
                }
                LanguageEvent::Status(s) => {
                    self.language_status = s;
                    self.completion_pending = false;
                }
                LanguageEvent::Completion {
                    path,
                    version,
                    items,
                } => {
                    self.completion_pending = false;
                    if self.source_event_current(&path, version) {
                        self.completions = items;
                        self.completion_version = version;
                    }
                }
                LanguageEvent::Diagnostics {
                    path,
                    version,
                    items,
                } => {
                    if self.source_event_current(&path, version) {
                        self.diagnostic_snapshot = Some((path, version));
                        self.diagnostics = items;
                    }
                }
                LanguageEvent::Definition {
                    navigation,
                    path,
                    version,
                    locations,
                } => {
                    if !self.definition_is_current(navigation, &path, version) {
                        continue;
                    }
                    match language::definition_path(&locations) {
                        Ok(Some((target, pos))) => {
                            self.open_at(target, open_ui::Intent::Definition(pos));
                        }
                        Ok(None) => self.status = "No definition found".into(),
                        Err(_) => {
                            self.status = format!("Library definition: {}", locations);
                        }
                    }
                }
            }
        }
    }
    fn semantic_smoke_step(&mut self) {
        if !self.semantic_smoke {
            return;
        }
        assert!(
            std::time::Instant::now() < self.smoke_deadline,
            "GUI K2 smoke timed out at step {}: {}",
            self.smoke_step,
            self.language_status
        );
        if self.smoke_step == 1 {
            self.smoke_source = self.doc.path.clone();
            let text = self.doc.rope().to_string();
            let probe = text.replace("greeter.greeting(title)", "greeter.gre");
            assert_ne!(text, probe, "smoke app must contain Greeter call");
            let at = probe.find("greeter.gre").unwrap() + "greeter.gre".len();
            self.apply_edit(0, self.doc.rope().len_chars(), &probe);
            self.edit.collapse(probe[..at].chars().count());
            self.request_language(false);
            self.smoke_step = 2;
        } else if self.smoke_step == 2 && self.completions.iter().any(|c| c.label == "greeting") {
            self.smoke_completion = true;
            let index = self
                .completions
                .iter()
                .position(|c| c.label == "greeting")
                .unwrap();
            self.accept_completion(index);
            self.smoke_step = 6;
        } else if self.smoke_step == 6 && self.doc.rope().to_string().contains("greeter.greeting") {
            assert!(self.doc.rope().to_string().contains("greeter.greeting"));
            let next_version = self.doc.version + 1;
            self.doc = Document::open(self.smoke_source.as_ref().unwrap()).unwrap();
            self.doc.version = next_version;
            self.last_sent = None;
            let text = self.doc.rope().to_string();
            let at = text.find("Greeter()").unwrap() + 2;
            self.edit.collapse(text[..at].chars().count());
            self.send_snapshot();
            self.request_language(true);
            self.smoke_step = 3;
        } else if self.smoke_step == 2
            && self.completions.is_empty()
            && self.last_edit.elapsed() > Duration::from_secs(2)
        {
            self.request_language(false);
            self.last_edit = std::time::Instant::now();
        } else if self.smoke_step == 3
            && self
                .doc
                .path
                .as_ref()
                .is_some_and(|p| p.ends_with("Greeter.kt"))
        {
            self.smoke_definition = true;
            self.doc = Document::open(self.smoke_source.as_ref().unwrap()).unwrap();
            self.last_sent = None;
            let text = self.doc.rope().to_string().replace(
                "val debug = BuildConfig.DEBUG",
                "val broken: Int = \"wrong\"\n        val debug = BuildConfig.DEBUG",
            );
            self.apply_edit(0, self.doc.rope().len_chars(), &text);
            self.send_snapshot();
            self.tab = 2;
            self.smoke_step = 4;
        } else if self.smoke_step == 4
            && self
                .diagnostics
                .iter()
                .any(|d| d.severity == Some(1) && d.message.contains("type mismatch"))
        {
            self.smoke_diagnostic = true;
            self.smoke_step = 5;
            self.rendered_frames = 4;
            self.language_status =
                "K2 verified: completion applied · source definition · live FIR diagnostic".into();
            eprintln!(
                "GUI_K2_OK: completion applied; definition Greeter.kt; FIR type mismatch rendered; disk source unchanged"
            );
        }
    }
    fn refresh_devices(&mut self) {
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.runtime.spawn(async move {
            let result = AdbClient::default()
                .devices()
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.try_send(Update::Devices(result));
            ctx.request_repaint();
        });
    }
    fn stop_logs(&mut self) {
        self.log_generation += 1;
        if let Some(task) = self.log_task.take() {
            task.abort();
        }
    }
    fn select_device(&mut self, serial: String) {
        if self.running
            || self.building
            || self.debug.connection.is_some()
            || self.debug.busy
            || self.selected.as_ref() == Some(&serial)
        {
            return;
        }
        self.stop_logs();
        self.ring.clear();
        self.last_run = None;
        self.compiled_filter.pid = None;
        self.selected = Some(serial);
    }
    fn start_logs(&mut self) {
        if self.running {
            return;
        }
        let Some(serial) = self.selected.clone() else {
            self.status = "Select a connected device".into();
            return;
        };
        self.compiled_filter.pid = None;
        self.attach_logs(None, serial, None);
    }
    fn attach_logs(
        &mut self,
        stream: Option<tokio::net::TcpStream>,
        serial: String,
        pid: Option<u32>,
    ) {
        self.stop_logs();
        let generation = self.log_generation;
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.tab = 0;
        self.chrome.dock_open = true;
        self.log_task = Some(self.runtime.spawn(async move {
            let result: anyhow::Result<()> = async {
                let mut stream = match stream {
                    Some(s) => s,
                    None => AdbClient::default().logcat(&serial).await?,
                };
                let mut decoder = Decoder::default();
                let mut bytes = [0; 16384];
                let mut dropped = 0u64;
                loop {
                    let n = stream.read(&mut bytes).await?;
                    if n == 0 {
                        decoder.finish()?;
                        break;
                    }
                    for e in decoder.feed(&bytes[..n])? {
                        if pid.is_some_and(|p| p != e.pid) {
                            continue;
                        }
                        match tx.try_send(Update::Log(generation, e)) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => dropped += 1,
                            Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                        }
                    }
                    if dropped > 0
                        && tx
                            .try_send(Update::LogStatus(
                                generation,
                                format!("Logcat UI queue dropped {dropped} records"),
                            ))
                            .is_ok()
                    {
                        dropped = 0;
                    }
                    ctx.request_repaint();
                }
                Ok(())
            }
            .await;
            let _ = tx.try_send(Update::LogStatus(
                generation,
                match result {
                    Ok(()) => "Logcat disconnected".into(),
                    Err(e) => format!("Logcat: {e:#}"),
                },
            ));
            ctx.request_repaint();
        }));
    }
    fn run(&mut self) {
        if self.closing_requested() || self.replace_blocked() || self.git.open {
            return;
        }
        if self.boot.pending
            || self.file_ui.open
            || self.running
            || self.building
            || self.debug.connection.is_some()
            || self.debug.busy
        {
            return;
        }
        let Some(serial) = self
            .selected
            .clone()
            .filter(|s| self.devices.iter().any(|d| &d.serial == s && d.usable()))
        else {
            self.status = "Select a connected device".into();
            return;
        };
        if !self.save_before(save_ui::After::Run) {
            return;
        }
        self.chrome.dock_open = true;
        self.stop_logs();
        self.last_run = None;
        self.ring.clear();
        self.output.clear();
        self.filter.clear();
        self.filter_error = None;
        self.compiled_filter = LogFilter::default();
        self.running = true;
        self.tab = 1;
        self.status = format!("Run · preparing {serial}");
        let root = self.root.clone();
        let module = self.app_module.clone();
        let variant = self.variant.clone();
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.run_task = Some(self.runtime.spawn(async move {
            let options = RunOptions {
                serial,
                module,
                variant,
                offline: true,
            };
            let result = run::deploy_project(&root, &AdbClient::default(), &options, &|e| {
                let update = match e {
                    RunEvent::Phase(s) => Update::Status(s),
                    RunEvent::BuildLine(s) => Update::BuildLine(s),
                };
                let _ = tx.try_send(update);
                ctx.request_repaint();
            })
            .await
            .map_err(|e| format!("Run failed: {e:#}"));
            let _ =
                tokio::task::spawn_blocking(move || tx.send(Update::RunReady(Box::new(result))))
                    .await;
            ctx.request_repaint();
        }));
    }
    fn run_semantics_ready(&self) -> bool {
        !self.compose_enabled
            || (self
                .diagnostic_snapshot
                .as_ref()
                .is_some_and(|(path, version)| {
                    self.doc.path.as_ref() == Some(path) && self.doc.version == *version
                })
                && self.diagnostics.iter().all(|d| d.severity != Some(1)))
    }
    fn run_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if self.run_smoke.is_none() {
            return;
        }
        // Own only this native smoke window, never the system pointer/focus of another app.
        input.focused = true;
        input.modifiers = egui::Modifiers::NONE;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        if let Some(wanted) = &self.run_smoke_variant
            && (&self.variant != wanted || self.variant_open)
        {
            if self.variant_open && !self.variant_smoke_visited {
                self.variant_smoke_visited = true;
                self.variant_smoke_pending = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            if self.variant_smoke_pending {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            if let Some((pos, until)) = self.variant_smoke_release.take() {
                input.events.push(egui::Event::PointerMoved(pos));
                if std::time::Instant::now() < until {
                    self.variant_smoke_release = Some((pos, until));
                } else {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    });
                    self.variant_smoke_click = 0;
                }
            } else {
                let key = if self.variant_open {
                    format!("variant-{wanted}")
                } else {
                    "variant".into()
                };
                if let Some(rect) = self.chrome.hits.get(&key) {
                    let pos = rect.center();
                    input.events.push(egui::Event::PointerMoved(pos));
                    if self.variant_smoke_click < 2 {
                        self.variant_smoke_click += 1;
                    } else {
                        input.events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        });
                        self.variant_smoke_release =
                            Some((pos, std::time::Instant::now() + Duration::from_millis(100)));
                    }
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if let Some((pos, deadline)) = self.run_smoke_release {
            input.events.push(egui::Event::PointerMoved(pos));
            if std::time::Instant::now() >= deadline {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
                self.run_smoke_release = None;
                self.run_smoke_click = 5;
            }
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if (1..=3).contains(&self.run_smoke_click)
            && let Some(rect) = self.chrome.hits.get("run")
        {
            let pos = rect.center();
            input.events.push(egui::Event::PointerMoved(pos));
            if self.run_smoke_click == 3 {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                self.run_smoke_release =
                    Some((pos, std::time::Instant::now() + Duration::from_millis(100)));
                self.run_smoke_click = 4;
            } else {
                self.run_smoke_click += 1;
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
    fn run_smoke_step(&mut self) {
        let Some(serial) = self.run_smoke.clone() else {
            return;
        };
        assert!(
            std::time::Instant::now() < self.smoke_deadline,
            "GUI phone Run timed out: {}",
            self.status
        );
        if self
            .run_smoke_variant
            .as_ref()
            .is_some_and(|v| v != &self.variant)
            || self.variant_open
        {
            return;
        }
        if !self.run_smoke_started
            && self
                .devices
                .iter()
                .any(|d| d.serial == serial && d.usable())
        {
            self.select_device(serial);
            self.run_smoke_started = true;
            self.run_smoke_click = 1;
        }
        if let Some(report) = &self.last_run {
            if self.rendered_frames.is_multiple_of(100) {
                eprintln!(
                    "RUN_SMOKE_WAIT pid={} id={} retained={} messages={:?}",
                    report.pid,
                    report.run_id,
                    self.ring.used_bytes(),
                    self.ring
                        .entries()
                        .filter(|e| e.tag == "WexRun")
                        .map(|e| &e.message)
                        .collect::<Vec<_>>()
                );
            }
            let found = |marker: &str| {
                self.ring.entries().any(|e| {
                    e.pid == report.pid
                        && e.message.contains(marker)
                        && e.message.contains(&report.run_id)
                })
            };
            if !self.run_smoke_done
                && found("WEX_RUN_OK")
                && found("WEX_ACTIVITY_RESUMED")
                && (!self.compose_enabled || found("WEX_COMPOSE_FRAME count=0"))
                && self.run_semantics_ready()
            {
                let path = self
                    .screenshot
                    .as_ref()
                    .expect("Run screenshot path")
                    .with_extension("json");
                let result = if report.device.emulator {
                    "GUI_EMULATOR_RUN_OK"
                } else {
                    "GUI_PHONE_RUN_OK"
                };
                std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({"result":result, "deployment":report, "entries":self.ring.entries().collect::<Vec<_>>(),"input":"native eframe RawInput pointer hover/press/release on actual Run button","run_button_clicked":self.run_smoke_click>=5,"variant_picker_clicked":self.variant_smoke_visited,"variant_picker_screenshot":self.variant_smoke_screenshot,"selected_variant":self.variant,"requested_variant":self.run_smoke_variant,"k2_diagnostics_observed":self.diagnostic_snapshot,"k2_diagnostics":self.diagnostics,"k2_status":self.language_status})).unwrap()).unwrap();
                self.filter = "WexRun".into();
                self.compiled_filter.expression = Some(regex::Regex::new("WexRun").unwrap());
                self.run_smoke_done = true;
                self.rendered_frames = 4;
                eprintln!(
                    "{result}: {} PID {} · build/install/launch/Logcat",
                    report.device.model, report.pid
                );
            }
        }
        if self.run_smoke_click == 6
            && self.save_ui.pending.is_none()
            && !self.running
            && self.last_run.is_none()
        {
            panic!("GUI Run failed: {}", self.status);
        }
        // Release is handled by the Run widget later in this frame's draw_shell.
        if self.run_smoke_click == 5 {
            self.run_smoke_click = 6;
        }
    }
    fn build(&mut self) {
        if self.closing_requested() || self.replace_blocked() || self.git.open {
            return;
        }
        if self.boot.pending
            || self.file_ui.open
            || self.building
            || self.running
            || self.debug.connection.is_some()
            || self.debug.busy
        {
            return;
        }
        if !self.save_before(save_ui::After::Build) {
            return;
        }
        self.chrome.dock_open = true;
        self.tab = 1;
        self.building = true;
        self.output.clear();
        let root = self.root.clone();
        let module = self.app_module.clone();
        let variant = self.variant.clone();
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.build_task = Some(self.runtime.spawn(async move {
            let result = wexandroidstudio::build::build(&root, &module, &variant, true, &|e| {
                let update = match e {
                    RunEvent::Phase(s) => Update::Status(s),
                    RunEvent::BuildLine(s) => Update::BuildLine(s),
                };
                let _ = tx.try_send(update);
                ctx.request_repaint();
            })
            .await;
            let result = result
                .map(Box::new)
                .map_err(|e| format!("Native build: {e:#}"));
            let _ = tokio::task::spawn_blocking(move || tx.send(Update::BuildDone(result))).await;
            ctx.request_repaint();
        }));
    }
    fn apply_edit(&mut self, start: usize, end: usize, text: &str) {
        self.doc.defer_syntax();
        match self.edit.replace_range(&mut self.doc, start..end, text) {
            Ok(()) => self.did_edit(),
            Err(e) => self.status = e.to_string(),
        }
    }
}
fn format(color: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::monospace(16.),
        color,
        ..Default::default()
    }
}
fn token_color(kind: &str) -> Color32 {
    if kind.contains("comment") {
        Color32::from_rgb(106, 128, 119)
    } else if kind.contains("string") {
        Color32::from_rgb(174, 210, 140)
    } else if kind.contains("literal") {
        Color32::from_rgb(225, 178, 122)
    } else if [
        "fun", "val", "var", "class", "package", "import", "return", "if", "else", "override",
        "private", "public",
    ]
    .contains(&kind)
    {
        Color32::from_rgb(184, 152, 236)
    } else {
        Color32::from_rgb(198, 207, 224)
    }
}
impl eframe::App for Workbench {
    fn on_exit(&mut self) {
        self.debug_close_exit_proof();
        if let Some(audit) = &self.close_ui.audit {
            eprintln!("{audit}");
        }
        if let Some(report) = self.debug.bp_report.take() {
            self.runtime.spawn_blocking(move || drop(report));
        }
        self.retire_format_review();
        if let Some(connection) = self.language.take() {
            language::retire_after_window(connection, self.runtime.clone());
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if self.close_raw_input(ctx, input) {
            return;
        }
        if self.semantic_smoke {
            input.focused = true;
            input
                .events
                .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        }
        self.syntax_smoke_input(ctx, input);
        self.save_smoke_input(ctx, input);
        self.history_smoke_input(ctx, input);
        self.terminal_smoke_input(ctx, input);
        self.ime_smoke_input(ctx, input);
        self.project_search_smoke_input(ctx, input);
        self.project_replace_smoke_input(ctx, input);
        self.project_replace_ux_smoke_input(ctx, input);
        self.git_smoke_input(ctx, input);
        self.git_flow_smoke_input(ctx, input);
        self.git_branches_smoke_input(ctx, input);
        self.git_branch_model_smoke_input(ctx, input);
        self.viewport_smoke_input(ctx, input);
        self.open_smoke_input(ctx, input);
        self.boot_smoke_input(ctx, input);
        self.files_smoke_input(ctx, input);
        self.project_nav_smoke_input(ctx, input);
        self.project_smoke_input(ctx, input);
        self.model_reload_smoke_input(ctx, input);
        self.compile_cache_smoke_input(ctx, input);
        self.analysis_smoke_input(ctx, input);
        self.run_smoke_input(ctx, input);
        self.refactor_smoke_input(ctx, input);
        self.format_smoke_input(ctx, input);
        self.java_format_smoke_input(ctx, input);
        self.debug_smoke_input(ctx, input);
        self.breakpoint_smoke_input(ctx, input);
        self.preference_smoke_input(ctx, input);
        self.preview_smoke_input(ctx, input);
        self.editor_smoke_input(ctx, input);
        self.ui_smoke_input(ctx, input);
        self.git_handoff_input(input);
    }
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_ui.allow {
            self.request_window_close();
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.close_fast_exit(ctx) {
            return;
        }
        if !self.first_update_seen {
            self.first_update_seen = true;
            self.startup_trace.mark("first_update");
        }
        if self.boot_smoke.is_some()
            || self.open_smoke.is_some()
            || self.save_smoke.is_some()
            || self.history_smoke.is_some()
            || self.syntax_smoke.is_some()
            || self.viewport_smoke.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        self.syntax_smoke_capture(ctx);
        self.save_smoke_capture(ctx);
        self.history_smoke_capture(ctx);
        self.viewport_smoke_capture(ctx);
        let viewport_timer = self
            .viewport_smoke
            .as_ref()
            .map(|_| std::time::Instant::now());
        self.open_smoke_capture(ctx);
        self.boot_smoke_capture(ctx);
        self.fonts.poll(ctx);
        self.boot_poll();
        if self.boot.pending {
            self.boot_screen(ctx);
            return;
        }

        if self.welcome {
            self.startup_capture(ctx);
            self.ime_ui_only(ctx);
            self.window_close_wait_bar(ctx);
            self.draw_shell(ctx);
            self.ime_finish(ctx);
            if self.startup_smoke.is_some() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        if self.variant_smoke_pending {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let path = self
                        .screenshot
                        .as_ref()
                        .expect("Run screenshot")
                        .with_file_name("variant-dialog.png");
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
                    self.variant_smoke_screenshot = Some(path);
                    self.variant_smoke_pending = false;
                }
            }
        }
        self.startup_capture(ctx);
        self.files_smoke_capture(ctx);
        self.project_nav_smoke_capture(ctx);
        self.project_smoke_capture(ctx);
        self.model_reload_smoke_capture(ctx);
        self.compile_cache_smoke_capture(ctx);
        self.analysis_smoke_capture(ctx);
        self.refactor_smoke_capture(ctx);
        self.format_smoke_capture(ctx);
        self.java_format_smoke_capture(ctx);
        self.breakpoint_smoke_capture(ctx);
        self.preference_smoke_capture(ctx);
        self.debug_smoke_capture(ctx);
        self.preview_smoke_capture(ctx);
        self.editor_smoke_capture(ctx);
        self.ui_smoke_capture(ctx);
        self.file_events();
        self.editor_updates(ctx);
        self.terminal.poll(self.tab == 5 && self.chrome.dock_open);
        self.terminal_smoke_capture(ctx);
        self.ime_smoke_capture(ctx);
        self.project_search_smoke_capture(ctx);
        self.project_replace_smoke_capture(ctx);
        self.project_replace_ux_smoke_capture(ctx);
        self.git_smoke_capture(ctx);
        self.git_flow_smoke_capture(ctx);
        self.git_branches_smoke_capture(ctx);
        self.git_branch_model_smoke_capture(ctx);
        self.git_step(ctx);
        self.project_search_step(ctx);
        self.semantic_shortcuts(ctx);
        self.editor_shortcuts(ctx);
        self.debug_shortcuts(ctx);
        self.language_events();
        if self.last_edit.elapsed() > Duration::from_millis(200) {
            self.send_snapshot();
        }
        self.semantic_smoke_step();
        self.run_smoke_step();
        self.rendered_frames += 1;
        if self.run_smoke.is_some() && self.rendered_frames == 1 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        if self.screenshot.is_some()
            && (!self.semantic_smoke || self.smoke_step == 5)
            && (self.run_smoke.is_none() || self.run_smoke_done)
        {
            if self.rendered_frames == 5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let pixels = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    let path = self.screenshot.take().unwrap();
                    image::save_buffer(
                        &path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .expect("save smoke screenshot");
                    eprintln!(
                        "GUI_SMOKE_OK: {}x{} wgpu screenshot {}",
                        image.size[0],
                        image.size[1],
                        path.display()
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            ctx.request_repaint();
        }
        for _ in 0..2048 {
            let Ok(update) = self.rx.try_recv() else {
                break;
            };
            match update {
                Update::Devices(Ok(devices)) => {
                    if !self.running
                        && self.debug.connection.is_none()
                        && !self.debug.busy
                        && self
                            .selected
                            .as_ref()
                            .is_none_or(|s| !devices.iter().any(|d| d.serial == *s && d.usable()))
                    {
                        self.stop_logs();
                        self.ring.clear();
                        self.last_run = None;
                        self.compiled_filter.pid = None;
                        self.selected = devices
                            .iter()
                            .find(|d| d.usable())
                            .map(|d| d.serial.clone());
                    }
                    self.devices = devices;
                }
                Update::Devices(Err(e)) => self.status = e,
                Update::Log(g, entry) => {
                    if g == self.log_generation {
                        self.ring.push(entry);
                    }
                }
                Update::LogStatus(g, s) => {
                    if g == self.log_generation {
                        self.status = s;
                    }
                }
                Update::PreviewReady(id, result) => self.preview_ready(id, result),
                Update::PreviewCatalog(id, catalog) => self.preview_catalog(id, *catalog),
                Update::PreviewPhase(id, note) => {
                    if self.preview.accepts(id) {
                        self.preview.note = note;
                    }
                }
                Update::DebugReady(session, result) => self.debug_ready(session, *result),
                Update::DebugReply(session, epoch, reply, result) => {
                    self.debug_reply(session, epoch, reply, result)
                }
                Update::RunReady(result) => {
                    self.running = false;
                    match *result {
                        Ok(deployment) => {
                            let r = deployment.report;
                            self.last_build = Some(r.native_build.clone());
                            self.compiled_filter.pid = Some(r.pid);
                            self.status = format!(
                                "Running {} · {} · PID {}",
                                r.apk.application_id, r.device.model, r.pid
                            );
                            self.attach_logs(Some(deployment.logs), r.serial.clone(), Some(r.pid));
                            self.last_run = Some(r);
                            self.connect_language(false);
                        }
                        Err(e) => self.status = e,
                    }
                }
                Update::FormattingApplied(id, result) => self.formatting_update(id, *result),
                Update::RefactorApplied(id, result) => self.refactor_applied(id, *result),
                Update::Status(s) => self.status = s,
                Update::BuildLine(line) => {
                    if self.output.len() >= 2000 {
                        self.output.pop_front();
                    }
                    self.output.push_back(line);
                }
                Update::BuildDone(result) => {
                    self.building = false;
                    match result {
                        Ok(report) => {
                            self.status = format!(
                                "BUILD SUCCESSFUL · {} · {}",
                                report.variant,
                                self.compilation_summary(&report.compilation_cache)
                            );
                            self.last_build = Some(*report);
                            self.connect_language(false);
                        }
                        Err(error) => self.status = error,
                    }
                }
            }
        }
        if !self.file_ui.open
            && !self.projects.open
            && !self.chrome.settings
            && !self.variant_open
            && !self.chrome.device_open
            && self.pending_close.is_none()
            && self.editor_ui.pending_reload.is_none()
            && !self.refactor.dialog
            && !self.replace_blocked()
            && !self.git.open
        {
            if Self::consume_exact_key(
                ctx,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                Key::S,
            ) || Self::consume_exact_key(
                ctx,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                Key::S,
            ) {
                self.save_before(save_ui::After::None);
            } else if Self::consume_exact_key(ctx, egui::Modifiers::COMMAND, Key::S)
                || Self::consume_exact_key(ctx, egui::Modifiers::CTRL, Key::S)
            {
                self.save_document();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, Key::B)) {
                self.build();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, Key::F10)) {
                self.run();
            }
        }
        self.debug_events();
        self.preview_step();
        self.draw_shell(ctx);
        self.finish_editor_frame(ctx);
        self.document_open_updates();
        if let Some(timer) = viewport_timer {
            self.editor_ui.viewport_profile.update_us = timer.elapsed().as_micros();
            self.editor_ui.viewport_profile.frame = ctx.cumulative_frame_nr();
        }
        self.document_save_updates();
        self.formatting_step();
        self.document_syntax_step();
        self.project_search_step(ctx);
        self.project_replace_step(ctx);
        self.ime_finish(ctx);
        self.breakpoint_step();
        self.window_close_step();
        if self.syntax_ui.pending.is_some()
            || !self.terminal.retiring.is_empty()
            || self.save_ui.pending.is_some()
            || self.startup_smoke.is_some()
            || self.project_nav_smoke.is_some()
            || self.project_smoke.is_some()
            || self.building
            || self.debug.bp_service.pending()
            || self.running
            || self.preview.busy
            || self.run_smoke.is_some()
            || self.language.is_some()
            || self.semantic_smoke
            || self.log_task.as_ref().is_some_and(|t| !t.is_finished())
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
impl Drop for Workbench {
    fn drop(&mut self) {
        if let Some(t) = self.run_task.take() {
            t.abort();
        }
        if let Some(t) = self.log_task.take() {
            t.abort();
        }
        if let Some(t) = self.build_task.take() {
            t.abort();
        }
    }
}
fn main() -> eframe::Result {
    let started = std::time::Instant::now();
    let first = std::env::args_os().nth(1);
    if first.as_deref() == Some(std::ffi::OsStr::new("--help")) {
        println!(
            "Wex Studio — native Android IDE\nUsage: wex-ide [PROJECT]\n       wex-ide --welcome\nNative projects use wex.json. No-project startup opens Recent without scanning the current directory.\nTool configuration: WEX_SUPPORT_HOME / WEX_TOOLS_HOME; inspect with wex doctor."
        );
        return Ok(());
    }
    let startup = matches!(
        first.as_deref().and_then(|v| v.to_str()),
        Some("--startup-smoke" | "--startup-retry-smoke")
    );
    let forced = startup || first.as_deref() == Some(std::ffi::OsStr::new("--welcome"));
    let root = if forced || first.is_none() {
        std::env::current_dir().map_err(|e| eframe::Error::AppCreation(Box::new(e)))?
    } else {
        PathBuf::from(first.clone().unwrap())
            .canonicalize()
            .map_err(|e| eframe::Error::AppCreation(Box::new(e)))?
    };
    let root = if root.is_file() && root.file_name().is_some_and(|n| n == "wex.json") {
        root.parent().unwrap().to_owned()
    } else {
        root
    };
    if !root.is_dir() {
        return Err(eframe::Error::AppCreation(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Choose a project directory or wex.json",
        ))));
    }
    let welcome = forced
        || (first.is_none()
            && !root.join("wex.json").is_file()
            && !root.join("settings.gradle.kts").is_file()
            && !root.join("settings.gradle").is_file());
    let startup_output = if startup {
        Some(PathBuf::from(
            std::env::args_os()
                .nth(2)
                .expect("startup output directory"),
        ))
    } else {
        None
    };
    let semantic_smoke = std::env::args().nth(2).as_deref() == Some("--k2-smoke");
    let run_smoke = std::env::args().nth(2).as_deref() == Some("--run-smoke");
    let _run_serial = if run_smoke {
        assert_eq!(std::env::args().nth(4).as_deref(), Some("--serial"));
        Some(std::env::args().nth(5).expect("explicit phone serial"))
    } else {
        None
    };
    let screenshot = if run_smoke
        || semantic_smoke
        || std::env::args().nth(2).as_deref() == Some("--screenshot")
    {
        Some(PathBuf::from(
            std::env::args_os().nth(3).expect("screenshot path"),
        ))
    } else {
        None
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400., 900.])
            .with_min_inner_size([760., 560.])
            .with_title(
                if matches!(
                    std::env::args().nth(2).as_deref(),
                    Some("--debug-smoke" | "--debug-close-smoke")
                ) {
                    format!("Wex Debug Check {}", std::process::id())
                } else if std::env::args().nth(2).as_deref() == Some("--files-smoke") {
                    format!("Wex File Check {}", std::process::id())
                } else {
                    "WexAndroidStudio".into()
                },
            ),
        ..Default::default()
    };
    let trace = startup::Trace::new(started);
    trace.mark("native_options_ready");
    let record = trace.clone();
    let result = startup::run(options, trace, move |cc| {
        wexandroidstudio::material::install(&cc.egui_ctx);
        let mut app = Workbench::for_launch(
            root.clone(),
            cc.egui_ctx.clone(),
            screenshot.clone(),
            semantic_smoke,
            welcome,
        );
        app.startup_trace = record.clone();
        if let Some(mode) = match std::env::args().nth(2).as_deref() {
            Some("--boot-open-smoke") => Some(boot_smoke::Mode::Open),
            Some("--boot-cancel-smoke") => Some(boot_smoke::Mode::Cancel),
            Some("--boot-repair-smoke") => Some(boot_smoke::Mode::Repair),
            _ => None,
        } {
            app.boot_smoke = Some(boot_smoke::Smoke::new(
                PathBuf::from(std::env::args_os().nth(3).expect("boot evidence")),
                mode,
                &app,
            ));
        }
        record.mark("app_created");
        if let Some(output) = startup_output.clone() {
            app.startup_smoke = Some(welcome::Startup::new(output, started));
        }
        if welcome {
            app.projects.history.ensure_loaded();
        }

        if welcome {
            app.configure_smokes();
        }
        Ok(Box::new(app))
    });
    let language_stopped = language::drain_on_exit();
    let breakpoints_stopped = wexandroidstudio::breakpoint_store::drain_on_exit();
    if let Some(path) = std::env::var_os("WEX_K2_SHUTDOWN_REPORT")
        && let Err(error) = std::fs::write(
            path,
            serde_json::to_vec_pretty(&language::shutdown_stats()).unwrap(),
        )
    {
        eprintln!("K2 shutdown report: {error}");
    }
    let terminal_stopped = wexandroidstudio::terminal::drain_on_exit();
    if let Err(e) = breakpoints_stopped {
        return Err(eframe::Error::AppCreation(e.into_boxed_dyn_error()));
    }
    if let Err(e) = language_stopped {
        return Err(eframe::Error::AppCreation(e.into_boxed_dyn_error()));
    }
    if let Err(e) = terminal_stopped {
        return Err(eframe::Error::AppCreation(e.into_boxed_dyn_error()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_focus_text_input_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Main.kt");
        std::fs::write(&path, "val x = 1\n").unwrap();
        let ctx = egui::Context::default();
        let mut app = Workbench::new(dir.path().to_owned(), ctx.clone(), None, false);
        for events in [vec![], vec![egui::Event::Text("// test\n".into())]] {
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let id = ui.make_persistent_id("rope-editor");
                        ui.memory_mut(|m| m.request_focus(id));
                        app.editor(ui, ctx);
                    });
                },
            );
        }
        assert_eq!(app.doc.rope().to_string(), "// test\nval x = 1\n");
        assert!(app.doc.dirty());
        app.doc.save().unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "// test\nval x = 1\n"
        );
    }
    #[test]
    fn selection_pins_serial_during_run_and_clears_previous_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = Workbench::new(dir.path().to_owned(), egui::Context::default(), None, false);
        app.selected = Some("usb-a".into());
        app.running = true;
        app.select_device("usb-b".into());
        assert_eq!(app.selected.as_deref(), Some("usb-a"));
        let generation = app.log_generation;
        app.running = false;
        app.compiled_filter.pid = Some(123);
        app.select_device("usb-b".into());
        assert_eq!(app.selected.as_deref(), Some("usb-b"));
        assert!(app.log_generation > generation);
        assert!(app.compiled_filter.pid.is_none());
    }
}

#[cfg(test)]
mod run_smoke_tests {
    use super::*;
    #[test]
    fn compose_run_waits_for_a_real_matching_clean_k2_diagnostic_snapshot() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val x=1").unwrap();
        let ctx = egui::Context::default();
        wexandroidstudio::material::install(&ctx);
        let mut app = Workbench::new(d.path().to_owned(), ctx, None, false);
        app.compose_enabled = true;
        assert!(!app.run_semantics_ready());
        app.diagnostic_snapshot = Some((app.doc.path.clone().unwrap(), app.doc.version));
        assert!(app.run_semantics_ready());
        app.diagnostic_snapshot.as_mut().unwrap().1 += 1;
        assert!(!app.run_semantics_ready());
        app.diagnostic_snapshot.as_mut().unwrap().1 = app.doc.version;
        app.diagnostics.push(Diagnostic {
            range: kotlin::Range {
                start: kotlin::Position {
                    line: 0,
                    character: 0,
                },
                end: kotlin::Position {
                    line: 0,
                    character: 1,
                },
            },
            message: "error".into(),
            severity: Some(1),
        });
        assert!(!app.run_semantics_ready());
    }
    #[test]
    fn native_run_smoke_emits_one_real_pointer_click_and_defers_failure_until_painted() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val x=1").unwrap();
        let ctx = egui::Context::default();
        wexandroidstudio::material::install(&ctx);
        let mut app = Workbench::new(d.path().to_owned(), ctx.clone(), None, false);
        app.run_smoke = Some("explicit-serial".into());
        app.run_smoke_started = true;
        app.run_smoke_click = 1;
        app.chrome.hits.insert(
            "run".into(),
            egui::Rect::from_min_size(egui::pos2(10., 10.), egui::vec2(80., 40.)),
        );
        let mut pressed = 0;
        let mut released = 0;
        for _ in 0..3 {
            let mut input = egui::RawInput::default();
            app.run_smoke_input(&ctx, &mut input);
            pressed += input
                .events
                .iter()
                .filter(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. }))
                .count();
            assert!(input.focused);
        }
        assert_eq!(app.run_smoke_click, 4);
        assert_eq!(pressed, 1);
        app.run_smoke_release.as_mut().unwrap().1 = std::time::Instant::now();
        let mut input = egui::RawInput::default();
        app.run_smoke_input(&ctx, &mut input);
        released += input
            .events
            .iter()
            .filter(|e| matches!(e, egui::Event::PointerButton { pressed: false, .. }))
            .count();
        assert_eq!(released, 1);
        assert_eq!(app.run_smoke_click, 5);
        app.run_smoke_step();
        assert_eq!(app.run_smoke_click, 6);
        assert!(!app.running);
        let mut input = egui::RawInput::default();
        app.run_smoke_input(&ctx, &mut input);
        assert!(
            !input
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::PointerButton { .. }))
        );
    }
    #[test]
    fn native_exit_hands_off_language_without_dropping_its_runtime_on_ui() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Main.kt"), "val x=1").unwrap();
        let mut app = Workbench::new(
            root.path().to_owned(),
            egui::Context::default(),
            None,
            false,
        );
        let (requests, mut commands) = tokio::sync::mpsc::channel(1);
        let (tx, events) = tokio::sync::mpsc::channel(1);
        tx.try_send(language::Event::Status("queued event".into()))
            .unwrap();
        let (finished, observed) = std::sync::mpsc::channel();
        let task = app.runtime.spawn(async move {
            let _ = tx
                .send(language::Event::Status(
                    "blocked until receiver closes".into(),
                ))
                .await;
            assert!(matches!(
                commands.recv().await,
                Some(language::Request::Stop)
            ));
            finished.send(()).unwrap();
        });
        app.language = Some(Connection {
            document: language::SnapshotSender::default(),
            requests,
            events,
            task,
        });
        eframe::App::on_exit(&mut app);
        assert!(app.language.is_none());
        drop(app);
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        language::drain_on_exit().unwrap();
    }
}
