//! Identical native-window CPU/capture probe used on original and modified source.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
#[derive(Default, Clone, serde::Serialize)]
pub(super) struct Profile {
    pub frame: u64,
    pub update_us: u128,
    pub editor_us: u128,
    pub syntax_format_us: u128,
    pub font_layout_us: u128,
    pub source_vertices: usize,
    pub submitted_vertices: usize,
    pub candidate_glyphs: usize,
    pub indexed_glyphs: usize,
    pub rows: usize,
}
#[derive(Default)]
pub(super) struct ColdProbe {
    step: u8,
    active_at_input: u64,
    revision_before: u64,
    revision_after: u64,
    pub complete: bool,
}
#[derive(Default)]
pub(super) struct GestureProbe {
    step: u8,
    pub complete: bool,
    a: f32,
    b: f32,
    revision_before: u64,
    active_at_batch: u64,
    deferred_before: u64,
    metrics: Option<Value>,
}
pub(super) struct Smoke {
    pub gesture: Option<GestureProbe>,
    pub cold: Option<ColdProbe>,
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    awaiting: Option<(String, Instant)>,
    shots: Vec<Value>,
    samples: Vec<Profile>,
    pub actions: Vec<Value>,
    release: Option<(Pos2, Instant)>,
    click: Option<(String, u8)>,
    path: PathBuf,
    hash: blake3::Hash,
    len: usize,
    target: usize,
    selected: String,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        assert!(app.doc.rope().line(0).len_chars() > 400000);
        let len = app.doc.rope().line(0).len_chars() - 1;
        let target = len - 3;
        let selected = app.doc.rope().slice(target - 3..target).to_string();
        Self {
            cold: None,
            gesture: None,
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            step: 0,
            awaiting: None,
            shots: vec![],
            samples: vec![],
            actions: vec![],
            release: None,
            click: None,
            path: app.doc.path.clone().unwrap(),
            hash: app.doc.content_hash(),
            len,
            target,
            selected,
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
    pub(super) fn viewport_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.viewport_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(240),
            "viewport smoke step{}: {}",
            s.step,
            self.status
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
            self.viewport_smoke = Some(s);
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
                    s.release = Some((pos, Instant::now() + Duration::from_millis(80)));
                }
            } else {
                s.click = Some((name, hover));
            }
            self.viewport_smoke = Some(s);
            return;
        }
        if let Some(mut cold) = s.cold.take() {
            if !cold.complete && s.awaiting.is_none() && Instant::now() >= s.next {
                match cold.step {
                    0 => {
                        if self.fonts.ready
                            && self.fonts.installed
                            && self.doc.tree().is_some()
                            && !self.editor_ui.long_lines.pending()
                        {
                            self.editor_ui.focus_editor = true;
                            key(input, Key::Home, true, false);
                            input.events.push(egui::Event::Paste("// cold 🧪 ".into()));
                            cold.step = 1;
                        }
                    }
                    1 => {
                        let metrics = self.editor_ui.long_lines.worker.metrics();
                        if metrics.active != 0 {
                            cold.active_at_input = metrics.active;
                            cold.revision_before = self.doc.revision;
                            input.events.push(egui::Event::Text("x".into()));
                            s.awaiting = Some(("00-cold-input".into(), Instant::now()));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                egui::UserData::default(),
                            ));
                            cold.step = 2;
                        }
                    }
                    2 => {
                        assert_eq!(self.doc.revision, cold.revision_before + 1);
                        assert_eq!(self.doc.rope().slice(..11).to_string(), "// cold 🧪 x");
                        cold.revision_after = self.doc.revision;
                        key(input, Key::Z, true, false);
                        key(input, Key::Z, true, false);
                        cold.step = 3;
                    }
                    3 => {
                        if !self.editor_ui.long_lines.pending() && self.doc.tree().is_some() {
                            assert_eq!(self.doc.content_hash(), s.hash);
                            assert!(self.editor_ui.long_lines.worker.metrics().superseded > 0);
                            assert!(
                                self.editor_ui.geometry.rows.get(&0).is_some_and(|(_, g)| g
                                    .job
                                    .text
                                    .len()
                                    > 1000000)
                            );
                            cold.complete = true;
                        }
                    }
                    _ => unreachable!(),
                }
            }
            let complete = cold.complete;
            s.cold = Some(cold);
            if !complete {
                self.viewport_smoke = Some(s);
                return;
            }
        }
        if let Some(mut g) = s.gesture.take() {
            if !g.complete && s.awaiting.is_none() && Instant::now() >= s.next {
                match g.step {
                    0 => {
                        if !self.editor_ui.long_lines.pending() && self.doc.tree().is_some() {
                            let (_, galley) = self.editor_ui.geometry.rows.get(&0).unwrap();
                            g.a = galley.pos_from_cursor(egui::text::CCursor::new(3)).left() + 0.1;
                            g.b = galley.pos_from_cursor(egui::text::CCursor::new(7)).left() + 0.1;
                            g.revision_before = self.doc.revision;
                            g.deferred_before = self.editor_ui.pointer_metrics.deferred;
                            key(input, Key::End, false, false);
                            input.events.push(egui::Event::Paste("X".into()));
                            key(input, Key::Home, true, false);
                            g.step = 1;
                        }
                    }
                    1 => {
                        let metrics = self.editor_ui.long_lines.worker.metrics();
                        if metrics.active != 0 {
                            assert_eq!(self.doc.revision, g.revision_before + 1);
                            g.active_at_batch = metrics.active;
                            let start =
                                self.editor_ui.geometry.origin + egui::vec2(58. + g.a, 23. / 2.);
                            let end =
                                self.editor_ui.geometry.origin + egui::vec2(58. + g.b, 23. / 2.);
                            assert!(
                                self.editor_ui.geometry.viewport.contains(start)
                                    && self.editor_ui.geometry.viewport.contains(end)
                            );
                            input.events.extend([
                                egui::Event::PointerMoved(start),
                                egui::Event::PointerButton {
                                    pos: start,
                                    button: PointerButton::Primary,
                                    pressed: true,
                                    modifiers: Default::default(),
                                },
                                egui::Event::PointerMoved(end),
                                egui::Event::PointerButton {
                                    pos: end,
                                    button: PointerButton::Primary,
                                    pressed: false,
                                    modifiers: Default::default(),
                                },
                                egui::Event::Paste("Q".into()),
                            ]);
                            s.awaiting = Some(("00-queued-gesture".into(), Instant::now()));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                Default::default(),
                            ));
                            g.step = 2;
                        }
                    }
                    2 => {
                        if self.doc.revision == g.revision_before + 2
                            && !self.editor_ui.long_lines.pending()
                        {
                            assert_eq!(self.doc.rope().slice(..6).to_string(), "// Qal");
                            let metrics = &self.editor_ui.pointer_metrics;
                            assert!(metrics.deferred > g.deferred_before);
                            assert_eq!(metrics.last_deferred_revision, g.revision_before + 1);
                            assert_eq!(
                                metrics.last_deferred_order[..4],
                                ["press", "move", "release", "paste"]
                            );
                            assert_eq!(metrics.last_selection, Some((3, 7)));
                            g.metrics = Some(serde_json::to_value(metrics).unwrap());
                            s.awaiting = Some(("00-gesture-applied".into(), Instant::now()));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                Default::default(),
                            ));
                            g.step = 3;
                        }
                    }
                    3 => {
                        key(input, Key::Z, true, false);
                        g.step = 4;
                    }
                    4 => {
                        assert_eq!(self.edit.range(), 3..7);
                        assert_eq!(self.doc.rope().slice(..9).to_string(), "// Actual");
                        key(input, Key::Z, true, false);
                        g.step = 5;
                    }
                    5 => {
                        if !self.editor_ui.long_lines.pending() && self.doc.tree().is_some() {
                            assert_eq!(self.doc.content_hash(), s.hash);
                            assert!(!self.doc.dirty());
                            g.complete = true;
                        }
                    }
                    _ => unreachable!(),
                }
            }
            let complete = g.complete;
            s.gesture = Some(g);
            if !complete {
                self.viewport_smoke = Some(s);
                return;
            }
        }
        if s.awaiting.is_some() || Instant::now() < s.next || self.editor_ui.long_lines.pending() {
            self.viewport_smoke = Some(s);
            return;
        }
        let mut shot = None;
        let mut wait = false;
        match s.step {
            0 => {
                if !self.fonts.ready || !self.fonts.installed || self.doc.tree().is_none() {
                    wait = true;
                } else {
                    self.editor_ui.focus_editor = true;
                    key(input, Key::Home, true, false);
                }
            }
            1 => {
                shot = Some("01-head".to_string());
            }
            2 => key(input, Key::End, false, false),
            3 => {
                assert_eq!(self.edit.cursor, s.len);
                assert!(self.edit.scroll_x > 100000.);
                shot = Some("02-tail".into());
            }
            4..=9 => {
                assert_eq!(self.edit.cursor, s.len);
                s.samples.push(self.editor_ui.viewport_profile.clone());
                shot = Some(format!("warm-{}", s.step - 4));
            }
            10 => {
                let (pos, g) = self.editor_ui.geometry.rows[&0].clone();
                let p = pos
                    + egui::vec2(
                        g.pos_from_cursor(egui::text::CCursor::new(s.target)).left() + 0.5,
                        10.,
                    );
                assert!(self.editor_ui.geometry.viewport.contains(p));
                input.events.push(egui::Event::PointerMoved(p));
                input.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                s.release = Some((p, Instant::now() + Duration::from_millis(80)));
            }
            11 => {
                assert_eq!(self.edit.cursor, s.target);
                for _ in 0..3 {
                    key(input, Key::ArrowLeft, false, true);
                }
            }
            12 => {
                assert_eq!(self.edit.selected_text(&self.doc), s.selected);
                input.events.push(egui::Event::Paste("日本語".into()));
            }
            13 => {
                if self.doc.tree().is_none() {
                    wait = true;
                } else {
                    assert!(self.doc.dirty());
                    shot = Some("03-edit".into());
                }
            }
            14 => key(input, Key::Z, true, false),
            15 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                input
                    .events
                    .push(egui::Event::Ime(egui::ImeEvent::Preedit("にほ".into())));
            }
            16 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(self.edit.composition.is_some());
                shot = Some("04-ime-preedit".into());
            }
            17 => {
                input
                    .events
                    .push(egui::Event::Ime(egui::ImeEvent::Commit("日本語".into())));
            }
            18 => {
                assert!(self.doc.dirty());
                assert!(self.edit.composition.is_none());
                shot = Some("05-ime-commit".into());
            }
            19 => key(input, Key::Z, true, false),
            20 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                key(input, Key::Home, true, false);
            }
            21 => {
                s.click = Some(("file-Tiny.kt".into(), 0));
            }
            22 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("Tiny.kt"))
                {
                    wait = true;
                } else {
                    s.click = Some(("tab-Large.kt".into(), 0));
                }
            }
            23 => {
                if self.doc.path.as_ref() != Some(&s.path) || self.open_ui.pending.is_some() {
                    wait = true;
                } else {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!self.doc.dirty());
                    shot = Some("06-returned".into());
                }
            }
            24 => {
                assert_eq!(blake3::hash(&std::fs::read(&s.path).unwrap()), s.hash);
                assert!(self.editor_ui.rendered_rows < 100);
                assert_eq!(s.samples.len(), 6);
                assert_eq!(
                    s.shots.len(),
                    if s.gesture.is_some() {
                        15
                    } else if s.cold.is_some() {
                        13
                    } else {
                        12
                    }
                );
                std::fs::write(s.dir.join("viewport-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_VIEWPORT_PROFILE_OK","ordered_pointer":s.gesture.as_ref().map(|g|json!({"complete":g.complete,"active_at_batch":g.active_at_batch,"revision_before":g.revision_before,"revision_after":g.revision_before+2,"metrics":g.metrics,"same_frame_press_move_release_paste":true,"undo_restored_selection":true,"protected_source_restored":true})),"background_layout":s.cold.as_ref().map(|p|json!({"complete":p.complete,"active_at_input":p.active_at_input,"revision_before":p.revision_before,"revision_after":p.revision_after,"foreground_thread":format!("{:?}",std::thread::current().id()),"layout_worker_thread":self.editor_ui.geometry.assets.first().map(|a|a.layout.worker_thread.clone()),"metrics":self.editor_ui.long_lines.worker.metrics(),"retirement":wexandroidstudio::document_retirement::snapshot()})),"renderer":"wgpu","release":!cfg!(debug_assertions),"source_bytes":self.doc.rope().len_bytes(),"source_chars":self.doc.rope().len_chars(),"long_line_chars":s.len,"screenshots":s.shots,"warm_samples":s.samples,"actions":s.actions,"native_mouse_tail_exact":true,"keyboard_selection":s.selected,"ime_preedit_immutable":true,"ime_commit_single_undo":true,"parked_view_preserved":true,"disk_source_unchanged":true,"rendered_rows":self.editor_ui.rendered_rows,"deployment_invoked":false})).unwrap()).unwrap();
                eprintln!(
                    "GPU_VIEWPORT_PROFILE_OK: actual{}-character Unicode line;native head/tail/hit/selection/Paste/Undo/IME/tabs;6 warm completed-update CPU samples;{} genuine GPU captures;disk unchanged",
                    s.len,
                    s.shots.len()
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
            s.awaiting = Some((name, Instant::now()));
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.viewport_smoke = Some(s);
    }
    pub(super) fn viewport_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.viewport_smoke else {
            return;
        };
        if let Some((name, start)) = s.awaiting.clone() {
            for e in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = e {
                    let elapsed = start.elapsed().as_micros();
                    let path = s.dir.join(format!("{name}.png"));
                    let b = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        &path,
                        &b,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    s.shots.push(json!({"name":name,"path":path,"width":image.size[0],"height":image.size[1],"native_capture_roundtrip_us":elapsed,"background_layout_active":self.editor_ui.long_lines.worker.metrics().active}));
                    s.awaiting = None;
                }
            }
        }
    }
}
