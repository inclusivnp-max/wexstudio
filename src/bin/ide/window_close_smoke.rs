//! Extends the original bounded, real-K2 preference tape with actual close controls.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
use wexandroidstudio::text_tracking::TextId;
pub(super) struct Smoke {
    pub armed: bool,
    dir: PathBuf,
    main: PathBuf,
    other: PathBuf,
    original: String,
    parked: String,
    step: u8,
    next: Instant,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    frames: Vec<u64>,
    failure_frames: Vec<u64>,
    guard: Option<(TextId, TextId, usize, usize)>,
}
fn key(i: &mut egui::RawInput, key: Key, mut modifiers: Modifiers) {
    if modifiers.command {
        modifiers.ctrl = true;
    }
    for pressed in [true, false] {
        i.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
}
impl Smoke {
    pub fn new(dir: PathBuf, main: PathBuf, other: PathBuf) -> Self {
        Self {
            original: std::fs::read_to_string(&main).unwrap(),
            parked: std::fs::read_to_string(&other).unwrap(),
            dir,
            main,
            other,
            armed: false,
            step: 0,
            next: Instant::now(),
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            frames: vec![],
            failure_frames: vec![],
            guard: None,
        }
    }
    fn markers(a: &Workbench) -> usize {
        a.debug.bp_report.as_ref().unwrap().rows.len()
    }
    fn minimum(&mut self, a: &Workbench, ctx: &egui::Context, failed: bool) -> bool {
        let rect = ctx.content_rect();
        assert!((rect.width() - 760.).abs() < 1. && (rect.height() - 560.).abs() < 1.);
        let frames = if failed {
            &mut self.failure_frames
        } else {
            &mut self.frames
        };
        let frame = ctx.cumulative_frame_nr();
        if !frames.contains(&frame) {
            frames.push(frame);
        }
        if frames.len() < 4 {
            return false;
        }
        for &name in if failed {
            &[
                "window-close-cancel",
                "window-close-retry",
                "window-close-review-discard",
            ][..]
        } else {
            &[
                "window-close-cancel",
                "window-close-discard-text",
                "window-close-save",
            ][..]
        } {
            assert!(
                rect.contains_rect(*a.chrome.hits.get(name).unwrap()),
                "close footer outside min viewport: {name}"
            );
        }
        true
    }
    fn signal(&self, name: &str) {
        std::fs::write(self.dir.join(name), b"ready\n").unwrap();
    }
    fn ack(&self, name: &str) -> bool {
        self.dir.join(name).exists()
    }
    fn exact(&self, a: &Workbench) {
        assert_eq!(a.doc.path.as_ref(), Some(&self.main));
        assert_eq!(a.doc.rope().to_string(), self.original);
        assert_eq!(a.parked[&self.other].0.rope().to_string(), self.parked);
        assert!(!a.doc.dirty() && !a.parked[&self.other].0.dirty());
        assert_eq!(std::fs::read_to_string(&self.main).unwrap(), self.original);
        assert_eq!(std::fs::read_to_string(&self.other).unwrap(), self.parked);
    }
    pub fn input(&mut self, a: &mut Workbench, ctx: &egui::Context, i: &mut egui::RawInput) {
        assert!(
            !self.dir.join("close-writer.failure").exists(),
            "independent lock worker failed"
        );
        if let Some((pos, until)) = self.release.take() {
            i.events.push(Event::PointerMoved(pos));
            if Instant::now() < until {
                self.release = Some((pos, until));
            } else {
                i.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
                self.next = Instant::now() + Duration::from_millis(180);
            }
            return;
        }
        if let Some(name) = self.click.take() {
            let pos = if name == "source" {
                a.editor_ui.geometry.viewport.min + egui::vec2(100., 20.)
            } else if name == "backdrop" {
                egui::pos2(8., 8.)
            } else {
                a.chrome
                    .hits
                    .get(&name)
                    .unwrap_or_else(|| panic!("missing close control {name}"))
                    .center()
            };
            assert!(ctx.content_rect().contains(pos));
            i.events.extend([
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            self.release = Some((pos, Instant::now() + Duration::from_millis(90)));
            eprintln!("WINDOW_CLOSE_REAL_POINTER: {name}");
            return;
        }
        if self.awaiting.is_some() || Instant::now() < self.next {
            return;
        }
        let phase = a.close_ui.phase();
        let mut wait = false;
        let mut shot = None;
        match self.step {
            0 => self.click = Some("source".into()),
            1 => {
                key(i, Key::Home, Modifiers::COMMAND);
                i.events.push(Event::Paste("// close active\n".into()));
            }
            2 => {
                if a.doc.dirty() {
                    self.click = Some("tab-BranchFeature.kt".into());
                } else {
                    wait = true;
                }
            }
            3 => {
                if a.doc.path.as_ref() == Some(&self.other) {
                    self.click = Some("source".into());
                } else {
                    wait = true;
                }
            }
            4 => {
                key(i, Key::Home, Modifiers::COMMAND);
                i.events.push(Event::Paste("// close parked\n".into()));
            }
            5 => {
                if a.doc.dirty() {
                    self.guard = Some((
                        a.doc.text_id().clone(),
                        a.parked[&self.main].0.text_id().clone(),
                        a.edit.anchor,
                        a.edit.cursor,
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    wait = true;
                }
            }
            6 => {
                if phase == Some(window_close::Phase::Choices) {
                    assert_eq!(a.close_ui.source_count(), 2);
                    key(i, Key::S, Modifiers::COMMAND);
                    key(i, Key::F5, Modifiers::NONE);
                    i.events.extend([
                        Event::Paste("BLOCKED".into()),
                        Event::Ime(egui::ImeEvent::Enabled),
                        Event::Ime(egui::ImeEvent::Preedit("入力".into())),
                        Event::Ime(egui::ImeEvent::Commit("入力".into())),
                    ]);
                    key(i, Key::Enter, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            7 => {
                if self.minimum(a, ctx, false) {
                    assert!(a.close_ui.open && phase == Some(window_close::Phase::Choices));
                    assert!(
                        !a.running && !a.building && !a.debug.busy && a.save_ui.pending.is_none()
                    );
                    assert_eq!(
                        a.doc.rope().to_string(),
                        format!("// close parked\n{}", self.parked)
                    );
                    assert_eq!(
                        a.parked[&self.main].0.rope().to_string(),
                        format!("// close active\n{}", self.original)
                    );
                    shot = Some("05-close-unsaved-minimum");
                    self.click = Some("window-close-cancel".into());
                } else {
                    wait = true;
                }
            }
            8 => {
                if !a.close_ui.open {
                    let g = self.guard.as_ref().unwrap();
                    assert_eq!(a.doc.text_id(), &g.0);
                    assert_eq!(a.parked[&self.main].0.text_id(), &g.1);
                    assert_eq!((a.edit.anchor, a.edit.cursor), (g.2, g.3));
                    self.signal("save-lock.ready");
                } else {
                    wait = true;
                }
            }
            9 => {
                if self.ack("save-lock.ack") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    wait = true;
                }
            }
            10 => {
                if phase == Some(window_close::Phase::Choices) {
                    self.click = Some("window-close-save".into());
                } else {
                    wait = true;
                }
            }
            11 => {
                if a.save_ui.phase.as_ref().is_some_and(|p| p.3) {
                    assert!(
                        phase == Some(window_close::Phase::Saving)
                            && a.save_ui.pending.is_some()
                            && !a.close_ui.allow
                    );
                    shot = Some("06-close-save-real-lock");
                    self.click = Some("window-close-cancel".into());
                } else {
                    wait = true;
                }
            }
            12 => {
                if !a.close_ui.open {
                    self.signal("save-release.ready");
                } else {
                    wait = true;
                }
            }
            13 => {
                if self.ack("save-release.ack") && a.save_ui.pending.is_none() {
                    assert!(
                        !a.closing_requested() && !a.doc.dirty() && !a.parked[&self.main].0.dirty()
                    );
                    self.click = Some("source".into());
                } else {
                    wait = true;
                }
            }
            14 => key(i, Key::Z, Modifiers::COMMAND),
            15 => {
                if a.doc.rope() == self.parked.as_str() {
                    key(i, Key::S, Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            16 => {
                if a.save_ui.pending.is_none() && !a.doc.dirty() {
                    self.click = Some("tab-MainActivity.kt".into());
                } else {
                    wait = true;
                }
            }
            17 => {
                if a.doc.path.as_ref() == Some(&self.main) {
                    self.click = Some("source".into());
                } else {
                    wait = true;
                }
            }
            18 => key(i, Key::Z, Modifiers::COMMAND),
            19 => {
                if a.doc.rope() == self.original.as_str() {
                    key(i, Key::S, Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            20 => {
                if a.save_ui.pending.is_none() && !a.doc.dirty() {
                    self.exact(a);
                    self.signal("journal-lock.ready");
                } else {
                    wait = true;
                }
            }
            21 => {
                if self.ack("journal-lock.ack") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    wait = true;
                }
            }
            22 => {
                if phase == Some(window_close::Phase::Failed)
                    && !a.debug.bp_service.pending()
                    && self.minimum(a, ctx, true)
                {
                    assert_eq!(Self::markers(a), 3);
                    shot = Some("07-close-backup-locked");
                    self.click = Some("window-close-review-discard".into());
                } else {
                    wait = true;
                }
            }
            23 => {
                if a.close_ui.discard_confirmation() {
                    shot = Some("08-close-explicit-discard");
                    self.click = Some("backdrop".into());
                } else {
                    wait = true;
                }
            }
            24 => {
                assert!(a.close_ui.open && a.close_ui.discard_confirmation());
                key(i, Key::Escape, Modifiers::NONE);
            }
            25 => {
                if !a.close_ui.open {
                    assert_eq!(Self::markers(a), 3);
                    self.exact(a);
                    self.signal("journal-release.ready");
                } else {
                    wait = true;
                }
            }
            26 => {
                if self.ack("journal-release.ack") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    wait = true;
                }
            }
            27 => {
                if phase == Some(window_close::Phase::Backup) && !a.debug.bp_service.pending() {
                    self.exact(a);
                    shot = Some("09-close-verified-backup");
                } else {
                    wait = true;
                }
            }
            28 => {
                self.exact(a);
                assert_eq!(self.shots.len(), 5);
                assert_eq!(Self::markers(a), 3);
                let proof = json!({"result":"NATIVE_WINDOW_CLOSE_REVIEW_OK","source_undo_and_original_bytes":true,"cancel_preserved_text_ids_and_selection":true,"actual_save_lock_cancel_receipts":true,"actual_journal_lock_error_retry":true,"discard_confirmation_cancel_kept_markers":true,"modal_paste_shortcuts_ime_blocked":true,"minimum_frames":self.frames,"failure_minimum_frames":self.failure_frames,"screenshots":self.shots,"reduced_motion":a.reduced_motion});
                std::fs::write(
                    self.dir.join("closing-window.json"),
                    serde_json::to_vec_pretty(&proof).unwrap(),
                )
                .unwrap();
                self.click = Some("window-close-backup".into());
            }
            29 => wait = true,
            _ => panic!("invalid closing stage {}", self.step),
        }
        if !wait {
            eprintln!("WINDOW_CLOSE_STAGE_OK {}", self.step);
            self.step += 1;
        }
        self.next = Instant::now() + Duration::from_millis(if wait { 40 } else { 180 });
        if let Some(name) = shot {
            self.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }
    pub fn capture(&mut self, ctx: &egui::Context) {
        for e in ctx.input(|i| i.events.clone()) {
            if let Event::Screenshot { image, .. } = e
                && let Some(name) = self.awaiting.take()
            {
                let path = self.dir.join(format!("{name}.png"));
                let pixels = image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>();
                image::save_buffer(
                    &path,
                    &pixels,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                self.shots
                    .push(json!({"path":path,"width":image.size[0],"height":image.size[1]}));
            }
        }
    }
}
