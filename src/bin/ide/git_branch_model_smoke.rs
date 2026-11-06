//! Different committed source/resource trees, real UI checkout and genuine K2/optional APK Run.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
use wexandroidstudio::editor_jobs::DiskState;
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    setup: u8,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    paste: Option<String>,
    after_capture: Vec<Event>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    snapshots: Vec<Value>,
    expected: Value,
    main: PathBuf,
    shared: PathBuf,
    legacy: PathBuf,
    feature: PathBuf,
    attempt: u64,
    deploy: bool,
}
impl Smoke {
    pub fn new(dir: PathBuf, a: &Workbench, deploy: bool) -> Self {
        let expected =
            serde_json::from_slice(&std::fs::read(dir.join("expectations.json")).unwrap()).unwrap();
        let main = a
            .root
            .join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            setup: 0,
            click: None,
            release: None,
            paste: None,
            after_capture: vec![],
            awaiting: None,
            shots: vec![],
            snapshots: vec![],
            expected,
            main,
            shared: a
                .root
                .join("shared/src/main/kotlin/dev/wex/shared/Greeter.kt"),
            legacy: a
                .root
                .join("app/src/main/kotlin/dev/wex/android/LegacyOnly.kt"),
            feature: a
                .root
                .join("app/src/main/kotlin/dev/wex/android/BranchFeature.kt"),
            attempt: 0,
            deploy,
        }
    }
    fn fill(&mut self, key: &str, text: &str) {
        self.click = Some(key.into());
        self.paste = Some(text.into());
    }
    fn branch_row(&self, a: &Workbench, name: &str) -> String {
        let c = a.git.branch_ui.catalog.as_ref().unwrap();
        format!(
            "git-branch-{}",
            c.items.iter().position(|b| b.name == name).unwrap()
        )
    }
    fn current(&self, a: &Workbench, name: &str) -> bool {
        !a.git.busy
            && !a.git.stale
            && a.git
                .branch_ui
                .catalog
                .as_ref()
                .is_some_and(|c| c.items.iter().any(|b| b.current && b.name == name))
    }
    fn clean(&self, a: &Workbench, feature: bool) -> bool {
        let expected = self.expected[if feature { "feature" } else { "main" }]
            .as_str()
            .unwrap();
        let shared = self.expected[if feature {
            "shared_feature"
        } else {
            "shared_main"
        }]
        .as_str()
        .unwrap();
        a.doc.path.as_ref() == Some(&self.main)
            && !a.doc.dirty()
            && a.doc.rope() == expected
            && a.parked
                .get(&self.shared)
                .is_some_and(|(d, _)| !d.dirty() && d.rope() == shared)
            && a.parked.get(&self.legacy).is_some_and(|(d, _)| !d.dirty())
            && if feature {
                matches!(
                    a.editor_ui.conflicts.get(&self.legacy),
                    Some(DiskState::Missing)
                ) && !a.last_buffers.iter().any(|(p, _, _)| p == &self.legacy)
                    && a.files.contains(&self.feature)
                    && !a.files.contains(&self.legacy)
            } else {
                !a.editor_ui.conflicts.contains_key(&self.legacy)
                    && a.last_buffers.iter().any(|(p, _, _)| p == &self.legacy)
                    && !a.files.contains(&self.feature)
                    && a.files.contains(&self.legacy)
            }
            && a.model_status.as_ref().is_some_and(|m| {
                m.phase == language::ModelPhase::Applied && m.attempt > self.attempt
            })
            && a.diagnostic_snapshot.as_ref() == Some(&(self.main.clone(), a.doc.version))
            && !a.diagnostics.iter().any(|d| d.severity == Some(1))
    }
    fn capture(&mut self, a: &Workbench, name: &str) {
        self.snapshots.push(json!({"name":name,"model":a.model_status,"diagnostic_snapshot":a.diagnostic_snapshot,"diagnostics":a.diagnostics,"active":a.doc.path,"active_version":a.doc.version,"published_parked":a.last_buffers.iter().map(|(p,v,r)|json!({"path":p,"version":v,"revision":r})).collect::<Vec<_>>(),"retained_parked":a.parked.iter().map(|(p,(d,_))|json!({"path":p,"dirty":d.dirty(),"hash":d.content_hash().to_hex().to_string()})).collect::<Vec<_>>() }));
    }
}
fn key(i: &mut egui::RawInput, key: Key, modifiers: Modifiers) {
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
const COMMAND: Modifiers = Modifiers {
    ctrl: true,
    command: true,
    ..Modifiers::NONE
};
impl Workbench {
    pub(super) fn git_branch_model_smoke_input(
        &mut self,
        ctx: &egui::Context,
        i: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.git_branch_model_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(if s.deploy { 600 } else { 300 }),
            "Branch model step{} timeout path{:?} model{:?} note{} status{} error{} diagnostics{:?}",
            s.step,
            self.doc.path,
            self.model_status,
            self.git.note,
            self.status,
            self.git.error,
            self.diagnostics
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
            self.git_branch_model_smoke = Some(s);
            return;
        }
        if let Some(k) = s.click.take() {
            if let Some(r) = self.chrome.hits.get(&k).copied() {
                assert!(ctx.content_rect().contains_rect(r), "{k} outside {r:?}");
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
                s.release = Some((p, Instant::now() + Duration::from_millis(50)));
            } else {
                s.click = Some(k);
            }
            self.git_branch_model_smoke = Some(s);
            return;
        }
        if let Some(text) = s.paste.take() {
            key(i, Key::A, COMMAND);
            if text.is_empty() {
                key(i, Key::Backspace, Modifiers::NONE);
            } else {
                i.events.push(Event::Paste(text));
            }
            self.git_branch_model_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.git_branch_model_smoke = Some(s);
            return;
        }
        if !s.after_capture.is_empty() {
            i.events.append(&mut s.after_capture);
            self.git_branch_model_smoke = Some(s);
            return;
        }
        let event_start = i.events.len();
        let mut wait = false;
        let mut shot = None;
        let mut done = false;
        match s.step {
            0 => {
                if self.doc.path.as_ref() != Some(&s.main) {
                    if s.setup == 0 {
                        s.fill("file-filter", "MainActivity.kt");
                        s.setup = 1;
                    } else if s.setup == 1 {
                        s.click = Some("file-MainActivity.kt".into());
                        s.setup = 2;
                    }
                    wait = true;
                } else if self
                    .model_status
                    .as_ref()
                    .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                    && self.diagnostic_snapshot.as_ref()
                        == Some(&(s.main.clone(), self.doc.version))
                    && !self.diagnostics.iter().any(|d| d.severity == Some(1))
                {
                    s.attempt = self.model_status.as_ref().unwrap().attempt;
                    shot = Some("01-main-tree-k2");
                    s.fill("file-filter", "LegacyOnly.kt");
                } else {
                    wait = true;
                }
            }
            1 => s.click = Some("file-LegacyOnly.kt".into()),
            2 => {
                if self.doc.path.as_ref() == Some(&s.legacy) && self.open_ui.pending.is_none() {
                    s.fill("file-filter", "Greeter.kt");
                } else {
                    wait = true;
                }
            }
            3 => s.click = Some("file-Greeter.kt".into()),
            4 => {
                if self.doc.path.as_ref() == Some(&s.shared) && self.open_ui.pending.is_none() {
                    shot = Some("02-open-clean-parked-sources");
                    s.click = Some("tab-MainActivity.kt".into());
                } else {
                    wait = true;
                }
            }
            5 => {
                if self.doc.path.as_ref() == Some(&s.main) {
                    s.fill("file-filter", "");
                } else {
                    wait = true;
                }
            }
            6 => key(i, Key::G, COMMAND | Modifiers::SHIFT),
            7 | 18 | 27 => {
                if self.git.open && !self.git.busy && !self.git.stale {
                    s.click = Some("git-branches".into());
                } else {
                    wait = true;
                }
            }
            8 | 19 | 28 => {
                if self.git.branch_ui.open && !self.git.busy && !self.git.stale {
                    s.click = Some(s.branch_row(
                        self,
                        if s.step == 19 {
                            "main"
                        } else {
                            "feature/model"
                        },
                    ));
                } else {
                    wait = true;
                }
            }
            9 | 20 | 29 => s.click = Some("git-branch-switch".into()),
            10 | 21 | 30 => {
                assert!(self.git.branch_ui.confirm);
                s.click = Some("git-branch-switch".into());
            }
            11 | 22 | 31 => {
                if s.current(
                    self,
                    if s.step == 22 {
                        "main"
                    } else {
                        "feature/model"
                    },
                ) {
                    if s.step == 11 {
                        shot = Some("03-feature-ref-committed");
                    }
                    s.click = Some("git-branch-back".into());
                } else {
                    wait = true;
                }
            }
            12 | 23 | 32 => {
                if !self.git.branch_ui.open && !self.git.busy {
                    s.click = Some("git-close".into());
                } else {
                    wait = true;
                }
            }
            13 => {
                if !self.git.open && s.clean(self, true) {
                    s.capture(self, "feature");
                    s.attempt = self.model_status.as_ref().unwrap().attempt;
                    shot = Some("04-feature-source-resource-model");
                    key(i, Key::End, COMMAND);
                    i.events.push(Event::Paste(
                        "\nfun branchGhostProbe() = LegacyOnly().value()\n".into(),
                    ));
                } else {
                    wait = true;
                }
            }
            14 => {
                if self.diagnostic_snapshot.as_ref() == Some(&(s.main.clone(), self.doc.version))
                    && self
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == Some(1) && d.message.contains("LegacyOnly"))
                {
                    s.capture(self, "removed-ghost-unresolved");
                    shot = Some("05-removed-symbol-real-error");
                    key(i, Key::Z, COMMAND);
                } else {
                    wait = true;
                }
            }
            15 => {
                if self.doc.rope() == s.expected["feature"].as_str().unwrap()
                    && !self.doc.dirty()
                    && self.diagnostic_snapshot.as_ref()
                        == Some(&(s.main.clone(), self.doc.version))
                    && !self.diagnostics.iter().any(|d| d.severity == Some(1))
                {
                    key(i, Key::G, COMMAND | Modifiers::SHIFT);
                } else {
                    wait = true;
                }
            }
            16 => {
                s.step = 18;
                wait = true;
            }
            17 => unreachable!(),
            24 => {
                if !self.git.open && s.clean(self, false) {
                    s.capture(self, "main-restored");
                    s.attempt = self.model_status.as_ref().unwrap().attempt;
                    shot = Some("06-main-source-uri-restored");
                    key(i, Key::G, COMMAND | Modifiers::SHIFT);
                } else {
                    wait = true;
                }
            }
            25 => {
                s.step = 27;
                wait = true;
            }
            26 => unreachable!(),
            33 => {
                if !self.git.open && s.clean(self, true) {
                    s.capture(self, "feature-again");
                    let text = s.expected["feature"].as_str().unwrap();
                    let line = text
                        .lines()
                        .position(|l| l.contains("val branchProof"))
                        .unwrap();
                    let col = text
                        .lines()
                        .nth(line)
                        .unwrap()
                        .find("BranchFeature")
                        .unwrap()
                        + 2;
                    key(i, Key::Home, COMMAND);
                    for _ in 0..line {
                        key(i, Key::ArrowDown, Modifiers::NONE);
                    }
                    key(i, Key::Home, Modifiers::NONE);
                    for _ in 0..col {
                        key(i, Key::ArrowRight, Modifiers::NONE);
                    }
                } else {
                    wait = true;
                }
            }
            34 => key(i, Key::F12, Modifiers::NONE),
            35 => {
                if self.doc.path.as_ref() == Some(&s.feature)
                    && self.status.starts_with("Definition")
                {
                    shot = Some("07-real-new-class-definition");
                    s.click = Some("tab-MainActivity.kt".into());
                } else {
                    wait = true;
                }
            }
            36 => {
                if self.doc.path.as_ref() == Some(&s.main) && !self.doc.dirty() {
                    if s.deploy {
                        if self
                            .devices
                            .iter()
                            .any(|d| d.serial == "emulator-5580" && d.usable())
                        {
                            self.select_device("emulator-5580".into());
                            s.click = Some("run".into());
                        } else {
                            wait = true;
                        }
                    } else {
                        shot = Some("08-desktop-final-feature");
                        done = true;
                    }
                } else {
                    wait = true;
                }
            }
            37 => {
                if self.last_run.as_ref().is_some_and(|r| {
                    r.serial == "emulator-5580"
                        && self.ring.entries().any(|e| {
                            e.pid == r.pid
                                && e.message.contains(
                                    "WEX_BRANCH_OK tree=FEATURE value=Feature, FEATURE_TREE!",
                                )
                                && e.message.contains(&r.run_id)
                        })
                }) {
                    s.click = Some("dock-0".into());
                } else {
                    wait = true;
                }
            }
            38 => s.fill("logcat-filter", "WEX_BRANCH_OK"),
            39 => {
                assert_eq!(self.filter, "WEX_BRANCH_OK");
                assert!(self.filter_error.is_none());
                shot = Some("08-feature-apk-logcat");
                done = true;
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            // Capture the asserted state before its next source mutation. In
            // particular, the real unresolved-symbol screenshot must precede Undo.
            s.after_capture.extend(i.events.drain(event_start..));
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            s.awaiting = Some(name.into());
        }
        if done {
            // Wait for the final real GPU screenshot before publishing or closing.
            s.step = 40;
        } else if !wait {
            s.step += 1;
        }
        s.next = Instant::now() + Duration::from_millis(140);
        self.git_branch_model_smoke = Some(s);
    }
    pub(super) fn git_branch_model_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.git_branch_model_smoke.take() else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for e in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = e {
                    let path = s.dir.join(format!("{name}.png"));
                    let data = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        &path,
                        &data,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    s.shots.push(json!({"path":path,"width":image.size[0],"height":image.size[1],"name":name}));
                    s.awaiting = None;
                    break;
                }
            }
        }
        if s.step == 40 && s.awaiting.is_none() {
            assert_eq!(self.git.mutations, 3);
            assert!(!self.doc.dirty());
            assert_eq!(
                self.doc.rope().to_string(),
                s.expected["feature"].as_str().unwrap()
            );
            let proof = json!({"result":"GPU_GIT_BRANCH_MODEL_OK","renderer":"wgpu","input":"actual native RawInput file tabs/filter/Git branch review/switch/source Paste-Undo/F12/optional Run","screenshots":s.shots,"snapshots":s.snapshots,"mutations":self.git.mutations,"active_and_parked_reloaded":true,"clean_deleted_buffer_retained_not_published":true,"restored_uri_republished":true,"fresh_generated_resource_model":true,"genuine_definition":s.feature,"ghost_symbol_unresolved":true,"source_undo_and_disk_preserved":true,"deployment":self.last_run,"logcat":self.ring.entries().collect::<Vec<_>>(),"phone_untouched":true});
            std::fs::write(
                s.dir.join("branch-model-window.json"),
                serde_json::to_vec_pretty(&proof).unwrap(),
            )
            .unwrap();
            println!(
                "GPU_GIT_BRANCH_MODEL_OK: different source/resource trees + real K2/removed ghost/F12/Undo;8 GPU captures;deploy={}",
                s.deploy
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.git_branch_model_smoke = Some(s);
    }
}
