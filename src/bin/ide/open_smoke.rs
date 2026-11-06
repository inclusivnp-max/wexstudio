//! Real explorer clicks while the single document worker parses a large source.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Instant};
pub(super) struct Smoke {
    pub dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    paste: Option<String>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    last: String,
    host: PathBuf,
    original: String,
    dirty: String,
    during: String,
    large_ticket: Option<wexandroidstudio::document_jobs::Ticket>,
    superseded: Option<wexandroidstudio::document_jobs::Ticket>,
    frames: usize,
    typing_proved: bool,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        let host = app.root.join("Host.kt");
        assert_eq!(app.doc.path.as_ref(), Some(&host));
        let original = std::fs::read_to_string(&host).unwrap();
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            pending: None,
            release: None,
            paste: None,
            awaiting: None,
            shots: vec![],
            last: String::new(),
            host,
            dirty: format!("{original}// unsaved before opening\n"),
            during: format!("{original}// typed while file parser is busy\n"),
            original,
            large_ticket: None,
            superseded: None,
            frames: 0,
            typing_proved: false,
        }
    }
    fn click(&mut self, key: &str) {
        self.pending = Some((key.into(), 0));
    }
    fn fill_editor(&mut self, text: String) {
        self.click("editor");
        self.paste = Some(text);
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
    pub(super) fn open_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.open_smoke.take() else {
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
        let state = format!(
            "step{} click={:?} pending={:?} error={:?} active={:?} worker={}",
            s.step,
            s.pending.as_ref().map(|p| &p.0),
            self.open_ui.pending.as_ref().map(|p| &p.path),
            self.open_ui.error,
            self.doc.path,
            self.open_ui.opener.active_id()
        );
        if state != s.last {
            eprintln!("OPEN_STATE {state}");
            s.last = state;
        }
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "async open smoke timeout {}",
            s.last
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
            self.open_smoke = Some(s);
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
            self.open_smoke = Some(s);
            return;
        }
        if let Some(text) = s.paste.take() {
            key(input, Key::A, true);
            input.events.push(egui::Event::Paste(text));
            s.next = Instant::now() + Duration::from_millis(130);
            self.open_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.open_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        let mut finish = false;
        match s.step {
            0 => s.fill_editor(s.dirty.clone()),
            1 => {
                assert_eq!(self.doc.rope().to_string(), s.dirty);
                s.click("file-Large.kt");
            }
            2 => {
                if let Some(p) = &self.open_ui.pending
                    && self.open_ui.opener.active_id() == p.ticket.id
                {
                    s.large_ticket = Some(p.ticket.clone());
                    assert_eq!(self.doc.path.as_ref(), Some(&s.host));
                    shot = Some("01-opening-editable-host");
                } else {
                    wait = true;
                }
            }
            3 => {
                assert!(self.open_ui.pending.is_some());
                s.fill_editor(s.during.clone());
            }
            4 => {
                assert_eq!(self.doc.rope().to_string(), s.during);
                assert!(self.open_ui.pending.is_some());
                assert!(
                    !s.large_ticket
                        .as_ref()
                        .unwrap()
                        .done
                        .load(Ordering::Acquire)
                );
                s.typing_proved = true;
                shot = Some("02-typed-during-parse");
                s.click("open-cancel");
            }
            5 => {
                if self.open_ui.pending.is_some()
                    || !s
                        .large_ticket
                        .as_ref()
                        .unwrap()
                        .done
                        .load(Ordering::Acquire)
                {
                    wait = true;
                } else {
                    assert_eq!(self.doc.path.as_ref(), Some(&s.host));
                    assert_eq!(self.doc.rope().to_string(), s.during);
                    assert_eq!(self.tabs.len(), 1);
                    shot = Some("03-cancel-preserved-buffer");
                }
            }
            6 => s.click("file-Large.kt"),
            7 => {
                if let Some(p) = &self.open_ui.pending
                    && self.open_ui.opener.active_id() == p.ticket.id
                {
                    s.superseded = Some(p.ticket.clone());
                    s.click("file-Tiny.kt");
                } else {
                    wait = true;
                }
            }
            8 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Tiny.kt"))
                {
                    wait = true;
                } else {
                    assert!(s.superseded.as_ref().unwrap().done.load(Ordering::Acquire));
                    assert_eq!(self.parked[&s.host].0.rope().to_string(), s.during);
                    assert!(!self.tabs.iter().any(|p| p.ends_with("Large.kt")));
                    shot = Some("04-latest-click-wins");
                    s.click("tab-Host.kt");
                }
            }
            9 => {
                assert_eq!(self.doc.path.as_ref(), Some(&s.host));
                assert_eq!(self.doc.rope().to_string(), s.during);
                self.editor_ui.focus_editor = true;
                key(input, Key::Z, true);
            }
            10 => {
                assert_eq!(self.doc.rope().to_string(), s.dirty);
                key(input, Key::Z, true);
            }
            11 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                assert!(!self.doc.dirty());
                s.click("file-Invalid.kt");
            }
            12 => {
                if self.open_ui.error.is_none() {
                    wait = true;
                } else {
                    assert_eq!(self.doc.path.as_ref(), Some(&s.host));
                    assert_eq!(self.doc.rope().to_string(), s.original);
                    shot = Some("05-read-error-last-good");
                }
            }
            13 => {
                if !s.dir.join("repair-ready").is_file() {
                    wait = true;
                } else {
                    s.click("open-retry");
                }
            }
            14 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Invalid.kt"))
                {
                    wait = true;
                } else {
                    assert!(self.doc.rope().to_string().contains("val repaired = 7"));
                    shot = Some("06-retry-repaired-file");
                    s.click("file-Medium.kt");
                }
            }
            15 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Medium.kt"))
                {
                    wait = true;
                } else {
                    assert!(self.doc.rope().len_lines() > 60000);
                    self.editor_ui.focus_editor = true;
                    key(input, Key::End, true);
                }
            }
            16 => {
                assert_eq!(self.edit.cursor, self.doc.rope().len_chars());
                assert!(self.editor_ui.rendered_rows < 200);
                shot = Some("07-large-file-ready");
            }
            17 => finish = true,
            _ => unreachable!(),
        }
        if wait {
            self.open_smoke = Some(s);
            return;
        }
        if finish {
            assert!(s.typing_proved);
            assert_eq!(std::fs::read_to_string(&s.host).unwrap(), s.original);
            std::fs::write(s.dir.join("open-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_ASYNC_FILE_OPEN_OK","root":self.root,"typed_while_real_parser_active":true,"cancel_acknowledged":s.large_ticket.as_ref().unwrap().done.load(Ordering::Acquire),"latest_wins_acknowledged":s.superseded.as_ref().unwrap().done.load(Ordering::Acquire),"dirty_parked_preserved":true,"two_undo_steps_restored":true,"disk_host_unchanged":true,"error_retained_last_good":true,"retry_loaded_actual_repaired_file":true,"document":self.doc.path,"lines":self.doc.rope().len_lines(),"rendered_rows":self.editor_ui.rendered_rows,"screenshots":s.shots,"input_frames":s.frames,"input":"actual native RawInput explorer/tab/Cancel/Retry/text/Undo/Ctrl+End","renderer":"wgpu","release":!cfg!(debug_assertions)})).unwrap()).unwrap();
            eprintln!(
                "GPU_ASYNC_FILE_OPEN_OK: real500k-line background parse; active tab edited through native input; Cancel acknowledged; later explorer click wins; dirty parked buffer and two Undo steps preserved; UTF8 error/repair/Retry; actual60k-line file+CtrlEnd/virtualized; disk host unchanged"
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(130);
        self.open_smoke = Some(s);
    }
    pub(super) fn open_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.open_smoke else {
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
                    if name == "05-read-error-last-good" {
                        std::fs::write(
                            s.dir.join("error-visible"),
                            "Actual error frame captured\n",
                        )
                        .unwrap();
                    }
                    s.awaiting = None;
                }
            }
        }
    }
}
