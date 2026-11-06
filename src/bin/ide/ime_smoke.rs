//! Native ordered IME/focus tape. Actual editor, Find and independent PTYs.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
use wexandroidstudio::terminal::State;
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    path: PathBuf,
    hash: blake3::Hash,
    pids: Vec<u32>,
    bytes: u64,
    query: String,
    other_hash: Option<blake3::Hash>,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            path: app.doc.path.clone().unwrap(),
            hash: app.doc.content_hash(),
            pids: vec![],
            bytes: 0,
            query: String::new(),
            other_hash: None,
        }
    }
}
fn key(i: &mut egui::RawInput, key: Key, m: Modifiers) {
    for pressed in [true, false] {
        i.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: m,
        });
    }
}
fn ime(i: &mut egui::RawInput, e: egui::ImeEvent) {
    i.events.push(Event::Ime(e));
}
fn undo(i: &mut egui::RawInput) {
    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
}
fn line(f: &wexandroidstudio::terminal::Frame, s: &str) -> bool {
    (0..f.rows).any(|r| f.selected((r, 0), (r, f.cols)) == s)
}
fn press(i: &mut egui::RawInput, p: Pos2) {
    i.events.extend([
        Event::PointerMoved(p),
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        },
    ]);
}
fn enter(i: &mut egui::RawInput) {
    key(i, Key::Enter, Modifiers::NONE);
}
impl Workbench {
    pub(super) fn ime_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.ime_smoke.take() else {
            return;
        };
        input.focused = true;
        input.modifiers = Default::default();
        for v in input.viewports.values_mut() {
            v.focused = Some(true);
        }
        input
            .events
            .retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(300),
            "native IME scope timeout step{}: status{} query{:?} error{:?}",
            s.step,
            self.status,
            self.editor_ui.find.options.query,
            self.terminal.error
        );
        if let Some((p, t)) = s.release.take() {
            input.events.push(Event::PointerMoved(p));
            if Instant::now() < t {
                s.release = Some((p, t));
            } else {
                input.events.push(Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.ime_smoke = Some(s);
            return;
        }
        if let Some((name, n)) = s.click.take() {
            if let Some(r) = self.chrome.hits.get(&name) {
                let p = r.center();
                input.events.push(Event::PointerMoved(p));
                if n < 3 {
                    s.click = Some((name, n + 1));
                } else {
                    input.events.push(Event::PointerButton {
                        pos: p,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    });
                    s.release = Some((p, Instant::now() + Duration::from_millis(80)));
                }
            } else {
                s.click = Some((name, n));
            }
            self.ime_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.ime_smoke = Some(s);
            return;
        }
        let f = self
            .terminal
            .sessions
            .get(self.terminal.active)
            .map(|s| s.frame.clone());
        let mut shot = None;
        let mut wait = false;
        match s.step {
            0 => {
                self.editor_ui.focus_editor = true;
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Preedit("候補".into()));
            }
            1 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert_eq!(self.edit.composition.as_ref().unwrap().text, "候補");
                shot = Some("01-source-preedit-immutable");
            }
            2 => {
                key(input, Key::F12, Modifiers::ALT);
                ime(input, egui::ImeEvent::Commit("候補".into()));
                input
                    .events
                    .push(Event::Text("printf 'TERMINAL_FRESH\\n'".into()));
                enter(input);
            }
            3 => {
                if let Some(f) = &f
                    && matches!(f.state, State::Running)
                    && line(f, "TERMINAL_FRESH")
                {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!f.text().contains("候補"));
                    s.pids.push(f.pid.unwrap());
                    std::fs::write(
                        s.dir.join("terminal-pids.json"),
                        serde_json::to_vec(&s.pids).unwrap(),
                    )
                    .unwrap();
                    shot = Some("02-late-source-commit-rejected-terminal-text-kept");
                } else {
                    wait = true;
                }
            }
            4 => {
                s.bytes = f.as_ref().unwrap().input_bytes;
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Preedit("別候補".into()));
            }
            5 => {
                assert_eq!(f.as_ref().unwrap().input_bytes, s.bytes);
                assert_eq!(self.terminal.preedit.as_deref(), Some("別候補"));
                shot = Some("03-terminal-preedit-no-write");
            }
            6 => {
                key(input, Key::F12, Modifiers::ALT);
                ime(input, egui::ImeEvent::Commit("別候補".into()));
                input.events.push(Event::Text("// fresh source\n".into()));
            }
            7 => {
                assert!(self.doc.rope().to_string().starts_with("// fresh source\n"));
                assert!(!self.chrome.dock_open);
                assert_eq!(f.as_ref().unwrap().input_bytes, s.bytes);
                shot = Some("04-close-and-source-text-one-frame");
            }
            8 => undo(input),
            9 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Preedit("に".into()));
            }
            10 => {
                ime(input, egui::ImeEvent::Commit("// 日本\n".into()));
                ime(input, egui::ImeEvent::Disabled);
                input.events.push(Event::Text("// ascii\n".into()));
            }
            11 => {
                assert!(
                    self.doc
                        .rope()
                        .to_string()
                        .starts_with("// 日本\n// ascii\n")
                );
                assert!(self.edit.composition.is_none());
                shot = Some("05-fresh-source-ime-and-post-disabled-text");
            }
            12 => {
                undo(input);
                undo(input);
            }
            13 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                key(input, Key::F, Modifiers::CTRL | Modifiers::COMMAND);
                input.events.push(Event::Text("base".into()));
            }
            14 => {
                if self.editor_ui.find.options.query == "base"
                    && ctx.memory(|m| m.has_focus(egui::Id::new("find-query")))
                {
                    ime(input, egui::ImeEvent::Enabled);
                    ime(input, egui::ImeEvent::Preedit("query候補".into()));
                } else {
                    wait = true;
                }
            }
            15 => {
                if self.editor_ui.find.options.query.contains("query候補") {
                    s.query = self.editor_ui.find.options.query.clone();
                    let Some((p, g)) = self.editor_ui.geometry.rows.get(&0) else {
                        self.ime_smoke = Some(s);
                        return;
                    };
                    let p = *p
                        + egui::vec2(
                            g.pos_from_cursor(egui::text::CCursor::new(0)).left() + 0.1,
                            11.5,
                        );
                    std::fs::write(s.dir.join("handoff-point.json"),serde_json::to_vec_pretty(&json!({"point":[p.x, p.y],"viewport_min":[self.editor_ui.geometry.viewport.min.x, self.editor_ui.geometry.viewport.min.y],"viewport_max":[self.editor_ui.geometry.viewport.max.x, self.editor_ui.geometry.viewport.max.y],"inside":self.editor_ui.geometry.viewport.contains(p),"layer_at":format!("{:?}",ctx.layer_id_at(p)),"source_layer":format!("{:?}",self.editor_ui.pointer_layer),"cursor":self.edit.cursor,"scroll_x":self.edit.scroll_x,"scroll_y":self.edit.scroll_y})).unwrap()).unwrap();
                    press(input, p);
                    ime(input, egui::ImeEvent::Commit("OLD_QUERY".into()));
                    input.events.push(Event::Text("// from find\n".into()));
                } else {
                    wait = true;
                }
            }
            16 => {
                if !self.doc.rope().to_string().starts_with("// from find\n") {
                    std::fs::write(s.dir.join("handoff-failure.json"),serde_json::to_vec_pretty(&json!({"prefix":self.doc.rope().slice(..self.doc.rope().len_chars().min(160)).to_string(),"cursor":self.edit.cursor,"anchor":self.edit.anchor,"composition":self.edit.composition.as_ref().map(|c|c.text.clone()),"find_query":self.editor_ui.find.options.query,"editor_focus":self.editor_ui.focus_editor,"terminal_focus":self.terminal.focus,"focused_id":format!("{:?}",ctx.memory(|m|m.focused())),"metrics":self.ime.metrics})).unwrap()).unwrap();
                }

                assert!(self.doc.rope().to_string().starts_with("// from find\n"));
                assert!(!self.doc.rope().to_string().contains("OLD_QUERY"));
                assert_eq!(self.editor_ui.find.options.query, s.query);
                shot = Some("06-find-handoff-no-ghost-source-commit");
            }
            17 => {
                undo(input);
                key(input, Key::Escape, Modifiers::NONE);
            }
            18 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Preedit("doc候補".into()));
            }
            19 => {
                s.click = Some(("file-ZImeOther.kt".into(), 0));
            }
            20 => {
                if self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("ZImeOther.kt"))
                    && self.open_ui.pending.is_none()
                {
                    s.other_hash = Some(self.doc.content_hash());
                    ime(input, egui::ImeEvent::Commit("OLD_DOC".into()));
                    input.events.push(Event::Text("// other fresh\n".into()));
                } else {
                    wait = true;
                }
            }
            21 => {
                assert!(self.doc.rope().to_string().starts_with("// other fresh\n"));
                assert!(!self.doc.rope().to_string().contains("OLD_DOC"));
                shot = Some("07-document-epoch-handoff");
            }
            22 => {
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Commit("// other commit\n".into()));
                ime(input, egui::ImeEvent::Disabled);
            }
            23 => {
                assert!(self.doc.rope().to_string().contains("// other commit\n"));
                undo(input);
                undo(input);
            }
            24 => {
                assert_eq!(self.doc.content_hash(), s.other_hash.unwrap());
                assert!(!self.doc.dirty());
                s.click = Some((
                    format!("tab-{}", s.path.file_name().unwrap().to_str().unwrap()),
                    0,
                ));
            }
            25 => {
                if self.doc.path.as_ref() == Some(&s.path) && self.open_ui.pending.is_none() {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    key(input, Key::F12, Modifiers::ALT);
                } else {
                    wait = true;
                }
            }
            26 => {
                if self.tab == 5
                    && self.chrome.dock_open
                    && f.as_ref().is_some_and(|f| f.text().contains("wex$"))
                {
                    s.bytes = f.as_ref().unwrap().input_bytes;
                    ime(input, egui::ImeEvent::Enabled);
                    ime(input, egui::ImeEvent::Preedit("session候補".into()));
                } else {
                    wait = true;
                }
            }
            27 => {
                s.click = Some(("terminal-new".into(), 0));
            }
            28 => {
                if self.terminal.sessions.len() == 2
                    && let Some(f) = &f
                    && matches!(f.state, State::Running)
                    && f.text().contains("wex$")
                {
                    s.pids.push(f.pid.unwrap());
                    std::fs::write(
                        s.dir.join("terminal-pids.json"),
                        serde_json::to_vec(&s.pids).unwrap(),
                    )
                    .unwrap();
                    ime(input, egui::ImeEvent::Commit("OLD_SESSION".into()));
                    input
                        .events
                        .push(Event::Text("printf 'SESSION_FRESH\\n'".into()));
                    enter(input);
                } else {
                    wait = true;
                }
            }
            29 => {
                if f.as_ref().is_some_and(|f| line(f, "SESSION_FRESH")) {
                    assert_eq!(self.terminal.sessions[0].frame.input_bytes, s.bytes);
                    assert!(!f.as_ref().unwrap().text().contains("OLD_SESSION"));
                    shot = Some("08-new-session-does-not-inherit-commit");
                } else {
                    wait = true;
                }
            }
            30 => {
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Commit("printf 'IME_LIVE\\n'".into()));
                ime(input, egui::ImeEvent::Disabled);
                enter(input);
            }
            31 => {
                if f.as_ref().is_some_and(|f| line(f, "IME_LIVE")) {
                    shot = Some("09-fresh-terminal-ime-and-post-disabled-enter");
                } else {
                    wait = true;
                }
            }
            32 => {
                ime(input, egui::ImeEvent::Enabled);
                ime(input, egui::ImeEvent::Preedit("blur候補".into()));
            }
            33 => {
                assert_eq!(self.terminal.preedit.as_deref(), Some("blur候補"));
                input.focused = false;
                for v in input.viewports.values_mut() {
                    v.focused = Some(false);
                }
            }
            34 => {
                assert!(self.terminal.preedit.is_none());
                ime(input, egui::ImeEvent::Commit("OLD_BLUR".into()));
                input
                    .events
                    .push(Event::Text("printf 'BLUR_FRESH\\n'".into()));
                enter(input);
            }
            35 => {
                if f.as_ref().is_some_and(|f| line(f, "BLUR_FRESH")) {
                    assert!(!f.as_ref().unwrap().text().contains("OLD_BLUR"));
                    shot = Some("10-window-focus-return-no-stale-commit");
                } else {
                    wait = true;
                }
            }
            36 => {
                s.click = Some(("terminal-close".into(), 0));
            }
            37 => {
                if self.terminal.sessions.len() == 1 && self.terminal.retiring.is_empty() {
                    s.click = Some(("terminal-close".into(), 0));
                } else {
                    wait = true;
                }
            }
            38 => {
                if self.terminal.sessions.is_empty()
                    && self.terminal.retiring.is_empty()
                    && wexandroidstudio::terminal::live_workers() == 0
                {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!self.doc.dirty());
                    assert!(!self.save_ui.saver.worker_started());
                    assert!(self.last_build.is_none() && !self.running);
                    assert!(self.ime.metrics.rejected >= 6 && self.ime.metrics.cancelled >= 6);
                    shot = Some("11-originals-restored-sessions-reaped");
                } else {
                    wait = true;
                }
            }
            39 => {
                assert_eq!(s.shots.len(), 11);
                for pid in &s.pids {
                    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
                }
                std::fs::write(s.dir.join("ime-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_IME_SCOPE_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"metrics":self.ime.metrics,"source_path":s.path,"source_hash":s.hash.to_hex().to_string(),"source_and_undo_restored":true,"source_preedit_ephemeral":true,"terminal_preedit_no_write":true,"source_terminal_late_commits_rejected":true,"close_then_source_text_same_frame":true,"fresh_source_commit_and_disabled_text":true,"find_source_handoff":true,"document_epoch_fenced":true,"session_id_fenced":true,"fresh_terminal_commit_and_disabled_enter":true,"window_focus_fenced":true,"terminal_pids":s.pids,"all_sessions_reaped":true,"save_worker_started":self.save_ui.saver.worker_started(),"deployment_invoked":false,"screenshots":s.shots,"input":"actual native winit RawInput IME/keyboard/pointer; actual Bash PTYs; not a system IME-engine automation"})).unwrap()).unwrap();
                eprintln!(
                    "GPU_IME_SCOPE_OK: actual editor/Find/PTYs, ephemeral preedit/late-commit ownership/new text/AltF12/document/session/window epochs/fresh commit after cancellation/post-Disabled input/Undo/source restoration;11 genuine GPU frames;no Gradle/APK/phone"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(140);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.ime_smoke = Some(s);
    }
    pub(super) fn ime_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.ime_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for event in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = event {
                    let p = s.dir.join(format!("{name}.png"));
                    let bytes = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        &p,
                        &bytes,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"metrics":self.ime.metrics,"pty":self.terminal.sessions.get(self.terminal.active).map(|s|json!({"pid":s.frame.pid,"input_bytes":s.frame.input_bytes,"output_bytes":s.frame.output_bytes,"generation":s.frame.generation}))}));
                    s.awaiting = None;
                }
            }
        }
    }
}
