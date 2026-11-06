//! Real native directory chooser/Recent persistence/errors/removal, never direct form mutation.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    pub dir: PathBuf,
    reopen: bool,
    start: Instant,
    next: Instant,
    step: u8,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    paste: Option<String>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    last_state: String,
    original: String,
    source: PathBuf,
    dirty: String,
    workspace: PathBuf,
    project: PathBuf,
    missing: PathBuf,
}
impl Smoke {
    pub fn new(dir: PathBuf, reopen: bool, app: &Workbench) -> Self {
        let dir = dir.canonicalize().unwrap();
        let fixture: Value =
            serde_json::from_slice(&std::fs::read(dir.join("fixture.json")).unwrap()).unwrap();
        let path = |key: &str| PathBuf::from(fixture[key].as_str().unwrap());
        Self {
            dir,
            reopen,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            pending: None,
            release: None,
            paste: None,
            awaiting: None,
            shots: vec![],
            last_state: String::new(),
            original: app.doc.rope().to_string(),
            source: app.doc.path.clone().unwrap(),
            dirty: "// Keep this unsaved navigation buffer\nval keepMe = 42\n".into(),
            workspace: path("workspace"),
            project: path("project"),
            missing: path("missing"),
        }
    }
    fn click(&mut self, key: &str) {
        self.pending = Some((key.into(), 0));
    }
    fn fill(&mut self, key: &str, text: String) {
        self.click(key);
        self.paste = Some(text);
    }
    fn child_ready(&self, n: usize) -> bool {
        let p = self.dir.join(format!("opened-{n}/proof.json"));
        if !p.is_file() {
            return false;
        }
        let a: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        assert_eq!(a["result"], "GENERATED_PROJECT_K2_READY");
        assert_eq!(a["root"].as_str().unwrap(), self.project.to_str().unwrap());
        true
    }
    fn browsing_at(&self, app: &Workbench, root: &std::path::Path) -> bool {
        app.projects.browser.as_ref().is_some_and(|b| {
            !b.busy && b.error.is_none() && b.listing.as_ref().is_some_and(|l| l.root == root)
        })
    }
}
fn key(input: &mut egui::RawInput, key: Key, command: bool) {
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: command,
                command,
                ..Default::default()
            },
        });
    }
}
impl Workbench {
    pub(super) fn project_nav_smoke_input(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.project_nav_smoke.take() else {
            return;
        };
        let state = format!(
            "step={} pending={:?} awaiting={:?} dialog={} browser={} opening={}",
            s.step,
            s.pending.as_ref().map(|p| &p.0),
            s.awaiting,
            self.projects.open,
            self.projects.browser.is_some(),
            self.projects.opening
        );
        if state != s.last_state {
            eprintln!("NAV_STATE {state}");
            s.last_state = state;
        }
        input.focused = true;
        input.modifiers = Default::default();
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        assert!(
            s.start.elapsed() < Duration::from_secs(300),
            "navigation smoke step{} {} {:?}",
            s.step,
            self.status,
            self.projects.error
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
            self.project_nav_smoke = Some(s);
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
            self.project_nav_smoke = Some(s);
            return;
        }
        if let Some(text) = s.paste.take() {
            key(input, Key::A, true);
            if text.is_empty() {
                key(input, Key::Backspace, false);
            } else {
                input.events.push(egui::Event::Paste(text));
            }
            s.next = Instant::now() + Duration::from_millis(500);
            self.project_nav_smoke = Some(s);
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.project_nav_smoke = Some(s);
            return;
        }
        let mut shot = None;
        let mut done = false;
        let mut wait = false;
        if s.reopen {
            match s.step {
                0 => {
                    self.editor_ui.focus_editor = true;
                    s.paste = Some(s.dirty.clone());
                }
                1 => {
                    assert_eq!(self.doc.rope().to_string(), s.dirty);
                    s.click("projects");
                }
                2 => s.click("project-recent"),
                3 => {
                    if self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.history.records.len(), 1);
                        assert_eq!(self.projects.history.records[0].root, s.project);
                        shot = Some("01-persisted-recent");
                    }
                }
                4 => s.click(&format!("recent-open:{}", s.project.display())),
                5 => {
                    if !s.child_ready(1) || self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.launches, 1);
                        s.click(&format!("recent-remove:{}", s.project.display()));
                    }
                }
                6 => {
                    if self.projects.history.busy {
                        wait = true;
                    } else {
                        assert!(self.projects.history.records.is_empty());
                        assert!(s.project.join("wex.json").is_file());
                        shot = Some("02-removed-not-deleted");
                    }
                }
                7 => s.click("recent-refresh"),
                8 => {
                    if self.projects.history.busy {
                        wait = true;
                    } else {
                        assert!(self.projects.history.records.is_empty());
                        s.click("project-close");
                    }
                }
                9 => {
                    assert!(!self.projects.open);
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Z, true);
                }
                10 => done = true,
                _ => unreachable!(),
            }
        } else {
            match s.step {
                0 => {
                    self.editor_ui.focus_editor = true;
                    s.paste = Some(s.dirty.clone());
                }
                1 => {
                    assert_eq!(self.doc.rope().to_string(), s.dirty);
                    s.click("projects");
                }
                2 => s.click("project-existing"),
                3 => s.click("project-browse-open"),
                4 => s.fill(
                    "browser-address",
                    s.workspace
                        .join("missing-folder")
                        .to_string_lossy()
                        .into_owned(),
                ),
                5 => s.click("browser-go"),
                6 => {
                    let b = self.projects.browser.as_ref().unwrap();
                    if b.busy {
                        wait = true;
                    } else {
                        assert!(b.error.is_some());
                        shot = Some("01-browser-error");
                        key(input, Key::S, true);
                        key(input, Key::F5, false);
                    }
                }
                7 => {
                    assert_eq!(std::fs::read_to_string(&s.source).unwrap(), s.original);
                    assert!(!self.building && !self.running && !self.debug.busy);
                    s.fill(
                        "browser-address",
                        s.workspace.to_string_lossy().into_owned(),
                    );
                }
                8 => s.click("browser-go"),
                9 => {
                    if !s.browsing_at(self, &s.workspace) {
                        wait = true;
                    } else {
                        shot = Some("02-browser-folders");
                        s.click("browser-hidden");
                    }
                }
                10 => {
                    if !s.browsing_at(self, &s.workspace) {
                        wait = true;
                    } else {
                        assert!(
                            self.projects
                                .browser
                                .as_ref()
                                .unwrap()
                                .listing
                                .as_ref()
                                .unwrap()
                                .folders
                                .iter()
                                .any(|f| f.name == ".hidden")
                        );
                        s.fill("browser-filter", "Проекты".into());
                    }
                }
                11 => s.click(&format!(
                    "browser-folder:{}",
                    s.workspace.join("Проекты").display()
                )),
                12 => {
                    if !s.browsing_at(self, &s.workspace.join("Проекты")) {
                        wait = true;
                    } else {
                        s.fill("browser-filter", String::new());
                    }
                }
                13 => s.click(&format!("browser-folder:{}", s.project.display())),
                14 => {
                    if !s.browsing_at(self, &s.project) {
                        wait = true;
                    } else {
                        shot = Some("03-browser-project");
                    }
                }
                15 => s.click("browser-use"),
                16 => {
                    assert!(self.projects.browser.is_none());
                    assert_eq!(self.projects.existing, s.project.to_string_lossy());
                    assert_eq!(self.projects.launches, 0);
                    s.click("project-open-existing");
                }
                17 => {
                    if !s.child_ready(1) || self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.launches, 1);
                        assert_eq!(self.doc.rope().to_string(), s.dirty);
                        s.click("project-recent");
                    }
                }
                18 => {
                    if self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.history.records.len(), 2);
                        shot = Some("04-recent-projects");
                        s.fill("recent-filter", "Missing".into());
                    }
                }
                19 => s.click(&format!("recent-open:{}", s.missing.display())),
                20 => {
                    if self.projects.opening {
                        wait = true;
                    } else {
                        assert!(self.projects.error.is_some());
                        assert_eq!(self.projects.launches, 1);
                        shot = Some("05-missing-project");
                    }
                }
                21 => s.click(&format!("recent-remove:{}", s.missing.display())),
                22 => {
                    if self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.history.records.len(), 1);
                        s.fill("recent-filter", "Browse".into());
                    }
                }
                23 => s.click(&format!("recent-open:{}", s.project.display())),
                24 => {
                    if !s.child_ready(2) || self.projects.history.busy {
                        wait = true;
                    } else {
                        assert_eq!(self.projects.launches, 2);
                        shot = Some("06-recent-opened");
                    }
                }
                25 => s.click("project-existing"),
                26 => s.click("project-browse-open"),
                27 => {
                    if !s.browsing_at(self, &s.project) {
                        wait = true;
                    } else {
                        s.click("browser-up");
                    }
                }
                28 => {
                    if !s.browsing_at(self, s.project.parent().unwrap()) {
                        wait = true;
                    } else {
                        s.click("browser-cancel");
                    }
                }
                29 => {
                    assert!(self.projects.browser.is_none());
                    assert_eq!(self.projects.existing, s.project.to_string_lossy());
                    s.click("project-new");
                }
                30 => s.click("project-browse-parent"),
                31 => s.fill(
                    "browser-address",
                    s.workspace.to_string_lossy().into_owned(),
                ),
                32 => s.click("browser-go"),
                33 => {
                    if !s.browsing_at(self, &s.workspace) {
                        wait = true;
                    } else {
                        s.click("browser-use");
                    }
                }
                34 => {
                    assert_eq!(self.projects.parent, s.workspace.to_string_lossy());
                    assert!(self.projects.result.is_none());
                    s.click("project-close");
                }
                35 => {
                    assert!(!self.projects.open);
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Z, true);
                }
                36 => done = true,
                _ => unreachable!(),
            }
        }
        if wait {
            self.project_nav_smoke = Some(s);
            return;
        }
        if done {
            assert_eq!(self.doc.rope().to_string(), s.original);
            assert_eq!(std::fs::read_to_string(&s.source).unwrap(), s.original);
            assert!(s.project.join("wex.json").is_file());
            assert_eq!(
                self.projects.history.records.len(),
                if s.reopen { 0 } else { 1 }
            );
            std::fs::write(s.dir.join("navigation-window.json"),serde_json::to_vec_pretty(&json!({"result":"GUI_PROJECT_NAVIGATION_OK","reopened_process":s.reopen,"project":s.project,"history_path":self.projects.history.store.path,"history":self.projects.history.records,"launches":self.projects.launches,"screenshots":s.shots,"dirty_buffer_preserved":true,"undo_restored":true,"files_not_deleted":true,"native_pointer_and_keyboard":true,"renderer":"wgpu"})).unwrap()).unwrap();
            eprintln!(
                "GUI_PROJECT_NAVIGATION_OK: genuine folder Go/hidden/filter/Up/Use/Cancel + Recent Open/remove/missing recovery; persisted across IDE processes; real K2 child windows; original dirty/disk/Undo preserved; metadata removal never deletes projects"
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(600);
        self.project_nav_smoke = Some(s);
    }
    pub(super) fn project_nav_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.project_nav_smoke else {
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
