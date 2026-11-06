//! Native editor regression. Inputs use real hitboxes; paste is read by eframe
//! from the OS clipboard. A separate Python process performs every disk mutation.
use super::*;
use egui::Pos2;
use std::{io::Write, time::Instant};
use wexandroidstudio::editor_jobs::DiskState;
struct Gesture {
    from: Pos2,
    to: Pos2,
    started: Instant,
}
pub(super) struct EditorSmoke {
    initial_stage: u8,
    dir: PathBuf,
    _file: tempfile::NamedTempFile,
    _other: tempfile::NamedTempFile,
    path: PathBuf,
    other: PathBuf,
    original: String,
    local: String,
    protected: Vec<(PathBuf, String)>,
    start: Instant,
    step_since: Instant,
    step: usize,
    next: Instant,
    gesture: Option<Gesture>,
    awaiting: Option<String>,
    deferred_click: Option<(String, u8)>,
    shots: Vec<serde_json::Value>,
    checks: Vec<String>,
    writes: Vec<serde_json::Value>,
    preedit_revision: u64,
    last_retry: Instant,
    retries: Vec<(usize, String)>,
}
impl EditorSmoke {
    pub(super) fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("editor-window.json"));
        let protected = app
            .files
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    blake3::hash(&std::fs::read(p).unwrap())
                        .to_hex()
                        .to_string(),
                )
            })
            .collect();
        let folder = app
            .files
            .iter()
            .find(|p| p.ends_with("MainActivity.kt"))
            .and_then(|p| p.parent())
            .unwrap_or(&app.root)
            .to_path_buf();
        let mut file = tempfile::Builder::new()
            .prefix("WexEditorSmoke_")
            .suffix(".kt")
            .tempfile_in(&folder)
            .unwrap();
        let mut other = tempfile::Builder::new()
            .prefix("WexParkedSmoke_")
            .suffix(".kt")
            .tempfile_in(&folder)
            .unwrap();
        let mut original="package dev.wex.android\n\n// Unicode: привет e\u{301} 👩🏽‍💻\nfun editorProbe() {\n    val alpha = \"alpha\"\n    val beta = \"ALPHA\"\n    val alphabet = \"other\"\n    println(alpha)\n}\n".to_string();
        for i in 0..10000 {
            original.push_str(&format!("// filler {i:05}\n"));
        }
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();
        other
            .write_all(b"package dev.wex.android\n// parked baseline\n")
            .unwrap();
        other.flush().unwrap();
        let path = file.path().canonicalize().unwrap();
        let other_path = other.path().canonicalize().unwrap();
        app.open_file(other_path.clone());

        Self {
            initial_stage: 0,
            dir,
            _file: file,
            _other: other,
            path,
            other: other_path,
            original,
            local: String::new(),
            protected,
            start: Instant::now(),
            step_since: Instant::now(),
            step: 0,
            next: Instant::now() + Duration::from_secs(3),
            gesture: None,
            awaiting: None,
            deferred_click: None,
            shots: vec![],
            checks: vec![],
            writes: vec![],
            preedit_revision: 0,
            last_retry: Instant::now(),
            retries: vec![],
        }
    }
    fn external(&mut self, path: &PathBuf, text: &str, operation: &str) {
        let code = "import os,pathlib,sys,tempfile\np=pathlib.Path(sys.argv[1]);op=sys.argv[2];text=sys.stdin.read()\nif op=='delete': p.unlink()\nelif op=='atomic':\n f=tempfile.NamedTemporaryFile(mode='w',encoding='utf-8',dir=p.parent,delete=False);f.write(text);f.close();os.replace(f.name,p)\nelse: p.write_text(text,encoding='utf-8')\nprint('EXTERNAL_WRITE_OK')";
        let mut child = std::process::Command::new("python3")
            .args(["-c", code, path.to_str().unwrap(), operation])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim(),
            "EXTERNAL_WRITE_OK"
        );
        self.writes.push(serde_json::json!({"program":"python3","operation":operation,"path":path,"input_blake3":blake3::hash(text.as_bytes()).to_hex().to_string(),"output":"EXTERNAL_WRITE_OK","exit":result.status.code()}));
    }
}
fn key(input: &mut egui::RawInput, key: Key, mut modifiers: egui::Modifiers) {
    if cfg!(target_os = "linux") && modifiers.command {
        modifiers.ctrl = true;
    }
    input.events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    });
    input.events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers,
    });
}
fn point(app: &Workbench, char: usize) -> Pos2 {
    let line = app.doc.rope().char_to_line(char);
    let start = app.doc.rope().line_to_char(line);
    let (pos, g) = app
        .editor_ui
        .geometry
        .rows
        .get(&line)
        .unwrap_or_else(|| panic!("live row {line} not visible"));
    let r = g.pos_from_cursor(egui::text::CCursor::new(char - start));
    *pos + egui::vec2(r.left() + 0.5, 10.)
}
fn select_keys(input: &mut egui::RawInput, app: &Workbench, range: std::ops::Range<usize>) {
    key(input, Key::Home, egui::Modifiers::COMMAND);
    let mut p = 0;
    while p < range.start {
        key(input, Key::ArrowRight, egui::Modifiers::NONE);
        p = wexandroidstudio::editing::boundary(app.doc.rope(), p, true);
    }
    while p < range.end {
        key(input, Key::ArrowRight, egui::Modifiers::SHIFT);
        p = wexandroidstudio::editing::boundary(app.doc.rope(), p, true);
    }
}
impl Workbench {
    pub(super) fn editor_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.editor_smoke.take() else {
            return;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(180),
            "editor smoke timed out at {}",
            s.step
        );
        if !input.focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.editor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        if s.initial_stage == 0 {
            if self.doc.path.as_ref() != Some(&s.other) {
                self.editor_smoke = Some(s);
                return;
            }
            self.open_file(s.path.clone());
            s.initial_stage = 1;
            self.editor_smoke = Some(s);
            return;
        }
        if s.initial_stage == 1 {
            if self.doc.path.as_ref() != Some(&s.path) {
                self.editor_smoke = Some(s);
                return;
            }
            self.edit.collapse(0);
            self.scroll_to_cursor = true;
            s.initial_stage = 2;
            s.next = Instant::now() + Duration::from_secs(2);
        }
        if let Some(g) = s.gesture.take() {
            if g.started.elapsed() < Duration::from_millis(90) {
                input.events.push(egui::Event::PointerMoved(
                    if g.started.elapsed() > Duration::from_millis(35) {
                        g.to
                    } else {
                        g.from
                    },
                ));
                s.gesture = Some(g);
            } else {
                input.events.push(egui::Event::PointerMoved(g.to));
                input.events.push(egui::Event::PointerButton {
                    pos: g.to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.editor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        // A newly opened modal is measured/repositioned by egui on its first
        // frame. Capture that frame, then use the final hitbox, not its measure rect.
        if s.awaiting.is_none()
            && let Some((key, hovered)) = s.deferred_click.take()
        {
            let r = *self
                .editor_ui
                .hits
                .get(&key)
                .or_else(|| self.chrome.hits.get(&key))
                .unwrap();
            let pos = r.center();
            input.events.push(egui::Event::PointerMoved(pos));
            if hovered < 2 {
                s.deferred_click = Some((key, hovered + 1));
                self.editor_smoke = Some(s);
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
            s.gesture = Some(Gesture {
                from: pos,
                to: pos,
                started: Instant::now(),
            });
            self.editor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if Instant::now() < s.next || s.awaiting.is_some() {
            self.editor_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        macro_rules! wait_for {
            ($condition:expr) => {
                if !$condition {
                    // Native focus/area measurement can invalidate an in-flight
                    // pointer press. Retry only an unacknowledged, still-needed
                    // action through its real control; never mutate UI state.
                    if s.step_since.elapsed() > Duration::from_millis(700)
                        && s.last_retry.elapsed() > Duration::from_millis(700)
                        && s.retries.iter().filter(|(step, _)| *step == s.step).count() < 3
                    {
                        let retry = match s.step {
                            22 if !self.editor_ui.find.options.whole_word => {
                                Some("find-word".to_string())
                            }
                            23 if !self.editor_ui.find.options.whole_word => {
                                Some("find-word".to_string())
                            }
                            23 if !self.editor_ui.find.options.case_sensitive => {
                                Some("find-case".to_string())
                            }
                            26 if !self.editor_ui.find.options.regex => {
                                Some("find-regex".to_string())
                            }
                            30 if self.valid_find()
                                && self.editor_ui.find.replacing.is_none()
                                && self.doc.rope().to_string().matches("alpha_x").count() == 0 =>
                            {
                                Some("replace-all".to_string())
                            }
                            31 if self.editor_ui.find.open => Some("find-close".to_string()),
                            37 if self.editor_ui.pending_reload.is_none() => {
                                Some("disk-reload".to_string())
                            }
                            38 if self.editor_ui.pending_reload.is_some() => {
                                Some("reload-cancel".to_string())
                            }
                            39 | 45 if self.editor_ui.conflicts.contains_key(&s.path) => {
                                Some("disk-keep".to_string())
                            }
                            41 if self.doc.path.as_ref() != Some(&s.other) => Some(format!(
                                "tab-{}",
                                s.other.file_name().unwrap().to_string_lossy()
                            )),
                            43 if self.doc.path.as_ref() != Some(&s.path) => Some(format!(
                                "tab-{}",
                                s.path.file_name().unwrap().to_string_lossy()
                            )),
                            _ => None,
                        };
                        if let Some(key) = retry {
                            s.last_retry = Instant::now();
                            s.retries.push((s.step, key.clone()));
                            s.deferred_click = Some((key, 0));
                        }
                    }
                    assert!(
                        s.step_since.elapsed() < Duration::from_secs(10),
                        "editor step {} condition timed out; status={} selected={:?}",
                        s.step,
                        self.status,
                        self.edit.range()
                    );
                    s.next = Instant::now() + Duration::from_millis(50);
                    self.editor_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(16));
                    return;
                }
            };
        }
        let mut click: Option<String> = None;
        let mut drag: Option<(Pos2, Pos2)> = None;
        let mut shot: Option<&str> = None;
        match s.step {
            0 => {
                wait_for!(self.editor_ui.geometry.rows.contains_key(&2));
                assert!(self.doc.rope().len_lines() > 10000);
                shot = Some("01-editor");
            }
            1 => {
                let byte = s.original.find("привет").unwrap();
                let start = s.original[..byte].chars().count();
                drag = Some((point(self, start), point(self, start + 6)));
            }
            2 => {
                assert_eq!(self.edit.selected_text(&self.doc), "привет");
                s.checks
                    .push("Mouse drag selects Unicode text using shaped glyph positions".into());
                shot = Some("02-selection");
                input.events.push(egui::Event::Copy);
            }
            3 => {
                key(input, Key::End, egui::Modifiers::COMMAND);
            }
            4 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            5 => {
                wait_for!(self.doc.rope().to_string().ends_with("привет"));
                assert_eq!(self.doc.rope().len_chars(), s.original.chars().count() + 6);
                s.checks.push(
                    "Copy → native OS clipboard → integration RequestPaste round-trips exact UTF-8"
                        .into(),
                );
                key(input, Key::Z, egui::Modifiers::COMMAND);
            }
            6 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                key(input, Key::Home, egui::Modifiers::COMMAND);
            }
            7 => {
                key(input, Key::ArrowRight, egui::Modifiers::SHIFT);
                key(input, Key::ArrowRight, egui::Modifiers::SHIFT);
            }
            8 => {
                assert_eq!(self.edit.range(), 0..2);
                input.events.push(egui::Event::Text("//".into()));
            }
            9 => {
                assert!(self.doc.rope().to_string().starts_with("//ckage"));
                key(input, Key::Z, egui::Modifiers::COMMAND);
            }
            10 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                assert_eq!(self.edit.range(), 0..2);
                s.checks
                    .push("Shift selection replaces text; Undo restores text and selection".into());
                let byte = s.original.find("привет").unwrap();
                let start = s.original[..byte].chars().count();
                select_keys(input, self, start..start + 6);
            }
            11 => {
                assert_eq!(self.edit.selected_text(&self.doc), "привет");
                s.preedit_revision = self.doc.revision;
                input.events.extend([
                    egui::Event::Ime(egui::ImeEvent::Enabled),
                    egui::Event::Ime(egui::ImeEvent::Preedit("にほん".into())),
                ]);
            }
            12 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                assert_eq!(self.doc.revision, s.preedit_revision);
                assert_eq!(self.edit.composition.as_ref().unwrap().text, "にほん");
                assert!(
                    self.editor_ui
                        .geometry
                        .viewport
                        .contains_rect(self.editor_ui.geometry.ime.unwrap())
                );
                shot = Some("03-ime-preedit");
                input
                    .events
                    .push(egui::Event::Ime(egui::ImeEvent::Preedit("日本語".into())));
            }
            13 => {
                input.events.extend([
                    egui::Event::Ime(egui::ImeEvent::Commit("日本語".into())),
                    egui::Event::Text("日本語".into()),
                ]);
            }
            14 => {
                assert_eq!(
                    self.doc.rope().to_string(),
                    s.original.replacen("привет", "日本語", 1)
                );
                assert_eq!(self.doc.revision, s.preedit_revision + 1);
                shot = Some("04-ime-commit");
                s.checks.push("IME preedit is transient; candidate rect is visible; CJK commit occurs once and is one Undo step".into());
                key(input, Key::Z, egui::Modifiers::COMMAND);
            }
            15 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                input.events.extend([
                    egui::Event::Ime(egui::ImeEvent::Enabled),
                    egui::Event::Ime(egui::ImeEvent::Preedit("cancelled".into())),
                ]);
            }
            16 => {
                key(input, Key::Escape, egui::Modifiers::NONE);
            }
            17 => {
                input.events.push(egui::Event::Ime(egui::ImeEvent::Commit(
                    "late stale commit".into(),
                )));
            }
            18 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                assert!(self.edit.composition.is_none());
                s.checks.push("IME cancellation preserves the original selection and rejects a late stale commit".into());
                key(input, Key::H, egui::Modifiers::COMMAND);
            }
            19 => {
                wait_for!(
                    self.editor_ui.find.open && self.editor_ui.hits.contains_key("find-query")
                );
                click = Some("find-query".into());
            }
            20 => {
                key(input, Key::A, egui::Modifiers::COMMAND);
                input.events.push(egui::Event::Text("alpha".into()));
            }
            21 => {
                wait_for!(
                    self.editor_ui.find.options.query == "alpha"
                        && self.editor_ui.find.results.matches.len() == 5
                );
                click = Some("find-word".into());
            }
            22 => {
                wait_for!(self.editor_ui.find.results.matches.len() == 4);
                click = Some("find-case".into());
            }
            23 => {
                wait_for!(self.editor_ui.find.results.matches.len() == 3);
                click = Some("find-regex".into());
            }
            24 => {
                click = Some("find-query".into());
            }
            25 => {
                key(input, Key::A, egui::Modifiers::COMMAND);
                input.events.push(egui::Event::Text("[".into()));
            }
            26 => {
                wait_for!(self.editor_ui.find.results.error.is_some());
                assert_eq!(self.doc.rope().to_string(), s.original);
                key(input, Key::A, egui::Modifiers::COMMAND);
                input.events.push(egui::Event::Text("(alpha)".into()));
            }
            27 => {
                wait_for!(self.editor_ui.find.results.matches.len() == 3);
                s.checks.push("Find panel: case, whole-word, regex and invalid-expression state work through native UI controls".into());
                click = Some("replace-query".into());
            }
            28 => {
                key(input, Key::A, egui::Modifiers::COMMAND);
                input.events.push(egui::Event::Text("${1}_x".into()));
            }
            29 => {
                wait_for!(self.editor_ui.find.replacement == "${1}_x" && self.valid_find());
                click = Some("replace-all".into());
            }
            30 => {
                wait_for!(
                    self.editor_ui.find.replacing.is_none()
                        && self.doc.rope().to_string().matches("alpha_x").count() == 3
                );
                assert!(self.doc.rope().to_string().contains("ALPHA"));
                assert!(self.doc.rope().to_string().contains("alphabet"));
                shot = Some("05-search-replace");
                click = Some("find-close".into());
            }
            31 => {
                wait_for!(!self.editor_ui.find.open);
                key(input, Key::Z, egui::Modifiers::COMMAND);
            }
            32 => {
                assert_eq!(self.doc.rope().to_string(), s.original);
                s.checks.push("Regex Replace All changes three matches atomically; one Undo restores the whole file".into());
                key(input, Key::S, egui::Modifiers::COMMAND);
            }
            33 => {
                wait_for!(self.save_ui.pending.is_none());
                assert!(!self.doc.dirty());
                let text = s.original.replace("other", "disk_version");
                let path = s.path.clone();
                s.external(&path, &text, "atomic");
            }
            34 => {
                wait_for!(
                    self.doc.rope().to_string().contains("disk_version") && !self.doc.dirty()
                );
                assert!(!self.editor_ui.conflicts.contains_key(&s.path));
                shot = Some("06-external-clean");
                s.checks.push("An independent process atomically replaces a clean file; watcher reloads it in the GPU window".into());
                key(input, Key::End, egui::Modifiers::COMMAND);
                input
                    .events
                    .push(egui::Event::Text("// local edit\n".into()));
            }
            35 => {
                assert!(self.doc.dirty());
                s.local = self.doc.rope().to_string();
                let remote = s.original.replace("other", "remote_version") + "// remote edit\n";
                let path = s.path.clone();
                s.external(&path, &remote, "atomic");
            }
            36 => {
                wait_for!(self.editor_ui.conflicts.contains_key(&s.path));
                assert_eq!(self.doc.rope().to_string(), s.local);
                shot = Some("07-external-conflict");
                click = Some("disk-reload".into());
            }
            37 => {
                wait_for!(self.editor_ui.pending_reload.is_some());
                shot = Some("08-reload-confirm");
                click = Some("reload-cancel".into());
            }
            38 => {
                wait_for!(self.editor_ui.pending_reload.is_none());
                assert_eq!(self.doc.rope().to_string(), s.local);
                click = Some("disk-keep".into());
            }
            39 => {
                wait_for!(!self.editor_ui.conflicts.contains_key(&s.path));
                key(input, Key::S, egui::Modifiers::COMMAND);
            }
            40 => {
                wait_for!(self.save_ui.pending.is_none());
                assert!(!self.doc.dirty());
                assert_eq!(std::fs::read_to_string(&s.path).unwrap(), s.local);
                s.checks.push("Dirty-file conflict preserves edits; Reload requires confirmation; Cancel keeps them; Keep mine + Save resolves explicitly".into());
                click = Some(format!(
                    "tab-{}",
                    s.other.file_name().unwrap().to_string_lossy()
                ));
            }
            41 => {
                wait_for!(self.doc.path.as_ref() == Some(&s.other));
                let next = format!("// background update\n{}", s.local);
                let path = s.path.clone();
                s.external(&path, &next, "atomic");
            }
            42 => {
                wait_for!(self.parked.get(&s.path).is_some_and(|(d, _)| {
                    d.rope().to_string().starts_with("// background update")
                }));
                assert_eq!(self.doc.path.as_ref(), Some(&s.other));
                click = Some(format!(
                    "tab-{}",
                    s.path.file_name().unwrap().to_string_lossy()
                ));
            }
            43 => {
                wait_for!(self.doc.path.as_ref() == Some(&s.path));
                assert!(
                    self.doc
                        .rope()
                        .to_string()
                        .starts_with("// background update")
                );
                assert!(!self.doc.dirty());
                s.checks.push(
                    "Watcher reloads a parked clean tab without switching the active document"
                        .into(),
                );
                shot = Some("09-watched-tabs");
                let path = s.path.clone();
                s.external(&path, "", "delete");
            }
            44 => {
                wait_for!(matches!(
                    self.editor_ui.conflicts.get(&s.path),
                    Some(DiskState::Missing)
                ));
                assert!(self.doc.rope().to_string().contains("local edit"));
                click = Some("disk-keep".into());
            }
            45 => {
                wait_for!(!self.editor_ui.conflicts.contains_key(&s.path));
                assert!(self.doc.dirty());
                key(input, Key::S, egui::Modifiers::COMMAND);
            }
            46 => {
                wait_for!(self.save_ui.pending.is_none());
                assert!(s.path.is_file());
                assert!(!self.doc.dirty());
                assert_eq!(
                    std::fs::read_to_string(&s.path).unwrap(),
                    self.doc.rope().to_string()
                );
                s.checks.push(
                    "Deletion preserves the buffer; explicit Recreate on Save restores the file"
                        .into(),
                );
                key(input, Key::End, egui::Modifiers::COMMAND);
            }
            47 => {
                assert!(self.doc.rope().len_lines() > 10000);
                assert!(self.editor_ui.rendered_rows < 100);
                assert!(self.edit.scroll_y > 10000.);
                shot = Some("10-large-file");
                s.checks.push(format!(
                    "10,000+ lines remain virtualized: {} visible rows",
                    self.editor_ui.rendered_rows
                ));
            }
            48 => {
                for (p, h) in &s.protected {
                    assert_eq!(
                        &blake3::hash(&std::fs::read(p).unwrap())
                            .to_hex()
                            .to_string(),
                        h
                    );
                }
                let report = serde_json::json!({"result":"GUI_EDITOR_OK","renderer":"wgpu","input":"eframe raw input + native OS clipboard RequestPaste","checks":s.checks,"interaction_retries":s.retries,"screenshots":s.shots,"external_process_operations":s.writes,"protected_files_blake3":s.protected,"protected_files_unchanged":true,"owned_scratch_files":[s.path,s.other],"source_lines":self.doc.rope().len_lines(),"rendered_rows":self.editor_ui.rendered_rows,"ime_test":"Enabled / Preedit / Commit / Disabled-or-Escape through egui native integration; candidate rectangle verified","deployment_invoked":false});
                std::fs::write(
                    s.dir.join("editor-window.json"),
                    serde_json::to_vec_pretty(&report).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "GUI_EDITOR_OK: mouse/keyboard selection; OS clipboard round-trip; search/replace; IME preedit/commit/cancel; clean/dirty/parked/deleted file watcher; 10000-line virtualization"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        if let Some(key) = click {
            s.deferred_click = Some((key, 0));
        }
        if let Some((from, to)) = drag {
            input.events.push(egui::Event::PointerMoved(from));
            input.events.push(egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
            s.gesture = Some(Gesture {
                from,
                to,
                started: Instant::now(),
            });
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.step_since = Instant::now();
        s.next = Instant::now() + Duration::from_millis(280);
        self.editor_smoke = Some(s);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub(super) fn editor_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.editor_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for e in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = e {
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
                    s.shots.push(serde_json::json!({"path":path,"width":image.size[0],"height":image.size[1]}));
                    s.awaiting = None;
                }
            }
        }
    }
}
