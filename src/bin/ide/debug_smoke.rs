//! Physical-phone/native-window regression. It drives the same Debug controls as users.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};
#[path = "debug_close_smoke.rs"]
mod close;
pub(super) struct DebugSmoke {
    dir: PathBuf,
    serial: String,
    source: PathBuf,
    line: u32,
    source_ready: bool,
    original: Vec<(PathBuf, String)>,
    bp_path: PathBuf,
    bp_before: Option<Vec<u8>>,
    start: Instant,
    since: Instant,
    next: Instant,
    step: usize,
    /// Ticket created by the gutter action; step 3 must observe its delivered report.
    bp_before_ticket: u64,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    shown: bool,
    awaiting: Option<String>,
    shots: Vec<Value>,
    snapshots: BTreeMap<String, Value>,
    child: u64,
    array: u64,
    retried: Vec<(usize, String)>,
    last_retry: Instant,
    scroll_settle: Instant,
    last_control: Option<(String, egui::Rect)>,
    pointer_audit: Vec<Value>,
    closing: Option<close::Close>,
    reported: bool,
}

impl Drop for DebugSmoke {
    fn drop(&mut self) {
        if let Some(b) = &self.bp_before {
            let _ = std::fs::write(&self.bp_path, b);
        } else {
            let _ = std::fs::remove_file(&self.bp_path);
        }
    }
}
impl DebugSmoke {
    pub fn new(dir: PathBuf, serial: String, app: &mut Workbench) -> Self {
        assert!(!serial.is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("debug-window.json"));
        let original = project::source_files(&app.root)
            .into_iter()
            .map(|p| {
                let h = blake3::hash(&std::fs::read(&p).unwrap())
                    .to_hex()
                    .to_string();
                (p, h)
            })
            .collect::<Vec<_>>();
        let source = original
            .iter()
            .find(|(p, _)| p.ends_with("MainActivity.kt"))
            .unwrap()
            .0
            .clone();
        let text = std::fs::read_to_string(&source).unwrap();
        let line = text
            .lines()
            .position(|l| l.contains("val answer = debugNested(doubled)"))
            .unwrap() as u32
            + 1;
        let bp_path = app.root.join(".wex/breakpoints.json");
        let bp_before = std::fs::read(&bp_path).ok();
        app.debug.breakpoints.clear();
        assert!(app.open_file(source.clone()), "debug source open rejected");
        eprintln!(
            "DEBUG_SOURCE_OPEN_ACCEPTED: initial={:?} initial_lines={} target={} pending={}",
            app.doc.path,
            app.doc.rope().len_lines(),
            source.display(),
            app.open_ui.pending.is_some()
        );
        Self {
            dir,
            serial,
            source,
            line,
            source_ready: false,
            original,
            bp_path,
            bp_before,
            start: Instant::now(),
            since: Instant::now(),
            next: Instant::now() + Duration::from_secs(3),
            step: 0,
            bp_before_ticket: 0,
            pending: None,
            release: None,
            shown: false,
            awaiting: None,
            shots: vec![],
            snapshots: BTreeMap::new(),
            child: 0,
            array: 0,
            retried: vec![],
            last_retry: Instant::now(),
            scroll_settle: Instant::now(),
            last_control: None,
            pointer_audit: vec![],
            reported: false,
            closing: (std::env::args().nth(2).as_deref() == Some("--debug-close-smoke"))
                .then(close::Close::default),
        }
    }
}
fn key(input: &mut egui::RawInput, key: Key, mut modifiers: egui::Modifiers) {
    if cfg!(target_os = "linux") && modifiers.command {
        modifiers.ctrl = true;
    }
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
}
fn current_vars(app: &Workbench) -> impl Iterator<Item = &Value> {
    app.debug
        .scopes
        .iter()
        .filter_map(|s| s["variablesReference"].as_u64())
        .filter_map(|id| app.debug.variables.get(&id))
        .flatten()
}
fn vars(app: &Workbench, name: &str, value: &str) -> bool {
    current_vars(app).any(|v| v["name"] == name && v["value"] == value)
}
fn top(app: &Workbench, method: &str) -> bool {
    app.debug
        .frames
        .first()
        .and_then(|f| f["name"].as_str())
        .is_some_and(|s| s.contains(method))
}
// A stopped event/stack is not a complete inspection: scopes and root values
// arrive in later DAP replies. Native frames legitimately have no locals.
fn inspection_ready(app: &Workbench) -> bool {
    app.debug.paused
        && !app.debug.frames.is_empty()
        && !app.debug.stack_loading
        && app.debug.stack_error.is_none()
        && !app.debug.scopes_loading
        && app.debug.scopes_error.is_none()
        && app.debug.loading.is_empty()
        && app.debug.variable_errors.is_empty()
        && app.debug.scopes.iter().all(|scope| {
            scope["variablesReference"]
                .as_u64()
                .is_some_and(|id| id == 0 || app.debug.variables.contains_key(&id))
        })
}
fn snapshot(app: &Workbench) -> Value {
    json!({"epoch":app.debug.epoch,"reason":app.debug.reason,"thread":app.debug.thread,"frames":app.debug.frames,"stack_loading":app.debug.stack_loading,"stack_error":app.debug.stack_error,"scopes":app.debug.scopes,"scopes_loading":app.debug.scopes_loading,"scopes_error":app.debug.scopes_error,"variables":app.debug.variables,"array_totals":app.debug.variable_totals,"variables_loading":app.debug.loading,"variable_errors":app.debug.variable_errors,"more":app.debug.variable_more,"execution":app.debug.execution,"breakpoints":app.debug.verified})
}
impl Workbench {
    pub(super) fn debug_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.debug_smoke.take() else {
            return;
        };
        if s.start.elapsed() >= Duration::from_secs(240) {
            let data = json!({"step":s.step,"status":self.status,"pending":s.pending,"array":s.array,"expanded":self.debug.expanded,"pointer_audit":s.pointer_audit});
            let _ = std::fs::write(
                s.dir.join("debug-failure.json"),
                serde_json::to_vec_pretty(&data).unwrap(),
            );
        }
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "native phone debug timed out at {}: {}",
            s.step,
            self.status
        );
        // The smoke owns this window's input, not the desktop pointer. Niri/Xwayland
        // may revoke native focus while another desktop app is active; isolate the
        // test stream so a physical FocusOut cannot cancel a synthetic press.
        input.focused = true;
        input.modifiers = egui::Modifiers::NONE;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        if !s.source_ready {
            // open_file means accepted, not loaded. Never position on the old tab.
            if self.doc.path.as_ref() != Some(&s.source) || self.open_ui.pending.is_some() {
                self.debug_smoke = Some(s);
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            self.edit
                .collapse(self.doc.rope().line_to_char(s.line as usize - 1));
            self.scroll_to_cursor = true;
            s.source_ready = true;
            eprintln!(
                "DEBUG_SOURCE_READY: source={} line={} cursor={} pending=false",
                s.source.display(),
                s.line,
                self.edit.cursor
            );
        }
        if !s.shown && s.start.elapsed() > Duration::from_secs(2) {
            // This targets only our own viewport, never another window by title.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            s.shown = true;
        }
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
            self.debug_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        // A wheel event moves rows over several frames; never press a moving hitbox.
        if Instant::now() < s.scroll_settle {
            self.debug_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if s.awaiting.is_none()
            && !s.closing.as_ref().is_some_and(|c| c.awaiting())
            && let Some((name, hover)) = s.pending.take()
        {
            let rect = *self
                .chrome
                .hits
                .get(&name)
                .unwrap_or_else(|| panic!("missing debugger control {name}"));
            if (name.starts_with("debug-more-") || name.starts_with("debug-var-"))
                && let Some(view) = self.chrome.hits.get("debug-variable-viewport")
                && !view.contains(rect.center())
            {
                input.events.push(egui::Event::PointerMoved(view.center()));
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0., (view.center().y - rect.center().y).clamp(-2000., 2000.)),
                    modifiers: Default::default(),
                });
                s.pending = Some((name, 0));
                s.last_control = None;
                if s.pointer_audit.len() < 512 {
                    s.pointer_audit.push(json!({"action":"wheel_to_control","step":s.step,"rect_y":[rect.min.y,rect.max.y],"viewport_y":[view.min.y,view.max.y],"delta_y":(view.center().y-rect.center().y).clamp(-2000.,2000.)}));
                }
                s.scroll_settle = Instant::now() + Duration::from_millis(250);
                self.debug_smoke = Some(s);
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            let pos = rect.center();
            input.events.push(egui::Event::PointerMoved(pos));
            let stable = s
                .last_control
                .as_ref()
                .is_some_and(|(old, r)| old == &name && r.center().distance(rect.center()) < 0.5);
            s.last_control = Some((name.clone(), rect));
            if hover < 2 || !stable {
                s.pending = Some((name, if stable { hover + 1 } else { 0 }));
            } else {
                if s.pointer_audit.len() < 512 {
                    s.pointer_audit.push(json!({"action":"press_stable_control","step":s.step,"name":name,"center":[pos.x,pos.y],"rect_y":[rect.min.y,rect.max.y],"viewport":self.chrome.hits.get("debug-variable-viewport").map(|r|json!({"min":[r.min.x,r.min.y],"max":[r.max.x,r.max.y]})),"stable_hover_frames":hover}));
                }
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                s.release = Some((pos, Instant::now() + Duration::from_millis(100)));
                s.last_control = None;
            }
            self.debug_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.debug_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        macro_rules! wait {
            ($v:expr) => {
                if !$v {
                    if s.since.elapsed() > Duration::from_millis(700)
                        && s.last_retry.elapsed() > Duration::from_millis(700)
                        && s.retried.iter().filter(|(step, _)| *step == s.step).count() < 3
                    {
                        let control = match s.step {
                            1 if !self.chrome.device_open => Some("device".to_string()),
                            2 if self.chrome.device_open => Some(format!("serial-{}", s.serial)),
                            3 if self.debug.bp_service.delivered <= s.bp_before_ticket
                                && !self.debug.bp_service.pending()
                                && !self
                                    .debug
                                    .breakpoints
                                    .get(&s.source)
                                    .is_some_and(|b| b.contains(&s.line)) =>
                            {
                                Some(format!("debug-gutter-{}", s.line))
                            }
                            4 if self.save_ui.pending.is_none()
                                && self.debug.connection.is_none()
                                && !self.running
                                && self.debug.reason.is_empty() =>
                            {
                                Some("debug".to_string())
                            }
                            13 if !self.debug.expanded.contains(&s.array) => {
                                Some(format!("debug-var-{}", s.array))
                            }
                            14 | 15
                                if !self.debug.loading.contains(&s.array)
                                    && self.debug.variable_more.contains(&s.array)
                                    && self.debug.variables.get(&s.array).is_some_and(|v| {
                                        v.len() == if s.step == 14 { 100 } else { 200 }
                                    }) =>
                            {
                                Some(format!("debug-more-{}", s.array))
                            }
                            17 if self.debug.selected_frame
                                != self.debug.frames[1]["id"].as_u64() =>
                            {
                                Some(format!("debug-frame-{}", self.debug.frames[1]["id"]))
                            }
                            18 if self.debug.selected_frame
                                != self.debug.frames[0]["id"].as_u64() =>
                            {
                                Some(format!("debug-frame-{}", self.debug.frames[0]["id"]))
                            }
                            19 if !self.debug.expanded.contains(&s.child) => {
                                Some(format!("debug-var-{}", s.child))
                            }
                            21 if self.debug.paused && !self.debug.busy => {
                                Some("debug-continue".to_string())
                            }
                            22 if self.tab != 0 => Some("dock-0".to_string()),
                            23 if self.tab != 3 => Some("dock-3".to_string()),
                            24 if !self.debug.paused && !self.debug.busy => {
                                Some("debug-pause".to_string())
                            }
                            27 if s.closing.is_none()
                                && self.debug.connection.is_some()
                                && !self.debug.busy =>
                            {
                                Some("debug-stop".to_string())
                            }
                            _ => None,
                        };
                        if let Some(control) = control {
                            s.retried.push((s.step, control.clone()));
                            s.last_retry = Instant::now();
                            s.pending = Some((control, 0));
                        }
                    }
                    assert!(
                        s.since.elapsed() < Duration::from_secs(if s.step == 4 { 150 } else { 20 }),
                        "debug step {}: {} / {}",
                        s.step,
                        self.status,
                        self.debug.reason
                    );
                    s.next = Instant::now() + Duration::from_millis(50);
                    self.debug_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
            };
        }
        let mut click = None;
        let mut shot = None;
        match s.step {
            0 => {
                wait!(
                    self.devices
                        .iter()
                        .any(|d| d.serial == s.serial && d.usable())
                );
                click = Some("device".to_string());
            }
            1 => {
                wait!(self.chrome.device_open);
                click = Some(format!("serial-{}", s.serial));
            }
            2 => {
                wait!(self.selected.as_ref() == Some(&s.serial) && !self.chrome.device_open);
                wait!(
                    self.chrome
                        .hits
                        .contains_key(&format!("debug-gutter-{}", s.line))
                );
                s.bp_before_ticket = self.debug.bp_service.requested;
                click = Some(format!("debug-gutter-{}", s.line));
            }
            3 => {
                wait!(self.debug.bp_service.delivered > s.bp_before_ticket);
                wait!(!self.debug.bp_service.pending());
                wait!(
                    self.debug
                        .breakpoints
                        .get(&s.source)
                        .is_some_and(|b| b.contains(&s.line))
                );
                shot = Some("01-breakpoint");
                click = Some("debug".into());
            }
            4 => {
                wait!(
                    self.debug.connection.is_some()
                        && self.debug.paused
                        && top(self, "debugProbe")
                        && vars(self, "doubled", "14")
                        && vars(self, "seed", "7")
                );
                assert_eq!(self.last_run.as_ref().unwrap().serial, s.serial);
                assert_eq!(
                    self.last_run.as_ref().unwrap().device.emulator,
                    std::env::var("WEX_DEBUG_EXPECT_EMULATOR").as_deref() == Ok("1")
                );
                assert!(
                    self.debug
                        .verified
                        .get(&s.source)
                        .unwrap()
                        .iter()
                        .any(|b| b.verified)
                );
                s.snapshots.insert("breakpoint".into(), snapshot(self));
                shot = Some("02-paused");
            }
            5 => {
                key(input, Key::F11, egui::Modifiers::NONE);
            }
            6 => {
                wait!(self.debug.paused && top(self, "debugNested") && vars(self, "value", "14"));
                s.snapshots.insert("stepInto".into(), snapshot(self));
                shot = Some("03-step-into");
            }
            7 => {
                key(input, Key::F10, egui::Modifiers::NONE);
            }
            8 => {
                wait!(
                    self.debug.paused && top(self, "debugNested") && vars(self, "adjusted", "17")
                );
                s.snapshots.insert("stepOver".into(), snapshot(self));
                shot = Some("04-step-over");
            }
            9 => {
                key(input, Key::F11, egui::Modifiers::SHIFT);
            }
            10 => {
                wait!(
                    self.debug.paused
                        && top(self, "debugProbe")
                        && vars(self, "->debugNested()", "17")
                );
                s.snapshots.insert("stepOut".into(), snapshot(self));
                shot = Some("05-step-out");
            }
            11 => {
                key(input, Key::F10, egui::Modifiers::NONE);
            }
            12 => {
                wait!(self.debug.paused && vars(self, "answer", "17"));
                s.snapshots.insert("returned".into(), snapshot(self));
                shot = Some("06-returned-value");
                s.array = current_vars(self)
                    .find(|v| v["name"] == "samples")
                    .and_then(|v| v["variablesReference"].as_u64())
                    .unwrap();
                click = Some(format!("debug-var-{}", s.array));
            }
            13 => {
                wait!(
                    self.debug
                        .variables
                        .get(&s.array)
                        .is_some_and(|v| v.len() == 100)
                );
                assert!(self.debug.variable_more.contains(&s.array));
                s.snapshots.insert("array100".into(), snapshot(self));
                shot = Some("07-array-page-1");
                click = Some(format!("debug-more-{}", s.array));
            }
            14 => {
                wait!(
                    self.debug
                        .variables
                        .get(&s.array)
                        .is_some_and(|v| v.len() == 200)
                );
                s.snapshots.insert("array200".into(), snapshot(self));
                shot = Some("08-array-page-2");
                click = Some(format!("debug-more-{}", s.array));
            }
            15 => {
                wait!(
                    self.debug
                        .variables
                        .get(&s.array)
                        .is_some_and(|v| v.len() == 250)
                );
                assert_eq!(self.debug.variables[&s.array][249]["value"], "747");
                assert!(!self.debug.variable_more.contains(&s.array));
                let last = self.debug.variables[&s.array][249]["name"]
                    .as_str()
                    .unwrap();
                let key = format!("debug-value-{}-{last}", s.array);
                wait!(self.chrome.hits.contains_key(&key));
                let rect = self.chrome.hits[&key];
                let view = self.chrome.hits["debug-variable-viewport"];
                if !view.contains(rect.center()) {
                    input.events.push(egui::Event::PointerMoved(view.center()));
                    input.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0., -2000.),
                        modifiers: Default::default(),
                    });
                    s.next = Instant::now() + Duration::from_millis(250);
                    s.scroll_settle = s.next;
                    self.debug_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
                s.snapshots.insert("array250".into(), snapshot(self));
                shot = Some("09-array-complete");
                click = Some(format!("debug-var-{}", s.array));
            }
            16 => {
                wait!(!self.debug.expanded.contains(&s.array));
                let id = self.debug.frames[1]["id"].as_u64().unwrap();
                click = Some(format!("debug-frame-{id}"));
            }
            17 => {
                wait!(
                    self.debug.selected_frame == self.debug.frames[1]["id"].as_u64()
                        && !self.debug.scopes.is_empty()
                );
                wait!(current_vars(self).any(|v| v["name"] == "nativeProof"));
                s.snapshots.insert("caller".into(), snapshot(self));
                shot = Some("10-call-stack");
                let id = self.debug.frames[0]["id"].as_u64().unwrap();
                click = Some(format!("debug-frame-{id}"));
            }
            18 => {
                wait!(
                    self.debug.selected_frame == self.debug.frames[0]["id"].as_u64()
                        && vars(self, "answer", "17")
                );
                s.child = current_vars(self)
                    .find(|v| v["name"] == "this")
                    .and_then(|v| v["variablesReference"].as_u64())
                    .unwrap();
                click = Some(format!("debug-var-{}", s.child));
            }
            19 => {
                wait!(
                    self.debug
                        .variables
                        .get(&s.child)
                        .is_some_and(|v| !v.is_empty())
                );
                let name = self.debug.variables[&s.child][0]["name"].as_str().unwrap();
                let value_key = format!("debug-value-{}-{name}", s.child);
                wait!(self.chrome.hits.contains_key(&value_key));
                let rect = self.chrome.hits[&value_key];
                let view = self.chrome.hits["debug-variable-viewport"];
                if !view.contains(rect.center()) {
                    input.events.push(egui::Event::PointerMoved(view.center()));
                    input.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(
                            0.,
                            (view.center().y - rect.center().y).clamp(-600., 600.),
                        ),
                        modifiers: Default::default(),
                    });
                    s.next = Instant::now() + Duration::from_millis(100);
                    self.debug_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
                s.snapshots.insert("object".into(), snapshot(self));
                shot = Some("11-object-variables");
                self.edit
                    .collapse(self.doc.rope().line_to_char(s.line as usize - 1));
                key(input, Key::F9, egui::Modifiers::NONE);
            }
            20 => {
                wait!(
                    !self.debug.breakpoints.contains_key(&s.source)
                        && self
                            .debug
                            .verified
                            .get(&s.source)
                            .is_some_and(|v| v.is_empty())
                );
                click = Some("debug-continue".into());
            }
            21 => {
                wait!(!self.debug.paused);
                let r = self.last_run.as_ref().unwrap();
                wait!(self.ring.entries().any(|e| e.pid == r.pid
                    && e.message.contains("WEX_DEBUG_OK result=17")
                    && e.message.contains(&r.run_id)));
                self.filter = "WEX_DEBUG_OK".into();
                self.compiled_filter.expression = Some(regex::Regex::new(&self.filter).unwrap());
                click = Some("dock-0".into());
            }
            22 => {
                wait!(self.tab == 0);
                shot = Some("12-logcat");
                click = Some("dock-3".into());
            }
            23 => {
                wait!(self.tab == 3);
                click = Some("debug-pause".into());
            }
            24 => {
                wait!(inspection_ready(self));
                s.snapshots.insert("pause".into(), snapshot(self));
                shot = Some("13-pause");
            }
            25 => {
                if s.closing.is_some() && !s.close_cancel_verified(self, ctx, input) {
                    self.debug_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
                key(input, Key::F5, egui::Modifiers::NONE);
            }
            26 => {
                wait!(!self.debug.paused);
                if s.closing.is_some() {
                    if !s.close_confirmed(self, ctx) {
                        self.debug_smoke = Some(s);
                        ctx.request_repaint_after(Duration::from_millis(16));
                        return;
                    }
                } else {
                    click = Some("debug-stop".into());
                }
            }
            27 => {
                wait!(
                    self.debug.connection.is_none()
                        && !self.debug.busy
                        && self.debug.stop_receipt.is_some()
                );
                assert_eq!(self.debug.stop_receipt.as_ref().unwrap()["pid_gone"], true);
                s.write_report(self);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        if let Some(name) = click {
            s.pending = Some((name, 0));
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.since = Instant::now();
        s.next = Instant::now() + Duration::from_millis(350);
        self.debug_smoke = Some(s);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub(super) fn debug_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.debug_smoke else {
            return;
        };
        if let Some(c) = &mut s.closing {
            c.capture(&s.dir, ctx);
        }
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

impl DebugSmoke {
    fn write_report(&mut self, a: &Workbench) {
        if self.reported {
            return;
        }
        self.close_stopped(a);
        for (p, h) in &self.original {
            assert_eq!(
                &blake3::hash(&std::fs::read(p).unwrap())
                    .to_hex()
                    .to_string(),
                h
            );
        }
        let logs = a
            .ring
            .entries()
            .filter(|e| e.message.contains("WEX_DEBUG_OK"))
            .collect::<Vec<_>>();
        let report = json!({"result":"GUI_ANDROID_DEBUG_OK","serial":self.serial,"run":a.last_run,"snapshots":self.snapshots,"interaction_retries":self.retried,"stop":a.debug.stop_receipt,"screenshots":self.shots,"logcat":logs,"source_hashes":self.original,"disk_source_unchanged":true,"breakpoint_preferences_restored_on_exit":true,"input":"controlled eframe RawInput pointer / keyboard in native GPU window; actual Java Debug Server and device ART","elapsed_ms":self.start.elapsed().as_millis()});
        std::fs::write(
            self.dir.join("debug-window.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        std::fs::write(
            self.dir.join("debug-pointer-audit.json"),
            serde_json::to_vec_pretty(&self.pointer_audit).unwrap(),
        )
        .unwrap();
        eprintln!(
            "GUI_ANDROID_DEBUG_OK: serial {}; gutter breakpoint; stepIn/next/stepOut; stack selection; object variables; array paging 100/100/50 last=747; continue/Logcat/pause/Stop; no Gradle",
            self.serial
        );

        self.reported = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_capture_waits_for_complete_inspection_without_extending_deadline() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("MainActivity.kt");
        std::fs::write(&source, "val answer = debugNested(doubled)\n").unwrap();
        let ctx = egui::Context::default();
        let mut app = Workbench::new(root.path().to_owned(), ctx.clone(), None, false);
        let mut smoke = DebugSmoke::new(root.path().join("proof"), "unit-serial".into(), &mut app);
        smoke.source_ready = true;
        smoke.shown = true;
        smoke.step = 24;
        smoke.next = Instant::now() - Duration::from_secs(1);
        app.debug_smoke = Some(smoke);
        app.debug.paused = true;
        app.debug.thread = 1;
        app.debug.frames = vec![json!({"id":1,"name":"nativePollOnce","line":-1,"column":1})];
        app.debug.selected_frame = Some(1);
        app.debug.scopes_loading = true;
        let started = app.debug_smoke.as_ref().unwrap().start;
        let mut input = egui::RawInput::default();
        let wait = |app: &mut Workbench, input: &mut egui::RawInput| {
            app.debug_smoke.as_mut().unwrap().next = Instant::now() - Duration::from_secs(1);
            app.debug_smoke_input(&ctx, input);
            let s = app.debug_smoke.as_ref().unwrap();
            assert_eq!(
                s.step, 24,
                "incomplete/error inspection must not advance Pause"
            );
            assert!(s.snapshots.is_empty() && s.awaiting.is_none());
            assert_eq!(
                s.start, started,
                "readiness must not reset the global deadline"
            );
        };
        wait(&mut app, &mut input);
        app.debug.scopes_loading = false;
        app.debug.stack_loading = true;
        wait(&mut app, &mut input);
        app.debug.stack_loading = false;
        app.debug.scopes_error = Some("scope failure".into());
        wait(&mut app, &mut input);
        app.debug.scopes_error = None;
        app.debug.stack_error = Some("stack failure".into());
        wait(&mut app, &mut input);
        app.debug.stack_error = None;
        app.debug.scopes = vec![json!({"name":"Locals","variablesReference":42})];
        wait(&mut app, &mut input);
        app.debug.variables.insert(42, vec![]);
        app.debug.loading.insert(42);
        wait(&mut app, &mut input);
        app.debug.loading.clear();
        app.debug
            .variable_errors
            .insert(42, "variables failure".into());
        wait(&mut app, &mut input);
        app.debug.variable_errors.clear();
        app.debug_smoke.as_mut().unwrap().next = Instant::now() - Duration::from_secs(1);
        app.debug_smoke_input(&ctx, &mut input);
        let s = app.debug_smoke.as_ref().unwrap();
        assert_eq!(s.step, 25);
        assert_eq!(s.awaiting.as_deref(), Some("13-pause"));
        assert_eq!(s.snapshots["pause"]["scopes_loading"], false);
        assert_eq!(s.snapshots["pause"]["variables_loading"], json!([]));
        assert_eq!(s.snapshots["pause"]["variable_errors"], json!({}));
        assert_eq!(s.start, started);
    }

    #[test]
    fn smoke_open_waits_for_actual_target_before_cursor_setup() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("A.kt");
        let target = root.path().join("MainActivity.kt");
        let disk = "// initial\nval a = 1\n// short\n";
        std::fs::write(&old, disk).unwrap();
        let text = (0..33)
            .map(|n| format!("// line {n}\n"))
            .collect::<String>()
            + "val answer = debugNested(doubled)\n";
        std::fs::write(&target, &text).unwrap();
        let ctx = egui::Context::default();
        let mut app = Workbench::new(root.path().to_owned(), ctx.clone(), None, false);
        assert_eq!(app.doc.path.as_ref(), Some(&old));
        app.doc.edit(0..0, "// unsaved ").unwrap();
        app.edit.collapse(2);
        let before = app.doc.rope().to_string();
        assert_eq!(app.doc.rope().len_lines(), 4);
        let output = root.path().join("debug-check");
        app.debug_smoke = Some(DebugSmoke::new(
            output,
            "test-explicit-serial".into(),
            &mut app,
        ));
        assert_eq!(app.doc.path.as_ref(), Some(&old));
        assert!(app.open_ui.pending.is_some());
        app.debug_smoke_input(&ctx, &mut egui::RawInput::default());
        assert_eq!(app.doc.rope().to_string(), before);
        assert_eq!(app.edit.cursor, 2);
        let end = Instant::now() + Duration::from_secs(8);
        while app.open_ui.pending.is_some() {
            assert!(
                Instant::now() < end,
                "actual document worker did not finish"
            );
            app.document_open_updates();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.open_ui.error.is_none());
        app.debug_smoke_input(&ctx, &mut egui::RawInput::default());
        assert_eq!(app.doc.path.as_ref(), Some(&target));
        assert_eq!(app.edit.cursor, app.doc.rope().line_to_char(33));
        assert_eq!(app.doc.rope().to_string(), text);
        assert!(!app.doc.dirty());
        assert_eq!(app.parked[&old].0.rope().to_string(), before);
        assert!(app.open_file(old.clone()));
        assert!(app.doc.undo().unwrap());
        assert_eq!(app.doc.rope().to_string(), disk);
        assert_eq!(std::fs::read_to_string(old).unwrap(), disk);
    }
}
