//! Native keyboard, buffer-only Save, atomic outside review and cancellation workflow.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::{process::Command, time::Instant};
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    setup: u8,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    clicks: usize,
    main: PathBuf,
    park: PathBuf,
    original: blake3::Hash,
    park_hash: Option<blake3::Hash>,
    expected: Option<PathBuf>,
    observed: usize,
    external: Vec<Value>,
    no_save_before_action: bool,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            setup: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            clicks: 0,
            main: app.doc.path.clone().unwrap(),
            park: app
                .root
                .join("app/src/main/kotlin/dev/wex/android/ZFlowPark.kt"),
            original: app.doc.content_hash(),
            park_hash: None,
            expected: None,
            observed: 0,
            external: vec![],
            no_save_before_action: false,
        }
    }
}
fn key(i: &mut egui::RawInput, k: Key, m: Modifiers) {
    for pressed in [true, false] {
        i.events.push(Event::Key {
            key: k,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: m,
        });
    }
}
fn change(path: &PathBuf, text: &str) -> Value {
    let mut child=Command::new("python3").args(["-c","import pathlib,sys;pathlib.Path(sys.argv[1]).write_text(sys.argv[2],encoding='utf-8');print('FLOW_EXTERNAL_WRITE_OK')"]).arg(path).arg(text).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    let mut stdout = String::new();
    use std::io::Read;
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(status.success() && stdout == "FLOW_EXTERNAL_WRITE_OK\n");
    json!({"path":path,"pid":pid,"exit":status.code(),"literal":stdout})
}
impl Workbench {
    pub(super) fn git_flow_smoke_input(&mut self, ctx: &egui::Context, i: &mut egui::RawInput) {
        let Some(mut s) = self.git_flow_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(180),
            "Git flow step{} timeout; busy{} saving{} path{:?} note{} error{} focus{:?}",
            s.step,
            self.git.busy,
            self.git.saving.is_some(),
            self.git.path,
            self.git.note,
            self.git.error,
            ctx.memory(|m| m.focused())
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
            self.git_flow_smoke = Some(s);
            return;
        }
        if let Some(k) = s.click.take() {
            if let Some(r) = self.chrome.hits.get(&k).copied() {
                assert!(
                    ctx.content_rect().contains_rect(r),
                    "{k} outside screen {r:?}"
                );
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
            self.git_flow_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.git_flow_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                key(i, Key::Home, Modifiers::CTRL | Modifiers::COMMAND);
                i.events.push(Event::Text("// FLOW_MAIN\n".into()));
            }
            1 => {
                assert!(self.doc.dirty());
                // The forty real Git rows put this source below the first explorer
                // viewport. Use the actual file filter rather than an invisible hit.
                match s.setup {
                    0 => s.click = Some("file-filter".into()),
                    1 => i.events.push(Event::Paste("ZFlowPark.kt".into())),
                    2 => s.click = Some("file-ZFlowPark.kt".into()),
                    _ => unreachable!(),
                }
                s.setup += 1;
                wait = s.setup < 3;
            }
            2 => {
                if self.doc.path.as_ref() == Some(&s.park) && self.open_ui.pending.is_none() {
                    s.park_hash = Some(self.doc.content_hash());
                    key(i, Key::Home, Modifiers::CTRL | Modifiers::COMMAND);
                    i.events.push(Event::Text("// FLOW_PARK\n".into()));
                } else {
                    wait = true;
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
                        Key::G,
                        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                    );
                    i.events
                        .push(Event::Text("SHORTCUT_SHOULD_NOT_LEAK".into()));
                } else {
                    wait = true;
                }
            }
            5 => {
                if self.git.open
                    && !self.git.busy
                    && let Some(snapshot) = &self.git.snapshot
                {
                    assert!(snapshot.entry_index(&s.main).is_none());
                    assert!(snapshot.entry_index(&s.park).is_none());
                    assert!(
                        !self
                            .doc
                            .rope()
                            .to_string()
                            .contains("SHORTCUT_SHOULD_NOT_LEAK")
                    );
                    assert!(self.doc.dirty() && self.parked[&s.park].0.dirty());
                    assert!(!self.save_ui.saver.worker_started());
                    s.no_save_before_action = true;
                    shot = Some("01-buffer-only-tabs-before-save");
                } else {
                    wait = true;
                }
            }
            6 => {
                key(i, Key::F6, Modifiers::NONE);
                i.events.push(Event::Paste("Flow commit 猫".into()));
            }
            7 => {
                assert_eq!(self.git.message, "Flow commit 猫");
                key(i, Key::Enter, Modifiers::NONE);
                assert_eq!(self.git.mutations, 0);
                shot = Some("02-message-enter-not-commit-or-open");
            }
            8 => {
                key(i, Key::F6, Modifiers::NONE);
                key(i, Key::End, Modifiers::NONE);
            }
            9 => {
                if !self.git.busy && self.git.path.as_ref() == Some(&s.park) {
                    let view = self.chrome.hits["git-list-viewport"];
                    let r = self.chrome.hits["git-selected-row"];
                    assert!(view.contains_rect(r) && r.height() >= 40.);
                    shot = Some("03-end-and-page-navigation");
                    let page = (view.height() / 44.).floor().max(1.) as usize;
                    let paths = self.git_flow_paths();
                    s.expected = Some(paths[paths.len() - 1 - page].clone());
                    key(i, Key::PageUp, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            10 => {
                if !self.git.busy && self.git.path == s.expected {
                    key(i, Key::PageDown, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            11 => {
                if !self.git.busy && self.git.path.as_ref() == Some(&s.park) {
                    key(i, Key::Home, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            12 => {
                if !self.git.busy
                    && self.git.path.as_ref() == Some(&self.root.join("Review.txt"))
                    && self.git.diff.contains("+reviewed after")
                {
                    shot = Some("04-home-key-real-working-diff");
                } else {
                    wait = true;
                }
            }
            13 => {
                s.observed = self.git.observed_changes;
                key(i, Key::F6, Modifiers::NONE);
                i.events.push(Event::Paste("_continued".into()));
                s.external.push(change(
                    &self.root.join("0-new.txt"),
                    "external tracked insertion\n",
                ));
                s.external
                    .push(change(&self.root.join("Review.txt"), "outside after\n"));
            }
            14 => {
                if !self.git.busy
                    && self.git.observed_changes > s.observed
                    && self.git.diff.contains("+outside after")
                {
                    assert_eq!(self.git.path, Some(self.root.join("Review.txt")));
                    assert_eq!(self.git.message, "Flow commit 猫_continued");
                    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("git-message"))));
                    assert!(self.git.selected.unwrap() > 0);
                    shot = Some("05-auto-review-keeps-path-message-focus");
                } else {
                    wait = true;
                }
            }
            15 => {
                key(i, Key::F6, Modifiers::NONE);
                key(i, Key::End, Modifiers::NONE);
                key(i, Key::ArrowUp, Modifiers::NONE);
            }
            16 => {
                if !self.git.busy && self.git.path.as_ref() == Some(&s.main) {
                    assert!(self.git.selected.is_none());
                    key(i, Key::S, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            17 => {
                if !self.git.busy
                    && self.git.saving.is_none()
                    && self.git.saved == 1
                    && self.git.diff.contains("+// FLOW_MAIN")
                {
                    assert!(!self.doc.dirty() && self.parked[&s.park].0.dirty());
                    assert_eq!(self.git.mutations, 0);
                    assert_eq!(self.git.message, "Flow commit 猫_continued");
                    let selected = self
                        .chrome
                        .hits
                        .get("git-selected-row")
                        .expect("explicitly saved file must remain visible");
                    assert!(selected.height() >= 43.);
                    assert!(self.chrome.hits["git-list-viewport"].contains_rect(*selected));
                    shot = Some("06-explicit-save-only-main-reviewed-not-staged");
                } else {
                    wait = true;
                }
            }
            18 => {
                key(i, Key::Enter, Modifiers::NONE);
                i.events.push(Event::Text("// FOLLOW_AFTER_OPEN\n".into()));
            }
            19 => {
                if !self.git.open
                    && self.doc.path.as_ref() == Some(&s.main)
                    && self.doc.rope().to_string().contains("FOLLOW_AFTER_OPEN")
                {
                    assert!(self.doc.dirty());
                    shot = Some("07-enter-open-following-text-targeted");
                } else {
                    wait = true;
                }
            }
            20 => {
                key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
            }
            21 => {
                assert!(!self.doc.dirty());
                key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
            }
            22 => {
                assert_eq!(self.doc.content_hash(), s.original);
                assert!(self.doc.dirty());
                key(i, Key::Y, Modifiers::CTRL | Modifiers::COMMAND);
            }
            23 => {
                assert!(!self.doc.dirty());
                assert!(self.doc.rope().to_string().starts_with("// FLOW_MAIN\n"));
                key(
                    i,
                    Key::G,
                    Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                );
            }
            24 => {
                if self.git.open && !self.git.busy && self.git.diff.contains("+// FLOW_MAIN") {
                    assert_eq!(self.git.path, Some(s.main.clone()));
                    shot = Some("08-source-undo-redo-and-saved-baseline-kept");
                } else {
                    wait = true;
                }
            }
            25 => {
                s.click = Some("git-stage".into());
            }
            26 => {
                if !self.git.busy && self.git.mutations == 1 {
                    s.click = Some("git-staged".into());
                } else {
                    wait = true;
                }
            }
            27 => {
                key(i, Key::Home, Modifiers::NONE);
            }
            28 => {
                if !self.git.busy && self.git.diff.contains("+// FLOW_MAIN") {
                    s.click = Some("git-commit".into());
                } else {
                    wait = true;
                }
            }
            29 => {
                if !self.git.busy && self.git.mutations == 2 {
                    assert!(self.git.message.is_empty());
                    assert!(self.parked[&s.park].0.dirty());
                    shot = Some("09-only-explicitly-saved-main-committed");
                } else {
                    wait = true;
                }
            }
            30 => {
                s.click = Some("git-working".into());
            }
            31 => {
                key(i, Key::F6, Modifiers::NONE);
                i.events
                    .push(Event::Paste("Keep draft after Save error".into()));
            }
            32 => {
                assert_eq!(self.git.message, "Keep draft after Save error");
                key(i, Key::F6, Modifiers::NONE);
                key(i, Key::End, Modifiers::NONE);
            }
            33 => {
                if !self.git.busy && self.git.path.as_ref() == Some(&s.park) {
                    let text = std::fs::read_to_string(&s.park).unwrap() + "// EXTERNAL_PARK\n";
                    s.external.push(change(&s.park, &text));
                    s.click = Some("git-save".into());
                } else {
                    wait = true;
                }
            }
            34 => {
                if !self.git.busy && self.git.saving.is_none() && !self.git.error.is_empty() {
                    assert_eq!(self.git.saved, 1);
                    assert!(self.git.stale);
                    assert!(self.parked[&s.park].0.dirty());
                    assert_eq!(self.git.message, "Keep draft after Save error");
                    assert!(
                        std::fs::read_to_string(&s.park)
                            .unwrap()
                            .contains("EXTERNAL_PARK")
                    );
                    assert!(
                        !self.parked[&s.park]
                            .0
                            .rope()
                            .to_string()
                            .contains("EXTERNAL_PARK")
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(760., 560.)));
                } else {
                    wait = true;
                }
            }
            35 => {
                if ctx.content_rect().width() <= 761. {
                    s.click = Some("git-details".into());
                } else {
                    wait = true;
                }
            }
            36 => {
                for k in [
                    "git-stage",
                    "git-unstage",
                    "git-save",
                    "git-open-file",
                    "git-message",
                    "git-commit",
                    "git-close",
                ] {
                    assert!(
                        ctx.content_rect().contains_rect(self.chrome.hits[k]),
                        "{k} outside minimum"
                    );
                }
                shot = Some("10-minimum-save-conflict-draft-footer");
            }
            37 => {
                s.click = Some("git-refresh".into());
            }
            38 => {
                if !self.git.busy && !self.git.stale && self.git.error.is_empty() {
                    shot = Some("11-minimum-refreshed-external-disk-unsaved-buffer");
                    std::fs::write(
                        s.dir.parent().unwrap().join("traps/slow"),
                        "owned read barrier",
                    )
                    .unwrap();
                } else {
                    wait = true;
                }
            }
            39 => {
                if self.git.busy
                    && s.dir
                        .parent()
                        .unwrap()
                        .join("traps/slow-started.json")
                        .exists()
                {
                    shot = Some("12-background-read-stop-is-reachable");
                } else {
                    wait = true;
                }
            }
            40 => {
                s.click = Some("git-close".into());
            }
            41 => {
                if !self.git.open && !self.git.busy {
                    assert_eq!(self.git.message, "Keep draft after Save error");
                    assert!(!self.doc.dirty());
                    assert!(self.parked[&s.park].0.dirty());
                    std::fs::remove_file(s.dir.parent().unwrap().join("traps/slow")).unwrap();
                    key(
                        i,
                        Key::G,
                        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                    );
                } else {
                    wait = true;
                }
            }
            42 => {
                if self.git.open && !self.git.busy && self.git.error.is_empty() {
                    assert_eq!(self.git.message, "Keep draft after Save error");
                    shot = Some("13-reopen-after-stop-preserves-state");
                } else {
                    wait = true;
                }
            }
            43 => {
                key(i, Key::Escape, Modifiers::NONE);
            }
            44 => {
                assert!(!self.git.open);
                assert_eq!(s.shots.len(), 13);
                assert_eq!(self.git.saved, 1);
                assert_eq!(self.git.mutations, 2);
                assert!(
                    self.parked[&s.park]
                        .0
                        .rope()
                        .to_string()
                        .starts_with("// FLOW_PARK\n")
                );
                assert!(!self.doc.dirty());
                assert!(self.last_build.is_none() && !self.running && !self.building);
                std::fs::write(s.dir.join("git-flow-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_GIT_FLOW_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"reduced_motion":self.reduced_motion,"no_implicit_save_before_action":s.no_save_before_action,"saved":self.git.saved,"mutations":self.git.mutations,"observations":self.git.observations,"observed_changes":self.git.observed_changes,"opened":self.git.opened,"actual_pointer_clicks":s.clicks,"external_writes":s.external,"screenshots":s.shots,"original_main_blake3":s.original.to_hex().to_string(),"original_park_blake3":s.park_hash.unwrap().to_hex().to_string(),"buffer_only_tabs_and_exact_save":true,"ordered_navigation_and_source_handoff":true,"source_undo_redo_kept":true,"outside_review_path_and_message_focus":true,"save_conflict_no_overwrite":true,"read_cancel_reopen_kept_draft":true})).unwrap()).unwrap();
                eprintln!(
                    "GPU_GIT_FLOW_OK: buffer-only active+parked rows; F6/list arrows/Home/End/Page; same-frame End+Up and Enter+source text; explicit selected Save/diff/no auto-stage; real outside writer+stable path/message focus; original Undo/Redo; local commit; Save conflict/minimum footer; real Git read cancellation/reopen;13 GPU frames"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            eprintln!("GIT_FLOW_STEP_OK: {}", s.step);
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(110);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.git_flow_smoke = Some(s);
    }
    pub(super) fn git_flow_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.git_flow_smoke else {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"selected":self.git.path,"saved":self.git.saved,"mutations":self.git.mutations,"footer":(["git-stage","git-unstage","git-save","git-open-file","git-message","git-commit","git-close"].into_iter().filter_map(|k|self.chrome.hits.get(k).map(|r|(k,[r.min.x,r.min.y,r.max.x,r.max.y]))).collect::<std::collections::BTreeMap<_,_>>())}));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
