//! Create/Open native Android projects without disturbing this window's buffers.
use super::*;
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    scaffold::{self, Options, Template},
};
#[derive(Default)]
pub(super) struct Projects {
    pub open: bool,
    pub options: Options,
    pub parent: String,
    pub existing: String,
    pub existing_mode: bool,
    pub recent_mode: bool,
    pub recent_filter: String,
    pub history: project_recent::HistoryUi,
    pub browser: Option<project_browser::Browser>,
    pub opening: bool,
    open_rx: Option<mpsc::Receiver<Result<(PathBuf, std::process::Child), String>>>,
    pub result: Option<scaffold::Created>,
    pub error: Option<String>,
    pub busy: bool,
    rx: Option<mpsc::Receiver<Result<scaffold::Created, String>>>,
    pub launched: Option<PathBuf>,
    pub launches: usize,
    pub(super) validation_key: String,
    validation: Option<Result<String, String>>,
    validator: Option<mpsc::Receiver<Result<String, String>>>,
    children: Vec<std::process::Child>,
}
impl Workbench {
    pub(super) fn show_projects(&mut self) {
        if self.file_ui.open || self.git.open {
            return;
        }
        self.chrome.settings = false;
        self.chrome.device_open = false;
        self.variant_open = false;
        if self.projects.parent.is_empty() {
            self.projects.parent = self
                .root
                .parent()
                .unwrap_or(&self.root)
                .to_string_lossy()
                .into_owned();
        }
        if self.projects.existing.is_empty() {
            self.projects.existing = self.root.to_string_lossy().into_owned();
        }
        self.projects.open = true;
        self.projects.validation_key.clear();
        self.projects.history.ensure_loaded();
    }
    fn create_project(&mut self) {
        let options = self.projects.options.clone();
        let parent = PathBuf::from(&self.projects.parent);
        let (tx, rx) = mpsc::sync_channel(1);
        self.projects.busy = true;
        self.projects.error = None;
        self.projects.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(scaffold::create(&parent, &options).map_err(|e| format!("{e:#}")));
        });
    }
    fn launch_project(&mut self, path: PathBuf) {
        if self.projects.opening {
            return;
        }
        let output = self
            .project_smoke
            .as_ref()
            .map(|s| s.dir.clone())
            .or_else(|| self.project_nav_smoke.as_ref().map(|s| s.dir.clone()))
            .map(|d| d.join(format!("opened-{}", self.projects.launches + 1)));
        let (tx, rx) = mpsc::sync_channel(1);
        self.projects.open_rx = Some(rx);
        self.projects.opening = true;
        self.projects.error = None;
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let root = path.canonicalize()?;
                wexandroidstudio::native::Config::load(&root)?;
                let mut command = std::process::Command::new(std::env::current_exe()?);
                command.arg(&root);
                if let Some(output) = output {
                    command.arg("--project-ready-smoke").arg(output);
                }
                let child = command.spawn()?;
                Ok((root, child))
            })();
            let _ = tx.send(result.map_err(|e| format!("Open project: {e:#}")));
        });
    }
    fn project_open_events(&mut self) {
        if let Some(rx) = &self.projects.open_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.projects.open_rx = None;
                    self.projects.opening = false;
                    match result {
                        Ok((root, child)) => {
                            self.projects.launches += 1;
                            self.projects.launched = Some(root.clone());
                            self.projects.children.push(child);
                            self.status = format!("Opened {} in a new window", root.display());
                            self.projects
                                .history
                                .submit(project_recent::Action::Record(root));
                        }
                        Err(e) => self.projects.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.projects.open_rx = None;
                    self.projects.opening = false;
                    self.projects.error = Some("Project opener stopped; retry".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn project_validation(&mut self, ctx: &egui::Context) {
        let key = format!(
            "{}\n{}\n{}\n{}",
            self.projects.existing_mode,
            self.projects.existing,
            self.projects.parent,
            serde_json::to_string(&self.projects.options).unwrap()
        );
        if key != self.projects.validation_key {
            self.projects.validation_key = key;
            self.projects.validation = None;
            let options = self.projects.options.clone();
            let parent = PathBuf::from(&self.projects.parent);
            let existing = PathBuf::from(&self.projects.existing);
            let mode = self.projects.existing_mode;
            let (tx, rx) = mpsc::sync_channel(1);
            self.projects.validator = Some(rx);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                let result = if mode {
                    wexandroidstudio::native::Config::load(&existing).map(|c| {
                        format!(
                            "{} · {} modules · SDK {}",
                            c.name,
                            c.modules.len(),
                            c.android.compile_sdk
                        )
                    })
                } else {
                    options
                        .destination(&parent)
                        .map(|p| p.to_string_lossy().into_owned())
                };
                let _ = tx.send(result.map_err(|e| e.to_string()));
            });
        }
        if let Some(rx) = &self.projects.validator {
            match rx.try_recv() {
                Ok(r) => {
                    self.projects.validation = Some(r);
                    self.projects.validator = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.projects.validation = Some(Err(
                        "Validation worker stopped; edit the path to retry".into(),
                    ));
                    self.projects.validator = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50))
                }
            }
        }
    }
    pub(super) fn projects_dialog(&mut self, ctx: &egui::Context) {
        self.project_open_events();
        self.projects.history.poll(ctx);
        if self.projects.opening {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.projects
            .children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        if let Some(rx) = &self.projects.rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.projects.busy = false;
                    self.projects.rx = None;
                    match result {
                        Ok(c) => self.projects.result = Some(c),
                        Err(e) => self.projects.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.projects.busy = false;
                    self.projects.rx = None;
                    self.projects.error = Some("Project creation worker stopped; retry".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.projects.open {
            return;
        }
        if self.projects.browser.is_some() {
            self.browser_dialog(ctx);
            return;
        }
        self.project_validation(ctx);
        let reduced = self.reduced_motion;
        let mut create = false;
        let mut launch = None;
        let mut close = false;
        let mut browse = None;
        let mut remove = None;
        let mut refresh_history = false;
        let modal=egui::Modal::new(egui::Id::new("projects-dialog")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-100.).clamp(320.,560.));
            ui.heading("Your next Android app");
            ui.label(RichText::new("Native projects · wex.json · no Gradle").color(m::MUTED));
            ui.add_space(12.);
            ui.horizontal(|ui|{
                for (mode,key,label) in [(0,"project-new","New"),(1,"project-existing","Open"),(2,"project-recent","Recent")] {
                    let active=if self.projects.recent_mode{2}else if self.projects.existing_mode{1}else{0};
                    let r=self.chrome.button(ui,key,label,Icon::Folder,if active==mode {Kind::Filled}else{Kind::Tonal},!self.projects.busy&&!self.projects.opening,130.,reduced);
                    if r.clicked(){self.projects.existing_mode=mode==1;self.projects.recent_mode=mode==2;self.projects.error=None;if mode==2{self.projects.history.ensure_loaded();}}
                }
            });
            ui.add_space(12.);
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-310.).max(180.)).show(ui,|ui|{
                if self.projects.recent_mode {
                    ui.horizontal(|ui|{let r=ui.add(egui::TextEdit::singleline(&mut self.projects.recent_filter).hint_text("Filter by name or path").id_salt("recent-filter").desired_width(ui.available_width()-115.));self.chrome.hits.insert("recent-filter".into(),r.rect);if self.chrome.button(ui,"recent-refresh","Refresh",Icon::Refresh,Kind::Ghost,!self.projects.history.busy,105.,reduced).clicked(){refresh_history=true;}});
                    if self.projects.history.busy{ui.label(RichText::new("Updating recent projects…").color(m::MUTED));}
                    if let Some(error)=&self.projects.history.error{ui.label(RichText::new(error).color(m::ERROR));ui.label("Last-good list retained. Repair the history file and Refresh; opening a project still works.");}
                    let filter=self.projects.recent_filter.to_lowercase();let mut count=0;
                    for entry in self.projects.history.records.clone(){if !entry.name.to_lowercase().contains(&filter)&&!entry.root.to_string_lossy().to_lowercase().contains(&filter){continue;}count+=1;
                        m::card().fill(m::CONTAINER).inner_margin(12).show(ui,|ui|{ui.horizontal(|ui|{
                            ui.vertical(|ui|{ui.set_width((ui.available_width()-225.).max(100.));ui.label(RichText::new(&entry.name).size(17.));ui.add(egui::Label::new(RichText::new(entry.root.to_string_lossy()).small().color(m::MUTED)).truncate()).on_hover_text_passive(entry.root.to_string_lossy());});
                            let key=format!("recent-open:{}",entry.root.display());if self.chrome.button(ui,&key,"Open",Icon::Folder,Kind::Filled,!self.projects.opening&&!self.projects.history.busy,92.,reduced).clicked(){launch=Some(entry.root.clone());}
                            let key=format!("recent-remove:{}",entry.root.display());if self.chrome.button(ui,&key,"Remove",Icon::Close,Kind::Ghost,!self.projects.history.busy&&!self.projects.opening,100.,reduced).on_hover_text_passive("Remove from recent list only; project files stay").clicked(){remove=Some(entry.root.clone());}
                        });});
                    }
                    if count==0{ui.add_space(20.);ui.label(RichText::new(if filter.is_empty(){"No recent projects yet. Create a new app or open a native project."}else{"No matching recent projects"}).color(m::MUTED));}
                    ui.add_space(12.);ui.label(RichText::new("Recent names are saved locally. Opening rechecks wex.json; moved or missing projects remain removable.").small().color(m::MUTED));
                } else if self.projects.existing_mode {
                    field(ui,&mut self.chrome.hits,"project-open-path","Project directory",&mut self.projects.existing);
                    if self.chrome.button(ui,"project-browse-open","Browse folders",Icon::Folder,Kind::Tonal,!self.projects.opening,180.,reduced).clicked(){browse=Some((project_browser::Purpose::Existing,PathBuf::from(&self.projects.existing)));}
                    ui.label(RichText::new("Select a directory containing wex.json. It opens in a new window; edits in this window stay here.").color(m::MUTED));
                    let path=PathBuf::from(&self.projects.existing);
                    match &self.projects.validation {
                        Some(Ok(summary))=>{ui.label(RichText::new(summary).color(m::SUCCESS));if self.chrome.button(ui,"project-open-existing","Open in new window",Icon::Folder,Kind::Filled,!self.projects.opening,230.,reduced).clicked(){launch=Some(path);}},
                        Some(Err(e))=>{ui.label(RichText::new(e).color(m::WARNING));},None=>{ui.label("Checking project…");}
                    }
                } else if let Some(created)=&self.projects.result {
                    ui.heading("Ready to make it yours");ui.label(RichText::new(created.root.to_string_lossy()).color(m::SUCCESS));ui.label(format!("{} · {} files",created.template.label(),created.files.len()));ui.label("Project created. Opening it keeps this window and its unsaved edits intact.");
                    if self.chrome.button(ui,"project-open-created","Open in new window",Icon::Folder,Kind::Filled,!self.projects.opening,230.,reduced).clicked(){launch=Some(created.root.clone());}
                    if ui.button("Create another project").clicked(){self.projects.result=None;self.projects.error=None;}
                } else {
                    ui.add_enabled_ui(!self.projects.busy,|ui|{
                        ui.label(RichText::new("Choose your starting point").size(18.));
                        ui.horizontal_wrapped(|ui|{for (template,key) in [(Template::Compose,"template-compose"),(Template::Kotlin,"template-kotlin"),(Template::Java,"template-java")] {
                            let r=self.chrome.button(ui,key,if template==Template::Compose{"Compose · M3"}else{template.label()},Icon::Code,if self.projects.options.template==template {Kind::Filled}else{Kind::Tonal},true,165.,reduced);if r.clicked(){self.projects.options.template=template;}
                        }});
                        field(ui,&mut self.chrome.hits,"project-name","App name",&mut self.projects.options.name);
                        field(ui,&mut self.chrome.hits,"project-package","Package / namespace",&mut self.projects.options.application_id);
                        field(ui,&mut self.chrome.hits,"project-parent","Parent directory",&mut self.projects.parent);
                        if self.chrome.button(ui,"project-browse-parent","Browse parent folder",Icon::Folder,Kind::Tonal,true,220.,reduced).clicked(){browse=Some((project_browser::Purpose::Parent,PathBuf::from(&self.projects.parent)));}
                        field(ui,&mut self.chrome.hits,"project-directory","New directory",&mut self.projects.options.directory);
                        ui.horizontal(|ui|{ui.label("Minimum SDK");let r=ui.add(egui::DragValue::new(&mut self.projects.options.min_sdk).range(23..=36));self.chrome.hits.insert("project-min-sdk".into(),r.rect);ui.label(RichText::new("Compile / target 36 · JDK 21").color(m::MUTED));});
                    });
                    let valid=self.projects.validation.as_ref().is_some_and(|r|r.is_ok());
                    match &self.projects.validation {Some(Ok(path))=>{ui.label(RichText::new(path).color(m::SUCCESS));},Some(Err(e))=>{ui.label(RichText::new(e).color(m::WARNING));},None=>{ui.label("Checking destination…");}}
                    ui.collapsing("What will be created",|ui|{if let Ok(files)=self.projects.options.files(){for p in files.keys(){ui.label(RichText::new(p.to_string_lossy()).small().monospace());}}});
                    ui.label(RichText::new("Pinned Kotlin 2.4.0 and SDK tools. Dependencies resolve when you open/build; no downloads during creation. Existing destinations are never replaced.").small().color(m::MUTED));
                    if self.chrome.button(ui,"project-create",if self.projects.busy{"Creating…"}else{"Create project"},Icon::Code,Kind::Filled,valid&&!self.projects.busy,190.,reduced).clicked(){create=true;}
                }
                if !self.projects.recent_mode && let Some(error)=&self.projects.history.error{ui.label(RichText::new(format!("Recent projects: {error}")).color(m::WARNING));}
                if self.projects.opening{ui.label(RichText::new("Opening in a new window…").color(m::MUTED));}
                if let Some(error)=&self.projects.error {ui.label(RichText::new(error).color(m::ERROR));}
                if let Some(path)=&self.projects.launched {ui.label(RichText::new(format!("Opened {}",path.display())).color(m::SUCCESS));}
            });
            ui.add_space(12.);
            if self.chrome.button(ui,"project-close","Done",Icon::Close,Kind::Tonal,!self.projects.busy,100.,reduced).clicked(){close=true;}
        });
        if let Some((purpose, path)) = browse {
            self.projects.browser = Some(project_browser::Browser::new(purpose, path));
        }
        if let Some(root) = remove {
            self.projects
                .history
                .submit(project_recent::Action::Remove(root));
        }
        if refresh_history {
            self.projects.history.submit(project_recent::Action::Load);
        }
        if create {
            self.create_project();
        }
        if let Some(path) = launch {
            self.launch_project(path);
        }
        let escape = modal.is_top_modal
            && !modal.any_popup_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if (close || escape) && !self.projects.busy {
            self.projects.open = false;
        }
        if self.projects.busy {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
fn field(
    ui: &mut egui::Ui,
    hits: &mut std::collections::BTreeMap<String, egui::Rect>,
    key: &str,
    label: &str,
    value: &mut String,
) {
    ui.label(label);
    let r = ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(key)
            .desired_width(f32::INFINITY),
    );
    hits.insert(key.into(), r.rect);
}
