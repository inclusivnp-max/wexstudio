//! Live native-window proof with actual K2 and unsaved cross-module buffers.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct RefactorSmoke {
    initial_stage: u8,
    dir: PathBuf,
    source: PathBuf,
    caller: PathBuf,
    original: Vec<(PathBuf, String)>,
    start: Instant,
    next: Instant,
    since: Instant,
    step: usize,
    click: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    changes: Vec<Value>,
    before: Value,
    after: Value,
}
impl RefactorSmoke {
    pub fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let original = project::source_files(&app.root)
            .into_iter()
            .map(|p| {
                let h = blake3::hash(&std::fs::read(&p).unwrap())
                    .to_hex()
                    .to_string();
                (p, h)
            })
            .collect::<Vec<_>>();
        let source = original
            .iter()
            .find(|(p, _)| p.ends_with("Greeter.kt"))
            .unwrap()
            .0
            .clone();
        let caller = original
            .iter()
            .find(|(p, _)| p.ends_with("MainActivity.kt"))
            .unwrap()
            .0
            .clone();
        Self {
            initial_stage: 0,
            dir,
            source,
            caller,
            original,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            since: Instant::now(),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            changes: vec![],
            before: Value::Null,
            after: Value::Null,
        }
    }
}
fn key(input: &mut egui::RawInput, key: Key, mut modifiers: egui::Modifiers) {
    if cfg!(target_os = "linux") && modifiers.command {
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
    pub(super) fn refactor_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.refactor_smoke.take() else {
            return;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "refactor smoke timeout step{}: {} / {}",
            s.step,
            self.language_status,
            self.refactor.error
        );
        input.focused = true;
        input.modifiers = Default::default();
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        match s.initial_stage {
            0 => {
                self.open_file(s.caller.clone());
                s.initial_stage = 1;
                self.refactor_smoke = Some(s);
                return;
            }
            1 => {
                if self.doc.path.as_ref() != Some(&s.caller) {
                    self.refactor_smoke = Some(s);
                    return;
                }
                let end = self.doc.rope().len_chars();
                self.doc.edit(end..end,"\n// greeting is a comment, not a symbol\nfun refactorUnsavedProbe() = Greeter().greeting(\"unsaved\")\n").unwrap();
                self.did_edit();
                self.open_file(s.source.clone());
                s.initial_stage = 2;
                self.refactor_smoke = Some(s);
                return;
            }
            2 => {
                if self.doc.path.as_ref() != Some(&s.source) {
                    self.refactor_smoke = Some(s);
                    return;
                }
                let text = self.doc.rope().to_string();
                let byte = text.find("greeting(").unwrap() + 3;
                self.edit.collapse(text[..byte].chars().count());
                self.scroll_to_cursor = true;
                self.send_snapshot();
                s.initial_stage = 3;
                s.next = Instant::now() + Duration::from_secs(2);
            }
            _ => {}
        }
        if let Some((pos, time)) = s.release.take() {
            if Instant::now() < time {
                s.release = Some((pos, time));
            } else {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.refactor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if let Some((name, hover)) = s.click.take() {
            let p = self
                .chrome
                .hits
                .get(&name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .center();
            input.events.push(egui::Event::PointerMoved(p));
            if hover < 2 {
                s.click = Some((name, hover + 1));
            } else {
                input.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                s.release = Some((p, Instant::now() + Duration::from_millis(100)));
            }
            self.refactor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.refactor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        macro_rules! wait {
            ($v:expr) => {
                if !$v {
                    assert!(
                        s.since.elapsed() < Duration::from_secs(180),
                        "refactor step{}: {} / {} dialog={} busy={} name={:?} status={} pending_open={}",
                        s.step,self.language_status,self.refactor.error,self.refactor.dialog,self.refactor.busy,self.refactor.name,self.status,self.open_ui.pending.is_some()
                    );
                    s.next = Instant::now() + Duration::from_millis(50);
                    self.refactor_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
            };
        }
        let mut shot = None;
        match s.step {
            0 => {
                wait!(self.language_status.starts_with("K2 "));
                key(input, Key::F12, egui::Modifiers::SHIFT);
            }
            1 => {
                wait!(!self.refactor.busy && self.refactor.usages.len() >= 4);
                assert_eq!(self.refactor.usages.len(), 4);
                s.before = json!(self.refactor.usages);
                shot = Some("01-usages-unsaved");
            }
            2 => {
                let i = self
                    .refactor
                    .usages
                    .iter()
                    .position(|(p, _, text)| {
                        p == &s.caller && text.contains("refactorUnsavedProbe")
                    })
                    .unwrap();
                s.click = Some((format!("usage-{i}"), 0));
            }
            3 => {
                wait!(
                    self.doc.path.as_ref() == Some(&s.caller)
                        && self.edit.selected_text(&self.doc) == "greeting"
                );
                shot = Some("02-source-navigation");
                key(input, Key::ArrowLeft, egui::Modifiers::NONE);
            }
            4 => {
                key(input, Key::F12, egui::Modifiers::NONE);
            }
            5 => {
                wait!(self.doc.path.as_ref() == Some(&s.source));
                key(input, Key::F6, egui::Modifiers::SHIFT);
            }
            6 => {
                wait!(
                    self.refactor.dialog && !self.refactor.busy && self.refactor.name == "greeting"
                );
                wait!(ctx.memory(|m| m.has_focus(egui::Id::new("rename-name"))));
                key(input, Key::A, egui::Modifiers::COMMAND);
                input.events.push(egui::Event::Text("salutation".into()));
            }
            7 => {
                wait!(self.refactor.name == "salutation");
                s.click = Some(("rename-preview".into(), 0));
            }
            8 => {
                wait!(!self.refactor.busy && self.refactor.plan.is_some());
                let plan = self.refactor.plan.as_ref().unwrap();
                assert_eq!(plan.files.len(), 2);
                assert_eq!(plan.edit_count, 16);
                s.changes=plan.files.iter().map(|f|json!({"path":f.path.strip_prefix(&self.root).unwrap(),"after":f.after,"edits":f.edits.len()})).collect();
                assert!(
                    s.changes
                        .iter()
                        .any(|f| f["after"].as_str().unwrap().contains("greetingCount"))
                );
                shot = Some("03-project-preview");
            }
            9 => {
                s.click = Some(("rename-apply".into(), 0));
            }
            10 => {
                wait!(
                    !self.refactor.dialog
                        && !self.refactor.busy
                        && self.doc.rope().to_string().contains("fun salutation(")
                );
                let caller = &self.parked[&s.caller].0;
                assert!(caller.dirty());
                assert!(
                    caller
                        .rope()
                        .to_string()
                        .contains("// greeting is a comment, not a symbol")
                );
                assert!(
                    caller
                        .rope()
                        .to_string()
                        .contains("refactorUnsavedProbe() = Greeter().salutation(")
                );
                shot = Some("04-applied-unsaved");
            }
            11 => {
                key(input, Key::F12, egui::Modifiers::SHIFT);
            }
            12 => {
                wait!(!self.refactor.busy && self.refactor.usages.len() == 4);
                assert!(
                    self.refactor
                        .usages
                        .iter()
                        .all(|(_, _, t)| t.contains("salutation"))
                );
                s.after = json!(self.refactor.usages);
                s.click = Some(("rename-undo".into(), 0));
            }
            13 => {
                wait!(self.doc.rope().to_string().contains("fun greeting("));
                assert!(!self.doc.dirty());
                assert!(
                    self.parked[&s.caller]
                        .0
                        .rope()
                        .to_string()
                        .contains("refactorUnsavedProbe() = Greeter().greeting(")
                );
                assert!(self.parked[&s.caller].0.dirty());
                shot = Some("05-project-undo");
            }
            14 => {
                for (p, h) in &s.original {
                    assert_eq!(
                        blake3::hash(&std::fs::read(p).unwrap()).to_hex().as_str(),
                        h
                    );
                }
                assert_eq!(s.shots.len(), 5);
                let result = json!({"result":"GUI_REFACTOR_OK","engine":"JetBrains K2 LSP263.6379.0","usages_before":s.before,"usages_after":s.after,"renamed_sources":s.changes,"screenshots":s.shots,"disk_source_unchanged":true,"unsaved_parked_buffer":true,"project_undo":true,"semantic_not_text_replace":true});
                std::fs::write(
                    s.dir.join("refactor-window.json"),
                    serde_json::to_vec_pretty(&result).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "GUI_REFACTOR_OK: actual K2; 4 usages/2 modules including unsaved parked source; navigation; prepare/preview/apply; dirty buffers resynchronized; project undo; 5 GPU screenshots; disk unchanged"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                s.step = 15;
                self.refactor_smoke = Some(s);
                return;
            }
            15 => {
                self.refactor_smoke = Some(s);
                return;
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.since = Instant::now();
        s.next = Instant::now() + Duration::from_millis(400);
        self.refactor_smoke = Some(s);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub(super) fn refactor_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.refactor_smoke else {
            return;
        };
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event
                && let Some(name) = s.awaiting.take()
            {
                let p = s.dir.join(format!("{name}.png"));
                let pixels = image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>();
                image::save_buffer(
                    &p,
                    &pixels,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                s.shots
                    .push(json!({"path":p,"width":image.size[0],"height":image.size[1]}));
            }
        }
    }
}
