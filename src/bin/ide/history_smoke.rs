//! Native input and GPU evidence for persistent history on a real 100k-line source.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
#[derive(Default)]
pub(super) struct CommandProbe {
    step: u8,
    pub complete: bool,
    before_version: i32,
    definition: Option<PathBuf>,
    saved_version: i32,
    home_sent: bool,
}
pub(super) struct Smoke {
    pub commands: Option<CommandProbe>,
    pub dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    pub actions: Vec<Value>,
    pub capture_us: Vec<u128>,
    path: PathBuf,
    hash: blake3::Hash,
    len: usize,
    first_line: usize,
    lines: usize,
    bytes: usize,
    native: bool,
}
const APPEND: &str = "// actual native history commit 👋\n";
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        assert!(app.doc.rope().len_lines() > 100000);
        Self {
            commands: None,
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            actions: vec![],
            capture_us: vec![],
            path: app.doc.path.clone().unwrap(),
            hash: app.doc.content_hash(),
            len: app.doc.rope().len_chars(),
            first_line: app.doc.rope().line(0).len_chars() - 1,
            lines: app.doc.rope().len_lines(),
            bytes: app.doc.rope().len_bytes(),
            native: app.root.join("wex.json").is_file(),
        }
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
impl Workbench {
    pub(super) fn history_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.history_smoke.take() else {
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
        assert!(
            s.start.elapsed() < Duration::from_secs(300),
            "native history smoke timeout step{}: status={} path={:?} cursor={} language={} model={:?} commands={:?}",
            s.step,
            self.status,
            self.doc.path,
            self.edit.cursor,
            self.language_status,
            self.model_status,
            s.commands.as_ref().map(|p| p.step)
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
            self.history_smoke = Some(s);
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
            self.history_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.history_smoke = Some(s);
            return;
        }
        if let Some(mut probe) = s.commands.take() {
            if !probe.complete {
                match probe.step {
                    0 => {
                        if k2_ready(self) && self.doc.tree().is_some() {
                            self.editor_ui.focus_editor = true;
                            if !probe.home_sent {
                                key(input, Key::Home, true, false);
                                probe.home_sent = true;
                            } else if self.edit.cursor == 0 && self.edit.range().is_empty() {
                                key(input, Key::F, true, false);
                                input.events.push(egui::Event::Text("query".into()));
                                probe.step = 1;
                            } else {
                                // Do not open Find over a stale selection from the
                                // parked-buffer smoke; retry the navigation first.
                                key(input, Key::Home, true, false);
                            }
                        }
                    }
                    1 => {
                        if self.editor_ui.find.options.query == "query" {
                            assert_eq!(
                                self.doc.content_hash(),
                                s.hash,
                                "Find handoff mutated source: query={:?} doc_len={} expected_len={} find_focus={} handoff={} handoff_ready={}",
                                self.editor_ui.find.options.query,
                                self.doc.rope().len_chars(),
                                s.len,
                                self.editor_ui.find.focus,
                                self.editor_ui.find.handoff.len(),
                                self.editor_ui.find.handoff_ready,
                            );
                            s.awaiting = Some("00-find-query-handoff".into());
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                Default::default(),
                            ));
                            probe.step = 2;
                        }
                    }
                    2 => {
                        // Find handoff is verified above; dismiss the transient panel before
                        // the source-owned click/paste/save/F12 transaction. Otherwise Ctrl+Z
                        // in the same tape is correctly consumed by the Find field.
                        self.close_find();
                        let (pos, g) = self.editor_ui.geometry.rows.get(&0).unwrap();
                        let p = *pos
                            + egui::vec2(
                                g.pos_from_cursor(egui::text::CCursor::new(0)).left() + 0.1,
                                23. / 2.,
                            );
                        assert!(self.editor_ui.geometry.viewport.contains(p));
                        probe.before_version = self.doc.version;
                        input.events.extend([
                            egui::Event::PointerMoved(p),
                            egui::Event::PointerButton {
                                pos: p,
                                button: PointerButton::Primary,
                                pressed: true,
                                modifiers: Default::default(),
                            },
                            egui::Event::PointerButton {
                                pos: p,
                                button: PointerButton::Primary,
                                pressed: false,
                                modifiers: Default::default(),
                            },
                            egui::Event::Paste("// focus saved 👋\n".into()),
                        ]);
                        key(input, Key::S, true, false);
                        key(input, Key::Home, true, false);
                        key(input, Key::ArrowDown, false, false);
                        key(input, Key::End, false, false);
                        for _ in 0..3 {
                            key(input, Key::ArrowLeft, false, false);
                        }
                        key(input, Key::F12, false, false);
                        probe.step = 3;
                    }
                    3 => {
                        if self
                            .doc
                            .path
                            .as_ref()
                            .is_some_and(|p| p.ends_with("Tiny.kt"))
                            && self.open_ui.pending.is_none()
                            && self.save_ui.pending.is_none()
                        {
                            assert_eq!(
                                self.doc.rope().line(0).to_string().trim(),
                                "fun snapshotTarget() = 7"
                            );
                            assert!(
                                std::fs::read_to_string(&s.path)
                                    .unwrap()
                                    .starts_with("// focus saved 👋\n")
                            );
                            assert!(!self.parked[&s.path].0.dirty());
                            assert_eq!(self.editor_ui.find.options.query, "query");
                            assert!(
                                !self.editor_ui.find.options.whole_word,
                                "source pointer must not also activate animated Find controls"
                            );
                            probe.saved_version = self.parked[&s.path].0.version;
                            assert!(probe.saved_version > probe.before_version);
                            probe.definition = self.doc.path.clone();
                            s.awaiting = Some("00-click-save-f12-real-k2".into());
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                Default::default(),
                            ));
                            probe.step = 4;
                        }
                    }
                    4 => {
                        s.click = Some(("tab-Large.kt".into(), 0));
                        probe.step = 5;
                    }
                    5 => {
                        if self.doc.path.as_ref() == Some(&s.path) && self.open_ui.pending.is_none()
                        {
                            self.editor_ui.focus_editor = true;
                            key(input, Key::Z, true, false);
                            key(input, Key::S, true, false);
                            probe.step = 6;
                        }
                    }
                    6 => {
                        if self.save_ui.pending.is_none()
                            && self.doc.content_hash() == s.hash
                            && !self.doc.dirty()
                            && k2_ready(self)
                        {
                            assert_eq!(blake3::hash(&std::fs::read(&s.path).unwrap()), s.hash);
                            key(input, Key::Escape, false, false);
                            probe.step = 7;
                        }
                    }
                    7 => {
                        if !self.editor_ui.find.open {
                            probe.complete = true;
                        }
                    }
                    _ => unreachable!(),
                }
                s.commands = Some(probe);
                self.history_smoke = Some(s);
                return;
            }
            s.commands = Some(probe);
        }
        let mut shot = None;
        let mut wait = false;
        match s.step {
            0 => {
                if s.native && !k2_ready(self) {
                    self.history_smoke = Some(s);
                    return;
                }
                self.editor_ui.focus_editor = true;
                key(input, Key::End, true, false);
            }
            1 => {
                assert_eq!(self.edit.cursor, s.len);
                assert!(self.editor_ui.rendered_rows < 200);
                shot = Some("01-real-100k-file");
            }
            2 => {
                input.events.push(egui::Event::Paste(APPEND.into()));
            }
            3 => {
                if self.doc.tree().is_none() {
                    self.history_smoke = Some(s);
                    return;
                }
                assert_eq!(self.doc.rope().len_chars(), s.len + APPEND.chars().count());
                assert!(self.doc.dirty());
                assert!(!self.doc.tree().unwrap().root_node().has_error());
                shot = Some("02-native-tail-edit");
            }
            4 => key(input, Key::Z, true, false),
            5 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert_eq!(self.edit.cursor, s.len);
                shot = Some("03-undo-exact-buffer-tree-cursor");
            }
            6 => key(input, Key::Y, true, false),
            7 => {
                assert_eq!(self.doc.rope().len_chars(), s.len + APPEND.chars().count());
                assert_eq!(self.edit.cursor, self.doc.rope().len_chars());
                assert!(self.doc.dirty());
                shot = Some("04-redo-exact-buffer-tree-cursor");
            }
            8 => {
                key(input, Key::Z, true, false);
                key(input, Key::Home, true, false);
            }
            9 => key(input, Key::End, false, true),
            10 => {
                assert_eq!(self.edit.range(), 0..s.first_line);
                input
                    .events
                    .push(egui::Event::Paste("// changed first line 日本語".into()));
            }
            11 => {
                if self.doc.tree().is_none() {
                    self.history_smoke = Some(s);
                    return;
                }
                assert!(self.doc.dirty());
                assert!(!self.doc.tree().unwrap().root_node().has_error());
                shot = Some("05-native-head-selection-edit");
                s.click = Some(("file-Tiny.kt".into(), 0));
            }
            12 => {
                // The parked-buffer proof runs before the semantic command tape. Keeping
                // the probe uninitialized here prevents it from racing the pending undo
                // that restores Large.kt's original first line.
                if self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Tiny.kt"))
                {
                    if s.native {
                        key(input, Key::End, true, false);
                        input.events.push(egui::Event::Paste(
                            "// parked snapshot proof unsaved\n".into(),
                        ));
                    }
                    s.click = Some(("tab-Large.kt".into(), 0));
                } else {
                    wait = true;
                }
            }
            13 => {
                if self.doc.path.as_ref() == Some(&s.path) {
                    assert!(self.doc.dirty());
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Z, true, false);
                } else {
                    wait = true;
                }
            }
            14 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert_eq!(self.edit.range(), 0..s.first_line);
                assert!(self.editor_ui.rendered_rows < 200);
                shot = Some("06-parked-history-and-selection-restored");
                if s.native && s.commands.is_none() {
                    s.commands = Some(CommandProbe::default());
                }
            }
            15 => {
                s.click = Some(("close-Large.kt".into(), 0));
            }
            16 => {
                if self.tabs.contains(&s.path) {
                    wait = true;
                } else {
                    assert!(!self.parked.contains_key(&s.path));
                    s.click = Some(("file-Large.kt".into(), 0));
                }
            }
            17 => {
                if self.doc.path.as_ref() != Some(&s.path) || self.open_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!self.doc.dirty());
                    shot = Some("07-closed-large-history-reopened");
                }
            }
            18 => {
                let retirement = wexandroidstudio::document_retirement::snapshot();
                if retirement.pending != 0 {
                    self.history_smoke = Some(s);
                    return;
                }
                assert_eq!(retirement.workers, 1);
                assert!(retirement.text_reclaimed > 0 && retirement.parsers_reclaimed > 0);
                assert_eq!(retirement.logical_bytes_pending, 0);
                assert_eq!(retirement.synchronous_fallbacks, 0);
                assert_ne!(
                    retirement.worker_thread.as_deref(),
                    Some(format!("{:?}", std::thread::current().id()).as_str())
                );
                assert_eq!(blake3::hash(&std::fs::read(&s.path).unwrap()), s.hash);
                assert_eq!(
                    s.actions.iter().filter(|a| a["action"] == "Undo").count(),
                    3 + usize::from(s.commands.is_some())
                );
                assert_eq!(
                    s.actions.iter().filter(|a| a["action"] == "Redo").count(),
                    1
                );
                assert_eq!(
                    s.actions.iter().filter(|a| a["action"] == "Paste").count(),
                    (if s.native { 3 } else { 2 }) + usize::from(s.commands.is_some())
                );
                if !s.native {
                    finish(self, &s, &retirement, ctx);
                    return;
                }
            }
            19 => {
                if !k2_ready(self) {
                    self.history_smoke = Some(s);
                    return;
                }
                self.editor_ui.focus_editor = true;
                key(input, Key::End, true, false);
                input.events.push(egui::Event::Paste(
                    "// immediate K2 snapshot F12 proof\n".into(),
                ));
                key(input, Key::Home, true, false);
                key(input, Key::End, false, false);
                key(input, Key::ArrowLeft, false, false);
                key(input, Key::ArrowLeft, false, false);
                key(input, Key::ArrowLeft, false, false);
                key(input, Key::F12, false, false);
            }
            20 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Tiny.kt"))
                    || self.open_ui.pending.is_some()
                {
                    wait = true;
                } else {
                    assert_eq!(
                        self.doc
                            .rope()
                            .line(self.doc.rope().char_to_line(self.edit.cursor))
                            .to_string()
                            .trim(),
                        "fun snapshotTarget() = 7"
                    );
                    assert!(self.doc.dirty());
                    assert!(
                        self.doc
                            .rope()
                            .to_string()
                            .ends_with("// parked snapshot proof unsaved\n")
                    );
                    assert!(self.parked[&s.path].0.dirty());
                    shot = Some("08-immediate-f12-real-k2-parked-dirty");
                }
            }
            21 => {
                self.editor_ui.focus_editor = true;
                key(input, Key::Z, true, false);
            }
            22 => {
                assert!(!self.doc.dirty());
                s.click = Some(("tab-Large.kt".into(), 0));
            }
            23 => {
                if self.doc.path.as_ref() != Some(&s.path) || self.open_ui.pending.is_some() {
                    wait = true;
                } else {
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Z, true, false);
                }
            }
            24 => {
                if !k2_ready(self) || self.doc.tree().is_none() {
                    wait = true;
                } else {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!self.doc.dirty());
                    shot = Some("09-current-k2-source-restored");
                }
            }
            25 => {
                let retirement = wexandroidstudio::document_retirement::snapshot();
                if retirement.pending != 0 {
                    wait = true;
                } else {
                    let metrics = self.language.as_ref().unwrap().document.metrics();
                    assert!(
                        metrics.materialized_bytes >= s.bytes as u64 && metrics.materialized > 0
                    );
                    assert_eq!(metrics.active, 0);
                    assert_ne!(
                        metrics.materializer_thread.as_deref(),
                        Some(format!("{:?}", std::thread::current().id()).as_str())
                    );
                    assert!(retirement.language_reclaimed > 0);
                    assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                    finish(self, &s, &retirement, ctx);
                    return;
                }
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(180);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.history_smoke = Some(s);
    }
    pub(super) fn history_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.history_smoke else {
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

fn k2_ready(app: &Workbench) -> bool {
    app.model_status
        .as_ref()
        .is_some_and(|s| s.phase == language::ModelPhase::Applied)
        && app.doc.path.as_ref().is_some_and(|p| {
            app.diagnostic_snapshot.as_ref() == Some(&(p.clone(), app.doc.version))
        })
        && !app.diagnostics.iter().any(|d| d.severity == Some(1))
}
fn finish(
    app: &Workbench,
    s: &Smoke,
    retirement: &wexandroidstudio::document_retirement::Snapshot,
    ctx: &egui::Context,
) {
    let metrics = app.language.as_ref().map(|l| l.document.metrics());
    std::fs::write(s.dir.join("history-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_PERSISTENT_HISTORY_OK","ordered_commands":s.commands.as_ref().map(|p|json!({"complete":p.complete,"find_query_handoff":true,"incoming_click_paste_save_f12_one_frame":true,"before_version":p.before_version,"saved_version":p.saved_version,"definition":p.definition,"saved_prefix_verified":true,"undo_save_restored_disk":true})),"source_lines":s.lines,"source_bytes":s.bytes,"retirement":retirement,"large_tab_closed_reopened":true,"foreground_thread":format!("{:?}",std::thread::current().id()),"rendered_rows":app.editor_ui.rendered_rows,"actions":s.actions,"screenshots":s.shots,"disk_source_unchanged":true,"head_and_tail_edits":true,"selection_cursor_restored":true,"dirty_baseline_restored":true,"parked_history_preserved":true,"input":"native RawInput Paste/CtrlZ/CtrlY/ShiftEnd + pointer tabs/explorer","renderer":"wgpu","release":!cfg!(debug_assertions),"deployment_invoked":false,"native_k2":s.native,"immediate_f12_with_new_unsaved_input":s.native,"dirty_parked_source_restored":s.native,"snapshot_metrics":metrics,"foreground_capture_us":s.capture_us,"diagnostic_snapshot":app.diagnostic_snapshot,"current_path":app.doc.path,"current_version":app.doc.version,"model_status":app.model_status})).unwrap()).unwrap();
    eprintln!(
        "GPU_PERSISTENT_HISTORY_OK: real100k-line source; native head/tail edits; CtrlZ/CtrlY restore exact buffer/tree/selection/dirty baseline; parked history retained; {} genuine GPU frames; large history/parser reclaimed on different thread; clean tab closed/reopened; disk unchanged; actual native K2={}",
        s.shots.len(),
        s.native
    );
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}
