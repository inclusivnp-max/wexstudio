//! Real native replacement widgets and shared keyboard history; no synthetic success images.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::{io::Write, time::Instant};
const MARK: &str = "WEX_REPLACE_検索";
const NEW: &str = "WEX_REPLACED_猫";
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
    stale: PathBuf,
    stale_text: String,
    writes: Vec<Value>,
    reviews: Vec<Value>,
    clicks: usize,
}
impl Smoke {
    pub fn new(dir: PathBuf, app: &Workbench) -> Self {
        let base = app.root.join("app/src/main/kotlin/dev/wex/android");
        let stale = base.join("ReplaceStale.txt");
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
            park: base.join("ReplacePark.kt"),
            park_hash: None,
            stale_text: std::fs::read_to_string(&stale).unwrap(),
            stale,
            writes: vec![],
            reviews: vec![],
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
fn external(s: &mut Smoke, text: &str) {
    let mut p=std::process::Command::new("python3").args(["-c","from pathlib import Path; import sys; Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read()); print('REPLACE_EXTERNAL_WRITE_OK')",s.stale.to_str().unwrap()]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    p.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let o = p.wait_with_output().unwrap();
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "REPLACE_EXTERNAL_WRITE_OK"
    );
    s.writes.push(json!({"path":s.stale,"hash":blake3::hash(text.as_bytes()).to_hex().to_string(),"result":"REPLACE_EXTERNAL_WRITE_OK","exit":o.status.code()}));
}
impl Workbench {
    pub(super) fn project_replace_smoke_input(
        &mut self,
        ctx: &egui::Context,
        i: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.project_replace_smoke.take() else {
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
            "replace native step{} timeout; {} / {} dialog{} busy{} applied{} undo{} redo{}",
            s.step,
            self.status,
            self.preplace.error,
            self.preplace.dialog,
            self.preplace.busy,
            self.preplace.applied,
            self.preplace.undone,
            self.preplace.redone
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
            self.project_replace_smoke = Some(s);
            return;
        }
        if let Some(k) = s.click.take() {
            if let Some(r) = self
                .chrome
                .hits
                .get(&k)
                .or_else(|| self.psearch.hits.get(&k))
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
            self.project_replace_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.project_replace_smoke = Some(s);
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
                s.click = Some("file-ReplacePark.kt".into());
            }
            2 => {
                if self.doc.path.as_ref() == Some(&s.park) && self.open_ui.pending.is_none() {
                    s.park_hash = Some(self.doc.content_hash());
                    i.events.push(Event::Text(format!("// {MARK} PARKED\n")));
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
                        Key::F,
                        Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                    );
                    i.events.push(Event::Text(MARK.into()));
                    key(i, Key::Enter, Modifiers::NONE);
                } else {
                    wait = true;
                }
            }
            5 => {
                if !self.psearch.pending
                    && !self.psearch.stale
                    && self
                        .psearch
                        .result
                        .as_ref()
                        .is_some_and(|r| r.stats.matches == 4)
                {
                    s.click = Some("ps-replace".into());
                } else {
                    wait = true;
                }
            }
            6 => {
                if self.preplace.dialog {
                    s.click = Some("pr-text".into());
                } else {
                    wait = true;
                }
            }
            7 => {
                i.events.push(Event::Paste(NEW.into()));
            }
            8 => {
                assert_eq!(self.preplace.replacement, NEW);
                s.click = Some("pr-preview".into());
            }
            9 | 17 => {
                if let Some(p) = self.preplace.plan.as_ref().filter(|_| !self.preplace.busy) {
                    assert_eq!(p.edits(), 4);
                    assert_eq!(p.files.len(), 4);
                    assert_eq!(p.files.iter().filter(|f| f.dirty).count(), 2);
                    assert_ne!(p.worker, format!("{:?}", std::thread::current().id()));
                    s.reviews.push(json!({"edits":p.edits(),"files":p.files.iter().map(|f|json!({"path":f.path,"dirty":f.dirty,"rows":f.rows})).collect::<Vec<_>>(),"worker":p.worker}));
                    shot = Some(if s.step == 9 {
                        "01-review-dirty-and-closed"
                    } else {
                        "04-reviewed-fresh-snapshots"
                    });
                } else {
                    wait = true;
                }
            }
            10 => {
                s.click = Some("pr-cancel".into());
            }
            11 => {
                assert!(!self.preplace.dialog);
                assert_eq!(self.preplace.applied, 0);
                assert_eq!(self.preplace.cancelled, 1);
                assert!(self.doc.rope().to_string().contains(MARK));
                shot = Some("02-cancel-no-buffer-changes");
                s.click = Some("ps-replace".into());
            }
            12 => {
                if self.preplace.dialog {
                    s.click = Some("pr-preview".into());
                } else {
                    wait = true;
                }
            }
            13 => {
                if self.preplace.plan.is_some() && !self.preplace.busy {
                    external(&mut s, "external change while preview was visible\n");
                    s.click = Some("pr-apply".into());
                } else {
                    wait = true;
                }
            }
            14 => {
                if !self.preplace.busy && self.preplace.rejected == 1 {
                    assert_eq!(self.preplace.applied, 0);
                    assert!(self.doc.rope().to_string().contains(MARK));
                    assert_eq!(self.tabs.len(), 2);
                    shot = Some("03-external-conflict-no-partial-apply");
                } else {
                    wait = true;
                }
            }
            15 => {
                s.click = Some("pr-cancel".into());
            }
            16 => {
                let t = s.stale_text.clone();
                external(&mut s, &t);
                s.click = Some("ps-replace".into());
            }
            // Preview requests come from the real production button.
            18 => {
                s.click = Some("pr-apply".into());
            }
            19 => {
                if self.preplace.applied == 1 && !self.preplace.busy {
                    assert!(!self.preplace.dialog);
                    assert_eq!(self.tabs.len(), 4);
                    assert!(self.doc.rope().to_string().contains(NEW));
                    assert!(
                        self.parked
                            .values()
                            .all(|(d, _)| d.dirty() && d.rope().to_string().contains(NEW))
                    );
                    shot = Some("05-applied-all-unsaved");
                } else {
                    wait = true;
                }
            }
            20 => {
                s.click = Some("tab-ReplacePark.kt".into());
            }
            21 => {
                if self.doc.path.as_ref() == Some(&s.park) {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            22 => {
                if self.preplace.undone == 1 && !self.preplace.busy {
                    assert!(self.doc.dirty() && self.doc.rope().to_string().contains(MARK));
                    assert!(
                        self.parked
                            .values()
                            .filter(|(d, _)| d
                                .path
                                .as_ref()
                                .is_some_and(|p| p.extension().is_some_and(|e| e == "txt")))
                            .all(|(d, _)| !d.dirty())
                    );
                    shot = Some("06-keyboard-shared-undo");
                } else {
                    wait = true;
                }
            }
            23 => {
                key(i, Key::Y, Modifiers::CTRL | Modifiers::COMMAND);
            }
            24 => {
                if self.preplace.redone == 1 && !self.preplace.busy {
                    assert!(self.doc.rope().to_string().contains(NEW));
                    assert!(
                        self.parked
                            .values()
                            .all(|(d, _)| d.dirty() && d.rope().to_string().contains(NEW))
                    );
                    shot = Some("07-keyboard-shared-redo");
                } else {
                    wait = true;
                }
            }
            25 => {
                s.click = Some("ps-undo".into());
            }
            26 => {
                if self.preplace.undone == 2 && !self.preplace.busy {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            27 => {
                assert_eq!(self.doc.content_hash(), s.park_hash.unwrap());
                assert!(!self.doc.dirty());
                s.click = Some("tab-MainActivity.kt".into());
            }
            28 => {
                if self.doc.path.as_ref() == Some(&s.main) {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            29 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                assert!(!self.save_ui.saver.worker_started());
                assert!(self.last_build.is_none() && !self.building && !self.running);
                shot = Some("08-all-original-buffers-and-local-history-restored");
            }
            30 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(920., 680.)));
                s.click = Some("ps-replace".into());
            }
            31 => {
                if self.preplace.dialog {
                    s.click = Some("pr-preview".into());
                } else {
                    wait = true;
                }
            }
            32 => {
                if let Some(p) = self.preplace.plan.as_ref().filter(|_| !self.preplace.busy) {
                    assert_eq!(p.files.len(), 2);
                    assert!(ctx.content_rect().height() <= 700.);
                    let r = self.chrome.hits["pr-cancel"];
                    assert!(ctx.content_rect().contains_rect(r));
                    shot = Some("09-compact-reviewed-modal");
                } else {
                    wait = true;
                }
            }
            33 => {
                key(i, Key::Escape, Modifiers::NONE);
            }
            34 => {
                assert!(!self.preplace.dialog);
                assert_eq!(s.shots.len(), 9);
                assert_eq!(self.preplace.applied, 1);
                assert_eq!(self.preplace.undone, 2);
                assert_eq!(self.preplace.redone, 1);
                assert_eq!(self.preplace.rejected, 1);
                assert_eq!(self.preplace.cancelled, 3);
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                std::fs::write(s.dir.join("project-replace-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_PROJECT_REPLACE_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"reduced_motion":self.reduced_motion,"applied":self.preplace.applied,"undone":self.preplace.undone,"redone":self.preplace.redone,"rejected":self.preplace.rejected,"cancelled":self.preplace.cancelled,"source_buffers_restored":true,"dirty_active_and_parked_reviewed":true,"closed_files_unsaved_not_written":true,"shared_keyboard_undo_redo":true,"local_history_preserved":true,"compact_modal":true,"actual_pointer_clicks":s.clicks,"reviews":s.reviews,"external_writes":s.writes,"save_worker_started":self.save_ui.saver.worker_started(),"deployment_invoked":false,"screenshots":s.shots,"foreground_thread":format!("{:?}",std::thread::current().id()),"input":"genuine native winit RawInput keys/pointer/Paste; production M3 widgets and replacement/history worker; separate Python disk writer"})).unwrap()).unwrap();
                eprintln!(
                    "GPU_PROJECT_REPLACE_OK: reviewed dirty active+parked+closed buffers; Cancel/external conflict; atomic Apply; keyboard shared Undo/Redo; original local history+disk restored; compact M3;9 GPU frames;no Gradle/APK/phone"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        // The second fresh dialog reuses the user's replacement, but requires a NEW review.
        if s.step == 17
            && self.preplace.plan.is_none()
            && !self.preplace.busy
            && self.preplace.dialog
        {
            s.click = Some("pr-preview".into());
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(100);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.project_replace_smoke = Some(s);
    }
    pub(super) fn project_replace_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.project_replace_smoke else {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"applied":self.preplace.applied,"undone":self.preplace.undone,"redone":self.preplace.redone,"error":self.preplace.error}));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
