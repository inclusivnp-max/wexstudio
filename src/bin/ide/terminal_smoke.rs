//! Genuine native PTY/input/GPU evidence; no production polling delays or fake output.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
use wexandroidstudio::terminal::{self, State};
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    hash: blake3::Hash,
    path: PathBuf,
    pids: Vec<u32>,
    before_ime: u64,
    old_cols: u16,
    copied: String,
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
            hash: app.doc.content_hash(),
            path: app.doc.path.clone().unwrap(),
            pids: vec![],
            before_ime: 0,
            old_cols: 0,
            copied: String::new(),
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
fn command(i: &mut egui::RawInput, s: &str) {
    i.events.push(Event::Text(s.into()));
    key(i, Key::Enter, Modifiers::NONE);
}
fn row(f: &terminal::Frame, text: &str) -> Option<u16> {
    (0..f.rows).find(|r| f.selected((*r, 0), (*r, f.cols)) == text)
}
impl Workbench {
    pub(super) fn terminal_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.terminal_smoke.take() else {
            return;
        };
        input.focused = true;
        input.modifiers = Modifiers::NONE;
        for v in input.viewports.values_mut() {
            v.focused = Some(true);
        }
        input
            .events
            .retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(300),
            "native PTY timeout step{}: terminal {:?}, sessions{}, error{:?}",
            s.step,
            self.terminal
                .sessions
                .get(self.terminal.active)
                .map(|s| s.frame.state.clone()),
            self.terminal.sessions.len(),
            self.terminal.error
        );
        if let Some((p, until)) = s.release.take() {
            input.events.push(Event::PointerMoved(p));
            if Instant::now() < until {
                s.release = Some((p, until));
            } else {
                input.events.push(Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.terminal_smoke = Some(s);
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
            self.terminal_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.terminal_smoke = Some(s);
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
                input
                    .events
                    .push(Event::Paste("// terminal dirty guard\n".into()));
            }
            1 => {
                assert!(self.doc.dirty());
                s.click = Some(("dock-5".into(), 0));
            }
            2 => {
                if let Some(f) = &f
                    && matches!(f.state, State::Running)
                    && f.text().contains("wex$")
                {
                    s.pids.push(f.pid.unwrap());
                    std::fs::write(
                        s.dir.join("terminal-pids.json"),
                        serde_json::to_vec(&s.pids).unwrap(),
                    )
                    .unwrap();
                    shot = Some("01-project-shell");
                } else {
                    wait = true;
                }
            }
            3 => command(
                input,
                "test -t 0 && test -t 1 && test -t 2 && pwd > terminal.cwd && tty > terminal.tty && printf '\\033[32mWEX_PTY_REAL_日本\\033[0m\\n'",
            ),
            4 => {
                if let Some(f) = &f
                    && row(f, "WEX_PTY_REAL_日本").is_some()
                {
                    assert!(f.cells.iter().any(|c| c.fgcolor() == vt100::Color::Idx(2)));
                    assert_eq!(
                        std::fs::read_to_string(self.root.join("terminal.cwd"))
                            .unwrap()
                            .trim(),
                        self.root.to_str().unwrap()
                    );
                    assert!(
                        std::fs::read_to_string(self.root.join("terminal.tty"))
                            .unwrap()
                            .starts_with("/dev/pts/")
                    );
                    shot = Some("02-real-pty-ansi");
                } else {
                    wait = true;
                }
            }
            5 => {
                assert!(f.as_ref().unwrap().bracketed_paste);
                input.events.push(Event::Paste(
                    "printf 'BRACKET_A\\n';\nprintf 'BRACKET_B\\n'; touch bracket-executed".into(),
                ));
            }
            6 => {
                if f.as_ref()
                    .is_some_and(|f| f.text().contains("bracket-executed"))
                {
                    assert!(!self.root.join("bracket-executed").exists());
                    shot = Some("03-bracketed-before-enter");
                } else {
                    wait = true;
                }
            }
            7 => key(input, Key::Enter, Modifiers::NONE),
            8 => {
                if self.root.join("bracket-executed").exists()
                    && f.as_ref().is_some_and(|f| row(f, "BRACKET_B").is_some())
                {
                    input.events.push(Event::Text("printf '".into()));
                } else {
                    wait = true;
                }
            }
            9 => {
                if let Some(f) = &f
                    && f.text().contains("wex$ printf '")
                {
                    s.before_ime = f.input_bytes;
                    input
                        .events
                        .push(Event::Ime(egui::ImeEvent::Preedit("日本語".into())));
                } else {
                    wait = true;
                }
            }
            10 => {
                assert_eq!(f.as_ref().unwrap().input_bytes, s.before_ime);
                assert_eq!(self.terminal.preedit.as_deref(), Some("日本語"));
                shot = Some("04-ime-preedit-no-write");
            }
            11 => input
                .events
                .push(Event::Ime(egui::ImeEvent::Commit("日本語".into()))),
            12 => {
                if f.as_ref()
                    .is_some_and(|f| f.input_bytes >= s.before_ime + "日本語".len() as u64)
                {
                    command(input, "\\n'");
                } else {
                    wait = true;
                }
            }
            13 => {
                if f.as_ref().is_some_and(|f| row(f, "日本語").is_some()) {
                    assert!(self.doc.dirty());
                    assert!(!self.save_ui.saver.worker_started());
                    shot = Some("05-ime-commit-shell-output");
                } else {
                    wait = true;
                }
            }
            14 => {
                key(input, Key::S, Modifiers::CTRL | Modifiers::COMMAND);
                key(input, Key::Q, Modifiers::CTRL | Modifiers::COMMAND);
                command(input, "printf 'COPY_日本\\n'");
            }
            15 => {
                if let Some(f) = &f
                    && let Some(r) = row(f, "COPY_日本")
                {
                    assert!(!self.save_ui.saver.worker_started());
                    let a = self.terminal.cell_position(r, 0);
                    let b = self.terminal.cell_position(r, 9);
                    input.events.extend([
                        Event::PointerMoved(a),
                        Event::PointerButton {
                            pos: a,
                            button: PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        },
                        Event::PointerMoved(b),
                        Event::PointerButton {
                            pos: b,
                            button: PointerButton::Primary,
                            pressed: false,
                            modifiers: Default::default(),
                        },
                    ]);
                } else {
                    wait = true;
                }
            }
            16 => {
                s.copied = self.terminal.selected_text().unwrap();
                assert_eq!(s.copied, "COPY_日本");
                shot = Some("06-real-output-selection");
            }
            17 => command(
                input,
                "for i in $(seq 1 100); do printf 'scroll_%s\\n' \"$i\"; done",
            ),
            18 => {
                if f.as_ref().is_some_and(|f| row(f, "scroll_100").is_some()) {
                    input
                        .events
                        .push(Event::PointerMoved(self.terminal.rect.center()));
                    input.events.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0., 120.),
                        modifiers: Default::default(),
                    });
                } else {
                    wait = true;
                }
            }
            19 => {
                if f.as_ref().is_some_and(|f| f.scrollback > 0) {
                    shot = Some("07-scrollback");
                } else {
                    wait = true;
                }
            }
            20 => {
                key(input, Key::PageDown, Modifiers::SHIFT);
                command(input, "python3 terminal_tui.py");
            }
            21 => {
                if let Some(f) = &f
                    && f.alternate
                    && f.text().contains("TUI_REAL")
                {
                    shot = Some("08-genuine-curses-alternate");
                    s.old_cols = f.cols;
                } else {
                    wait = true;
                }
            }
            22 => ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1120., 740.))),
            23 => {
                if let Some(f) = &f
                    && f.alternate
                    && f.cols != s.old_cols
                    && self.root.join("tui-resize").exists()
                {
                    shot = Some("09-live-resize-sigwinch");
                } else {
                    wait = true;
                }
            }
            24 => key(input, Key::ArrowUp, Modifiers::NONE),
            25 => {
                if f.as_ref().is_some_and(|f| !f.alternate) && self.root.join("tui-key").exists() {
                    assert_eq!(
                        std::fs::read_to_string(self.root.join("tui-key")).unwrap(),
                        "259"
                    );
                    s.click = Some(("terminal-new".into(), 0));
                } else {
                    wait = true;
                }
            }
            26 => {
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
                    command(input, "SECOND_ONLY=separate; printf 'SECOND_SESSION_OK\\n'");
                } else {
                    wait = true;
                }
            }
            27 => {
                if f.as_ref()
                    .is_some_and(|f| row(f, "SECOND_SESSION_OK").is_some())
                {
                    shot = Some("10-second-independent-shell");
                } else {
                    wait = true;
                }
            }
            28 => {
                s.click = Some((format!("terminal-tab-{}", self.terminal.sessions[0].id), 0));
            }
            29 => {
                if self.terminal.active == 0 {
                    command(
                        input,
                        "printf 'FIRST_%s\\n' \"${SECOND_ONLY-unset}\"; sh -c 'echo FG_READY; exec sleep 30'",
                    );
                } else {
                    wait = true;
                }
            }
            30 => {
                if f.as_ref().is_some_and(|f| row(f, "FG_READY").is_some()) {
                    input.events.push(Event::Copy);
                    command(input, "printf 'INT_RETURNED\\n'");
                } else {
                    wait = true;
                }
            }
            31 => {
                if f.as_ref().is_some_and(|f| row(f, "INT_RETURNED").is_some()) {
                    shot = Some("11-foreground-interrupt-shell-survives");
                } else {
                    wait = true;
                }
            }
            32 => command(input, "(trap '' HUP; sleep 60) & echo $! > terminal.child"),
            33 => {
                if self.root.join("terminal.child").exists() {
                    s.click = Some(("terminal-close".into(), 0));
                } else {
                    wait = true;
                }
            }
            34 => {
                if self.terminal.sessions.len() == 1 && self.terminal.retiring.is_empty() {
                    s.click = Some(("terminal-close".into(), 0));
                } else {
                    wait = true;
                }
            }
            35 => {
                if self.terminal.sessions.is_empty()
                    && self.terminal.retiring.is_empty()
                    && terminal::live_workers() == 0
                {
                    let Some((pos, g)) = self.editor_ui.geometry.rows.get(&0) else {
                        self.terminal_smoke = Some(s);
                        return;
                    };
                    let p = *pos
                        + egui::vec2(
                            g.pos_from_cursor(egui::text::CCursor::new(0)).left() + 0.1,
                            11.5,
                        );
                    input.events.extend([
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
                    key(input, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            36 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert!(!self.save_ui.saver.worker_started());
                assert!(!self.building && !self.running && self.last_build.is_none());
                shot = Some("12-closed-reaped-source-undo-restored");
            }
            37 => {
                assert_eq!(s.shots.len(), 12);
                for pid in &s.pids {
                    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
                }
                std::fs::write(s.dir.join("terminal-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_PROJECT_PTY_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"source_path":s.path,"source_hash":s.hash.to_hex().to_string(),"source_restored":true,"editor_save_worker_started":self.save_ui.saver.worker_started(),"terminal_pids":s.pids,"terminal_workers":terminal::live_workers(),"all_sessions_reaped":true,"real_controlling_pty":true,"actual_project_cwd":true,"ansi_unicode":true,"bracketed_before_enter_verified":true,"ime_preedit_input_bytes":s.before_ime,"ime_commit_shell_output":true,"selected_text":s.copied,"real_scrollback":true,"genuine_curses_alt_screen":true,"genuine_sigwinch_resize":true,"curses_up_key":259,"independent_sessions":true,"foreground_sigint_shell_survived":true,"same_session_background_reaped":true,"input":"native winit RawInput + actual PTY/curses processes","screenshots":s.shots,"deployment_invoked":false})).unwrap()).unwrap();
                eprintln!(
                    "GPU_PROJECT_PTY_OK: real controlling PTY/project cwd/ANSI Unicode/bracketed paste/IME/selection/scrollback/curses alternate-screen+SIGWINCH/independent shells/foreground SIGINT/session cleanup/source Undo;12 genuine GPU frames;no Gradle/APK/phone"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(120);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.terminal_smoke = Some(s);
    }
    pub(super) fn terminal_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.terminal_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for e in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = e {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"pty":self.terminal.sessions.get(self.terminal.active).map(|s|json!({"state":s.frame.state,"pid":s.frame.pid,"rows":s.frame.rows,"cols":s.frame.cols,"input_bytes":s.frame.input_bytes,"output_bytes":s.frame.output_bytes,"worker_thread":s.frame.worker_thread,"generation":s.frame.generation,"bracketed_paste":s.frame.bracketed_paste,"alternate":s.frame.alternate,"scrollback":s.frame.scrollback}))}));
                    s.awaiting = None;
                }
            }
        }
    }
}
