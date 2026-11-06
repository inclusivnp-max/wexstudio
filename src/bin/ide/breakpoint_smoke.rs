//! Actual native input, source anchors, Save/reopen and M3 unresolved-marker review.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    dir: PathBuf,
    path: PathBuf,
    original: String,
    line: u32,
    reopen: bool,
    step: u8,
    start: Instant,
    next: Instant,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    resized: bool,
    minimum_frames: Vec<u64>,
}
fn key(i: &mut egui::RawInput, key: Key, mut mods: Modifiers) {
    if mods.command {
        mods.ctrl = true;
    }
    for pressed in [true, false] {
        i.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: mods,
        });
    }
}
fn goto(i: &mut egui::RawInput, line: u32) {
    key(i, Key::Home, Modifiers::COMMAND);
    for _ in 1..line {
        key(i, Key::ArrowDown, Modifiers::NONE);
    }
}
impl Smoke {
    pub fn new(dir: PathBuf, a: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let path = a
            .root
            .join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
        let original = std::fs::read_to_string(&path).unwrap();
        let line = original
            .lines()
            .position(|l| l.contains("val answer = debugNested(doubled)"))
            .unwrap() as u32
            + 1;
        assert!(a.open_file(path.clone()));
        Self {
            dir,
            path,
            original,
            line,
            reopen: std::env::var_os("WEX_BP_REOPEN").is_some(),
            step: 0,
            start: Instant::now(),
            next: Instant::now(),
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            resized: false,
            minimum_frames: vec![],
        }
    }
}
impl Workbench {
    pub(super) fn breakpoint_smoke_input(&mut self, ctx: &egui::Context, i: &mut egui::RawInput) {
        let Some(mut s) = self.breakpoint_smoke.take() else {
            return;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "breakpoint smoke step{} status={} rows={:?}",
            s.step,
            self.status,
            self.debug.bp_report.as_ref().map(|r| &r.rows)
        );
        i.focused = true;
        i.modifiers = Modifiers::NONE;
        i.events.retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        if let Some((p, end)) = s.release.take() {
            i.events.push(Event::PointerMoved(p));
            if Instant::now() < end {
                s.release = Some((p, end));
            } else {
                i.events.push(Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
            }
            self.breakpoint_smoke = Some(s);
            return;
        }
        if let Some(name) = s.click.take() {
            let r = if name == "source" {
                self.editor_ui.geometry.viewport
            } else {
                *self.chrome.hits.get(&name).unwrap()
            };
            let p = if name == "source" {
                r.min + egui::vec2(100., 20.)
            } else {
                r.center()
            };
            assert!(ctx.content_rect().contains(p));
            i.events.extend([
                Event::PointerMoved(p),
                Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            s.release = Some((p, Instant::now() + Duration::from_millis(90)));
            self.breakpoint_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.breakpoint_smoke = Some(s);
            return;
        }
        let ready = |a: &Workbench, line: u32| {
            !a.debug.bp_service.pending()
                && a.breakpoint_marker_current(&s.path)
                && a.debug
                    .breakpoints
                    .get(&s.path)
                    .is_some_and(|v| v.contains(&line))
        };
        let mut wait = false;
        let mut shot = None;
        let mut done = false;
        match s.step {
            0 => {
                if self.doc.path.as_ref() == Some(&s.path)
                    && self.open_ui.pending.is_none()
                    && self
                        .model_status
                        .as_ref()
                        .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                    && self.diagnostic_snapshot.as_ref()
                        == Some(&(s.path.clone(), self.doc.version))
                {
                    s.click = Some("nav-debug".into());
                } else {
                    wait = true
                }
            }
            1 => s.click = Some("source".into()),
            2 => {
                goto(i, s.line);
                if !s.reopen {
                    key(i, Key::F9, Modifiers::NONE);
                }
            }
            3 => {
                if ready(self, s.line) {
                    shot = Some("01-source-marker");
                } else {
                    wait = true
                }
            }
            4 => {
                if s.reopen {
                    key(i, Key::F9, Modifiers::NONE);
                } else {
                    key(i, Key::Home, Modifiers::COMMAND);
                    i.events
                        .push(Event::Paste("// native breakpoint anchor 😀\n".into()));
                }
            }
            5 => {
                if s.reopen {
                    if !self.debug.bp_service.pending()
                        && self
                            .debug
                            .bp_report
                            .as_ref()
                            .is_some_and(|r| r.rows.is_empty())
                    {
                        shot = Some("02-reopened-marker-removed");
                    } else {
                        wait = true
                    }
                } else if ready(self, s.line + 1) {
                    shot = Some("02-inserted-line");
                } else {
                    wait = true
                }
            }
            6 => {
                if s.reopen {
                    done = true
                } else {
                    key(i, Key::Z, Modifiers::COMMAND);
                }
            }
            7 => {
                if ready(self, s.line) {
                    key(i, Key::Y, Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            8 => {
                if ready(self, s.line + 1) {
                    goto(i, s.line + 1);
                    key(i, Key::Home, Modifiers::NONE);
                    key(i, Key::End, Modifiers::SHIFT);
                    key(i, Key::Backspace, Modifiers::NONE);
                } else {
                    wait = true
                }
            }
            9 => {
                if !self.debug.bp_service.pending()
                    && self.debug.bp_report.as_ref().is_some_and(|r| {
                        r.rows.len() == 1
                            && !r.rows[0].resolved
                            && r.rows[0]
                                .issue
                                .as_ref()
                                .is_some_and(|e| e.contains("deleted"))
                    })
                {
                    if !s.resized {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            760., 560.,
                        )));
                        s.resized = true;
                        wait = true;
                    } else if ctx.content_rect().size() == egui::vec2(760., 560.) {
                        let id = self.debug.bp_report.as_ref().unwrap().rows[0].id;
                        let rects = ["open", "rebind", "remove"].map(|name| {
                            *self
                                .chrome
                                .hits
                                .get(&format!("bp-{name}-{id}"))
                                .expect("review button")
                        });
                        if rects
                            .iter()
                            .all(|r| ctx.content_rect().contains_rect(*r) && r.width() >= 80.)
                        {
                            let frame = ctx.cumulative_frame_nr();
                            if s.minimum_frames.last() != Some(&frame) {
                                s.minimum_frames.push(frame);
                            }
                            if s.minimum_frames.len() >= 4 {
                                shot = Some("03-deleted-code-review");
                            } else {
                                wait = true;
                            }
                        } else {
                            wait = true;
                        }
                    } else {
                        wait = true;
                    }
                } else {
                    wait = true
                }
            }
            10 => key(i, Key::Z, Modifiers::COMMAND),
            11 => {
                if ready(self, s.line + 1) {
                    key(i, Key::S, Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            12 => {
                if self.save_ui.pending.is_none() && !self.doc.dirty() {
                    key(i, Key::Z, Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            13 => {
                if ready(self, s.line) {
                    key(i, Key::S, Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            14 => {
                if self.save_ui.pending.is_none()
                    && !self.doc.dirty()
                    && ready(self, s.line)
                    && self.diagnostic_snapshot.as_ref()
                        == Some(&(s.path.clone(), self.doc.version))
                {
                    assert_eq!(self.doc.rope().to_string(), s.original);
                    assert_eq!(std::fs::read_to_string(&s.path).unwrap(), s.original);
                    shot = Some("04-saved-original-marker");
                } else {
                    wait = true
                }
            }
            15 => done = true,
            _ => panic!("unexpected breakpoint step"),
        }
        if !wait {
            s.step += 1;
        }
        s.next = Instant::now() + Duration::from_millis(if wait { 40 } else { 180 });
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        if done {
            assert!(
                self.save_ui.pending.is_none()
                    && !self.doc.dirty()
                    && !self.debug.bp_service.pending()
            );
            assert_eq!(self.doc.rope().to_string(), s.original);
            assert_eq!(std::fs::read_to_string(&s.path).unwrap(), s.original);
            let stored: Value = serde_json::from_slice(
                &std::fs::read(self.root.join(".wex/breakpoints.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(stored["schema"], 2);
            assert_eq!(s.shots.len(), if s.reopen { 2 } else { 4 });
            std::fs::write(s.dir.join("breakpoints-window.json"),serde_json::to_vec_pretty(&json!({"result":"NATIVE_BREAKPOINT_ANCHORS_OK","reopen":s.reopen,"reduced_motion":self.reduced_motion,"line":s.line,"actual_input":"pointer/key/Paste/selection/F9/CtrlZ/CtrlY/CtrlS","genuine_K2_checkpoint":true,"source_original_restored":true,"minimum_native_frames":s.minimum_frames,"persisted":stored,"screenshots":s.shots})).unwrap()).unwrap();
            eprintln!(
                "NATIVE_BREAKPOINT_ANCHORS_OK reopen={} line={} captures={}",
                s.reopen,
                s.line,
                s.shots.len()
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.breakpoint_smoke = Some(s);
    }
    pub(super) fn breakpoint_smoke_capture(&mut self, ctx: &egui::Context) {
        if let Some(s) = &mut self.breakpoint_smoke {
            for event in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = event
                    && let Some(name) = s.awaiting.take()
                {
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
                }
            }
        }
    }
}
