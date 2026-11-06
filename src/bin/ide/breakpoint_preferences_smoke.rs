//! Real native conflict/recovery tape; independent Python process writes preferences.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    dir: PathBuf,
    path: PathBuf,
    parked: PathBuf,
    original: String,
    other: String,
    line: u32,
    recovery: bool,
    step: u8,
    start: Instant,
    next: Instant,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    frames: Vec<u64>,
    review_id: u64,
    checkpoint: Vec<u8>,
    late_requested: bool,
    late_refreshed: bool,
    closing: Option<window_close_smoke::Smoke>,
    stable_choice: Option<(String, egui::Rect, u64, usize)>,
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
        let parked = a
            .root
            .join("app/src/main/kotlin/dev/wex/android/BranchFeature.kt");
        let original = std::fs::read_to_string(&path).unwrap();
        let other = std::fs::read_to_string(&parked).unwrap();
        let line = original
            .lines()
            .position(|l| l.contains("val answer = debugNested(doubled)"))
            .unwrap() as u32
            + 1;
        let recovery = std::env::var_os("WEX_BP_RECOVERY").is_some();
        if recovery {
            assert!(a.open_file(path.clone()));
        } else {
            assert_eq!(a.doc.path.as_ref(), Some(&parked));
        }
        Self {
            closing: (std::env::args().nth(2).as_deref() == Some("--window-close-smoke"))
                .then(|| window_close_smoke::Smoke::new(dir.clone(), path.clone(), parked.clone())),
            dir,
            path,
            parked,
            original,
            other,
            line,
            recovery,
            step: 0,
            start: Instant::now(),
            next: Instant::now(),
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            frames: vec![],
            review_id: 0,
            checkpoint: vec![],
            late_requested: false,
            late_refreshed: false,
            stable_choice: None,
        }
    }
    fn min_frames(&mut self, a: &Workbench, ctx: &egui::Context) -> bool {
        let rect = ctx.content_rect();
        if (rect.width() - 760.).abs() > 1. || (rect.height() - 560.).abs() > 1. {
            return false;
        }
        let n = ctx.cumulative_frame_nr();
        if !self.frames.contains(&n) {
            self.frames.push(n);
        }
        if self.frames.len() < 4 {
            return false;
        }
        for key in [
            "bp-review-cancel",
            "bp-review-refresh",
            "bp-review-apply",
            "bp-mode-local",
        ] {
            let r = a.chrome.hits.get(key).unwrap();
            assert!(
                rect.contains_rect(*r),
                "footer {key} outside native viewport: {r:?}"
            );
        }
        true
    }
}
impl Workbench {
    pub(super) fn preference_smoke_input(&mut self, ctx: &egui::Context, i: &mut egui::RawInput) {
        let Some(mut s) = self.preference_smoke.take() else {
            return;
        };
        assert!(
            !s.dir.join("writer.failure").exists(),
            "independent preference writer failed; see writer.failure"
        );
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "preference smoke step{}: {}",
            s.step,
            self.status
        );
        i.focused = true;
        i.modifiers = Modifiers::NONE;
        i.events.retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        if let Some(closing) = &mut s.closing
            && closing.armed
        {
            closing.input(self, ctx, i);
            self.preference_smoke = Some(s);
            return;
        }
        if let Some((pos, end)) = s.release.take() {
            i.events.push(Event::PointerMoved(pos));
            if Instant::now() < end {
                s.release = Some((pos, end));
            } else {
                i.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
            }
            self.preference_smoke = Some(s);
            return;
        }
        if let Some(name) = s.click.take() {
            let r = if name == "source" {
                self.editor_ui.geometry.viewport
            } else {
                *self
                    .chrome
                    .hits
                    .get(&name)
                    .unwrap_or_else(|| panic!("missing real control {name}"))
            };
            if name.starts_with("bp-choice-") {
                let id = self
                    .debug
                    .bp_report
                    .as_ref()
                    .and_then(|r| r.review.as_ref())
                    .map(|r| r.id);
                let exact = id.is_some() && self.debug.bp_review.painted == id;
                let frame = ctx.cumulative_frame_nr();
                let count = if exact {
                    if let Some((old, rect, last, count)) = &s.stable_choice
                        && old == &name
                        && *rect == r
                    {
                        *count + usize::from(*last != frame)
                    } else {
                        1
                    }
                } else {
                    0
                };
                s.stable_choice = Some((name.clone(), r, frame, count));
                if count < 4 {
                    s.click = Some(name);
                    self.preference_smoke = Some(s);
                    return;
                }
                eprintln!(
                    "PREFERENCE_REAL_CHOICE_READY review={id:?} completed_stable_frames={count} rect={r:?}"
                );
                s.stable_choice = None;
            }
            let pos = if name == "source" {
                r.min + egui::vec2(100., 20.)
            } else {
                r.center()
            };
            assert!(
                ctx.content_rect().contains(pos),
                "control {name} outside viewport {pos:?}"
            );
            i.events.extend([
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            s.release = Some((pos, Instant::now() + Duration::from_millis(90)));
            self.preference_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.preference_smoke = Some(s);
            return;
        }
        let pending = self.debug.bp_service.pending();
        let report = self.debug.bp_report.as_ref();
        let ready = |a: &Workbench, path: &PathBuf| {
            a.doc.path.as_ref() == Some(path)
                && a.open_ui.pending.is_none()
                && a.model_status
                    .as_ref()
                    .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                && a.diagnostic_snapshot.as_ref() == Some(&(path.clone(), a.doc.version))
        };
        let mut wait = false;
        let mut shot = None;
        let mut done = false;
        if s.recovery {
            match s.step {
                0 => {
                    if ready(self, &s.path) {
                        s.click = Some("nav-debug".into());
                    } else {
                        wait = true;
                    }
                }
                1 => {
                    if !s.late_requested {
                        std::fs::write(s.dir.join("refresh.ready"), b"ready").unwrap();
                        s.late_requested = true;
                        wait = true;
                    } else if !s.late_refreshed && s.dir.join("refresh.ack").exists() {
                        s.click = Some("bp-refresh".into());
                        s.late_refreshed = true;
                        wait = true;
                    } else if s.late_refreshed
                        && !pending
                        && let Some(draft) = report.and_then(|r| r.recoveries.first())
                    {
                        assert!(draft.issue.is_none());
                        s.click = Some(format!("bp-recover-{}", draft.name));
                    } else {
                        wait = true;
                    }
                }
                2 => {
                    if report
                        .and_then(|r| r.review.as_ref())
                        .is_some_and(|r| r.recovery.is_some())
                        && !pending
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            760., 560.,
                        )));
                    } else {
                        wait = true;
                    }
                }
                3 => {
                    if s.min_frames(self, ctx) {
                        shot = Some("01-recovery-review");
                        s.click = Some("bp-mode-local".into());
                    } else {
                        wait = true;
                    }
                }
                4 => {
                    let view = report.and_then(|r| r.review.as_ref()).unwrap();
                    assert_eq!(self.debug.bp_review.result_summary(view), (3, 0));
                    s.click = Some("bp-review-apply".into());
                }
                5 => {
                    if !pending
                        && !self.debug.bp_review.open
                        && report.is_some_and(|r| {
                            r.error.is_none() && r.rows.len() == 3 && r.recoveries.is_empty()
                        })
                    {
                        shot = Some("02-recovered-markers");
                    } else {
                        wait = true;
                    }
                }
                6 => done = true,
                _ => panic!("invalid recovery step"),
            }
        } else {
            match s.step {
                0 => {
                    if ready(self, &s.parked) {
                        s.click = Some("source".into());
                    } else {
                        wait = true;
                    }
                }
                1 => {
                    key(i, Key::Home, Modifiers::COMMAND);
                    i.events
                        .push(Event::Paste("// parked review buffer\n".into()));
                }
                2 => {
                    if self.doc.dirty() {
                        s.click = Some("file-MainActivity.kt".into());
                    } else {
                        wait = true;
                    }
                }
                3 => {
                    if ready(self, &s.path) {
                        s.click = Some("nav-debug".into());
                    } else {
                        wait = true;
                    }
                }
                4 => s.click = Some("source".into()),
                5 => {
                    goto(i, s.line);
                    key(i, Key::F9, Modifiers::NONE);
                }
                6 => {
                    if !pending && report.is_some_and(|r| r.rows.len() == 1 && r.error.is_none()) {
                        std::fs::write(s.dir.join("writer-1.ready"), b"ready").unwrap();
                    } else {
                        wait = true;
                    }
                }
                7 => {
                    if s.dir.join("writer-1.ack").exists() {
                        key(i, Key::Home, Modifiers::COMMAND);
                        i.events
                            .push(Event::Paste("// active review buffer\n".into()));
                    } else {
                        wait = true;
                    }
                }
                8 => {
                    if !pending
                        && self.doc.dirty()
                        && report.is_some_and(|r| r.rows[0].line == s.line + 1)
                    {
                        goto(i, s.line);
                        s.click = Some("bp-rebind-1".into());
                    } else {
                        wait = true;
                    }
                }
                9 => {
                    if !pending && report.is_some_and(|r| r.error.is_some()) {
                        shot = Some("01-external-conflict");
                        s.click = Some("bp-review".into());
                    } else {
                        wait = true;
                    }
                }
                10 => {
                    if !pending && let Some(review) = report.and_then(|r| r.review.as_ref()) {
                        assert_eq!(review.conflicts, 1);
                        assert_eq!(self.debug.bp_review.result_summary(review), (1, 1));
                        s.review_id = review.id;
                        s.checkpoint =
                            std::fs::read(self.root.join(".wex/breakpoints.json")).unwrap();
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            760., 560.,
                        )));
                        key(i, Key::S, Modifiers::COMMAND);
                        key(i, Key::F5, Modifiers::NONE);
                        i.events.push(Event::Paste("SHOULD_NOT_EDIT".into()));
                    } else {
                        wait = true;
                    }
                }
                11 => {
                    if s.min_frames(self, ctx) {
                        assert_eq!(
                            self.doc.rope().to_string(),
                            format!("// active review buffer\n{}", s.original)
                        );
                        assert_eq!(
                            self.parked[&s.parked].0.rope().to_string(),
                            format!("// parked review buffer\n{}", s.other)
                        );
                        assert!(
                            !self.building
                                && !self.running
                                && !self.debug.busy
                                && self.save_ui.pending.is_none()
                        );
                        shot = Some("02-minimum-conflict-review");
                        s.click = Some("bp-review-cancel".into());
                    } else {
                        wait = true;
                    }
                }
                12 => {
                    if !pending && !self.debug.bp_review.open {
                        assert_eq!(
                            std::fs::read(self.root.join(".wex/breakpoints.json")).unwrap(),
                            s.checkpoint
                        );
                        s.click = Some("bp-review".into());
                    } else {
                        wait = true;
                    }
                }
                13 => {
                    if !pending && let Some(review) = report.and_then(|r| r.review.as_ref()) {
                        assert!(review.id > s.review_id);
                        s.click = Some("bp-choice-1-Local".into());
                    } else {
                        wait = true;
                    }
                }
                14 => {
                    let view = report.and_then(|r| r.review.as_ref()).unwrap();
                    assert_eq!(self.debug.bp_review.result_summary(view), (2, 0));
                    s.click = Some("bp-review-apply".into());
                }
                15 => {
                    if !pending
                        && !self.debug.bp_review.open
                        && report.is_some_and(|r| r.rows.len() == 2 && r.error.is_none())
                    {
                        key(i, Key::Z, Modifiers::COMMAND);
                    } else {
                        wait = true;
                    }
                }
                16 => {
                    if !self.doc.dirty() && ready(self, &s.path) {
                        assert_eq!(self.doc.rope().to_string(), s.original);
                        shot = Some("03-merged-with-undo");
                        s.click = Some("tab-BranchFeature.kt".into());
                    } else {
                        wait = true;
                    }
                }
                17 => {
                    if self.doc.path.as_ref() == Some(&s.parked) {
                        assert_eq!(
                            self.doc.rope().to_string(),
                            format!("// parked review buffer\n{}", s.other)
                        );
                        s.click = Some("source".into());
                    } else {
                        wait = true;
                    }
                }
                18 => key(i, Key::Z, Modifiers::COMMAND),
                19 => {
                    if !self.doc.dirty() {
                        assert_eq!(self.doc.rope().to_string(), s.other);
                        s.click = Some("tab-MainActivity.kt".into());
                    } else {
                        wait = true;
                    }
                }
                20 => {
                    if ready(self, &s.path) && !pending {
                        std::fs::write(s.dir.join("writer-2.ready"), b"ready").unwrap();
                    } else {
                        wait = true;
                    }
                }
                21 => {
                    if s.dir.join("writer-2.ack").exists() {
                        goto(i, s.line);
                        key(i, Key::F9, Modifiers::NONE);
                    } else {
                        wait = true;
                    }
                }
                22 => {
                    if !pending && report.is_some_and(|r| r.rows.len() == 3 && r.error.is_some()) {
                        shot = Some("04-failed-save-before-close");
                    } else {
                        wait = true;
                    }
                }
                23 => done = true,
                _ => panic!("invalid conflict step"),
            }
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
            assert_eq!(self.doc.rope().to_string(), s.original);
            assert_eq!(std::fs::read_to_string(&s.path).unwrap(), s.original);
            assert_eq!(std::fs::read_to_string(&s.parked).unwrap(), s.other);
            assert!(!self.doc.dirty() && self.parked.values().all(|(d, _)| !d.dirty()));
            assert!(!pending);
            assert_eq!(s.shots.len(), if s.recovery { 2 } else { 4 });
            let proof = json!({"result":"NATIVE_PREFERENCE_REVIEW_OK","recovery":s.recovery,"expected_close_error":!s.recovery,"result_counts_checked":true,"late_recovery_refreshed":s.recovery&&s.late_refreshed,"source_original_restored":true,"genuine_K2_checkpoint":true,"minimum_frames":s.frames,"reduced_motion":self.reduced_motion,"screenshots":s.shots,"rows":self.debug.bp_report.as_ref().unwrap().rows.as_ref(),"input":"real native RawInput keys/Paste/pointer, independent external preference writer"});
            std::fs::write(
                s.dir.join("preferences-window.json"),
                serde_json::to_vec_pretty(&proof).unwrap(),
            )
            .unwrap();
            eprintln!(
                "NATIVE_PREFERENCE_REVIEW_OK recovery={} expected_close_error={}",
                s.recovery, !s.recovery
            );
            if let Some(closing) = &mut s.closing {
                closing.armed = true;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.preference_smoke = Some(s);
    }
    pub(super) fn preference_smoke_capture(&mut self, ctx: &egui::Context) {
        if let Some(s) = &mut self.preference_smoke {
            if let Some(closing) = &mut s.closing {
                closing.capture(ctx);
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = event
                    && let Some(name) = s.awaiting.take()
                {
                    let path = s.dir.join(format!("{name}.png"));
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
                    s.shots
                        .push(json!({"path":path,"width":image.size[0],"height":image.size[1]}));
                }
            }
        }
    }
}
