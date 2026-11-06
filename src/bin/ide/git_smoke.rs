//! Genuine native Git review/index/commit UX using production pointer and keyboard input.
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
    clicks: usize,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        Self {
            dir,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(1),
            step: 0,
            click: None,
            release: None,
            awaiting: None,
            shots: vec![],
            hash: app.doc.content_hash(),
            clicks: 0,
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
impl Workbench {
    fn git_row_key(&self, name: &str) -> String {
        format!(
            "git-file-{}",
            self.git
                .snapshot
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .position(|e| e.path.file_name() == Some(std::ffi::OsStr::new(name)))
                .unwrap()
        )
    }
    pub(super) fn git_smoke_input(&mut self, ctx: &egui::Context, i: &mut egui::RawInput) {
        let Some(mut s) = self.git_smoke.take() else {
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
            s.start.elapsed() < Duration::from_secs(150),
            "Git step{} timeout busy{} note{} error{}",
            s.step,
            self.git.busy,
            self.git.note,
            self.git.error
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
            self.git_smoke = Some(s);
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
            self.git_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.git_smoke = Some(s);
            return;
        }
        let mut wait = false;
        let mut shot = None;
        match s.step {
            0 => {
                key(i, Key::Home, Modifiers::CTRL | Modifiers::COMMAND);
                i.events.push(Event::Text("// UNSAVED_GIT_BUFFER\n".into()));
            }
            1 => {
                assert!(self.doc.dirty());
                s.click = Some("git-open".into());
            }
            2 => {
                if self.git.open && !self.git.busy && self.git.snapshot.is_some() {
                    shot = Some("01-disk-index-unsaved-separation");
                } else {
                    wait = true;
                }
            }
            3 => {
                s.click = Some(self.git_row_key("MainActivity.kt"));
            }
            4 => {
                if !self.git.busy {
                    assert!(!self.git.diff.contains("UNSAVED_GIT_BUFFER"));
                    s.click = Some("git-stage".into());
                } else {
                    wait = true;
                }
            }
            5 => {
                assert_eq!(self.git.mutations, 0);
                assert!(!self.save_ui.saver.worker_started());
                shot = Some("02-dirty-file-stage-disabled");
            }
            6 => {
                s.click = Some(self.git_row_key("Review.txt"));
            }
            7 => {
                if !self.git.busy && self.git.diff.contains("+reviewed after") {
                    shot = Some("03-real-working-diff");
                } else {
                    wait = true;
                }
            }
            8 => {
                s.click = Some("git-stage".into());
            }
            9 => {
                if !self.git.busy && self.git.mutations == 1 {
                    s.click = Some("git-staged".into());
                } else {
                    wait = true;
                }
            }
            10 => {
                s.click = Some(self.git_row_key("Review.txt"));
            }
            11 => {
                if !self.git.busy && self.git.diff.contains("+reviewed after") {
                    shot = Some("04-staged-diff");
                } else {
                    wait = true;
                }
            }
            12 => {
                s.click = Some("git-unstage".into());
            }
            13 => {
                if !self.git.busy && self.git.mutations == 2 {
                    s.click = Some("git-working".into());
                } else {
                    wait = true;
                }
            }
            14 => {
                s.click = Some(self.git_row_key("Review.txt"));
            }
            15 => {
                if !self.git.busy {
                    s.click = Some("git-stage".into());
                } else {
                    wait = true;
                }
            }
            16 => {
                if !self.git.busy && self.git.mutations == 3 {
                    s.click = Some("git-message".into());
                } else {
                    wait = true;
                }
            }
            17 => {
                i.events
                    .push(Event::Paste("Reviewed UI fixture commit".into()));
                key(i, Key::Enter, Modifiers::CTRL | Modifiers::COMMAND);
            }
            18 => {
                assert_eq!(self.git.mutations, 3);
                assert_eq!(self.git.message, "Reviewed UI fixture commit");
                shot = Some("05-explicit-commit-draft");
            }
            19 => {
                s.click = Some("git-commit".into());
            }
            20 => {
                if !self.git.busy && self.git.mutations == 4 {
                    assert!(self.git.message.is_empty());
                    assert!(
                        !self
                            .git
                            .snapshot
                            .as_ref()
                            .unwrap()
                            .entries
                            .iter()
                            .any(|e| e.path == std::path::Path::new("Review.txt"))
                    );
                    s.click = Some(self.git_row_key("New 猫.txt"));
                } else {
                    wait = true;
                }
            }
            21 => {
                if !self.git.busy && self.git.diff.contains("+new unicode") {
                    shot = Some("06-commit-success-untracked-review");
                } else {
                    wait = true;
                }
            }
            22 => {
                std::fs::write(self.root.join(".git/index.lock"), "owned fixture lock\n").unwrap();
                s.click = Some("git-stage".into());
            }
            23 => {
                if !self.git.busy && !self.git.error.is_empty() {
                    assert!(self.git.error.contains("locked"));
                    assert!(self.git.stale);
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(760., 560.)));
                } else {
                    wait = true;
                }
            }
            24 => {
                if ctx.content_rect().width() <= 761. {
                    s.click = Some("git-details".into());
                } else {
                    wait = true;
                }
            }
            25 => {
                for k in [
                    "git-stage",
                    "git-unstage",
                    "git-message",
                    "git-commit",
                    "git-close",
                ] {
                    assert!(
                        ctx.content_rect().contains_rect(self.chrome.hits[k]),
                        "{k} offscreen"
                    );
                }
                assert_eq!(
                    std::fs::read_to_string(self.root.join(".git/index.lock")).unwrap(),
                    "owned fixture lock\n"
                );
                shot = Some("07-minimum-error-details-footer");
            }
            26 => {
                std::fs::remove_file(self.root.join(".git/index.lock")).unwrap();
                s.click = Some("git-refresh".into());
            }
            27 => {
                if !self.git.busy && !self.git.stale && self.git.error.is_empty() {
                    shot = Some("08-recovery-retains-unsaved-source");
                } else {
                    wait = true;
                }
            }
            28 => {
                key(i, Key::Escape, Modifiers::NONE);
            }
            29 => {
                assert!(!self.git.open);
                key(i, Key::Enter, Modifiers::NONE);
            }
            30 => {
                if self.git.open && !self.git.busy {
                    shot = Some("09-cancel-focus-keyboard-reopen");
                } else {
                    wait = true;
                }
            }
            31 => {
                key(i, Key::Escape, Modifiers::NONE);
            }
            32 => {
                s.click = Some("editor".into());
            }
            33 => {
                key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
            }
            34 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert!(!self.save_ui.saver.worker_started());
                assert_eq!(s.shots.len(), 9);
                std::fs::write(s.dir.join("git-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_GIT_UX_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"reduced_motion":self.reduced_motion,"mutations":self.git.mutations,"actual_pointer_clicks":s.clicks,"dirty_buffer_preserved_and_undone":true,"no_implicit_save":!self.save_ui.saver.worker_started(),"screenshots":s.shots})).unwrap()).unwrap();
                eprintln!(
                    "GPU_GIT_UX_OK: status/disk+staged diff/dirty Stage disabled/stage/unstage/explicit commit/foreign lock+Refresh/minimum footer/focus reopen/local Undo;9 GPU frames"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(110);
        }
        if let Some(n) = shot {
            s.awaiting = Some(n.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.git_smoke = Some(s);
    }
    pub(super) fn git_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.git_smoke else {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"mutations":self.git.mutations,"dialog":self.git.open,"error":self.git.error,"footer": (["git-stage","git-unstage","git-message","git-commit","git-close"].into_iter().filter_map(|k|self.chrome.hits.get(k).map(|r|(k,[r.min.x,r.min.y,r.max.x,r.max.y]))).collect::<std::collections::BTreeMap<_,_>>()) }));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
