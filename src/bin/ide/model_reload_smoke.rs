//! Real keyboard/pointer editor + external resource edit regression, no fake events.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    dir: PathBuf,
    source: PathBuf,
    shared: PathBuf,
    resource: PathBuf,
    disk: String,
    shared_disk: String,
    dirty: String,
    changed: String,
    parked: String,
    valid: String,
    step: u8,
    start: Instant,
    next: Instant,
    attempt: u64,
    failed: u64,
    job: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    snapshots: Vec<Value>,
}
fn clean(app: &Workbench) -> bool {
    app.doc
        .path
        .as_ref()
        .is_some_and(|p| app.diagnostic_snapshot.as_ref() == Some(&(p.clone(), app.doc.version)))
        && !app.diagnostics.iter().any(|d| d.severity == Some(1))
}
fn key(input: &mut egui::RawInput, key: Key, modifiers: egui::Modifiers) {
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
fn replace(input: &mut egui::RawInput, text: &str) {
    key(
        input,
        Key::A,
        egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        },
    );
    input.events.push(egui::Event::Paste(text.to_owned()));
}
impl Smoke {
    pub(super) fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!app.root.join(".wex/analysis").exists() && !app.root.join(".wex/native").exists());
        let source = app
            .root
            .join("app/src/main/kotlin/dev/wex/android/ReloadProbe.kt");
        let shared = app
            .root
            .join("shared/src/main/kotlin/dev/wex/shared/Greeter.kt");
        let resource = app.root.join("app/src/main/res/values/strings.xml");
        let disk = std::fs::read_to_string(&source).unwrap();
        let shared_disk = std::fs::read_to_string(&shared).unwrap();
        let parked = shared_disk.replacen(
            "class Greeter {",
            "class Greeter {\n    fun parkedOnly(): String = \"parked\"",
            1,
        );
        let dirty = format!(
            "{disk}\n// unsaved active overlay\nfun dirtyDependency(): String = dev.wex.shared.Greeter().parkedOnly()\n"
        );
        let changed = dirty.replace("R.string.app_name", "R.string.watch_added");
        let valid = std::fs::read_to_string(&resource).unwrap().replace(
            "</resources>",
            "<string name=\"watch_added\">watched</string></resources>",
        );
        app.open_file(source.clone());
        app.editor_ui.focus_editor = true;
        Self {
            dir,
            source,
            shared,
            resource,
            disk,
            shared_disk,
            dirty,
            changed,
            parked,
            valid,
            step: 0,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            attempt: 0,
            failed: 0,
            job: None,
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            snapshots: vec![],
        }
    }
    fn write(&mut self, text: String) {
        let path = self.resource.clone();
        self.job = Some(std::thread::spawn(move || std::fs::write(path, text)));
    }
    fn snap(&mut self, app: &Workbench) {
        self.snapshots.push(json!({"step":self.step,"path":app.doc.path,"version":app.doc.version,"diagnostic_snapshot":app.diagnostic_snapshot,"diagnostics":app.diagnostics,"dirty":app.doc.dirty(),"model":app.model_status,"language_status":app.language_status,"variant":app.variant,"parked":app.parked.iter().map(|(p,(d,_))|json!({"path":p,"dirty":d.dirty(),"text":d.rope().to_string()})).collect::<Vec<_>>()}));
    }
}
impl Workbench {
    pub(super) fn model_reload_smoke_input(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.model_reload_smoke.take() else {
            return;
        };
        input.focused = true;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(500),
            "reload smoke step{} {} {:?}",
            s.step,
            self.language_status,
            self.model_status
        );
        if let Some(job) = &s.job {
            if !job.is_finished() {
                self.model_reload_smoke = Some(s);
                return;
            }
            s.job.take().unwrap().join().unwrap().unwrap();
        }
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
            self.model_reload_smoke = Some(s);
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
                s.pending = Some((name, 0));
            }
            self.model_reload_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.model_reload_smoke = Some(s);
            return;
        }
        let mut shot = None;
        match s.step {
            0 => {
                if !clean(self)
                    || !self
                        .model_status
                        .as_ref()
                        .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                s.attempt = self.model_status.as_ref().unwrap().attempt;
                shot = Some("01-first-model");
            }
            1 => {
                self.open_file(s.shared.clone());
            }
            2 => {
                if self.doc.path.as_ref() != Some(&s.shared) {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                if *self.doc.rope() != s.parked {
                    self.editor_ui.focus_editor = true;
                    replace(input, &s.parked);
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert!(self.doc.dirty());
                self.open_file(s.source.clone());
                self.editor_ui.focus_editor = true;
                replace(input, &s.dirty);
            }
            3 => {
                if !clean(self) || *self.doc.rope() != s.dirty {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert_eq!(self.model_status.as_ref().unwrap().attempt, s.attempt);
                shot = Some("02-dirty-active-parked");
            }
            4 => {
                s.write(s.valid.clone());
                self.editor_ui.focus_editor = true;
                replace(input, &s.changed);
            }
            5 => {
                if !clean(self)
                    || !self.model_status.as_ref().is_some_and(|m| {
                        m.phase == language::ModelPhase::Applied && m.attempt > s.attempt
                    })
                {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert_eq!(self.doc.rope().to_string(), s.changed);
                assert!(self.doc.dirty());
                s.attempt = self.model_status.as_ref().unwrap().attempt;
                shot = Some("03-resource-auto-refreshed");
            }
            6 => {
                s.write("<resources><broken>".into());
            }
            7 => {
                if !self
                    .model_status
                    .as_ref()
                    .is_some_and(|m| m.phase == language::ModelPhase::Failed && m.retained)
                {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert!(clean(self) && self.doc.dirty());
                s.failed = self.model_status.as_ref().unwrap().attempt;
                shot = Some("04-invalid-resource-last-good");
            }
            8 => {
                s.pending = Some(("model-retry".into(), 0));
            }
            9 => {
                if !self.model_status.as_ref().is_some_and(|m| {
                    m.phase == language::ModelPhase::Failed && m.attempt > s.failed
                }) {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert!(clean(self));
                shot = Some("05-pointer-retry-error-retained");
            }
            10 => {
                s.write(s.valid.clone());
            }
            11 => {
                if !clean(self)
                    || !self.model_status.as_ref().is_some_and(|m| {
                        m.phase == language::ModelPhase::Applied && m.attempt > s.attempt
                    })
                {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert_eq!(self.doc.rope().to_string(), s.changed);
                s.attempt = self.model_status.as_ref().unwrap().attempt;
                shot = Some("06-external-repair-recovered");
            }
            12 => {
                self.editor_ui.focus_editor = true;
                key(
                    input,
                    Key::Z,
                    egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                );
            }
            13 => {
                if !clean(self) || *self.doc.rope() != s.dirty {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert!(self.doc.dirty());
                shot = Some("07-undo-history-retained");
            }
            14 => {
                if !self.variant_open {
                    s.pending = Some(("variant".into(), 0));
                    self.model_reload_smoke = Some(s);
                    return;
                }
                s.pending = Some(("variant-release".into(), 0));
            }
            15 => {
                if self.variant != "release" {
                    s.step = 14;
                    self.model_reload_smoke = Some(s);
                    return;
                }
                if !clean(self) || !self.language_status.contains("· release ·") {
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert_eq!(self.doc.rope().to_string(), s.dirty);
                assert!(self.doc.dirty());
                shot = Some("08-release-dirty-buffers");
            }
            16 => {
                let at = s.dirty.find("BuildConfig.DEBUG").unwrap() + 2;
                self.edit.collapse(s.dirty[..at].chars().count());
                self.editor_ui.focus_editor = true;
                self.scroll_to_cursor = true;
                key(input, Key::F12, egui::Modifiers::NONE);
            }
            17 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("BuildConfig.java"))
                {
                    if self.status == "No definition found" {
                        s.step = 16;
                    }
                    self.model_reload_smoke = Some(s);
                    return;
                }
                assert!(
                    self.doc
                        .path
                        .as_ref()
                        .unwrap()
                        .starts_with(self.root.join(".wex/analysis/release"))
                );
                assert!(self.doc.rope().to_string().contains("DEBUG = false"));
                shot = Some("09-real-release-definition");
            }
            18 => {
                assert_eq!(std::fs::read_to_string(&s.source).unwrap(), s.disk);
                assert_eq!(std::fs::read_to_string(&s.shared).unwrap(), s.shared_disk);
                assert!(
                    self.parked
                        .get(&s.source)
                        .is_some_and(|(d, _)| d.dirty() && *d.rope() == s.dirty)
                );
                assert!(
                    self.parked
                        .get(&s.shared)
                        .is_some_and(|(d, _)| d.dirty() && *d.rope() == s.parked)
                );
                assert!(!self.root.join(".wex/native").exists());
                let value = json!({"result":"GUI_MODEL_RELOAD_OK","renderer":"wgpu","screenshots":s.shots,"snapshots":s.snapshots,"source_disk_unchanged":true,"dirty_active_and_parked_retained":true,"undo_history_retained":true,"retry_button_pointer_clicked":true,"release_definition":self.doc.path,"apk_built":false,"gradle_invoked":false});
                std::fs::write(
                    s.dir.join("model-reload-window.json"),
                    serde_json::to_vec_pretty(&value).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "GUI_MODEL_RELOAD_OK: native resource auto-update/error/actual Retry pointer/external repair; dirty active+parked buffers and Undo survive isolated K2 swaps; actual Release picker/F12 DEBUG=false;9 genuine GPU frames; no APK/Gradle"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            s.snap(self);
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(400);
        self.model_reload_smoke = Some(s);
    }
    pub(super) fn model_reload_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.model_reload_smoke else {
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
