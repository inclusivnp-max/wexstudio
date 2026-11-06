//! Native GPU responsiveness while a real large source is streaming/parsing, plus
//! Cancel acknowledgement and actual invalid-config repair/Retry/K2 readiness.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Open,
    Cancel,
    Repair,
}
pub(super) struct Smoke {
    dir: PathBuf,
    mode: Mode,
    start: Instant,
    next: Instant,
    step: u8,
    done: Arc<AtomicBool>,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    worker_live_at_first_frame: bool,
    first_update_ms: Option<u128>,
    frames: u64,
    last_state: String,
}
impl Smoke {
    pub fn new(dir: PathBuf, mode: Mode, app: &Workbench) -> Self {
        Self {
            dir,
            mode,
            start: Instant::now(),
            next: Instant::now(),
            step: 0,
            done: app.boot.job.as_ref().unwrap().done.clone(),
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            worker_live_at_first_frame: false,
            first_update_ms: None,
            frames: 0,
            last_state: String::new(),
        }
    }
    fn click(&mut self, key: &str) {
        self.pending = Some((key.into(), 0));
    }
}
impl Workbench {
    pub(super) fn boot_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.boot_smoke.take() else {
            return;
        };
        input.focused = true;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        s.frames += 1;
        let state = format!(
            "step={} pending={:?} awaiting={:?} loading={} welcome={} worker_done={}",
            s.step,
            s.pending.as_ref().map(|p| &p.0),
            s.awaiting,
            self.boot.pending,
            self.welcome,
            s.done.load(Ordering::Acquire)
        );
        if state != s.last_state {
            eprintln!("BOOT_STATE {state}");
            s.last_state = state;
        }
        assert!(
            s.start.elapsed() < Duration::from_secs(180),
            "Boot smoke timeout step{} {:?} {}",
            s.step,
            self.boot.error,
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
            self.boot_smoke = Some(s);
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
            self.boot_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.boot_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        let mut finish = false;
        match (s.mode, s.step) {
            (_, 0) => {
                assert!(
                    self.boot.pending
                        && self.doc.path.is_none()
                        && self.language.is_none()
                        && self.devices.is_empty()
                );
                assert_eq!(self.editor_ui.services.active_workers(), 0);
                s.worker_live_at_first_frame = !s.done.load(Ordering::Acquire);
                s.first_update_ms = Some(s.start.elapsed().as_millis());
                shot = Some("01-loading");
            }
            (Mode::Cancel, 1) => {
                assert!(
                    s.worker_live_at_first_frame,
                    "real worker completed before first frame"
                );
                assert!(
                    !s.done.load(Ordering::Acquire),
                    "large parse already finished before Cancel"
                );
                s.click("boot-cancel");
            }
            (Mode::Cancel, 2) => {
                if !self.welcome || !s.done.load(Ordering::Acquire) {
                    wait = true;
                } else {
                    assert!(
                        self.doc.path.is_none()
                            && self.language.is_none()
                            && self.devices.is_empty()
                            && !self.boot.pending
                    );
                    assert_eq!(self.editor_ui.services.active_workers(), 0);
                    shot = Some("02-cancelled");
                }
            }
            (Mode::Cancel, 3) => finish = true,
            (Mode::Open, 1) => {
                if self.boot.pending {
                    wait = true;
                } else {
                    assert!(self.boot.error.is_none());
                    assert!(self.doc.rope().len_lines() >= 50000);
                    assert!(s.worker_live_at_first_frame);
                    shot = Some("02-document-ready");
                }
            }
            (Mode::Open, 2) => {
                assert!(self.editor_ui.rendered_rows < 200);
                self.editor_ui.focus_editor = true;
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key {
                        key: Key::End,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers {
                            ctrl: true,
                            command: true,
                            ..Default::default()
                        },
                    });
                }
            }
            (Mode::Open, 3) => {
                assert_eq!(self.edit.cursor, self.doc.rope().len_chars());
                shot = Some("03-large-rope-end");
            }
            (Mode::Open, 4) => finish = true,
            (Mode::Repair, 1) => {
                if self.boot.error.is_none() {
                    wait = true;
                } else {
                    assert!(
                        self.doc.path.is_none()
                            && self.language.is_none()
                            && self.devices.is_empty()
                            && !self.projects.history.busy
                    );
                    shot = Some("02-load-error");
                }
            }
            (Mode::Repair, 2) => {
                if !s.dir.join("repair-ready").is_file() {
                    wait = true;
                } else {
                    s.click("boot-retry");
                }
            }
            (Mode::Repair, 3) => {
                if self.boot.pending
                    || !self
                        .model_status
                        .as_ref()
                        .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                    || self.diagnostic_snapshot.as_ref()
                        != self
                            .doc
                            .path
                            .as_ref()
                            .map(|p| (p.clone(), self.doc.version))
                            .as_ref()
                {
                    wait = true;
                } else {
                    assert!(!self.diagnostics.iter().any(|d| d.severity == Some(1)));
                    shot = Some("03-repaired-k2-ready");
                }
            }
            (Mode::Repair, 4) => finish = true,
            _ => unreachable!(),
        }
        if wait {
            self.boot_smoke = Some(s);
            return;
        }
        if finish {
            let result = match s.mode {
                Mode::Open => "GPU_ASYNC_PROJECT_OPEN_OK",
                Mode::Cancel => "GPU_ASYNC_PROJECT_CANCEL_OK",
                Mode::Repair => "GPU_ASYNC_PROJECT_REPAIR_OK",
            };
            std::fs::write(s.dir.join("boot-window.json"),serde_json::to_vec_pretty(&json!({"result":result,"root":self.root,"worker_live_at_first_frame":s.worker_live_at_first_frame,"first_input_hook_ms_after_app_creator":s.first_update_ms,"worker_acknowledged_end":s.done.load(Ordering::Acquire),"document":self.doc.path,"document_lines":self.doc.rope().len_lines(),"document_ready_ms":self.boot.loaded_ms,"editor_rendered_rows":self.editor_ui.rendered_rows,"welcome":self.welcome,"k2":self.model_status,"diagnostic_snapshot":self.diagnostic_snapshot,"diagnostics":self.diagnostics,"reduced_motion":self.reduced_motion,"font_fallback_ready":self.fonts.ready,"font_fallback_installed":self.fonts.installed,"source_discovery_calls":project::discovery_calls(),"startup_trace":self.startup_trace.json(),"screenshots":s.shots,"input_frames":s.frames,"input":"actual native eframe RawInput pointer/keyboard and GPU screenshots","renderer":"wgpu"})).unwrap()).unwrap();
            eprintln!(
                "{result}: actual worker, native GPU loading/content/error/Cancel/Retry; no UI-thread project scan/read/parse; input frames {}",
                s.frames
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(100);
        self.boot_smoke = Some(s);
    }
    pub(super) fn boot_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.boot_smoke else {
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
                    if name == "02-load-error" {
                        std::fs::write(s.dir.join("error-visible"), "GPU error frame captured\n")
                            .unwrap();
                    }
                    s.awaiting = None;
                }
            }
        }
    }
}
