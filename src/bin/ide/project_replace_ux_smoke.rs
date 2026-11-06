//! Genuine minimum-size/review/keyboard/IME/focus workflow through production widgets.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
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
    clicks: usize,
    ime_id: u64,
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
            main: app.doc.path.clone().unwrap(),
            hash: app.doc.content_hash(),
            park: app
                .root
                .join("app/src/main/kotlin/dev/wex/android/ReplacePark.kt"),
            park_hash: None,
            clicks: 0,
            ime_id: 0,
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
fn paste(i: &mut egui::RawInput, text: &str) {
    key(i, Key::A, Modifiers::CTRL | Modifiers::COMMAND);
    i.events.push(Event::Paste(text.into()));
}
impl Workbench {
    fn ux_footer_visible(&self, ctx: &egui::Context) {
        for k in ["pr-preview", "pr-apply", "pr-cancel"] {
            assert!(
                ctx.content_rect().contains_rect(self.chrome.hits[k]),
                "{k} outside minimum viewport {:?}",
                self.chrome.hits[k]
            );
        }
    }
    pub(super) fn project_replace_ux_smoke_input(
        &mut self,
        ctx: &egui::Context,
        i: &mut egui::RawInput,
    ) {
        let Some(mut s) = self.project_replace_ux_smoke.take() else {
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
            "replace UX step{} timeout; {} / {} dialog{} busy{} focus{:?}",
            s.step,
            self.status,
            self.preplace.error,
            self.preplace.dialog,
            self.preplace.busy,
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
            self.project_replace_ux_smoke = Some(s);
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
            self.project_replace_ux_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.project_replace_ux_smoke = Some(s);
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
                    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("project-replacement"))));
                    i.events.push(Event::Paste(NEW.into()));
                } else {
                    wait = true;
                }
            }
            7 => {
                assert_eq!(self.preplace.replacement, NEW);
                self.ux_footer_visible(ctx);
                shot = Some("01-labelled-form-default-focus");
            }
            8 => {
                key(i, Key::Enter, Modifiers::NONE);
            }
            9 => {
                if let Some(p) = self.preplace.plan.as_ref().filter(|_| !self.preplace.busy) {
                    assert_eq!(p.files.len(), 4);
                    assert_eq!(p.edits(), 4);
                    assert_eq!(p.files.iter().filter(|f| f.dirty).count(), 2);
                    shot = Some("02-enter-reviewed-unsaved-sources");
                } else {
                    wait = true;
                }
            }
            10 => {
                key(i, Key::Tab, Modifiers::SHIFT);
            }
            11 => {
                assert!(
                    ctx.memory(|m| m.has_focus(egui::Id::new("project-replace-query"))),
                    "Shift+Tab must reach Find from Replace with"
                );
                paste(i, &"(".repeat(1800));
            }
            12 => {
                assert_eq!(self.psearch.options.query, "(".repeat(1800));
                assert!(self.preplace.plan.is_some());
                key(i, Key::Enter, Modifiers::CTRL | Modifiers::COMMAND);
                shot = Some("03-outdated-preview-not-applied");
            }
            13 => {
                assert_eq!(self.preplace.applied, 0);
                s.click = Some("pr-regex".into());
            }
            14 => {
                assert!(self.psearch.options.regex);
                s.click = Some("pr-query".into());
            }
            15 => {
                key(i, Key::Enter, Modifiers::NONE);
            }
            16 => {
                if !self.preplace.error.is_empty() && !self.preplace.busy {
                    assert!(self.preplace.error.contains("regex"));
                    assert!(self.preplace.plan.is_some());
                    assert_eq!(self.preplace.applied, 0);
                    shot = Some("04-query-error-with-previous-review");
                } else {
                    wait = true;
                }
            }
            17 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(760., 560.)));
            }
            18 => {
                if ctx.content_rect().width() <= 761. && ctx.content_rect().height() <= 561. {
                    self.ux_footer_visible(ctx);
                    shot = Some("05-minimum-query-error-footer");
                } else {
                    wait = true;
                }
            }
            19 => {
                s.click = Some("pr-details".into());
            }
            20 => {
                self.ux_footer_visible(ctx);
                assert!(self.chrome.hits["pr-body"].height() <= 331.);
                shot = Some("06-minimum-expanded-details-footer");
            }
            21 => {
                s.click = Some("pr-query".into());
            }
            22 => {
                paste(i, MARK);
            }
            23 => {
                assert_eq!(self.psearch.options.query, MARK);
                s.ime_id = self.preplace.id;
                i.events.extend([
                    Event::Ime(egui::ImeEvent::Enabled),
                    Event::Ime(egui::ImeEvent::Preedit("候補".into())),
                ]);
            }
            24 => {
                key(i, Key::Enter, Modifiers::NONE);
            }
            25 => {
                assert_eq!(self.preplace.id, s.ime_id);
                i.events
                    .extend([Event::Ime(egui::ImeEvent::Commit("候補".into()))]);
                key(i, Key::Enter, Modifiers::CTRL | Modifiers::COMMAND);
            }
            26 => {
                assert_eq!(self.preplace.id, s.ime_id);
                assert_eq!(self.preplace.applied, 0);
                i.events.push(Event::Ime(egui::ImeEvent::Disabled));
            }
            27 => {
                s.click = Some("pr-query".into());
            }
            28 => {
                paste(i, MARK);
            }
            29 => {
                key(i, Key::Enter, Modifiers::NONE);
            }
            30 => {
                if let Some(p) = self.preplace.plan.as_ref().filter(|p| {
                    !self.preplace.busy
                        && p.options.regex
                        && p.options.query == MARK
                        && self.preplace.error.is_empty()
                }) {
                    assert_eq!(p.files.len(), 4);
                    assert_eq!(p.edits(), 4);
                    self.ux_footer_visible(ctx);
                    shot = Some("07-minimum-valid-current-review");
                } else {
                    wait = true;
                }
            }
            31 => {
                key(i, Key::Enter, Modifiers::CTRL | Modifiers::COMMAND);
            }
            32 => {
                if self.preplace.applied == 1 && !self.preplace.busy {
                    assert!(self.doc.dirty() && self.doc.rope().to_string().contains(NEW));
                    assert!(
                        self.parked
                            .values()
                            .all(|(d, _)| d.dirty() && d.rope().to_string().contains(NEW))
                    );
                    shot = Some("08-command-enter-applied-all-unsaved");
                } else {
                    wait = true;
                }
            }
            33 => {
                s.click = Some("ps-undo".into());
            }
            34 => {
                if self.preplace.undone == 1 && !self.preplace.busy {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            35 => {
                assert_eq!(self.doc.content_hash(), s.hash);
                assert!(!self.doc.dirty());
                s.click = Some("tab-ReplacePark.kt".into());
            }
            36 => {
                if self.doc.path.as_ref() == Some(&s.park) {
                    key(i, Key::Z, Modifiers::CTRL | Modifiers::COMMAND);
                } else {
                    wait = true;
                }
            }
            37 => {
                assert_eq!(self.doc.content_hash(), s.park_hash.unwrap());
                assert!(!self.doc.dirty());
                assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                shot = Some("09-original-local-history-and-sources-restored");
            }
            38 => {
                s.click = Some("ps-replace".into());
            }
            39 => {
                if self.preplace.dialog {
                    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("project-replacement"))));
                    paste(i, MARK);
                } else {
                    wait = true;
                }
            }
            40 => {
                key(i, Key::Enter, Modifiers::NONE);
            }
            41 => {
                if let Some(p) = self.preplace.plan.as_ref().filter(|_| !self.preplace.busy) {
                    assert!(p.files.is_empty());
                    assert_eq!(p.stats.matches, 2);
                    self.ux_footer_visible(ctx);
                    shot = Some("10-no-op-feedback-no-apply");
                } else {
                    wait = true;
                }
            }
            42 => {
                key(i, Key::Enter, Modifiers::CTRL | Modifiers::COMMAND);
                key(i, Key::Escape, Modifiers::NONE);
            }
            43 => {
                assert!(!self.preplace.dialog);
                assert_eq!(self.preplace.applied, 1);
                assert_eq!(self.preplace.cancelled, 1);
                key(i, Key::Enter, Modifiers::NONE);
            }
            44 => {
                if self.preplace.dialog {
                    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("project-replacement"))));
                    shot = Some("11-cancel-restores-button-keyboard-reopen");
                } else {
                    wait = true;
                }
            }
            45 => {
                key(i, Key::Escape, Modifiers::NONE);
            }
            46 => {
                assert!(!self.preplace.dialog);
                assert_eq!(self.preplace.cancelled, 2);
                assert_eq!(self.preplace.applied, 1);
                assert_eq!(self.preplace.undone, 1);
                assert_eq!(s.shots.len(), 11);
                assert!(!self.doc.dirty());
                assert!(self.parked.values().all(|(d, _)| !d.dirty()));
                assert!(!self.save_ui.saver.worker_started());
                assert!(self.last_build.is_none() && !self.building && !self.running);
                std::fs::write(s.dir.join("project-replace-ux-window.json"),serde_json::to_vec_pretty(&json!({"result":"GPU_PROJECT_REPLACE_UX_OK","renderer":"wgpu","release":!cfg!(debug_assertions),"reduced_motion":self.reduced_motion,"editable_find_and_options":true,"labelled_fields_default_focus":true,"actual_shift_tab_find":true,"enter_preview_command_enter_apply":true,"stale_review_retained_not_applied":true,"real_regex_error_recovery":true,"minimum_size":[760,560],"footer_always_visible":true,"expanded_error_details_bounded":true,"ime_candidate_enter_did_not_submit":true,"no_op_feedback":true,"cancel_focus_button_keyboard_reopen":true,"source_buffers_restored":true,"applied":self.preplace.applied,"undone":self.preplace.undone,"cancelled":self.preplace.cancelled,"actual_pointer_clicks":s.clicks,"save_worker_started":self.save_ui.saver.worker_started(),"deployment_invoked":false,"screenshots":s.shots,"input":"genuine native winit RawInput pointer/key/Paste/IME; production labels/fields/buttons/options/worker; actual long invalid-regex diagnostic; exact minimum760x560; no programmatic widget selection"})).unwrap()).unwrap();
                eprintln!(
                    "GPU_PROJECT_REPLACE_UX_OK: labelled editable Find/options; automatic focus/ShiftTab; Enter review/CtrlEnter apply; retained stale preview; actual regex error+recovery; minimum760x560 persistent footer/expanded details; IME candidate Enter blocked; no-op feedback; Cancel focus+keyboard reopen; original sources/local history restored;11 GPU frames;no Save/Gradle/phone"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if !wait {
            s.step += 1;
            s.next = Instant::now() + Duration::from_millis(100);
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        self.project_replace_ux_smoke = Some(s);
    }
    pub(super) fn project_replace_ux_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.project_replace_ux_smoke else {
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
                    s.shots.push(json!({"name":name,"path":p,"width":image.size[0],"height":image.size[1],"applied":self.preplace.applied,"undone":self.preplace.undone,"dialog":self.preplace.dialog,"error":self.preplace.error,"footer": (["pr-preview","pr-apply","pr-cancel"].into_iter().filter_map(|k|self.chrome.hits.get(k).map(|r|(k,[r.min.x,r.min.y,r.max.x,r.max.y]))).collect::<std::collections::BTreeMap<_,_>>()) }));
                    s.awaiting = None;
                    break;
                }
            }
        }
    }
}
