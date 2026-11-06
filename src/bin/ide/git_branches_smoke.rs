//! Real GPU window inputs for local branches, including unsaved protection/recovery.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) struct Smoke {
    dir: PathBuf,
    start: Instant,
    next: Instant,
    step: u8,
    click: Option<String>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    hash: blake3::Hash,
}
impl Smoke {
    pub fn new(dir: PathBuf, a: &Workbench) -> Self {
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            hash: a.doc.content_hash(),
        }
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
impl Workbench {
    pub(super) fn git_branches_smoke_input(&mut self, ctx: &egui::Context, i: &mut egui::RawInput) {
        let Some(mut s) = self.git_branches_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(120),
            "Branch step{} timeout busy{} note{} error{} focus{:?}",
            s.step,
            self.git.busy,
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
            self.git_branches_smoke = Some(s);
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
            self.git_branches_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.git_branches_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                key(i, Key::Home, Modifiers::CTRL | Modifiers::COMMAND);
                i.events.push(Event::Text("// UNSAVED_BRANCH\n".into()));
            }
            1 => {
                assert!(self.doc.dirty());
                key(
                    i,
                    Key::G,
                    Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                );
            }
            2 => {
                if self.git.open && !self.git.busy && !self.git.stale {
                    s.click = Some("git-branches".into());
                } else {
                    wait = true;
                }
            }
            3 => {
                if self.git.branch_ui.open && !self.git.busy && !self.git.stale {
                    shot = Some("01-unsaved-switch-protected");
                    s.click = Some("git-branch-name".into());
                } else {
                    wait = true;
                }
            }
            4 => i.events.push(Event::Paste("feature/live".into())),
            5 => s.click = Some("git-branch-create".into()),
            6 => {
                if !self.git.busy
                    && self.git.branch_ui.catalog.as_ref().is_some_and(|c| {
                        c.items
                            .iter()
                            .any(|b| b.name == "feature/live" && !b.current)
                    })
                {
                    assert!(self.doc.dirty());
                    assert_eq!(self.git.mutations, 1);
                    shot = Some("02-created-with-unsaved-buffer");
                    key(i, Key::F6, Modifiers::NONE);
                    key(i, Key::Home, Modifiers::NONE);
                    key(i, Key::Enter, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            7 => {
                assert!(self.git.branch_ui.confirm && self.doc.dirty());
                assert_eq!(self.git.mutations, 1);
                shot = Some("03-keyboard-review-no-switch");
                s.click = Some("git-branch-back".into());
            }
            8 => {
                if !self.git.branch_ui.open && !self.git.busy {
                    s.click = Some("git-close".into());
                } else {
                    wait = true;
                }
            }
            9 => {
                if !self.git.open {
                    s.click = Some("editor".into());
                } else {
                    wait = true;
                }
            }
            10 => key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND),
            11 => {
                assert!(!self.doc.dirty() && self.doc.content_hash() == s.hash);
                key(
                    i,
                    Key::G,
                    Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                );
            }
            12 => {
                if self.git.open && !self.git.busy && !self.git.stale {
                    s.click = Some("git-branches".into());
                } else {
                    wait = true;
                }
            }
            13 => {
                if self.git.branch_ui.open && !self.git.busy && !self.git.stale {
                    let c = self.git.branch_ui.catalog.as_ref().unwrap();
                    let n = c
                        .items
                        .iter()
                        .position(|b| b.name == "feature/live")
                        .unwrap();
                    s.click = Some(format!("git-branch-{n}"));
                } else {
                    wait = true;
                }
            }
            14 => {
                assert_eq!(self.git.mutations, 1);
                assert!(!self.git.branch_ui.confirm);
                s.click = Some("git-branch-switch".into());
            }
            15 => {
                assert!(self.git.branch_ui.confirm);
                shot = Some("04-explicit-switch-review");
                s.click = Some("git-branch-switch".into());
            }
            16 => {
                if !self.git.busy
                    && self.git.branch_ui.catalog.as_ref().is_some_and(|c| {
                        c.items
                            .iter()
                            .any(|b| b.name == "feature/live" && b.current)
                    })
                {
                    assert_eq!(self.git.mutations, 2);
                    assert_eq!(self.doc.content_hash(), s.hash);
                    shot = Some("05-switched-source-undo-kept");
                    s.click = Some("git-branch-create".into());
                } else {
                    wait = true;
                }
            }
            17 => {
                if !self.git.busy && !self.git.error.is_empty() {
                    assert!(self.git.stale);
                    assert_eq!(self.git.branch_ui.name, "feature/live");
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(760., 560.)));
                } else {
                    wait = true;
                }
            }
            18 => {
                if ctx.content_rect().width() <= 761. {
                    s.click = Some("git-branch-details".into());
                } else {
                    wait = true;
                }
            }
            19 => {
                assert!(self.git.details);
                shot = Some("06-minimum-error-footer");
                s.click = Some("git-branch-refresh".into());
            }
            20 => {
                if !self.git.busy && !self.git.stale && self.git.error.is_empty() {
                    s.click = Some("git-branch-name".into());
                } else {
                    wait = true;
                }
            }
            21 => {
                key(i, Key::A, Modifiers::CTRL | Modifiers::COMMAND);
                i.events.push(Event::Paste("feature/recovered".into()));
            }
            22 => s.click = Some("git-branch-create".into()),
            23 => {
                if !self.git.busy
                    && self.git.branch_ui.catalog.as_ref().is_some_and(|c| {
                        c.items
                            .iter()
                            .any(|b| b.name == "feature/recovered" && !b.current)
                    })
                {
                    assert_eq!(self.git.mutations, 3);
                    shot = Some("07-refresh-recovery");
                    s.click = Some("git-branch-back".into());
                } else {
                    wait = true;
                }
            }
            24 => {
                if !self.git.branch_ui.open && !self.git.busy {
                    s.click = Some("git-close".into());
                } else {
                    wait = true;
                }
            }
            25 => {
                if !self.git.open {
                    assert!(!self.doc.dirty());
                    assert_eq!(self.doc.content_hash(), s.hash);
                    let report = json!({"result":"GPU_GIT_BRANCHES_OK","renderer":"wgpu","input":"real native winit RawInput keyboard/pointer/clipboard; actual installed Git","screenshots":s.shots,"mutations":self.git.mutations,"dirty_switch_blocked":true,"create_preserves_unsaved":true,"selection_and_enter_never_switch":true,"source_undo_preserved":true,"explicit_review_then_switch":true,"duplicate_error_refresh_recovery":true,"no_save_build_deployment":!self.save_ui.saver.worker_started()&&!self.building&&!self.running,"reduced_motion":self.reduced_motion});
                    std::fs::write(
                        s.dir.join("git-branches-window.json"),
                        serde_json::to_vec_pretty(&report).unwrap(),
                    )
                    .unwrap();
                    println!(
                        "GPU_GIT_BRANCHES_OK: real local create/review/switch, dirty source Undo, keyboard-only review, duplicate/error/minimum footer/Refresh recovery;7 native GPU captures; no APK/VM/phone"
                    );
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                } else {
                    wait = true;
                }
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            s.awaiting = Some(name.into());
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(140);
        }
        self.git_branches_smoke = Some(s);
    }
    pub(super) fn git_branches_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.git_branches_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for event in ctx.input(|i| i.events.clone()) {
                if let Event::Screenshot { image, .. } = event {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"footer":(["git-branch-name","git-branch-create","git-branch-switch","git-branch-refresh","git-branch-back"].into_iter().filter_map(|k|self.chrome.hits.get(k).map(|r|(k,[r.min.x,r.min.y,r.max.x,r.max.y]))).collect::<std::collections::BTreeMap<_,_>>())}));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
