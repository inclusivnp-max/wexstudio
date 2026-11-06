//! Actual Save/Stop/Retry input with another process holding real advisory locks.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    pub dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    large: PathBuf,
    tiny: PathBuf,
    original: String,
    small: String,
    large_states: Vec<String>,
    tiny_latest: String,
    frames: usize,
    typed_waiting: bool,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        let large = app.root.join("Large.kt");
        let tiny = app.root.join("Tiny.kt");
        assert_eq!(app.doc.path.as_ref(), Some(&large));
        assert!(app.doc.rope().len_lines() > 100000);
        let original = std::fs::read_to_string(&large).unwrap();
        let small = std::fs::read_to_string(&tiny).unwrap();
        let mut large_states = vec![original.clone()];
        for label in [
            "snapshot one",
            "typed after snapshot",
            "after external revision",
            "before partial save",
        ] {
            let next = format!("{}// {label} 👋\n", large_states.last().unwrap());
            large_states.push(next);
        }
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            large,
            tiny,
            tiny_latest: format!("{small}// first tiny edit\n// late tiny edit\n"),
            original,
            small,
            large_states,
            frames: 0,
            typed_waiting: false,
        }
    }
    fn click(&mut self, name: &str) {
        self.pending = Some((name.into(), 0));
    }
    fn mark(&self, name: &str) {
        std::fs::write(self.dir.join(name), "native phase reached\n").unwrap();
    }
}
fn key(input: &mut egui::RawInput, key: Key, cmd: bool) {
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: cmd,
                command: cmd,
                ..Default::default()
            },
        });
    }
}
impl Workbench {
    pub(super) fn save_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.save_smoke.take() else {
            return;
        };
        input.focused = true;
        input.modifiers = Default::default();
        for v in input.viewports.values_mut() {
            v.focused = Some(true);
        }
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        s.frames += 1;
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "Save smoke timeout step{} status={} error={:?}",
            s.step,
            self.status,
            self.save_ui.error
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
            self.save_smoke = Some(s);
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
                    s.release = Some((pos, Instant::now() + Duration::from_millis(90)));
                }
            } else {
                s.pending = Some((name, hover));
            }
            self.save_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.save_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::End, true);
            }
            1 => {
                self.editor_ui.focus_editor = true;
                input
                    .events
                    .push(egui::Event::Paste("// snapshot one 👋\n".into()));
            }
            2 => key(input, Key::S, true),
            3 => {
                if self
                    .save_ui
                    .phase
                    .as_ref()
                    .is_some_and(|(p, _, _, w)| p == &s.large && *w)
                {
                    shot = Some("01-real-lock-wait-editable");
                } else {
                    wait = true;
                }
            }
            4 => {
                self.editor_ui.focus_editor = true;
                input
                    .events
                    .push(egui::Event::Paste("// typed after snapshot 👋\n".into()));
            }
            5 => {
                assert_eq!(self.doc.rope().to_string(), s.large_states[2]);
                assert!(
                    self.save_ui
                        .pending
                        .as_ref()
                        .is_some_and(|p| !p.ticket.done.load(std::sync::atomic::Ordering::Acquire))
                );
                assert!(self.doc.dirty());
                s.typed_waiting = true;
                shot = Some("02-newer-edits-during-save");
                s.mark("release-large-first");
            }
            6 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert_eq!(
                        std::fs::read_to_string(&s.large).unwrap(),
                        s.large_states[1]
                    );
                    assert_eq!(self.doc.rope().to_string(), s.large_states[2]);
                    assert!(self.doc.dirty());
                    shot = Some("03-saved-snapshot-kept-newer-buffer");
                }
            }
            7 => key(input, Key::Z, true),
            8 => {
                assert_eq!(self.doc.rope().to_string(), s.large_states[1]);
                assert!(!self.doc.dirty());
                key(input, Key::Y, true);
            }
            9 => {
                assert_eq!(self.doc.rope().to_string(), s.large_states[2]);
                assert!(self.doc.dirty());
                s.click("save-retry");
            }
            10 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert!(!self.doc.dirty());
                    assert_eq!(
                        std::fs::read_to_string(&s.large).unwrap(),
                        s.large_states[2]
                    );
                    shot = Some("04-save-latest-and-history-retained");
                    s.mark("hold-large-second");
                }
            }
            11 => {
                self.editor_ui.focus_editor = true;
                if !s.dir.join("large-second-held").exists() {
                    wait = true;
                } else {
                    input
                        .events
                        .push(egui::Event::Paste("// after external revision 👋\n".into()));
                }
            }
            12 => key(input, Key::S, true),
            13 => {
                if self
                    .save_ui
                    .phase
                    .as_ref()
                    .is_some_and(|(p, _, _, w)| p == &s.large && *w)
                {
                    s.mark("write-external");
                } else {
                    wait = true;
                }
            }
            14 => {
                if !s.dir.join("external-ready").exists() {
                    wait = true;
                } else {
                    s.next = Instant::now() + Duration::from_millis(1100);
                }
            }
            15 => {
                assert!(self.save_ui.pending.is_some());
                assert!(!self.editor_ui.conflicts.contains_key(&s.large));
                shot = Some("05-external-change-during-real-lock-wait");
                s.mark("release-large-second");
            }
            16 => {
                if self.save_ui.pending.is_some()
                    || !self.editor_ui.conflicts.contains_key(&s.large)
                {
                    wait = true;
                } else {
                    assert!(self.save_ui.error.is_some());
                    assert_eq!(self.doc.rope().to_string(), s.large_states[3]);
                    assert_eq!(
                        std::fs::read_to_string(&s.large).unwrap(),
                        "// external disk revision\nval external=7\n"
                    );
                    assert!(self.doc.dirty());
                    shot = Some("06-save-error-and-replayed-disk-conflict");
                }
            }
            17 => s.click("disk-keep"),
            18 => {
                if self.editor_ui.conflicts.contains_key(&s.large) {
                    wait = true;
                } else {
                    s.click("save-retry");
                }
            }
            19 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert!(self.save_ui.error.is_none());
                    assert!(!self.doc.dirty());
                    assert_eq!(
                        std::fs::read_to_string(&s.large).unwrap(),
                        s.large_states[3]
                    );
                    shot = Some("07-explicit-keep-mine-save-latest");
                }
            }
            20 => {
                self.editor_ui.focus_editor = true;
                input
                    .events
                    .push(egui::Event::Paste("// before partial save 👋\n".into()));
            }
            21 => s.click("file-Tiny.kt"),
            22 => {
                if self.doc.path.as_ref() != Some(&s.tiny) {
                    wait = true;
                } else {
                    self.editor_ui.focus_editor = true;
                    key(input, Key::End, true);
                    input
                        .events
                        .push(egui::Event::Paste("// first tiny edit\n".into()));
                }
            }
            23 => key(input, Key::B, true),
            24 => {
                if self
                    .save_ui
                    .phase
                    .as_ref()
                    .is_some_and(|(p, i, n, w)| p == &s.tiny && *w && *i == 1 && *n == 2)
                {
                    assert_eq!(
                        std::fs::read_to_string(&s.large).unwrap(),
                        s.large_states[4]
                    );
                    shot = Some("08-one-committed-second-file-waiting");
                } else {
                    wait = true;
                }
            }
            25 => {
                self.editor_ui.focus_editor = true;
                input
                    .events
                    .push(egui::Event::Paste("// late tiny edit\n".into()));
            }
            26 => s.click("save-stop"),
            27 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert!(!self.parked[&s.large].0.dirty());
                    assert!(self.doc.dirty());
                    assert_eq!(self.doc.rope().to_string(), s.tiny_latest);
                    assert_eq!(std::fs::read_to_string(&s.tiny).unwrap(), s.small);
                    assert!(self.build_task.is_none() && self.run_task.is_none());
                    shot = Some("09-stop-kept-committed-and-newer-edits");
                    s.mark("release-tiny");
                }
            }
            28 => {
                if !s.dir.join("tiny-released").exists() {
                    wait = true;
                } else {
                    s.click("save-retry");
                }
            }
            29 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert!(!self.doc.dirty());
                    assert_eq!(std::fs::read_to_string(&s.tiny).unwrap(), s.tiny_latest);
                }
            }
            30 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::A, true);
                input.events.push(egui::Event::Paste(s.small.clone()));
            }
            31 => key(input, Key::S, true),
            32 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    s.click("tab-Large.kt");
                }
            }
            33 => {
                if self.doc.path.as_ref() != Some(&s.large) {
                    wait = true;
                } else {
                    self.editor_ui.focus_editor = true;
                    for _ in 0..4 {
                        key(input, Key::Z, true);
                    }
                }
            }
            34 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                key(input, Key::S, true);
            }
            35 => {
                if self.save_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert!(!self.doc.dirty());
                    assert_eq!(std::fs::read_to_string(&s.large).unwrap(), s.original);
                    assert_eq!(std::fs::read_to_string(&s.tiny).unwrap(), s.small);
                    shot = Some("10-original-bytes-restored-through-history");
                }
            }
            36 => {
                assert!(s.typed_waiting);
                assert!(self.build_task.is_none() && self.run_task.is_none());
                std::fs::write(s.dir.join("save-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_ASYNC_SAVE_OK","source_lines":self.doc.rope().len_lines(),"screenshots":s.shots,"input_frames":s.frames,"typed_while_real_os_lock_waiting":s.typed_waiting,"saved_snapshot_kept_newer_edits":true,"undo_redo_baseline_preserved":true,"parked_baselines_merged":true,"external_write_rejected":true,"suppressed_watcher_event_replayed":true,"explicit_keep_mine_retry":true,"partial_stop_acknowledged_committed_files":true,"build_task_started":self.build_task.is_some(),"run_task_started":self.run_task.is_some(),"original_bytes_restored":true,"renderer":"wgpu","release":!cfg!(debug_assertions),"input":"native RawInput CtrlS/Build/Undo/Redo/Paste + actual Save latest/Stop remaining/Keep mine/tab/explorer buttons; independent OS flock holder/external writer"})).unwrap()).unwrap();
                eprintln!(
                    "GPU_ASYNC_SAVE_OK: real100k file/independent OS lock holder; current editor editable; snapshot vs newer edits/Undo/Redo; external write rejected+watcher replay+Keep mine; partial Stop acknowledges committed files and prevents Build; exact original bytes restored; 10 GPU frames"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            if s.step != 15 {
                s.next = Instant::now() + Duration::from_millis(140);
            }
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.save_smoke = Some(s);
    }
    pub(super) fn save_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.save_smoke else {
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
