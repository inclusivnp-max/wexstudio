//! Actual ART session ownership through Cancel and the real close transaction.
//! This extends, rather than replaces, the original 240s debugger tape.
use super::*;
use egui::{Event, Modifiers};
#[derive(Default)]
pub(super) struct Close {
    pause_stage: u8,
    confirm_stage: u8,
    session: u64,
    epoch: u64,
    pid: u32,
    run_id: String,
    cancel_frames: Vec<u64>,
    confirm_frames: Vec<u64>,
    last_rects: Option<(egui::Rect, egui::Rect)>,
    awaiting: Option<&'static str>,
    shots: Vec<Value>,
    before: Option<Value>,
    after: Option<Value>,
    fresh: Option<Value>,
}
impl Close {
    pub(super) fn awaiting(&self) -> bool {
        self.awaiting.is_some()
    }
    fn same_session(&self, a: &Workbench) {
        let run = a.last_run.as_ref().unwrap();
        assert!(a.debug.connection.is_some());
        assert_eq!(a.debug.session, self.session);
        assert_eq!(run.pid, self.pid);
        assert_eq!(run.run_id, self.run_id);
        assert_eq!(a.selected.as_ref(), Some(&run.serial));
    }
    fn minimum(&mut self, a: &Workbench, ctx: &egui::Context, confirm: bool) -> bool {
        let rect = ctx.content_rect();
        if !a.close_ui.choices_painted()
            || (rect.width() - 760.).abs() > 1.
            || (rect.height() - 560.).abs() > 1.
        {
            return false;
        }
        let Some(keep) = a.chrome.hits.get("window-close-cancel").copied() else {
            return false;
        };
        let Some(close) = a.chrome.hits.get("window-close-save").copied() else {
            return false;
        };
        assert!(rect.contains_rect(keep) && rect.contains_rect(close));
        assert_eq!(a.close_ui.source_count(), 0);
        let frames = if confirm {
            &mut self.confirm_frames
        } else {
            &mut self.cancel_frames
        };
        if self.last_rects != Some((keep, close)) {
            frames.clear();
            self.last_rects = Some((keep, close));
        }
        let n = ctx.cumulative_frame_nr();
        if !frames.contains(&n) {
            frames.push(n);
        }
        frames.len() >= 4
    }
    fn shot(&mut self, name: &'static str, ctx: &egui::Context) {
        self.awaiting = Some(name);
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    pub(super) fn capture(&mut self, dir: &std::path::Path, ctx: &egui::Context) {
        for event in ctx.input(|i| i.events.clone()) {
            if let Event::Screenshot { image, .. } = event
                && let Some(name) = self.awaiting.take()
            {
                let path = dir.join(format!("{name}.png"));
                let pixels = image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>();
                image::save_buffer(
                    &path,
                    &pixels,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                self.shots
                    .push(json!({"path":path,"width":image.size[0],"height":image.size[1]}));
            }
        }
    }
}
impl DebugSmoke {
    pub(super) fn close_cancel_verified(
        &mut self,
        a: &mut Workbench,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) -> bool {
        assert!(
            self.since.elapsed() < Duration::from_secs(20),
            "close Cancel exceeded original step bound"
        );
        let mut c = self.closing.take().unwrap();
        let mut done = false;
        if !c.awaiting() {
            match c.pause_stage {
                0 => {
                    assert!(inspection_ready(a));
                    let run = a.last_run.as_ref().unwrap();
                    assert_eq!(run.serial, self.serial);
                    c.session = a.debug.session;
                    c.epoch = a.debug.epoch;
                    c.pid = run.pid;
                    c.run_id = run.run_id.clone();
                    c.before = Some(snapshot(a));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(760., 560.)));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    c.pause_stage = 1;
                }
                1 => {
                    if c.minimum(a, ctx, false) {
                        c.same_session(a);
                        assert!(inspection_ready(a));
                        key(input, Key::F5, Modifiers::NONE);
                        key(input, Key::F5, Modifiers::SHIFT);
                        key(input, Key::S, Modifiers::COMMAND);
                        input.events.extend([
                            Event::Paste("BLOCKED_CLOSE_EDIT".into()),
                            Event::Ime(egui::ImeEvent::Commit("入力".into())),
                        ]);
                        key(input, Key::Enter, Modifiers::NONE);
                        c.pause_stage = 2;
                    }
                }
                2 => {
                    c.same_session(a);
                    assert!(a.close_ui.choices_painted() && inspection_ready(a));
                    assert_eq!(a.debug.epoch, c.epoch);
                    assert!(a.save_ui.pending.is_none());
                    c.shot("14-close-paused-confirmation", ctx);
                    self.pending = Some(("window-close-cancel".into(), 0));
                    c.pause_stage = 3;
                }
                3 => {
                    if !a.closing_requested() {
                        c.same_session(a);
                        assert!(inspection_ready(a));
                        assert_eq!(a.debug.epoch, c.epoch);
                        assert_eq!(snapshot(a), c.before.as_ref().unwrap().clone());
                        c.after = Some(snapshot(a));
                        key(input, Key::F5, Modifiers::NONE);
                        c.pause_stage = 4;
                    }
                }
                4 => {
                    if !a.debug.paused && !a.debug.busy {
                        c.same_session(a);
                        self.pending = Some(("debug-pause".into(), 0));
                        c.pause_stage = 5;
                    }
                }
                5 => {
                    if a.debug.epoch > c.epoch && inspection_ready(a) {
                        c.same_session(a);
                        c.fresh = Some(snapshot(a));
                        c.shot("15-close-cancel-fresh-pause", ctx);
                        c.pause_stage = 6;
                    }
                }
                6 => {
                    c.same_session(a);
                    assert!(inspection_ready(a));
                    done = true;
                }
                _ => panic!("invalid close Cancel stage"),
            }
        }
        self.closing = Some(c);
        done
    }
    pub(super) fn close_confirmed(&mut self, a: &Workbench, ctx: &egui::Context) -> bool {
        assert!(
            self.since.elapsed() < Duration::from_secs(20),
            "close confirmation exceeded original step bound"
        );
        let mut c = self.closing.take().unwrap();
        let mut done = false;
        if !c.awaiting() {
            match c.confirm_stage {
                0 => {
                    c.same_session(a);
                    assert!(!a.debug.paused);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    c.last_rects = None;
                    c.confirm_stage = 1;
                }
                1 => {
                    if c.minimum(a, ctx, true) {
                        c.same_session(a);
                        assert!(!a.debug.paused);
                        c.shot("16-close-running-confirmation", ctx);
                        self.pending = Some(("window-close-save".into(), 0));
                        c.confirm_stage = 2;
                    }
                }
                2 => {
                    done = true;
                }
                _ => panic!("invalid close confirmation stage"),
            }
        }
        self.closing = Some(c);
        done
    }
    pub(super) fn close_stopped(&self, a: &Workbench) {
        let Some(c) = &self.closing else {
            return;
        };
        assert_eq!(c.pause_stage, 6);
        assert_eq!(c.confirm_stage, 2);
        assert_eq!(c.shots.len(), 3);
        assert!(a.debug.connection.is_none() && !a.debug.busy);
        assert_eq!(a.debug.stop_receipt.as_ref().unwrap()["pid_gone"], true);
        let report = json!({"result":"GUI_ANDROID_DEBUG_CLOSE_OK","reduced_motion":a.reduced_motion,"serial":self.serial,"pid":c.pid,"run_id":c.run_id,"session":c.session,"cancel_epoch":c.epoch,"before":c.before,"after_cancel":c.after,"fresh_pause":c.fresh,"stop":a.debug.stop_receipt,"cancel_frames":c.cancel_frames,"confirm_frames":c.confirm_frames,"screenshots":c.shots,"original_deadline_seconds":240,"original_step_bound_seconds":20,"input":"real native viewport Close / pointer / keyboard, genuine ART and independent next Pause; no programmatic confirmation"});
        std::fs::write(
            self.dir.join("debug-close-window.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        eprintln!(
            "GUI_ANDROID_DEBUG_CLOSE_STOPPED serial={} pid={} source_save=true",
            self.serial, c.pid
        );
    }
}
impl Workbench {
    pub(crate) fn debug_close_exit_proof(&mut self) {
        let Some(mut s) = self.debug_smoke.take() else {
            return;
        };
        if s.closing.is_none() {
            self.debug_smoke = Some(s);
            return;
        }
        s.write_report(self);
        let c = s.closing.as_ref().unwrap();
        assert!(
            self.close_ui.allow
                && self.close_ui.audit.is_some()
                && self.debug.connection.is_none()
                && !self.debug.busy
        );
        assert_eq!(c.confirm_stage, 2);
        assert_eq!(self.debug.stop_receipt.as_ref().unwrap()["pid_gone"], true);
        let report = json!({"result":"REVIEWED_ANDROID_DEBUG_EXIT_OK","reduced_motion":self.reduced_motion,"serial":s.serial,"pid":c.pid,"run_id":c.run_id,"audit":self.close_ui.audit,"stop":self.debug.stop_receipt,"source_clean":!self.doc.dirty()&&self.parked.values().all(|(d,_)|!d.dirty()),"closed_via_transaction":self.close_ui.allow});
        std::fs::write(
            s.dir.join("debug-close-exit.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        eprintln!(
            "REVIEWED_ANDROID_DEBUG_EXIT_OK serial={} pid={}",
            s.serial, c.pid
        );
        self.debug_smoke = Some(s);
    }
}
