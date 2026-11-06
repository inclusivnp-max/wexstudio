//! Expressive debugger controls. All DAP/JDWP/compiler work stays off the UI thread.
use super::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use wexandroidstudio::{
    debugger::{self, Breakpoint, Breakpoints, Connection as DebugConnection, StopTarget},
    material::{self as m, ButtonKind as Kind, Icon},
};
#[derive(Clone)]
pub(super) enum Reply {
    Control(String),
    Threads,
    Stack(u64),
    Scopes(u64),
    Variables(u64, usize),
    Breakpoints(PathBuf, u64),
}
#[derive(Default)]
pub(super) struct DebugUi {
    pub bp_service: wexandroidstudio::breakpoint_store::Service,
    pub bp_review: super::breakpoint_review_ui::Ui,
    pub bp_report: Option<wexandroidstudio::breakpoint_store::Report>,
    pub bp_launch: Option<super::breakpoint_ui::Launch>,
    pub connection: Option<DebugConnection>,
    pub breakpoints: Breakpoints,
    pub verified: BTreeMap<PathBuf, Vec<Breakpoint>>,
    pub session: u64,
    pub epoch: u64,
    pub paused: bool,
    pub busy: bool,
    pub thread: u64,
    pub threads: Vec<Value>,
    pub frames: Vec<Value>,
    pub stack_loading: bool,
    pub stack_error: Option<String>,
    pub selected_frame: Option<u64>,
    pub scopes: Vec<Value>,
    pub scopes_loading: bool,
    pub scopes_error: Option<String>,
    pub variables: BTreeMap<u64, Vec<Value>>,
    pub expanded: BTreeSet<u64>,
    pub variable_more: BTreeSet<u64>,
    pub variable_totals: BTreeMap<u64, Option<usize>>,
    pub variable_errors: BTreeMap<u64, String>,
    pub loading: BTreeSet<u64>,
    pub execution: Option<(PathBuf, u32)>,
    pub reason: String,
    pub stop_receipt: Option<Value>,
    /// Original device/package/PID/run identity retained when the UI connection is gone.
    pub stop_target: Option<StopTarget>,
    pub stop_error: Option<String>,
    pub built_sources: BTreeMap<PathBuf, String>,
    pub source_changed: bool,
    control_was_paused: bool,
    // setBreakpoints replaces the whole file: never send overlapping replacements.
    breakpoint_clock: u64,
    breakpoint_versions: BTreeMap<PathBuf, u64>,
    breakpoint_inflight: BTreeMap<PathBuf, u64>,
}
impl DebugUi {
    pub fn load(root: &std::path::Path) -> Self {
        Self::with_preferences(root, wexandroidstudio::breakpoint_store::load(root))
    }
    pub fn with_preferences(
        root: &std::path::Path,
        loaded: wexandroidstudio::breakpoint_store::Loaded,
    ) -> Self {
        Self {
            bp_service: wexandroidstudio::breakpoint_store::Service::new(root.to_owned(), loaded),
            ..Self::default()
        }
    }
    pub(super) fn breakpoint_changed(&mut self, path: &std::path::Path) {
        self.breakpoint_clock += 1;
        self.breakpoint_versions
            .insert(path.to_owned(), self.breakpoint_clock);
        self.verified.remove(path);
    }
    fn begin_breakpoint_update(&mut self, path: &std::path::Path) -> Option<(u64, BTreeSet<u32>)> {
        if self.breakpoint_inflight.contains_key(path) {
            return None;
        }
        let version = *self.breakpoint_versions.get(path)?;
        self.breakpoint_inflight.insert(path.to_owned(), version);
        Some((
            version,
            self.breakpoints.get(path).cloned().unwrap_or_default(),
        ))
    }
    fn finish_breakpoint_update(&mut self, path: &std::path::Path, version: u64) -> Option<bool> {
        if self.breakpoint_inflight.get(path) != Some(&version) {
            return None;
        }
        self.breakpoint_inflight.remove(path);
        Some(self.breakpoint_versions.get(path) == Some(&version))
    }
    fn terminated(&mut self) {
        self.connection.take();
        self.session += 1;
        self.clear_stop();
        self.breakpoint_inflight.clear();
        self.verified.clear();
        self.paused = false;
        self.busy = false;
        self.threads.clear();
        self.thread = 0;
        self.reason = "Disconnected".into();
    }
    fn clear_stop(&mut self) {
        self.stop_receipt = None;
        self.stop_error = None;
        self.epoch += 1;
        self.frames.clear();
        self.stack_loading = false;
        self.stack_error = None;
        self.scopes.clear();
        self.scopes_loading = false;
        self.scopes_error = None;
        self.variables.clear();
        self.variable_more.clear();
        self.variable_totals.clear();
        self.variable_errors.clear();
        self.expanded.clear();
        self.loading.clear();
        self.selected_frame = None;
        self.execution = None;
    }
}
/// A non-paging adapter must return the complete stop snapshot. Never present a
/// partial/malformed response as a complete stack, or mutate the last good data.
fn parse_stack(value: &Value) -> Result<Vec<Value>, String> {
    let frames = value["stackFrames"]
        .as_array()
        .ok_or("Adapter omitted stackFrames")?;
    if frames.len() > 10_000 {
        return Err("Call stack exceeds inspection limit (10,000 frames)".into());
    }
    if value
        .get("totalFrames")
        .is_some_and(|v| v.as_u64().is_none())
    {
        return Err("Adapter returned invalid totalFrames".into());
    }
    if value["totalFrames"]
        .as_u64()
        .is_some_and(|n| n != frames.len() as u64)
    {
        return Err("Adapter returned an incomplete call stack; retry inspection".into());
    }
    let mut ids = BTreeSet::new();
    for frame in frames {
        let id = frame["id"]
            .as_u64()
            .ok_or("Adapter returned invalid frame ID")?;
        // Java Debug Server reports ART/native frames with line=-1. Keep the
        // complete stack; these frames have variables but no navigable source line.
        let valid_line = frame["line"]
            .as_i64()
            .is_some_and(|n| (-1..=i64::from(u32::MAX)).contains(&n));
        if !ids.insert(id) || frame["name"].as_str().is_none() || !valid_line {
            return Err("Adapter returned malformed or duplicate stack frame".into());
        }
    }
    Ok(frames.clone())
}
/// Validate inspection snapshots before replacing last-good data. An omitted array
/// is a protocol error, not an empty scope/object. A Java array has a fixed length
/// for the entire stop epoch, so short or oversized known-length pages are errors.
fn parse_scopes(value: &Value) -> Result<Vec<Value>, String> {
    let scopes = value["scopes"].as_array().ok_or("Adapter omitted scopes")?;
    if scopes.len() > 128 {
        return Err("Adapter exceeded scope inspection limit (128)".into());
    }
    for scope in scopes {
        if scope["name"].as_str().is_none() || scope["variablesReference"].as_u64().is_none() {
            return Err("Adapter returned malformed scope".into());
        }
    }
    Ok(scopes.clone())
}
fn parse_variables(
    value: &Value,
    page: Option<(usize, Option<usize>)>,
) -> Result<Vec<Value>, String> {
    let variables = value["variables"]
        .as_array()
        .ok_or("Adapter omitted variables")?;
    if let Some((start, total)) = page {
        let requested = total.map_or(100, |n| n.saturating_sub(start).min(100));
        if variables.len() > requested {
            return Err("Adapter exceeded requested array page".into());
        }
        if total.is_some() && variables.len() != requested {
            return Err("Adapter returned an incomplete array page; retry inspection".into());
        }
    } else if variables.len() > 10_000 {
        return Err("Adapter exceeded variable inspection limit (10,000)".into());
    }
    for variable in variables {
        if variable["name"].as_str().is_none()
            || variable["value"].as_str().is_none()
            || variable["variablesReference"].as_u64().is_none()
            || ["indexedVariables", "namedVariables"]
                .iter()
                .any(|field| variable.get(*field).is_some_and(|v| v.as_u64().is_none()))
        {
            return Err("Adapter returned malformed variable".into());
        }
    }
    Ok(variables.clone())
}

fn navigable_frame_line(frame: &Value) -> Option<u32> {
    frame["line"]
        .as_u64()
        .filter(|n| (1..=u64::from(u32::MAX)).contains(n))
        .map(|n| n as u32)
}
fn variable_viewport(clip: egui::Rect, content: egui::Rect) -> egui::Rect {
    let mut view = clip;
    view.min.x = view.min.x.max(content.min.x);
    view.max.x = view.max.x.min(content.max.x);
    view
}

impl Workbench {
    pub(super) fn toggle_breakpoint(&mut self, path: PathBuf, line: u32) {
        if self.running || self.building || line == 0 {
            return;
        }
        if self.debug.connection.is_some()
            && (self.dirty_path(&path)
                || !self.debug.built_sources.get(&path).is_some_and(|h| {
                    self.doc.path.as_ref() == Some(&path)
                        && self
                            .doc
                            .disk_hash()
                            .is_some_and(|hash| hash.to_hex().as_str() == h)
                }))
        {
            self.status = "Source modified · restart Debug to use edited code".into();
            return;
        }
        if self.doc.path.as_ref() != Some(&path) {
            return;
        }
        self.breakpoint_request(Some(wexandroidstudio::breakpoint_store::Action::Toggle(
            path,
            line,
            self.doc.tracking_capture(),
        )));
    }
    pub(super) fn sync_breakpoints(&mut self, path: PathBuf) {
        if self.debug.source_changed {
            return;
        }
        if self.debug.connection.is_some() && self.debug.bp_report.is_some() {
            let doc = if self.doc.path.as_ref() == Some(&path) {
                Some(&self.doc)
            } else {
                self.parked.get(&path).map(|(d, _)| d)
            };
            if doc.is_some_and(|d| {
                d.dirty()
                    || !self.debug.built_sources.get(&path).is_some_and(|h| {
                        d.disk_hash()
                            .is_some_and(|hash| hash.to_hex().as_str() == h)
                    })
            }) {
                self.debug.source_changed = true;
                self.debug.execution = None;
                self.status = "Source changed · restart Debug before updating breakpoints".into();
                return;
            }
        }
        let Some(client) = self.debug.connection.as_ref().map(|c| c.client.clone()) else {
            return;
        };
        let Some((version, lines)) = self.debug.begin_breakpoint_update(&path) else {
            return;
        };
        let reply = Reply::Breakpoints(path.clone(), version);
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        let session = self.debug.session;
        let epoch = self.debug.epoch;
        self.runtime.spawn(async move {
            let result = debugger::set_breakpoints(&client, &path, &lines)
                .await
                .and_then(|b| Ok(serde_json::to_value(b)?))
                .map_err(|e| e.to_string());
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::DebugReply(session, epoch, reply, result))
            })
            .await;
            ctx.request_repaint();
        });
    }
    pub(super) fn start_debug(&mut self) {
        if self.closing_requested() || self.replace_blocked() || self.git.open {
            return;
        }
        if self.variant != "debug" {
            self.status = "Debug requires debug variant · select Debug in APK variants".into();
            return;
        }
        if self.file_ui.open
            || self.running
            || self.building
            || self.debug.connection.is_some()
            || self.debug.busy
        {
            return;
        }
        let Some(serial) = self
            .selected
            .clone()
            .filter(|s| self.devices.iter().any(|d| &d.serial == s && d.usable()))
        else {
            self.status = "Select a connected device for Debug".into();
            return;
        };
        if !self.save_before(save_ui::After::Debug) {
            return;
        }
        self.chrome.dock_open = true;
        self.tab = 3;
        self.breakpoint_validate_for_debug(serial);
    }
    pub(super) fn launch_debug_anchors(
        &mut self,
        serial: String,
        sources: BTreeMap<PathBuf, String>,
    ) {
        self.stop_logs();
        self.ring.clear();
        self.output.clear();
        self.last_run = None;
        self.running = true;
        self.debug.session += 1;
        self.debug.clear_stop();
        self.debug.verified.clear();
        self.debug.breakpoint_inflight.clear();
        self.debug.threads.clear();
        self.debug.thread = 0;
        self.debug.reason = "Building and attaching…".into();
        self.chrome.dock_open = true;
        self.tab = 3;
        let options = RunOptions {
            serial,
            module: self.app_module.clone(),
            variant: "debug".into(),
            offline: true,
        };
        let root = self.root.clone();
        let bp = self.debug.breakpoints.clone();
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        let session = self.debug.session;
        self.run_task = Some(self.runtime.spawn(async move {
            let result = debugger::launch_checked(
                &root,
                &AdbClient::default(),
                &options,
                &bp,
                &|e| {
                    let update = match e {
                        RunEvent::Phase(s) => Update::Status(s),
                        RunEvent::BuildLine(s) => Update::BuildLine(s),
                    };
                    let _ = tx.try_send(update);
                    ctx.request_repaint();
                },
                &sources,
            )
            .await
            .map_err(|e| format!("Debug failed: {e:#}"));
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::DebugReady(session, Box::new(result)))
            })
            .await;
            ctx.request_repaint();
        }));
    }
    fn debug_request(&mut self, reply: Reply, command: &str, args: Value) {
        let Some(connection) = &self.debug.connection else {
            return;
        };
        let client = connection.client.clone();
        let command = command.to_string();
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        let session = self.debug.session;
        let epoch = self.debug.epoch;
        self.runtime.spawn(async move {
            let result = client
                .request(&command, args)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::DebugReply(session, epoch, reply, result))
            })
            .await;
            ctx.request_repaint();
        });
    }
    pub(super) fn debug_control(&mut self, command: &str) {
        if command == "disconnect" {
            self.stop_debug();
            return;
        }
        if self.debug.connection.is_none() || self.debug.busy {
            return;
        }
        if command != "pause" && !self.debug.paused {
            return;
        }
        if command == "pause" && self.debug.paused {
            return;
        }
        if command == "pause" && self.debug.thread == 0 {
            self.status = "Waiting for debugger threads…".into();
            return;
        }
        self.debug.busy = true;
        self.debug.control_was_paused = self.debug.paused;
        if command != "pause" {
            self.debug.paused = false;
            self.debug.clear_stop();
        }
        self.debug_request(
            Reply::Control(command.into()),
            command,
            json!({"threadId":self.debug.thread}),
        );
    }
    pub(super) fn stop_debug(&mut self) {
        self.debug.bp_launch = None;
        let connection = self.debug.connection.take();
        let target = self.debug.stop_target.clone().or_else(|| {
            connection.as_ref().and_then(|c| {
                self.last_run.as_ref().map(|run| StopTarget {
                    serial: c.serial.clone(),
                    package: c.package.clone(),
                    pid: run.pid,
                    run_id: run.run_id.clone(),
                })
            })
        });
        let Some(target) = target else {
            self.debug.busy = false;
            self.debug.stop_error =
                Some("No owned Android debug target is available to stop".into());
            self.debug.reason = "Stop failed · Retry".into();
            self.status = "Debug stop failed: missing session target".into();
            return;
        };
        self.debug.session += 1;
        self.debug.breakpoint_inflight.clear();
        self.debug.verified.clear();
        let session = self.debug.session;
        self.debug.clear_stop();
        self.debug.stop_target = Some(target.clone());
        self.debug.paused = false;
        self.debug.busy = true;
        self.debug.reason = "Disconnecting…".into();
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.runtime.spawn(async move {
            let dap_error = if let Some(connection) = connection.as_ref() {
                connection
                    .client
                    .request("disconnect", json!({"terminateDebuggee":false}))
                    .await
                    .err()
                    .map(|e| format!("DAP disconnect: {e:#}"))
            } else {
                None
            };
            let result = debugger::stop_app(&AdbClient::default(), &target)
                .await
                .map(|mut receipt| {
                    if let Some(dap) = dap_error {
                        receipt["dap_disconnect_warning"] = json!(dap);
                    }
                    receipt
                })
                .map_err(|e| format!("{e:#}"));
            drop(connection);
            let _ = tokio::task::spawn_blocking(move || {
                tx.send(Update::DebugReply(
                    session,
                    0,
                    Reply::Control("disconnect".into()),
                    result,
                ))
            })
            .await;
            ctx.request_repaint();
        });
    }
    pub(super) fn debug_ready(&mut self, session: u64, result: Result<debugger::Started, String>) {
        if session != self.debug.session {
            return;
        }
        self.running = false;
        match result {
            Ok(started) => {
                self.debug.connection = Some(started.connection);
                self.debug.built_sources = started.source_hashes;
                self.debug.source_changed = false;
                self.debug.verified = started.breakpoints;
                let report = started.deployment.report;
                self.debug.stop_target = Some(StopTarget {
                    serial: report.serial.clone(),
                    package: report.apk.application_id.clone(),
                    pid: report.pid,
                    run_id: report.run_id.clone(),
                });
                self.debug.stop_error = None;
                self.debug.reason = "Running".into();
                self.debug.busy = false;
                self.compiled_filter.pid = Some(report.pid);
                self.attach_logs(
                    Some(started.deployment.logs),
                    report.serial.clone(),
                    Some(report.pid),
                );
                self.last_run = Some(report);
                self.status = "Debugger attached · running".into();
                self.debug_request(Reply::Threads, "threads", json!({}));
            }
            Err(e) => {
                self.debug.reason = e.clone();
                self.status = e;
                self.debug.busy = false;
            }
        }
    }
    pub(super) fn debug_reply(
        &mut self,
        session: u64,
        epoch: u64,
        reply: Reply,
        result: Result<Value, String>,
    ) {
        if session != self.debug.session {
            return;
        }
        if let Reply::Breakpoints(path, version) = reply {
            let Some(current) = self.debug.finish_breakpoint_update(&path, version) else {
                return;
            };
            if current {
                match result.and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())) {
                    Ok(bp) => {
                        self.debug.verified.insert(path.clone(), bp);
                    }
                    Err(e) => {
                        self.status = format!("Breakpoints: {e}");
                    }
                }
            } else {
                // Coalesce any number of toggles to the most recent complete file set.
                self.sync_breakpoints(path);
            }
            return;
        }
        if !matches!(&reply, Reply::Control(c) if c == "disconnect") && epoch != self.debug.epoch {
            return;
        }
        // Ignore late failures as well as successes before touching UI error/loading state.
        if matches!(&reply, Reply::Scopes(frame) if Some(*frame) != self.debug.selected_frame)
            || matches!(&reply, Reply::Variables(id, start) if *start != self.debug.variables.get(id).map_or(0, Vec::len))
        {
            return;
        }
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("Debugger: {e}");
                self.debug.reason = format!("Debug error: {e}");
                if matches!(reply, Reply::Control(_)) {
                    self.debug.busy = false;
                }
                if matches!(reply, Reply::Control(ref command) if command == "disconnect") {
                    self.debug.stop_error = Some(e.clone());
                    self.debug.stop_receipt = None;
                    self.debug.reason = format!("Stop failed · {e} · Retry");
                    self.status = format!("Debug stop failed: {e}; press Retry");
                    return;
                }
                if matches!(reply, Reply::Control(_))
                    && self.debug.control_was_paused
                    && self.debug.connection.is_some()
                {
                    self.debug.paused = true;
                    self.request_debug_stack();
                }
                if let Reply::Stack(thread) = reply
                    && thread == self.debug.thread
                {
                    self.debug.stack_loading = false;
                    self.debug.stack_error = Some(e.clone());
                }
                if let Reply::Scopes(_) = reply {
                    self.debug.scopes_loading = false;
                    self.debug.scopes_error = Some(e.clone());
                }
                if let Reply::Variables(id, _) = reply {
                    self.debug.loading.remove(&id);
                    self.debug.variable_errors.insert(id, e);
                }
                return;
            }
        };
        match reply {
            Reply::Control(command) => {
                self.debug.busy = false;
                if command == "disconnect" {
                    self.debug.stop_receipt = Some(value);
                    self.debug.stop_target = None;
                    self.debug.stop_error = None;
                    self.debug.reason = "Stopped".into();
                    self.status = "Debug session stopped".into();
                }
            }
            Reply::Threads => {
                self.debug.threads = value["threads"].as_array().cloned().unwrap_or_default();
                if !self
                    .debug
                    .threads
                    .iter()
                    .any(|t| t["id"].as_u64() == Some(self.debug.thread))
                {
                    self.debug.thread = self
                        .debug
                        .threads
                        .iter()
                        .find(|t| t["name"].as_str() == Some("main"))
                        .or_else(|| self.debug.threads.first())
                        .and_then(|t| t["id"].as_u64())
                        .unwrap_or(0);
                }
            }
            Reply::Stack(thread) => {
                if thread != self.debug.thread || !self.debug.paused {
                    return;
                }
                self.debug.stack_loading = false;
                match parse_stack(&value) {
                    Ok(frames) => {
                        self.debug.stack_error = None;
                        self.debug.frames = frames;
                        if self.debug.frames.is_empty() {
                            self.debug.selected_frame = None;
                            self.debug.scopes.clear();
                            self.debug.scopes_loading = false;
                            self.debug.scopes_error = None;
                            self.debug.execution = None;
                        }
                        if !self
                            .debug
                            .frames
                            .iter()
                            .any(|f| f["id"].as_u64() == self.debug.selected_frame)
                            && let Some(first) =
                                self.debug.frames.first().and_then(|f| f["id"].as_u64())
                        {
                            self.select_debug_frame(first);
                        }
                    }
                    Err(error) => self.debug.stack_error = Some(error),
                }
            }
            Reply::Scopes(_) => {
                self.debug.scopes_loading = false;
                match parse_scopes(&value) {
                    Ok(scopes) => {
                        self.debug.scopes_error = None;
                        self.debug.scopes = scopes;
                        let refs = self
                            .debug
                            .scopes
                            .iter()
                            .filter_map(|s| s["variablesReference"].as_u64())
                            .filter(|id| *id > 0)
                            .collect::<Vec<_>>();
                        for r in refs {
                            self.debug.expanded.insert(r);
                            self.request_variables(r);
                        }
                    }
                    Err(error) => self.debug.scopes_error = Some(error),
                }
            }
            Reply::Variables(id, start) => {
                self.debug.loading.remove(&id);
                // Insert only an empty slot here; errors retain all previously loaded values.
                self.debug.variables.entry(id).or_default();
                let page = self
                    .debug
                    .variable_totals
                    .get(&id)
                    .map(|total| (start, *total));
                let values = match parse_variables(&value, page) {
                    Ok(values) => values,
                    Err(error) => {
                        self.debug.variable_errors.insert(id, error);
                        return;
                    }
                };
                if let Some((_, total)) = page {
                    if total.map_or(values.len() == 100, |n| start + values.len() < n) {
                        self.debug.variable_more.insert(id);
                    } else {
                        self.debug.variable_more.remove(&id);
                    }
                }
                self.debug.variables.get_mut(&id).unwrap().extend(values);
                self.debug.variable_errors.remove(&id);
            }
            Reply::Breakpoints(..) => unreachable!("breakpoint replies handled before stop epoch"),
        }
    }
    pub(super) fn debug_events(&mut self) {
        let mut events = Vec::new();
        if let Some(c) = &mut self.debug.connection {
            for _ in 0..128 {
                match c.events.try_recv() {
                    Ok(e) => events.push(e),
                    Err(_) => break,
                }
            }
        }
        for e in events {
            match e["event"].as_str() {
                Some("stopped") => {
                    self.debug.clear_stop();
                    self.debug.paused = true;
                    self.debug.busy = false;
                    self.debug.thread = e["body"]["threadId"].as_u64().unwrap_or(0);
                    self.debug.reason = e["body"]["reason"].as_str().unwrap_or("Paused").into();
                    self.tab = 3;
                    self.chrome.dock_open = true;
                    self.status = format!("Paused · {}", self.debug.reason);
                    self.debug_request(Reply::Threads, "threads", json!({}));
                    self.request_debug_stack();
                }
                Some("continued") => {
                    if self.debug.paused {
                        self.debug.paused = false;
                        self.debug.clear_stop();
                    }
                    self.debug.reason = "Running".into();
                }
                Some("breakpoint") => {
                    let b = &e["body"]["breakpoint"];
                    for all in self.debug.verified.values_mut() {
                        for bp in all {
                            if b["id"].as_u64() == bp.id && bp.id.is_some() {
                                bp.verified = b["verified"].as_bool().unwrap_or(false);
                                bp.message = b["message"].as_str().unwrap_or("").into();
                            }
                        }
                    }
                }
                Some("terminated") => {
                    self.debug.terminated();
                    self.status = "Debug adapter/device disconnected".into();
                    break;
                }
                Some("output") => {
                    if let Some(s) = e["body"]["output"].as_str() {
                        if self.output.len() > 2000 {
                            self.output.pop_front();
                        }
                        self.output.push_back(s.chars().take(4000).collect());
                    }
                }
                _ => {}
            }
        }
    }
    fn request_debug_stack(&mut self) {
        if !self.debug.paused || self.debug.stack_loading || self.debug.connection.is_none() {
            return;
        }
        self.debug.stack_loading = true;
        self.debug.stack_error = None;
        // The pinned Java adapter advertises supportsDelayedStackTraceLoading=false.
        // Omit levels/startFrame: a fixed limit silently hides the rest of the stack.
        self.debug_request(
            Reply::Stack(self.debug.thread),
            "stackTrace",
            json!({"threadId":self.debug.thread}),
        );
    }
    fn select_debug_frame(&mut self, id: u64) {
        self.debug.selected_frame = Some(id);
        self.debug.scopes.clear();
        self.debug.scopes_loading = false;
        self.debug.scopes_error = None;
        self.debug.execution = None;
        if let Some(frame) = self
            .debug
            .frames
            .iter()
            .find(|f| f["id"].as_u64() == Some(id))
            .cloned()
            && let Some(p) = frame["source"]["path"].as_str()
        {
            let path = if p.starts_with("file:") {
                url::Url::parse(p).ok().and_then(|u| u.to_file_path().ok())
            } else {
                Some(PathBuf::from(p))
            };
            if let Some(path) = path
                && let Some(line) = navigable_frame_line(&frame)
            {
                self.open_at(
                    path,
                    open_ui::Intent::Debug {
                        session: self.debug.session,
                        epoch: self.debug.epoch,
                        frame: id,
                        line,
                    },
                );
            }
        }
        self.request_debug_scopes();
    }
    fn request_debug_scopes(&mut self) {
        let Some(id) = self.debug.selected_frame else {
            return;
        };
        if !self.debug.paused || self.debug.scopes_loading || self.debug.connection.is_none() {
            return;
        }
        self.debug.scopes_loading = true;
        self.debug.scopes_error = None;
        self.debug_request(Reply::Scopes(id), "scopes", json!({"frameId":id}));
    }
    fn request_variables(&mut self, id: u64) {
        if !self.debug.variables.contains_key(&id) {
            self.request_variable_page(id);
        }
    }
    fn request_variable_page(&mut self, id: u64) {
        if id == 0 || !self.debug.paused || self.debug.loading.contains(&id) {
            return;
        }
        let start = self.debug.variables.get(&id).map_or(0, Vec::len);
        let array = self.debug.variables.values().flatten().find(|v| {
            v["variablesReference"].as_u64() == Some(id)
                && v["type"].as_str().is_some_and(|s| s.ends_with("[]"))
        });
        if let Some(array) = array {
            let total = array["indexedVariables"].as_u64().map(|n| n as usize);
            self.debug.variable_totals.insert(id, total);
        }
        let args = if let Some(total) = self.debug.variable_totals.get(&id) {
            let count = total.map_or(100, |n| n.saturating_sub(start).min(100));
            if count == 0 {
                self.debug.variables.entry(id).or_default();
                self.debug.variable_more.remove(&id);
                return;
            }
            json!({"variablesReference":id,"filter":"indexed","start":start,"count":count})
        } else {
            if start > 0 {
                return;
            }
            json!({"variablesReference":id})
        };
        self.debug.loading.insert(id);
        self.debug.variable_errors.remove(&id);
        self.debug_request(Reply::Variables(id, start), "variables", args);
    }
    pub(super) fn debug_shortcuts(&mut self, ctx: &egui::Context) {
        if self.replace_blocked() || self.git.open {
            return;
        }
        if self.file_ui.open
            || self.projects.open
            || self.variant_open
            || self.refactor.dialog
            || self.chrome.settings
            || self.chrome.device_open
            || self.pending_close.is_some()
            || self.close_ui.open
            || self.editor_ui.pending_reload.is_some()
        {
            return;
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F9)
            && let Some(path) = self.doc.path.clone()
        {
            self.toggle_breakpoint(
                path,
                self.doc.rope().char_to_line(self.edit.cursor) as u32 + 1,
            );
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::SHIFT, Key::F5) {
            self.stop_debug();
        } else if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F5) {
            if self.debug.connection.is_some() {
                self.debug_control("continue");
            } else {
                self.start_debug();
            }
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F6) {
            self.debug_control("pause");
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F10) {
            self.debug_control("next");
        }
        if Self::consume_exact_key(ctx, egui::Modifiers::SHIFT, Key::F11) {
            self.debug_control("stepOut");
        } else if Self::consume_exact_key(ctx, egui::Modifiers::NONE, Key::F11) {
            self.debug_control("stepIn");
        }
    }
    pub(super) fn debug_panel(&mut self, ui: &mut egui::Ui) {
        self.breakpoint_panel(ui);
        let active = self.debug.connection.is_some();
        let paused = self.debug.paused;
        let ready = active && !self.debug.busy;
        ui.horizontal_wrapped(|ui| {
            for (key, label, icon, command, enabled) in [
                (
                    "debug-continue",
                    "",
                    Icon::Play,
                    "continue",
                    ready && paused,
                ),
                (
                    "debug-pause",
                    "",
                    Icon::Pause,
                    "pause",
                    ready && !paused && self.debug.thread != 0,
                ),
                ("debug-next", "", Icon::StepOver, "next", ready && paused),
                ("debug-in", "", Icon::StepInto, "stepIn", ready && paused),
                ("debug-out", "", Icon::StepOut, "stepOut", ready && paused),
                ("debug-stop", "", Icon::Close, "disconnect", active),
            ] {
                if self
                    .chrome
                    .button(
                        ui,
                        key,
                        label,
                        icon,
                        if command == "continue" {
                            Kind::Filled
                        } else {
                            Kind::Tonal
                        },
                        enabled,
                        40.,
                        self.reduced_motion,
                    )
                    .on_hover_text_passive(match command {
                        "continue" => "Continue · F5",
                        "pause" => "Pause · F6",
                        "next" => "Step over · F10",
                        "stepIn" => "Step into · F11",
                        "stepOut" => "Step out · Shift+F11",
                        _ => "Stop · Shift+F5",
                    })
                    .clicked()
                {
                    self.debug_control(command);
                }
            }
            if self.debug.stop_error.is_some()
                && self
                    .chrome
                    .button(
                        ui,
                        "debug-stop-retry",
                        "Retry",
                        Icon::Refresh,
                        Kind::Tonal,
                        !self.debug.busy && self.debug.stop_target.is_some(),
                        80.,
                        self.reduced_motion,
                    )
                    .on_hover_text_passive("Retry stopping the original serial/package/PID session")
                    .clicked()
            {
                self.stop_debug();
            }
            ui.label(RichText::new(&self.debug.reason).color(if paused {
                m::PRIMARY
            } else {
                m::MUTED
            }));
            if active && (self.doc.dirty() || self.debug.source_changed) {
                ui.colored_label(m::ERROR, "Source edited · restart to rebuild");
            }
        });
        if !active {
            ui.label("Set breakpoints in the left gutter (or F9), then Debug · F5.");
            return;
        }
        let old = self.debug.thread;
        egui::ComboBox::from_id_salt("debug-thread")
            .selected_text(
                self.debug
                    .threads
                    .iter()
                    .find(|t| t["id"].as_u64() == Some(old))
                    .and_then(|t| t["name"].as_str())
                    .unwrap_or("Thread"),
            )
            .show_ui(ui, |ui| {
                for t in &self.debug.threads {
                    if let Some(id) = t["id"].as_u64() {
                        ui.selectable_value(
                            &mut self.debug.thread,
                            id,
                            t["name"].as_str().unwrap_or("Thread"),
                        );
                    }
                }
            });
        if old != self.debug.thread && paused {
            self.debug.clear_stop();
            self.request_debug_stack();
        }
        if !paused {
            ui.label("Application running · Pause to inspect frames and variables");
            return;
        }
        let height = ui.available_height().max(40.);
        ui.columns(2, |cols| {
            self.debug_stack_panel(&mut cols[0], height);
            cols[1].label(RichText::new("Variables").size(15.).color(m::TEXT));
            let mut visible = egui::Rect::NOTHING;
            egui::ScrollArea::vertical()
                .id_salt("debug-variables")
                .max_height(height - 25.)
                .show(&mut cols[1], |ui| {
                    visible = variable_viewport(ui.clip_rect(), ui.max_rect());
                    self.debug_scope_status(ui);
                    for scope in self.debug.scopes.clone() {
                        ui.label(
                            RichText::new(scope["name"].as_str().unwrap_or("Locals"))
                                .color(m::PRIMARY),
                        );
                        if let Some(id) = scope["variablesReference"].as_u64() {
                            self.variable_rows(ui, id, 0);
                        }
                    }
                });
            self.chrome
                .hits
                .insert("debug-variable-viewport".into(), visible);
        });
    }
    fn debug_scope_status(&mut self, ui: &mut egui::Ui) {
        if self.debug.scopes_loading {
            ui.label("Loading scopes…");
        }
        if let Some(error) = &self.debug.scopes_error {
            ui.colored_label(m::ERROR, error);
            if self
                .chrome
                .button(
                    ui,
                    "debug-scopes-retry",
                    "Retry variables",
                    Icon::Debug,
                    Kind::Tonal,
                    !self.debug.scopes_loading,
                    32.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.request_debug_scopes();
            }
        }
    }
    fn debug_stack_panel(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.label(
            RichText::new(format!("Call stack · {} frames", self.debug.frames.len()))
                .size(15.)
                .color(m::TEXT),
        );
        if self.debug.stack_loading {
            ui.label("Loading call stack…");
        }
        if let Some(error) = &self.debug.stack_error {
            ui.colored_label(m::ERROR, error);
            if self
                .chrome
                .button(
                    ui,
                    "debug-stack-retry",
                    "Retry call stack",
                    Icon::Debug,
                    Kind::Tonal,
                    !self.debug.stack_loading,
                    32.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.request_debug_stack();
            }
        }
        let mut selected = None;
        let row_height = ui.text_style_height(&egui::TextStyle::Body).max(20.);
        egui::ScrollArea::vertical()
            .id_salt("debug-stack")
            .max_height(ui.available_height().max(30.).min(height - 25.))
            .show_rows(ui, row_height, self.debug.frames.len(), |ui, rows| {
                for row in rows {
                    let f = &self.debug.frames[row];
                    let id = f["id"].as_u64().unwrap_or(0);
                    let location = navigable_frame_line(f)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "no source line".into());
                    let label = format!("{} : {location}", f["name"].as_str().unwrap_or("frame"));
                    let r = ui.add_sized(
                        [ui.available_width(), row_height],
                        egui::Button::selectable(self.debug.selected_frame == Some(id), label)
                            .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                    self.chrome.hits.insert(format!("debug-frame-{id}"), r.rect);
                    if r.clicked() {
                        selected = Some(id);
                    }
                }
            });
        if let Some(id) = selected {
            self.select_debug_frame(id);
        }
    }
    fn variable_rows(&mut self, ui: &mut egui::Ui, id: u64, depth: usize) {
        if depth > 6 {
            ui.label("Expand deeper in a new inspection");
            return;
        }
        let values = self.debug.variables.get(&id).cloned().unwrap_or_default();
        if self.debug.loading.contains(&id) {
            ui.label("Loading…");
        }
        for variable in values {
            let reference = variable["variablesReference"].as_u64().unwrap_or(0);
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 12.);
                if reference > 0 {
                    let opened = self.debug.expanded.contains(&reference);
                    let r = self.chrome.button(
                        ui,
                        &format!("debug-var-{reference}"),
                        "",
                        if opened { Icon::Expand } else { Icon::Chevron },
                        Kind::Ghost,
                        true,
                        28.,
                        self.reduced_motion,
                    );
                    if r.clicked() {
                        if opened {
                            self.debug.expanded.remove(&reference);
                        } else {
                            self.debug.expanded.insert(reference);
                            self.request_variables(reference);
                        }
                    }
                } else {
                    ui.add_space(18.);
                }
                ui.label(RichText::new(variable["name"].as_str().unwrap_or("?")).color(m::PRIMARY));
                let value_response = ui
                    .label(RichText::new(variable["value"].as_str().unwrap_or("null")).monospace())
                    .on_hover_text_passive(variable["type"].as_str().unwrap_or(""));
                self.chrome.hits.insert(
                    format!(
                        "debug-value-{id}-{}",
                        variable["name"].as_str().unwrap_or("?")
                    ),
                    value_response.rect,
                );
            });
            if reference > 0 && self.debug.expanded.contains(&reference) {
                self.variable_rows(ui, reference, depth + 1);
            }
        }
        if let Some(error) = self.debug.variable_errors.get(&id) {
            ui.colored_label(m::ERROR, error);
        }
        let more = self.debug.variable_more.contains(&id);
        let retry = self.debug.variable_errors.contains_key(&id);
        if more || retry {
            let count = self.debug.variables.get(&id).map_or(0, Vec::len);
            let total = self.debug.variable_totals.get(&id).copied().flatten();
            if more {
                ui.label(total.map_or_else(
                    || format!("{count} loaded"),
                    |n| format!("{count} / {n} elements"),
                ));
            }
            if self
                .chrome
                .button(
                    ui,
                    &format!("debug-more-{id}"),
                    if retry {
                        "Retry variables"
                    } else {
                        "Load next 100"
                    },
                    Icon::Expand,
                    Kind::Tonal,
                    !self.debug.loading.contains(&id),
                    32.,
                    self.reduced_motion,
                )
                .clicked()
            {
                self.request_variable_page(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "class Main {\n val value=7\n}\n").unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let a = Workbench::new(d.path().to_owned(), ctx, None, false);
        (d, a)
    }
    fn stack(count: usize) -> Value {
        json!({"totalFrames":count,"stackFrames":(0..count).map(|i|json!({"id":i+1,"name":format!("recursive({i})"),"line":i+1})).collect::<Vec<_>>()})
    }
    #[test]
    fn variable_viewport_preserves_clipped_y_even_when_content_scrolls_above_it() {
        let clip = egui::Rect::from_min_max(egui::pos2(0., 700.), egui::pos2(1400., 850.));
        let content = egui::Rect::from_min_max(egui::pos2(870., -4000.), egui::pos2(1370., 900.));
        let view = variable_viewport(clip, content);
        assert_eq!(
            view,
            egui::Rect::from_min_max(egui::pos2(870., 700.), egui::pos2(1370., 850.))
        );
        assert!(!view.contains(egui::pos2(900., 875.)));
    }
    #[test]
    fn genuine_empty_stack_clears_previous_frame_scopes_and_execution() {
        let (_d, mut a) = app();
        a.debug.paused = true;
        a.debug.thread = 7;
        a.debug.frames = parse_stack(&stack(1)).unwrap();
        a.debug.selected_frame = Some(1);
        a.debug.scopes = vec![json!({"name":"Locals","variablesReference":9})];
        a.debug.scopes_loading = true;
        a.debug.scopes_error = Some("old error".into());
        a.debug.execution = Some((a.doc.path.clone().unwrap(), 2));
        a.debug_reply(0, 0, Reply::Stack(7), Ok(stack(0)));
        assert!(a.debug.frames.is_empty() && a.debug.selected_frame.is_none());
        assert!(a.debug.scopes.is_empty() && a.debug.execution.is_none());
        assert!(!a.debug.scopes_loading && a.debug.scopes_error.is_none());
        a.debug_reply(
            0,
            0,
            Reply::Scopes(1),
            Ok(json!({"scopes":[{"name":"stale","variablesReference":10}]})),
        );
        assert!(a.debug.scopes.is_empty());
    }
    #[test]
    fn malformed_variables_are_errors_not_empty_success_and_retry_is_atomic() {
        let (_d, mut a) = app();
        let original = vec![json!({"name":"0","value":"7","variablesReference":0})];
        a.debug.variables.insert(9, original.clone());
        a.debug.variable_totals.insert(9, Some(3));
        a.debug.variable_more.insert(9);
        for bad in [
            json!({}),
            json!({"variables":null}),
            json!({"variables":[{"name":"1","value":"8","variablesReference":0}]}),
            json!({"variables":[{"name":"1","value":"8","variablesReference":0}, {"name":"2","value":"9","variablesReference":-1}]}),
        ] {
            a.debug.loading.insert(9);
            a.debug_reply(0, 0, Reply::Variables(9, 1), Ok(bad));
            assert_eq!(a.debug.variables[&9], original);
            assert!(a.debug.variable_errors.contains_key(&9));
            assert!(a.debug.variable_more.contains(&9) && !a.debug.loading.contains(&9));
        }
        a.debug_reply(
            0,
            0,
            Reply::Variables(9, 1),
            Ok(json!({"variables":[
            {"name":"1","value":"8","variablesReference":0},
            {"name":"2","value":"9","variablesReference":0}]})),
        );
        assert_eq!(a.debug.variables[&9].len(), 3);
        assert!(!a.debug.variable_errors.contains_key(&9) && !a.debug.variable_more.contains(&9));
    }
    #[test]
    fn final_array_page_must_match_actual_requested_count() {
        let var = json!({"name":"item","value":"7","variablesReference":0});
        for count in [0, 49, 51, 100] {
            assert!(
                parse_variables(
                    &json!({"variables":vec![var.clone();count]}),
                    Some((200, Some(250)))
                )
                .is_err()
            );
        }
        assert_eq!(
            parse_variables(&json!({"variables":vec![var;50]}), Some((200, Some(250))))
                .unwrap()
                .len(),
            50
        );
        assert!(
            parse_variables(&json!({"variables":[]}), Some((250, Some(250))))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn inspection_snapshot_required_fields_and_budgets_are_validated() {
        for variable in [
            json!({"name":"x"}),
            json!({"name":"x","value":7,"variablesReference":0}),
            json!({"name":"x","value":"7","variablesReference":0,"indexedVariables":-1}),
        ] {
            assert!(parse_variables(&json!({"variables":[variable]}), None).is_err());
        }
        let var = json!({"name":"x","value":"7","variablesReference":0});
        assert!(parse_variables(&json!({"variables":vec![var;10001]}), None).is_err());
        for scope in [
            json!({"name":"Locals"}),
            json!({"name":"Locals","variablesReference":-1}),
        ] {
            assert!(parse_scopes(&json!({"scopes":[scope]})).is_err());
        }
        assert!(
            parse_scopes(
                &json!({"scopes":vec![json!({"name":"Locals","variablesReference":7});129]})
            )
            .is_err()
        );
        assert!(parse_scopes(&json!({"scopes":[]})).unwrap().is_empty());
        assert!(
            parse_variables(&json!({"variables":[]}), None)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn late_frame_errors_and_duplicate_page_errors_do_not_mutate_current_inspection() {
        let (_d, mut a) = app();
        a.debug.selected_frame = Some(2);
        a.debug.scopes_loading = true;
        a.debug.reason = "Paused".into();
        a.debug.variables.insert(
            9,
            vec![json!({"name":"0","value":"7","variablesReference":0})],
        );
        a.debug.loading.insert(9);
        a.debug_reply(0, 0, Reply::Scopes(1), Err("late old frame".into()));
        a.debug_reply(0, 0, Reply::Variables(9, 0), Err("late old page".into()));
        assert_eq!(a.debug.reason, "Paused");
        assert!(a.debug.scopes_loading && a.debug.scopes_error.is_none());
        assert!(a.debug.loading.contains(&9) && a.debug.variable_errors.is_empty());
    }
    #[test]
    fn scope_error_retains_last_good_and_valid_retry_clears_it() {
        let (_d, mut a) = app();
        a.debug.selected_frame = Some(1);
        let original = vec![json!({"name":"Locals","variablesReference":7})];
        a.debug.scopes = original.clone();
        a.debug.scopes_loading = true;
        a.debug_reply(0, 0, Reply::Scopes(1), Ok(json!({})));
        assert_eq!(a.debug.scopes, original);
        assert!(a.debug.scopes_error.is_some() && !a.debug.scopes_loading);
        a.debug_reply(0, 0, Reply::Scopes(1), Err("frame unavailable".into()));
        assert_eq!(a.debug.scopes_error.as_deref(), Some("frame unavailable"));
        a.debug_reply(0, 0, Reply::Scopes(1), Ok(json!({"scopes":original})));
        assert_eq!(a.debug.scopes, original);
        assert!(a.debug.scopes_error.is_none());
        a.debug.clear_stop();
        assert!(
            a.debug.scopes.is_empty() && !a.debug.scopes_loading && a.debug.scopes_error.is_none()
        );
    }
    #[test]
    fn scope_retry_uses_expressive_control_and_respects_loading() {
        let (_d, mut a) = app();
        a.debug.scopes_error = Some("frame unavailable".into());
        let ctx = a.context.clone();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| a.debug_scope_status(ui));
        });
        assert!(a.chrome.hits.contains_key("debug-scopes-retry"));
        a.debug.scopes_loading = true;
        a.request_debug_scopes();
        assert!(a.debug.scopes_loading);
    }
    #[test]
    fn android_native_negative_line_retains_complete_stack_and_source_frame() {
        let (_d, mut a) = app();
        a.debug.thread = 7;
        a.debug.paused = true;
        let mut value = stack(19);
        value["stackFrames"][16] = json!({"id":17,"name":"Method.invoke(Object,Object[])[native method]","line":-1,"column":1,"presentationHint":"subtle"});
        a.debug_reply(0, 0, Reply::Stack(7), Ok(value));
        assert_eq!(a.debug.frames.len(), 19);
        assert!(a.debug.stack_error.is_none());
        assert_eq!(a.debug.frames[16]["line"], -1);
        assert_eq!(navigable_frame_line(&a.debug.frames[16]), None);
        assert_eq!(navigable_frame_line(&a.debug.frames[0]), Some(1));
    }
    #[test]
    fn unknown_source_line_cannot_navigate_to_line_one_and_malformed_coordinates_are_rejected() {
        for line in [json!(-1), json!(0)] {
            let value = json!({"id":1,"name":"frame","line":line});
            assert!(parse_stack(&json!({"stackFrames":[value]})).is_ok());
            assert_eq!(navigable_frame_line(&value), None);
        }
        for line in [
            json!(-2),
            json!(4294967296u64),
            json!(1.5),
            json!("1"),
            Value::Null,
        ] {
            assert!(
                parse_stack(&json!({"stackFrames":[{"id":1,"name":"frame","line":line}]})).is_err()
            );
        }
    }
    #[test]
    fn complete_stack_retains_frames_beyond_fifty_and_current_selection() {
        let (_d, mut a) = app();
        a.debug.thread = 7;
        a.debug.paused = true;
        a.debug.stack_loading = true;
        a.debug.selected_frame = Some(62);
        a.debug.scopes = vec![json!({"name":"caller locals","variablesReference":4})];
        a.debug_reply(0, 0, Reply::Stack(7), Ok(stack(73)));
        assert_eq!(a.debug.frames.len(), 73);
        assert_eq!(a.debug.selected_frame, Some(62));
        assert_eq!(a.debug.scopes[0]["variablesReference"], 4);
        assert!(!a.debug.stack_loading && a.debug.stack_error.is_none());
    }
    #[test]
    fn incomplete_or_malformed_stack_is_visible_and_preserves_last_good_frames() {
        let (_d, mut a) = app();
        a.debug.thread = 7;
        a.debug.paused = true;
        let original = parse_stack(&stack(73)).unwrap();
        a.debug.frames = original.clone();
        for bad in [
            json!({}),
            json!({"totalFrames":80,"stackFrames":original}),
            json!({"totalFrames":1,"stackFrames":original}),
            json!({"stackFrames":[{"id":1,"line":4}]}),
            json!({"totalFrames":-1,"stackFrames":[]}),
        ] {
            a.debug.stack_loading = true;
            a.debug_reply(0, 0, Reply::Stack(7), Ok(bad));
            assert!(a.debug.stack_error.is_some() && !a.debug.stack_loading);
            assert_eq!(a.debug.frames, original);
        }
        a.debug.stack_loading = true;
        a.debug_reply(0, 0, Reply::Stack(7), Err("device offline".into()));
        assert_eq!(a.debug.stack_error.as_deref(), Some("device offline"));
        assert_eq!(a.debug.frames, original);
    }
    #[test]
    fn stale_stop_or_other_thread_stack_reply_cannot_reset_current_loading() {
        let (_d, mut a) = app();
        a.debug.epoch = 2;
        a.debug.thread = 7;
        a.debug.paused = true;
        a.debug.stack_loading = true;
        a.debug_reply(0, 1, Reply::Stack(7), Ok(stack(73)));
        a.debug_reply(0, 2, Reply::Stack(9), Ok(stack(73)));
        a.debug_reply(0, 1, Reply::Stack(7), Err("late error".into()));
        assert!(
            a.debug.stack_loading && a.debug.frames.is_empty() && a.debug.stack_error.is_none()
        );
        a.debug.clear_stop();
        assert!(!a.debug.stack_loading && a.debug.stack_error.is_none());
    }
    #[test]
    fn stack_snapshot_limits_and_duplicate_ids_are_checked() {
        let mut duplicate = stack(2);
        duplicate["stackFrames"][1]["id"] = json!(1);
        assert!(parse_stack(&duplicate).is_err());
        assert!(parse_stack(&stack(10_001)).unwrap_err().contains("10,000"));
        assert_eq!(parse_stack(&json!({"stackFrames":[]})).unwrap().len(), 0);
    }
    #[test]
    fn ten_thousand_stack_frames_paint_only_viewport_and_expose_expressive_retry() {
        let (_d, mut a) = app();
        a.debug.frames = parse_stack(&stack(10_000)).unwrap();
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760., 560.),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| a.debug_stack_panel(ui, 300.));
            },
        );
        let painted = a
            .chrome
            .hits
            .keys()
            .filter(|k| k.starts_with("debug-frame-"))
            .count();
        assert!(painted > 0 && painted < 30, "painted {painted}");
        a.debug.stack_error = Some("device offline".into());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| a.debug_stack_panel(ui, 300.));
        });
        assert!(a.chrome.hits.contains_key("debug-stack-retry"));
    }
    #[test]
    fn array_pages_append_in_order_and_stop_at_known_total() {
        let (_d, mut a) = app();
        a.debug.variable_totals.insert(9, Some(250));
        let page = |start: usize, count: usize| json!({"variables":(start..start+count).map(|i| json!({"name":i.to_string(),"value":(i*3).to_string(),"variablesReference":0})).collect::<Vec<_>>()});
        for (start, count) in [(0, 100), (100, 100), (200, 50)] {
            a.debug_reply(0, 0, Reply::Variables(9, start), Ok(page(start, count)));
            assert_eq!(a.debug.variables[&9].len(), start + count);
            assert_eq!(a.debug.variable_more.contains(&9), start + count < 250);
        }
        assert_eq!(a.debug.variables[&9][249]["value"], "747");
        a.debug_reply(0, 0, Reply::Variables(9, 100), Ok(page(100, 100)));
        assert_eq!(a.debug.variables[&9].len(), 250);
    }
    #[test]
    fn empty_array_expands_without_out_of_range_request_or_stuck_loading() {
        let (_d, mut a) = app();
        a.debug.paused = true;
        a.debug.variables.insert(
            1,
            vec![
                json!({"name":"empty","type":"int[]","indexedVariables":0,"variablesReference":9}),
            ],
        );
        a.request_variables(9);
        assert!(a.debug.variables[&9].is_empty());
        assert!(!a.debug.loading.contains(&9) && !a.debug.variable_more.contains(&9));
    }
    #[test]
    fn failed_array_page_keeps_loaded_values_and_exposes_retry() {
        let (_d, mut a) = app();
        a.debug
            .variables
            .insert(9, vec![json!({"name":"0","value":"0"})]);
        a.debug.variable_more.insert(9);
        a.debug.loading.insert(9);
        a.debug_reply(0, 0, Reply::Variables(9, 1), Err("object collected".into()));
        assert_eq!(a.debug.variables[&9].len(), 1);
        assert!(a.debug.variable_more.contains(&9));
        assert_eq!(a.debug.variable_errors[&9], "object collected");
        assert!(!a.debug.loading.contains(&9));
        a.debug.clear_stop();
        assert!(a.debug.variable_errors.is_empty() && a.debug.variable_more.is_empty());
    }
    #[test]
    fn oversized_array_page_is_not_silently_accepted() {
        let (_d, mut a) = app();
        a.debug.variable_totals.insert(9, None);
        a.debug_reply(
            0,
            0,
            Reply::Variables(9, 0),
            Ok(json!({"variables":vec![json!({"name":"x"});101]})),
        );
        assert!(a.debug.variables[&9].is_empty());
        assert!(a.debug.variable_errors[&9].contains("exceeded"));
    }
    #[test]
    fn paging_and_retry_controls_use_expressive_buttons() {
        let (_d, mut a) = app();
        a.debug.variable_more.insert(9);
        a.debug.variable_totals.insert(9, Some(250));
        a.debug
            .variables
            .insert(9, vec![json!({"name":"0","value":"0"})]);
        let ctx = a.context.clone();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| a.variable_rows(ui, 9, 0));
        });
        assert!(a.chrome.hits.contains_key("debug-more-9"));
        a.debug.variable_more.clear();
        a.debug.variable_errors.insert(9, "offline".into());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| a.variable_rows(ui, 9, 0));
        });
        assert!(a.chrome.hits.contains_key("debug-more-9"));
    }
    #[test]
    fn failed_stop_reports_error_and_unlocks_without_false_pid_receipt() {
        let (_d, mut a) = app();
        a.debug.session = 5;
        a.debug.epoch = 8;
        a.debug.busy = true;
        a.debug.stop_target = Some(StopTarget {
            serial: "emulator-5580".into(),
            package: "dev.example.app".into(),
            pid: 1234,
            run_id: "run-failing".into(),
        });
        a.debug_reply(
            5,
            0,
            Reply::Control("disconnect".into()),
            Err("device offline".into()),
        );
        assert!(!a.debug.busy);
        assert!(a.debug.stop_receipt.is_none());
        assert!(a.debug.reason.contains("device offline"));
        assert!(a.debug.stop_error.as_deref() == Some("device offline"));
        assert!(a.debug.stop_target.is_some());
    }
    #[test]
    fn stale_disconnect_success_cannot_clear_a_new_stop_target() {
        let (_d, mut a) = app();
        a.debug.session = 9;
        a.debug.stop_target = Some(StopTarget {
            serial: "emulator-5580".into(),
            package: "dev.example.app".into(),
            pid: 1234,
            run_id: "run-new".into(),
        });
        a.debug_reply(
            8,
            0,
            Reply::Control("disconnect".into()),
            Ok(json!({"pid_gone": true})),
        );
        assert!(a.debug.stop_receipt.is_none());
        assert!(a.debug.stop_target.is_some());
    }
    #[test]
    fn thread_refresh_prefers_main_and_preserves_existing_selection() {
        let (_d, mut a) = app();
        let threads = json!({"threads":[{"id":8,"name":"worker"},{"id":7,"name":"main"}]});
        a.debug_reply(0, 0, Reply::Threads, Ok(threads.clone()));
        assert_eq!(a.debug.thread, 7);
        a.debug.thread = 8;
        a.debug_reply(0, 0, Reply::Threads, Ok(threads));
        assert_eq!(a.debug.thread, 8);
        a.debug_reply(0, 0, Reply::Threads, Ok(json!({"threads":[]})));
        assert_eq!(a.debug.thread, 0);
    }
    #[test]
    fn rapid_breakpoint_toggles_coalesce_to_one_latest_replacement() {
        let p = PathBuf::from("Main.kt");
        let mut d = DebugUi::default();
        d.breakpoints.insert(p.clone(), BTreeSet::from([2]));
        d.breakpoint_changed(&p);
        let (first, lines) = d.begin_breakpoint_update(&p).unwrap();
        assert_eq!(lines, BTreeSet::from([2]));
        for lines in [BTreeSet::from([2, 3]), BTreeSet::from([3]), BTreeSet::new()] {
            d.breakpoints.insert(p.clone(), lines);
            d.breakpoint_changed(&p);
            assert!(d.begin_breakpoint_update(&p).is_none());
        }
        assert_eq!(d.finish_breakpoint_update(&p, first), Some(false));
        let (last, lines) = d.begin_breakpoint_update(&p).unwrap();
        assert!(last > first && lines.is_empty());
        assert_eq!(d.finish_breakpoint_update(&p, last), Some(true));
        assert!(d.breakpoint_inflight.is_empty());
    }
    #[test]
    fn duplicate_breakpoint_reply_does_not_acknowledge_a_newer_request() {
        let p = PathBuf::from("Main.kt");
        let mut d = DebugUi::default();
        d.breakpoint_changed(&p);
        let (first, _) = d.begin_breakpoint_update(&p).unwrap();
        d.breakpoint_changed(&p);
        assert_eq!(d.finish_breakpoint_update(&p, first), Some(false));
        let (next, _) = d.begin_breakpoint_update(&p).unwrap();
        assert_eq!(d.finish_breakpoint_update(&p, first), None);
        assert_eq!(d.breakpoint_inflight[&p], next);
    }
    #[test]
    fn breakpoint_replacements_are_independent_across_files() {
        let mut d = DebugUi::default();
        let a = PathBuf::from("A.kt");
        let b = PathBuf::from("B.kt");
        d.breakpoint_changed(&a);
        d.breakpoint_changed(&b);
        let (av, _) = d.begin_breakpoint_update(&a).unwrap();
        let (bv, _) = d.begin_breakpoint_update(&b).unwrap();
        assert_eq!(d.finish_breakpoint_update(&b, bv), Some(true));
        assert_eq!(d.breakpoint_inflight[&a], av);
    }
    #[test]
    fn stale_successful_control_reply_does_not_unlock_a_newer_step() {
        let (_d, mut a) = app();
        a.debug.session = 5;
        a.debug.epoch = 8;
        a.debug.busy = true;
        a.debug_reply(5, 7, Reply::Control("next".into()), Ok(json!({})));
        assert!(a.debug.busy);
        a.debug_reply(5, 8, Reply::Control("next".into()), Ok(json!({})));
        assert!(!a.debug.busy);
    }
    #[test]
    fn failed_variable_inspection_does_not_unlock_a_pending_control() {
        let (_d, mut a) = app();
        a.debug.busy = true;
        a.debug.loading.insert(7);
        a.debug_reply(
            0,
            0,
            Reply::Variables(7, 0),
            Err("object unavailable".into()),
        );
        assert!(a.debug.busy);
        assert!(!a.debug.loading.contains(&7));
    }
    #[test]
    fn terminated_session_rejects_late_threads_and_controls() {
        let (_d, mut a) = app();
        let old_session = a.debug.session;
        a.debug.terminated();
        let epoch = a.debug.epoch;
        a.debug_reply(
            old_session,
            epoch,
            Reply::Threads,
            Ok(json!({"threads":[{"id":7}]})),
        );
        a.debug_reply(
            old_session,
            epoch,
            Reply::Control("disconnect".into()),
            Ok(json!({"pid_gone":true})),
        );
        assert!(a.debug.threads.is_empty() && a.debug.stop_receipt.is_none());
        assert_eq!(a.debug.reason, "Disconnected");
    }
    #[test]
    fn current_breakpoint_reply_survives_a_step_epoch_change() {
        let (_d, mut a) = app();
        let p = a.doc.path.clone().unwrap();
        a.debug.breakpoint_changed(&p);
        let (version, _) = a.debug.begin_breakpoint_update(&p).unwrap();
        a.debug.clear_stop();
        a.debug_reply(0, 0, Reply::Breakpoints(p.clone(), version), Ok(json!([])));
        assert!(a.debug.verified[&p].is_empty());
        assert!(a.debug.breakpoint_inflight.is_empty());
    }
    #[test]
    fn stale_breakpoint_reply_never_reinstates_a_removed_dot() {
        let (_d, mut a) = app();
        let p = a.doc.path.clone().unwrap();
        a.debug.breakpoint_changed(&p);
        let (version, _) = a.debug.begin_breakpoint_update(&p).unwrap();
        a.debug.breakpoint_changed(&p);
        a.debug_reply(
            0,
            0,
            Reply::Breakpoints(p.clone(), version),
            Ok(json!([{"id":1,"line":2,"verified":true,"message":""}])),
        );
        assert!(!a.debug.verified.contains_key(&p));
    }
    #[test]
    fn breakpoint_toggle_persists_and_removal_is_replace_all_for_the_file() {
        let (d, mut a) = app();
        let p = a.doc.path.clone().unwrap();
        a.toggle_breakpoint(p.clone(), 2);
        super::breakpoint_ui::settle(&mut a);
        assert!(a.debug.breakpoints[&p].contains(&2));
        super::breakpoint_ui::settle(&mut a);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(d.path().join(".wex/breakpoints.json")).unwrap())
                .unwrap();
        assert_eq!(stored["schema"], 2);
        assert_eq!(stored["files"][p.to_str().unwrap()][0]["line"], 2);
        a.toggle_breakpoint(p.clone(), 2);
        super::breakpoint_ui::settle(&mut a);
        assert!(!a.debug.breakpoints.contains_key(&p));
        assert!(DebugUi::load(d.path()).breakpoints.is_empty());
    }
    #[test]
    fn stale_session_and_stop_epoch_replies_do_not_replace_new_variables() {
        let (_d, mut a) = app();
        a.debug.session = 5;
        a.debug.epoch = 8;
        a.debug_reply(
            4,
            8,
            Reply::Variables(1, 0),
            Ok(json!({"variables":[{"name":"old"}]})),
        );
        assert!(a.debug.variables.is_empty());
        a.debug_reply(
            5,
            7,
            Reply::Variables(1, 0),
            Ok(json!({"variables":[{"name":"old"}]})),
        );
        assert!(a.debug.variables.is_empty());
        a.debug_reply(
            5,
            8,
            Reply::Variables(1, 0),
            Ok(json!({"variables":[{"name":"current","value":"7","variablesReference":0}]})),
        );
        assert_eq!(a.debug.variables[&1][0]["name"], "current");
    }
    #[test]
    fn debugger_controls_and_dock_fit_minimum_and_wide_windows() {
        for width in [760., 1000., 1440.] {
            let (_d, mut a) = app();
            a.reduced_motion = true;
            a.tab = 3;
            let ctx = a.context.clone();
            for _ in 0..4 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 560.),
                        )),
                        ..Default::default()
                    },
                    |ctx| a.draw_shell(ctx),
                );
            }
            for key in [
                "debug",
                "run",
                "save",
                "build",
                "device",
                "nav-debug",
                "dock-3",
                "debug-continue",
                "debug-pause",
                "debug-next",
                "debug-in",
                "debug-out",
                "debug-stop",
            ] {
                assert!(
                    ctx.content_rect().contains_rect(a.chrome.hits[key]),
                    "{key} clipped at {width}: {:?}",
                    a.chrome.hits[key]
                );
            }
        }
    }
    #[test]
    fn resume_invalidates_inspection_and_previous_stop_receipt() {
        let mut d = DebugUi {
            epoch: 8,
            stop_receipt: Some(json!({"pid_gone":true})),
            ..Default::default()
        };
        d.frames.push(json!({"id":7}));
        d.variables.insert(3, vec![json!({"name":"old"})]);
        d.execution = Some((PathBuf::from("Main.kt"), 9));
        d.clear_stop();
        assert_eq!(d.epoch, 9);
        assert!(
            d.frames.is_empty()
                && d.variables.is_empty()
                && d.execution.is_none()
                && d.stop_receipt.is_none()
        );
    }
}
