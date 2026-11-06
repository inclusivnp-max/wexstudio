//! Real GPU/RawInput creation form, invalid-field recovery, child editor import, dirty-buffer preservation.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    pub dir: PathBuf,
    ready: bool,
    start: Instant,
    next: Instant,
    step: u8,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    paste: Option<String>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    original: String,
    original_path: PathBuf,
    package: String,
    dirty: String,
}
impl Smoke {
    pub fn new(dir: PathBuf, ready: bool, app: &Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        if !ready {
            std::fs::create_dir_all(dir.join("projects")).unwrap();
        }
        Self {
            dir,
            ready,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            pending: None,
            release: None,
            paste: None,
            awaiting: None,
            shots: vec![],
            original: app.doc.rope().to_string(),
            original_path: app.doc.path.clone().expect("source document"),
            package: format!("dev.wex.wizard.case{}", std::process::id()),
            dirty: "// Unsaved parent window\nval preserve = 42\n".into(),
        }
    }
    fn click(&mut self, key: &str) {
        self.pending = Some((key.into(), 0));
    }
    fn fill(&mut self, key: &str, text: String) {
        self.click(key);
        self.paste = Some(text);
    }
}
fn key(input: &mut egui::RawInput, key: Key, mut modifiers: egui::Modifiers) {
    if modifiers.command {
        modifiers.ctrl = true;
    }
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
}
impl Workbench {
    pub(super) fn project_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.project_smoke.take() else {
            return;
        };
        input.focused = true;
        input.modifiers = Default::default();
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(300),
            "project smoke step{} {} {}",
            s.step,
            self.status,
            self.language_status
        );
        if let Some((pos, until)) = s.release.take() {
            input.events.push(egui::Event::PointerMoved(pos));
            if Instant::now() < until {
                s.release = Some((pos, until));
            } else {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.project_smoke = Some(s);
            return;
        }
        if let Some((name, hover)) = s.pending.take() {
            if let Some(rect) = self.chrome.hits.get(&name) {
                let pos = rect.center();
                input.events.push(egui::Event::PointerMoved(pos));
                if hover < 3 {
                    s.pending = Some((name, hover + 1));
                } else {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    });
                    s.release = Some((pos, Instant::now() + Duration::from_millis(100)));
                }
            } else {
                s.pending = Some((name, hover));
            }
            self.project_smoke = Some(s);
            return;
        }
        if let Some(text) = s.paste.take() {
            key(input, Key::A, egui::Modifiers::COMMAND);
            input.events.push(egui::Event::Paste(text));
            s.next = Instant::now() + Duration::from_millis(500);
            self.project_smoke = Some(s);
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.project_smoke = Some(s);
            return;
        }
        let mut shot = None;
        if s.ready {
            let clean = self
                .model_status
                .as_ref()
                .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                && self.diagnostic_snapshot.as_ref()
                    == Some(&(s.original_path.clone(), self.doc.version))
                && self.diagnostics.iter().all(|d| d.severity != Some(1));
            if !clean {
                self.project_smoke = Some(s);
                return;
            }
            if s.step == 0 {
                shot = Some("01-ready");
            } else {
                assert_eq!(
                    std::fs::read_to_string(&s.original_path).unwrap(),
                    s.original
                );
                std::fs::write(s.dir.join("proof.json"),serde_json::to_vec_pretty(&json!({"result":"GENERATED_PROJECT_K2_READY","root":self.root,"source":s.original_path,"source_unchanged":true,"diagnostics":self.diagnostics,"diagnostic_checkpoint":self.diagnostic_snapshot,"model":self.model_status,"screenshots":s.shots})).unwrap()).unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        } else {
            match s.step {
                0 => {
                    self.editor_ui.focus_editor = true;
                    s.paste = Some(s.dirty.clone());
                }
                1 => {
                    assert_eq!(self.doc.rope().to_string(), s.dirty);
                    assert!(self.doc.dirty());
                    s.click("projects");
                }
                2 => {
                    assert!(self.projects.open);
                    shot = Some("01-new-project");
                }
                3 => s.click("template-java"),
                4 => {
                    assert_eq!(
                        self.projects.options.template,
                        wexandroidstudio::scaffold::Template::Java
                    );
                    s.click("template-kotlin");
                }
                5 => {
                    assert_eq!(
                        self.projects.options.template,
                        wexandroidstudio::scaffold::Template::Kotlin
                    );
                    s.click("template-compose");
                }
                6 => s.fill("project-name", "Мой & \"App\" ' %s".into()),
                7 => s.fill("project-package", "com.when".into()),
                8 => {
                    assert!(self.projects.options.validate().is_err());
                    assert!(self.projects.result.is_none());
                    key(input, Key::S, egui::Modifiers::COMMAND);
                    key(input, Key::F5, Default::default());
                    shot = Some("02-invalid-package");
                }
                9 => {
                    assert_eq!(
                        std::fs::read_to_string(&s.original_path).unwrap(),
                        s.original
                    );
                    assert!(!self.building && !self.running && !self.debug.busy);
                    s.fill("project-package", s.package.clone());
                }
                10 => s.fill(
                    "project-parent",
                    s.dir.join("projects").to_string_lossy().into_owned(),
                ),
                11 => s.fill("project-directory", "Wizard App".into()),
                12 => {
                    assert!(
                        self.projects
                            .options
                            .destination(std::path::Path::new(&self.projects.parent))
                            .is_ok()
                    );
                    shot = Some("03-ready-to-create");
                }
                13 => s.click("project-create"),
                14 => {
                    if self.projects.result.is_none() {
                        assert!(self.projects.error.is_none(), "{:?}", self.projects.error);
                        self.project_smoke = Some(s);
                        return;
                    }
                    assert!(!self.projects.busy);
                    let created = self.projects.result.as_ref().unwrap();
                    assert_eq!(created.root, s.dir.join("projects/Wizard App"));
                    let c = wexandroidstudio::native::Config::load(&created.root).unwrap();
                    assert_eq!(c.android.application_id, s.package);
                    assert!(c.build.compose.is_some());
                    shot = Some("04-created");
                }
                15 => s.click("project-open-created"),
                16 | 23 => {
                    let number = if s.step == 16 { 1 } else { 2 };
                    let proof = s.dir.join(format!("opened-{number}/proof.json"));
                    if !proof.is_file() {
                        self.project_smoke = Some(s);
                        return;
                    }
                    let value: Value =
                        serde_json::from_slice(&std::fs::read(proof).unwrap()).unwrap();
                    assert_eq!(value["result"], "GENERATED_PROJECT_K2_READY");
                    assert_eq!(self.projects.launches, number);
                    assert_eq!(self.doc.rope().to_string(), s.dirty);
                    assert_eq!(
                        std::fs::read_to_string(&s.original_path).unwrap(),
                        s.original
                    );
                }
                17 => s.click("project-existing"),
                18 => s.fill(
                    "project-open-path",
                    s.dir.join("missing-project").to_string_lossy().into_owned(),
                ),
                19 => {
                    assert!(
                        wexandroidstudio::native::Config::load(std::path::Path::new(
                            &self.projects.existing
                        ))
                        .is_err()
                    );
                    assert_eq!(self.projects.launches, 1);
                    shot = Some("05-open-invalid");
                }
                20 => s.fill(
                    "project-open-path",
                    s.dir
                        .join("projects/Wizard App")
                        .to_string_lossy()
                        .into_owned(),
                ),
                21 => {
                    assert!(
                        wexandroidstudio::native::Config::load(std::path::Path::new(
                            &self.projects.existing
                        ))
                        .is_ok()
                    );
                    shot = Some("06-open-existing");
                }
                22 => s.click("project-open-existing"),
                24 => s.click("project-close"),
                25 => {
                    assert!(!self.projects.open);
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Z, egui::Modifiers::COMMAND);
                }
                26 => {
                    assert_eq!(self.doc.rope().to_string(), s.original);
                    assert_eq!(
                        std::fs::read_to_string(&s.original_path).unwrap(),
                        s.original
                    );
                    std::fs::write(s.dir.join("project-window.json"),serde_json::to_vec_pretty(&json!({"result":"GUI_NATIVE_PROJECT_WIZARD_OK","created":self.projects.result,"launches":self.projects.launches,"screenshots":s.shots,"dirty_buffer_preserved":true,"disk_source_unchanged":true,"undo_restored":true,"invalid_package_recovered":true,"invalid_open_recovered":true,"actual_pointer_create_and_open":true,"renderer":"wgpu","gradle_invoked":false})).unwrap()).unwrap();
                    eprintln!(
                        "GUI_NATIVE_PROJECT_WIZARD_OK: real pointer/keyboard template/name/package/paths/Create/Open; invalid recovery; two actual child windows imported real K2; six GPU frames + two editor frames; dirty parent buffer/disk/Undo preserved; no Gradle"
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                _ => unreachable!(),
            }
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(600);
        self.project_smoke = Some(s);
    }
    pub(super) fn project_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.project_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let path = s.dir.join(format!("{name}.png"));
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
                    s.shots
                        .push(json!({"path":path,"width":image.size[0],"height":image.size[1]}));
                    s.awaiting = None;
                }
            }
        }
    }
}
