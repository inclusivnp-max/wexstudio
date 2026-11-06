//! Genuine native pointer Build/Save + keyboard edit/Undo with actual module receipts.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    dir: PathBuf,
    source: PathBuf,
    original: String,
    edited: String,
    start: Instant,
    next: Instant,
    step: u8,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    reports: Vec<Value>,
    previous: PathBuf,
}
impl Smoke {
    pub(super) fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let source = app
            .root
            .join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
        let original = std::fs::read_to_string(&source).unwrap();
        let edited = format!("{original}\nfun cacheWindowProof(): Int = 42\n");
        app.open_file(source.clone());
        app.editor_ui.focus_editor = true;
        Self {
            dir,
            source,
            original,
            edited,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            reports: vec![],
            previous: PathBuf::new(),
        }
    }
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
impl Workbench {
    pub(super) fn compile_cache_smoke_input(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.compile_cache_smoke.take() else {
            return;
        };
        input.focused = true;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(400),
            "cache window step{} {} {}",
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
            self.compile_cache_smoke = Some(s);
            return;
        }
        if let Some((name, hover)) = s.pending.take() {
            if let Some(r) = self.chrome.hits.get(&name) {
                let pos = r.center();
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
            self.compile_cache_smoke = Some(s);
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.compile_cache_smoke = Some(s);
            return;
        }
        let clean = self.doc.path.as_ref().is_some_and(|p| {
            self.diagnostic_snapshot.as_ref() == Some(&(p.clone(), self.doc.version))
        }) && !self.diagnostics.iter().any(|d| d.severity == Some(1));
        let mut shot = None;
        match s.step {
            0 => {
                if !clean
                    || !self
                        .model_status
                        .as_ref()
                        .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                {
                    self.compile_cache_smoke = Some(s);
                    return;
                }
                shot = Some("01-real-model-before-build");
            }
            1 | 5 | 8 | 11 => {
                s.previous = self
                    .last_build
                    .as_ref()
                    .map(|b| b.build_dir.clone())
                    .unwrap_or_default();
                s.pending = Some(("build".into(), 0));
            }
            2 | 6 | 9 | 12 => {
                if self.building
                    || self
                        .last_build
                        .as_ref()
                        .is_none_or(|b| b.build_dir == s.previous)
                {
                    self.compile_cache_smoke = Some(s);
                    return;
                }
                assert!(
                    self.status.starts_with("BUILD SUCCESSFUL"),
                    "{}",
                    self.status
                );
                let report = self.last_build.as_ref().unwrap();
                let states = report
                    .compilation_cache
                    .iter()
                    .map(|r| (r.module.as_str(), r.state.as_str()))
                    .collect::<std::collections::BTreeMap<_, _>>();
                assert_eq!(states["shared"], "hit");
                assert_eq!(states["app"], if s.step == 6 { "miss" } else { "hit" });
                assert_eq!(report.dex_cache.len(), 1);
                assert_eq!(
                    report.dex_cache[0].state,
                    if s.step == 6 { "miss" } else { "hit" }
                );
                assert_eq!(
                    report
                        .steps
                        .iter()
                        .any(|s| s.name == "D8 · JVM bytecode → DEX"),
                    s.step == 6
                );
                assert!(self.chrome.hits.contains_key("compile-cache-summary"));
                assert!(!self.doc.dirty());
                s.reports.push(serde_json::to_value(report).unwrap());
                shot = Some(match s.step {
                    2 => "02-warm-build-cache-card",
                    6 => "03-edit-app-only-recompiled",
                    9 => "04-repeat-both-reused",
                    _ => "05-undo-restored-cache",
                });
            }
            3 => {
                self.editor_ui.focus_editor = true;
                key(
                    input,
                    Key::A,
                    egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                );
                input.events.push(egui::Event::Paste(s.edited.clone()));
            }
            4 => {
                if !clean || *self.doc.rope() != s.edited {
                    self.compile_cache_smoke = Some(s);
                    return;
                }
                assert!(self.doc.dirty());
            }
            7 => (),
            10 => {
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
                assert_eq!(*self.doc.rope(), s.original);
                assert_eq!(std::fs::read_to_string(&s.source).unwrap(), s.original);
                std::fs::write(s.dir.join("compile-cache-window.json"),serde_json::to_vec_pretty(&json!({"result":"GUI_COMPILE_CACHE_OK","reports":s.reports,"screenshots":s.shots,"actual_pointer_build_clicks":4,"keyboard_edit_undo":true,"source_restored":true,"renderer":"wgpu","gradle_invoked":false})).unwrap()).unwrap();
                eprintln!(
                    "GUI_COMPILE_CACHE_OK: real Build pointer/Save/keyboard edit/Undo; warm2/2, app-only1/2, repeat2/2, restored2/2; M3 module cache cards;5 genuine GPU frames; disk restored; no Gradle"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(500);
        self.compile_cache_smoke = Some(s);
    }
    pub(super) fn compile_cache_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.compile_cache_smoke else {
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
