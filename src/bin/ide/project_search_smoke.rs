//! Native production-path Find in Files tape; real worker, widgets, source input and OS writer.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::{io::Write, time::Instant};
const MARK: &str = "WEX_PROJECT_検索";
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    main: PathBuf,
    hash: blake3::Hash,
    park: PathBuf,
    park_hash: Option<blake3::Hash>,
    disk: PathBuf,
    stale: PathBuf,
    stale_text: String,
    writes: Vec<Value>,
    snapshots: Vec<Value>,
    clicks: usize,
    keyboard_phase: u8,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        let base = app.root.join("app/src/main/kotlin/dev/wex/android");
        let stale = base.join("ProjectStale.txt");
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            main: app.doc.path.clone().unwrap(),
            hash: app.doc.content_hash(),
            park: base.join("ProjectPark.kt"),
            park_hash: None,
            disk: base.join("ProjectDisk.txt"),
            stale_text: std::fs::read_to_string(&stale).unwrap(),
            stale,
            writes: vec![],
            snapshots: vec![],
            clicks: 0,
            keyboard_phase: 0,
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
fn query(i: &mut egui::RawInput, q: &str, submit: bool) {
    key(
        i,
        Key::F,
        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
    );
    key(i, Key::A, Modifiers::CTRL | Modifiers::COMMAND);
    i.events.push(Event::Paste(q.into()));
    if submit {
        key(i, Key::Enter, Modifiers::NONE);
    }
}
fn write_external(s: &mut Smoke, path: &PathBuf, text: &str) {
    let mut p=std::process::Command::new("python3").args(["-c","from pathlib import Path; import sys; Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read()); print('SEARCH_EXTERNAL_WRITE_OK')",path.to_str().unwrap()]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    p.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let r = p.wait_with_output().unwrap();
    assert!(r.status.success());
    assert_eq!(
        String::from_utf8_lossy(&r.stdout).trim(),
        "SEARCH_EXTERNAL_WRITE_OK"
    );
    s.writes.push(json!({"path":path,"bytes":text.len(),"hash":blake3::hash(text.as_bytes()).to_hex().to_string(),"result":"SEARCH_EXTERNAL_WRITE_OK","exit":r.status.code()}));
}
impl Workbench {
    fn search_row(&self, path: &PathBuf) -> String {
        let fi = self
            .psearch
            .result
            .as_ref()
            .unwrap()
            .files
            .iter()
            .position(|f| &f.path == path)
            .unwrap();
        let row = self
            .psearch
            .rows
            .iter()
            .position(|(f, m)| *f == fi && *m == Some(0))
            .unwrap();
        format!("row-{row}")
    }
    fn search_ready(&self, n: usize) -> bool {
        self.psearch
            .result
            .as_ref()
            .is_some_and(|r| r.stats.matches == n)
            && !self.psearch.pending
            && !self.psearch.stale
    }
    pub(super) fn project_search_smoke_input(
        &mut self,
        ctx: &egui::Context,
        i: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.project_search_smoke.take() else {
            return;
        };
        i.focused = true;
        i.modifiers = Modifiers::NONE;
        for v in i.viewports.values_mut() {
            v.focused = Some(true);
        }
        i.events.retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        assert!(
            s.start.elapsed() < Duration::from_secs(240),
            "project search native step{} timeout: query{:?} note{} pending{} stale{} nav{} rejects{} active{}",
            s.step,
            self.psearch.options.query,
            self.psearch.note,
            self.psearch.pending,
            self.psearch.stale,
            self.psearch.navigated,
            self.psearch.rejected_navigation,
            self.psearch.worker.active_id()
        );
        if let Some((p, t)) = s.release.take() {
            i.events.push(Event::PointerMoved(p));
            if Instant::now() < t {
                s.release = Some((p, t));
            } else {
                i.events.push(Event::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
            }
            self.project_search_smoke = Some(s);
            return;
        }
        if let Some(k) = s.click.take() {
            if let Some(r) = self
                .psearch
                .hits
                .get(&k)
                .or_else(|| self.chrome.hits.get(&k))
                .or_else(|| self.editor_ui.hits.get(&k))
                .copied()
            {
                let p = r.center();
                i.events.extend([
                    Event::PointerMoved(p),
                    Event::PointerButton {
                        pos: p,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ]);
                s.release = Some((p, Instant::now() + Duration::from_millis(40)));
                s.clicks += 1;
            } else {
                s.click = Some(k);
            }
            self.project_search_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.project_search_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                key(i, Key::Home, Modifiers::CTRL | Modifiers::COMMAND);
                i.events.push(Event::Text(format!("// {MARK} ACTIVE\n")));
            }
            1 => {
                assert!(self.doc.dirty());
                s.click = Some("file-ProjectPark.kt".into());
            }
            2 => {
                if self.doc.path.as_ref() == Some(&s.park) && self.open_ui.pending.is_none() {
                    s.park_hash = Some(self.doc.content_hash());
                    i.events.push(Event::Text(format!("// {MARK} PARKED\n")));
                } else {
                    wait = true
                }
            }
            3 => {
                assert!(self.doc.dirty());
                s.click = Some("tab-MainActivity.kt".into());
            }
            4 => {
                if self.doc.path.as_ref() == Some(&s.main) {
                    key(
                        i,
                        Key::F,
                        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                    );
                    i.events.push(Event::Text(MARK.into()));
                    key(i, Key::Enter, Modifiers::NONE);
                } else {
                    wait = true
                }
            }
            5 => {
                if self.search_ready(6) {
                    let r = self.psearch.result.as_ref().unwrap();
                    assert_eq!(self.psearch.options.query, MARK);
                    assert_eq!(r.files.iter().filter(|f| f.unsaved).count(), 2);
                    assert!(r.stats.skipped_binary >= 1);
                    assert_ne!(
                        r.worker_thread,
                        format!("{:?}", std::thread::current().id())
                    );
                    s.snapshots.push(json!({"name":"all-sources","result":r}));
                    shot = Some("01-project-results-unsaved-snapshots");
                } else {
                    wait = true
                }
            }
            6 => {
                s.click = Some(self.search_row(&s.park));
            }
            7 => {
                if self.doc.path.as_ref() == Some(&s.park)
                    && self.edit.selected_text(&self.doc) == MARK
                {
                    assert!(self.doc.dirty());
                    shot = Some("02-parked-dirty-match-navigation");
                } else {
                    wait = true
                }
            }
            8 => {
                key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
            }
            9 => {
                assert_eq!(self.doc.content_hash(), s.park_hash.unwrap());
                assert!(!self.doc.dirty());
                key(i, Key::Y, Modifiers::CTRL | Modifiers::COMMAND);
            }
            10 => {
                assert!(self.doc.dirty());
                s.click = Some("ps-refresh".into());
            }
            11 => {
                if self.search_ready(6) {
                    shot = Some("03-undo-redo-history-and-rescan");
                } else {
                    wait = true
                }
            }
            12 => match s.keyboard_phase {
                0 => {
                    key(
                        i,
                        Key::F,
                        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                    );
                    s.keyboard_phase = 1;
                    wait = true;
                }
                1 => {
                    if ctx.memory(|m| m.has_focus(egui::Id::new("project-search-query"))) {
                        key(i, Key::ArrowDown, Modifiers::NONE);
                        s.keyboard_phase = 2;
                    }
                    wait = true;
                }
                2 => {
                    if ctx.memory(|m| m.has_focus(egui::Id::new("project-search-results"))) {
                        key(i, Key::Enter, Modifiers::NONE);
                        s.keyboard_phase = 3;
                    }
                    wait = true;
                }
                3 => {
                    if self
                        .doc
                        .path
                        .as_ref()
                        .is_some_and(|p| p.ends_with(".search-note"))
                        && self.edit.selected_text(&self.doc) == MARK
                    {
                        s.keyboard_phase = 4;
                        s.click = Some(self.search_row(&s.disk));
                    } else {
                        wait = true;
                    }
                }
                _ => unreachable!(),
            },
            13 => {
                if self.doc.path.as_ref() == Some(&s.disk)
                    && self.edit.selected_text(&self.doc) == MARK
                {
                    assert_eq!(self.edit.range(), 2..2 + MARK.chars().count());
                    assert!(!self.doc.dirty());
                    shot = Some("04-closed-unicode-text-exact-selection");
                } else {
                    wait = true
                }
            }
            14 => {
                if s.keyboard_phase == 4 {
                    s.click = Some("query".into());
                    s.keyboard_phase = 5;
                    wait = true;
                } else {
                    assert!(
                        ctx.memory(|m| m.has_focus(egui::Id::new("project-search-query"))),
                        "actual pointer query re-entry must retain query focus"
                    );
                    key(i, Key::A, Modifiers::CTRL | Modifiers::COMMAND);
                    i.events.push(Event::Paste("[".into()));
                    s.keyboard_phase = 6;
                }
            }
            15 => {
                s.click = Some("option-.*".into());
            }
            16 => {
                if !self.psearch.pending && self.psearch.note.contains("Search error") {
                    assert!(self.psearch.stale && self.psearch.result.is_some());
                    shot = Some("05-invalid-regex-keeps-last-good-disabled");
                } else {
                    wait = true
                }
            }
            17 => {
                query(i, MARK, true);
            }
            18 => {
                if self.search_ready(6) {
                    assert!(self.psearch.options.regex);
                    shot = Some("06-valid-regex-recovery");
                } else {
                    wait = true
                }
            }
            19 => {
                let p = s.stale.clone();
                write_external(&mut s, &p, "external replacement without marker\n");
                s.click = Some(self.search_row(&s.stale));
            }
            20 => {
                if self.psearch.rejected_navigation == 1 && self.open_ui.pending.is_none() {
                    assert_eq!(self.doc.path, Some(s.disk.clone()));
                    assert_eq!(self.edit.range(), 2..2 + MARK.chars().count());
                    assert!(self.psearch.stale);
                    shot = Some("07-external-disk-change-rejected-before-navigation");
                } else {
                    wait = true
                }
            }
            21 => {
                s.click = Some("ps-refresh".into());
            }
            22 => {
                if self.search_ready(5) {
                    assert!(
                        self.psearch
                            .result
                            .as_ref()
                            .unwrap()
                            .files
                            .iter()
                            .all(|f| f.path != s.stale)
                    );
                    shot = Some("08-refreshed-disk-snapshot");
                } else {
                    wait = true
                }
            }
            23 => {
                query(i, &format!("{MARK}X"), false);
            }
            24 => {
                s.click = Some("ps-cancel".into());
            }
            25 => {
                if self.psearch.note.contains("Cancelled") {
                    assert!(
                        self.psearch.stale
                            && !self.psearch.pending
                            && self.psearch.result.is_some()
                    );
                    shot = Some("09-stop-debounced-search-retains-last-good");
                } else {
                    wait = true
                }
            }
            26 => {
                query(i, "WEX_NO_SUCH_MATCH_9831", true);
            }
            27 => {
                if self.search_ready(0) {
                    assert!(self.psearch.result.as_ref().unwrap().files.is_empty());
                    shot = Some("10-zero-results-explicit-success");
                } else {
                    wait = true
                }
            }
            28 => {
                query(i, MARK, true);
            }
            29 => {
                if self.search_ready(5) {
                    shot = Some("11-final-current-result-set");
                } else {
                    wait = true
                }
            }
            30 => {
                s.click = Some(self.search_row(&s.park));
            }
            31 => {
                if self.doc.path.as_ref() == Some(&s.park) {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            32 => {
                assert_eq!(self.doc.content_hash(), s.park_hash.unwrap());
                assert!(!self.doc.dirty());
                s.click = Some("tab-MainActivity.kt".into());
            }
            33 => {
                if self.doc.path.as_ref() == Some(&s.main) {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true
                }
            }
            34 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                let p = s.stale.clone();
                let t = s.stale_text.clone();
                write_external(&mut s, &p, &t);
                s.click = Some("ps-close".into());
            }
            35 => {
                if !self.psearch.open && self.psearch.worker.active_id() == 0 {
                    assert_eq!(self.doc.content_hash(), s.hash);
                    assert!(!self.doc.dirty());
                    assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                    assert!(self.last_build.is_none() && !self.building && !self.running);
                    assert!(!self.save_ui.saver.worker_started());
                    shot = Some("12-source-and-history-restored-no-save");
                } else {
                    wait = true
                }
            }
            36 => {
                assert_eq!(s.shots.len(), 12);
                std::fs::write(s.dir.join("project-search-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_PROJECT_SEARCH_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"reduced_motion":self.reduced_motion,"source":s.main,"source_hash":s.hash.to_hex().to_string(),"parked_source":s.park,"parked_hash":s.park_hash.unwrap().to_hex().to_string(),"source_buffers_restored":true,"dirty_active_and_parked_overlaid":true,"actual_widget_result_navigation":true,"actual_keyboard_result_navigation":s.keyboard_phase==6,"actual_pointer_query_reentry":s.keyboard_phase==6,"actual_query_shortcut_same_frame":true,"unicode_character_selection":true,"undo_redo_preserved":true,"invalid_regex_last_good":true,"disk_stale_navigation_rejected":self.psearch.rejected_navigation,"refresh_recovered":true,"stop_debounced_search":true,"zero_results":true,"actual_pointer_clicks":s.clicks,"accepted_searches":self.psearch.accepted,"discarded_searches":self.psearch.discarded,"worker_active":self.psearch.worker.active_id(),"foreground_thread":format!("{:?}",std::thread::current().id()),"snapshots":s.snapshots,"external_writes":s.writes,"save_worker_started":self.save_ui.saver.worker_started(),"deployment_invoked":false,"screenshots":s.shots,"input":"genuine native winit RawInput keys/pointer/clipboard; actual background root-scoped file/Rope worker; genuine separate Python disk writer; no source/programmatic match selection"})).unwrap()).unwrap();
                eprintln!(
                    "GPU_PROJECT_SEARCH_OK: actual grouped project results/disk+dirty active+parked snapshots/same-frame CtrlShiftF/real widget navigation/exact Unicode/Undo+Redo/regex error+recovery/stale disk rejection/Refresh/Stop/zero results/source restoration;12 genuine GPU frames;no Gradle/APK/phone"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now()
                + Duration::from_millis(if (23..=25).contains(&s.step) { 25 } else { 80 });
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.project_search_smoke = Some(s);
    }
    pub(super) fn project_search_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.project_search_smoke else {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"query":self.psearch.options.query,"note":self.psearch.note,"pending":self.psearch.pending,"stale":self.psearch.stale,"navigated":self.psearch.navigated,"rejected_navigation":self.psearch.rejected_navigation}));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
