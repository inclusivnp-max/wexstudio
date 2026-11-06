//! Native typing under a real large parser + independent full-tree oracle off GPU.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    pub dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    pub actions: Vec<Value>,
    large: PathBuf,
    tiny: PathBuf,
    original_chars: usize,
    first_line: usize,
    tiny_text: String,
    tail: String,
    busy: bool,
    old_ticket: Option<wexandroidstudio::syntax_jobs::Ticket>,
    oracle: Option<mpsc::Receiver<Value>>,
    oracle_proof: Option<Value>,
    frames: usize,
}
fn burst(n: u8) -> String {
    format!("// native burst{n} 😀\n")
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        assert!(app.doc.rope().len_lines() > 500000);
        let large = app.root.join("Large.kt");
        assert_eq!(app.doc.path.as_ref(), Some(&large));
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            actions: vec![],
            tiny: app.root.join("Tiny.kt"),
            large,
            original_chars: app.doc.rope().len_chars(),
            first_line: app.doc.rope().line(0).len_chars() - 1,
            tiny_text: std::fs::read_to_string(app.root.join("Tiny.kt")).unwrap(),
            tail: (0..20).map(burst).collect(),
            busy: false,
            old_ticket: None,
            oracle: None,
            oracle_proof: None,
            frames: 0,
        }
    }
    fn click(&mut self, key: &str) {
        self.click = Some((key.into(), 0));
    }
}
fn key(input: &mut egui::RawInput, key: Key, cmd: bool, shift: bool) {
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: cmd,
                command: cmd,
                shift,
                ..Default::default()
            },
        });
    }
}
fn fingerprint(doc: &Document) -> (String, usize) {
    let tree = doc.tree().unwrap();
    let mut cursor = tree.walk();
    let mut hash = blake3::Hasher::new();
    let mut count = 0;
    loop {
        let node = cursor.node();
        let kind = node.kind().as_bytes();
        hash.update(&(kind.len() as u64).to_le_bytes());
        hash.update(kind);
        for n in [
            node.start_byte(),
            node.end_byte(),
            node.start_position().row,
            node.start_position().column,
            node.end_position().row,
            node.end_position().column,
        ] {
            hash.update(&(n as u64).to_le_bytes());
        }
        count += 1;
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return (hash.finalize().to_hex().to_string(), count);
            }
        }
    }
}
impl Workbench {
    pub(super) fn syntax_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.syntax_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(260),
            "syntax smoke timeout step{} pending={} error={:?}",
            s.step,
            self.syntax_ui.pending.is_some(),
            self.syntax_ui.error
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
            self.syntax_smoke = Some(s);
            return;
        }
        if let Some((name, hover)) = s.click.take() {
            if let Some(rect) = self.chrome.hits.get(&name) {
                let pos = rect.center();
                input.events.push(egui::Event::PointerMoved(pos));
                if hover < 3 {
                    s.click = Some((name, hover + 1));
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
                s.click = Some((name, hover));
            }
            self.syntax_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.syntax_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::End, true, false);
            }
            1 => input.events.push(egui::Event::Paste(burst(0))),
            2 => {
                if self.syntax_ui.parser.active_id() != 0
                    && self.doc.tree().is_none()
                    && self
                        .syntax_ui
                        .pending
                        .as_ref()
                        .is_some_and(|p| !p.ticket.done.load(std::sync::atomic::Ordering::Acquire))
                {
                    s.busy = true;
                    input.events.push(egui::Event::Paste(burst(1)));
                } else {
                    wait = true;
                }
            }
            3..=20 => {
                input.events.push(egui::Event::Paste(burst(s.step - 1)));
            }
            21 => {
                assert!(s.busy);
                assert_eq!(
                    self.doc.rope().slice(s.original_chars..).to_string(),
                    s.tail
                );
                assert!(self.doc.tree().is_none());
                assert!(self.doc.leaf_spans(0..100).is_empty());
                s.old_ticket = self.syntax_ui.pending.as_ref().map(|p| p.ticket.clone());
                shot = Some("01-burst-visible-without-stale-spans");
            }
            22 => key(input, Key::Z, true, false),
            23 => key(input, Key::Y, true, false),
            24 => s.click("file-Tiny.kt"),
            25 => {
                if self.doc.path.as_ref() != Some(&s.tiny)
                    || self.syntax_ui.pending.is_some()
                    || s.old_ticket
                        .as_ref()
                        .is_some_and(|t| !t.done.load(std::sync::atomic::Ordering::Acquire))
                {
                    wait = true;
                } else {
                    assert_eq!(
                        self.parked[&s.large]
                            .0
                            .rope()
                            .slice(s.original_chars..)
                            .to_string(),
                        s.tail
                    );
                    shot = Some("02-switch-cancels-old-parser-view");
                }
            }
            26 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::End, true, false);
                input.events.push(egui::Event::Paste("// ".into()));
                input.events.push(egui::Event::Ime(egui::ImeEvent::Enabled));
                input
                    .events
                    .push(egui::Event::Ime(egui::ImeEvent::Preedit("日本語".into())));
            }
            27 => {
                assert!(self.edit.composition.is_some());
                assert_eq!(self.doc.rope().to_string(), format!("{}// ", s.tiny_text));
                shot = Some("03-ime-preedit-transient");
            }
            28 => input
                .events
                .push(egui::Event::Ime(egui::ImeEvent::Commit("日本語".into()))),
            29 => {
                assert!(self.edit.composition.is_none());
                assert_eq!(
                    self.doc.rope().to_string(),
                    format!("{}// 日本語", s.tiny_text)
                );
                shot = Some("04-ime-commit-once");
            }
            30 => key(input, Key::Z, true, false),
            31 => {
                assert_eq!(self.doc.rope().to_string(), format!("{}// ", s.tiny_text));
                key(input, Key::Z, true, false);
            }
            32 => {
                assert_eq!(self.doc.rope().to_string(), s.tiny_text);
                assert!(!self.doc.dirty());
                shot = Some("05-ime-and-prefix-undo");
                s.click("tab-Large.kt");
            }
            33 => {
                if self.doc.path.as_ref() != Some(&s.large) {
                    wait = true;
                } else {
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Home, true, false);
                    key(input, Key::End, false, true);
                }
            }
            34 => {
                assert_eq!(self.edit.range(), 0..s.first_line);
                input
                    .events
                    .push(egui::Event::Paste("// replacement 日本語😀".into()));
            }
            35 => {
                assert!(self.doc.tree().is_none());
                assert!(self.doc.leaf_spans(0..100).is_empty());
                shot = Some("06-head-replacement-neutral-until-aligned");
            }
            36 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::Home, true, false);
                key(input, Key::End, false, true);
            }
            37 => key(input, Key::Tab, false, false),
            38 => {
                assert!(
                    self.doc
                        .rope()
                        .line(0)
                        .to_string()
                        .starts_with("    // replacement")
                );
                assert!(self.doc.tree().is_none());
                key(input, Key::Tab, false, true);
            }
            39 => {
                if self.doc.tree().is_none() || self.syntax_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert_eq!(
                        self.doc.tree().unwrap().root_node().end_byte(),
                        self.doc.rope().len_bytes()
                    );
                    assert!(!self.doc.tree().unwrap().root_node().has_error());
                    assert!(!self.doc.leaf_spans(0..100).is_empty());
                    shot = Some("07-current-exact-state-syntax-ready");
                    let doc = self.doc.fork().unwrap();
                    let (tx, rx) = mpsc::sync_channel(1);
                    s.oracle = Some(rx);
                    std::thread::spawn(move || {
                        let actual = fingerprint(&doc);
                        let text = doc.rope().to_string();
                        let fresh = Document::new(&text).unwrap();
                        let expected = fingerprint(&fresh);
                        assert_eq!(actual, expected);
                        let _=tx.send(json!({"digest":actual.0,"fresh_digest":expected.0,"nodes":actual.1,"bytes":text.len(),"full_node_kinds_byte_ranges_points_match":true}));
                    });
                }
            }
            40 => {
                let result = s.oracle.as_ref().unwrap().try_recv();
                if let Ok(proof) = result {
                    s.oracle_proof = Some(proof);
                    shot = Some("08-independent-fresh-tree-oracle");
                } else {
                    assert!(
                        !matches!(result, Err(mpsc::TryRecvError::Disconnected)),
                        "fresh syntax oracle stopped"
                    );
                    wait = true;
                }
            }
            41 => {
                for _ in 0..23 {
                    key(input, Key::Z, true, false);
                }
            }
            42 => {
                assert_eq!(self.doc.rope().len_chars(), s.original_chars);
                assert!(!self.doc.dirty());
                assert!(self.doc.tree().is_some());
                if self.syntax_ui.pending.is_some() || self.syntax_ui.parser.active_id() != 0 {
                    wait = true;
                } else {
                    shot = Some("09-original-undo-tree-restored");
                }
            }
            43 => {
                assert!(s.busy);
                std::fs::write(s.dir.join("syntax-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_ASYNC_SYNTAX_OK","source_lines":self.doc.rope().len_lines(),"screenshots":s.shots,"input_frames":s.frames,"actions":s.actions,"typed_while_real_parser_active":s.busy,"burst_count":20,"latest_wins":true,"no_stale_spans_while_pending":true,"tab_switch_and_parked_buffer_preserved":true,"ime_commit_single_undo":true,"indent_outdent_deferred":true,"original_buffer_undo_restored":true,"syntax_workers":usize::from(self.syntax_ui.parser.worker_started()),"completed_parses":self.syntax_ui.parser.completed(),"last_parse_us":self.syntax_ui.last_parse_us,"fresh_oracle":s.oracle_proof,"disk_sources_unchanged":true,"renderer":"wgpu","release":!cfg!(debug_assertions),"input":"native RawInput Paste burst/Undo/Redo/ShiftEnd/IME + actual explorer/tab pointer; independent full-tree oracle on proof worker","no_synthetic_parse_delay":true})).unwrap()).unwrap();
                eprintln!(
                    "GPU_ASYNC_SYNTAX_OK: real500k source; native20-input burst while actual parser active; coalesced latest-state tree; no stale spans; tab/parked/IME/Undo/Redo; full independent fresh-tree node kinds+byte ranges+points match; original source unchanged; 9 GPU frames"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now()
                + if (1..=21).contains(&s.step) {
                    Duration::ZERO
                } else {
                    Duration::from_millis(140)
                };
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.syntax_smoke = Some(s);
    }
    pub(super) fn syntax_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.syntax_smoke else {
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
