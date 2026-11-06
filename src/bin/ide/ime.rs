//! Composition ownership follows ordered surface changes, not aggregate final focus.
use super::*;
use egui::{Event, Id, ImeEvent};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Editor { path: PathBuf, epoch: u64 },
    Terminal(u64),
    Widget(Option<Id>),
}
#[derive(Clone, Debug)]
pub(super) struct Tag {
    pub target: Target,
    pub ticket: u64,
}
#[derive(Default, Clone, serde::Serialize)]
pub(super) struct Metrics {
    pub accepted: u64,
    pub rejected: u64,
    pub cancelled: u64,
    pub resets: u64,
}
#[derive(Default)]
pub(super) struct Ime {
    target: Option<Target>,
    live: Option<Tag>,
    reject: bool,
    next: u64,
    reset: bool,
    guard: Option<(wexandroidstudio::editor::RenderSnapshot, usize, usize)>,
    exhausted: bool,
    pub metrics: Metrics,
}
impl Ime {
    pub fn focus(&mut self, target: Target, force: bool) -> Option<Target> {
        let old = if self
            .live
            .as_ref()
            .is_some_and(|s| force || s.target != target)
        {
            self.cancel()
        } else {
            None
        };
        self.target = Some(target);
        old
    }
    pub fn cancel(&mut self) -> Option<Target> {
        let old = self.live.take().map(|s| s.target);
        if old.is_some() {
            self.reject = true;
            self.reset = true;
            self.metrics.cancelled += 1;
        }
        self.guard = None;
        old
    }
    fn ticket(&mut self) -> Option<u64> {
        if let Some(next) = self.next.checked_add(1) {
            self.next = next;
            Some(next)
        } else {
            self.exhausted = true;
            self.live = None;
            self.guard = None;
            self.reject = true;
            self.reset = true;
            self.metrics.rejected += 1;
            None
        }
    }
    pub fn accept(&mut self, target: Target, event: &ImeEvent) -> Option<Tag> {
        let _ = self.focus(target.clone(), false);
        let accepted = match event {
            ImeEvent::Enabled => {
                let id = self.ticket()?;
                self.live = Some(Tag {
                    target: target.clone(),
                    ticket: id,
                });
                self.reject = false;
                self.reset = false;
                self.live.clone()
            }
            ImeEvent::Preedit(_) => {
                if self.reject {
                    None
                } else {
                    if self.live.is_none() {
                        let id = self.ticket()?;
                        self.live = Some(Tag {
                            target: target.clone(),
                            ticket: id,
                        });
                    }
                    self.live.clone()
                }
            }
            ImeEvent::Commit(_) => {
                if self.reject {
                    None
                } else {
                    let tag = if let Some(tag) = self.live.take() {
                        tag
                    } else {
                        Tag {
                            target: target.clone(),
                            ticket: self.ticket()?,
                        }
                    };
                    self.guard = None;
                    Some(tag)
                }
            }
            ImeEvent::Disabled => {
                self.live = None;
                self.guard = None;
                self.reject = false;
                Some(Tag { target, ticket: 0 })
            }
        };
        if accepted.is_some() {
            self.metrics.accepted += 1;
        } else {
            self.metrics.rejected += 1;
        }
        accepted
    }
}
impl Workbench {
    pub(super) fn ime_editor_target(&self) -> Option<Target> {
        self.doc.path.clone().map(|path| Target::Editor {
            path,
            epoch: self.editor_ui.epoch,
        })
    }
    pub(super) fn ime_terminal_target(&self) -> Target {
        Target::Terminal(
            self.terminal
                .sessions
                .get(self.terminal.active)
                .map_or(self.terminal.next_id(), |s| s.id),
        )
    }
    pub(super) fn ime_actual_target(&self, ctx: &egui::Context) -> Target {
        let focused = ctx.memory(|m| m.focused());
        if self.editor_blocked() {
            return Target::Widget(
                focused
                    .filter(|id| Some(*id) != self.editor_ui.input_id && *id != self.terminal.id()),
            );
        }
        if self.terminal.visible && (self.terminal.focus || focused == Some(self.terminal.id())) {
            return self.ime_terminal_target();
        }
        if self.psearch.open && (self.psearch.focus || !self.psearch.handoff.is_empty()) {
            return Target::Widget(Some(Id::new("project-search-query")));
        }
        if self.editor_ui.focus_editor
            && let Some(target) = self.ime_editor_target()
        {
            return target;
        }
        if self.editor_ui.find.open
            && (self.editor_ui.find.focus || !self.editor_ui.find.handoff.is_empty())
        {
            return Target::Widget(Some(Id::new("find-query")));
        }
        if focused == self.editor_ui.input_id
            && let Some(target) = self.ime_editor_target()
        {
            return target;
        }
        Target::Widget(focused)
    }
    pub(super) fn ime_clear(&mut self, target: &Target) {
        match target {
            Target::Editor { .. } => self.edit.cancel_composition(),
            Target::Terminal(_) => self.terminal.preedit = None,
            Target::Widget(_) => {}
        }
    }
    pub(super) fn ime_cancel_external(&mut self) {
        if let Some(old) = self.ime.cancel() {
            self.ime_clear(&old);
        }
    }
    pub(super) fn ime_begin(&mut self, ctx: &egui::Context) {
        if self.ime.guard.as_ref().is_some_and(|(snapshot, a, c)| {
            !snapshot.matches(&self.doc) || (*a, *c) != (self.edit.anchor, self.edit.cursor)
        }) {
            self.ime_cancel_external();
        }
        let target = self.ime_actual_target(ctx);
        if let Some(old) = self.ime.focus(target.clone(), false) {
            self.ime_clear(&old);
        }
        if self.ime.live.is_none()
            && !self.ime.reject
            && ((matches!(target, Target::Editor { .. }) && self.edit.composition.is_some())
                || (matches!(target, Target::Terminal(_)) && self.terminal.preedit.is_some()))
        {
            let _ = self.ime.accept(target, &ImeEvent::Enabled);
        }
    }
    pub(super) fn ime_document_changed(&mut self) {
        if self
            .ime
            .live
            .as_ref()
            .is_some_and(|s| matches!(s.target, Target::Editor { .. }))
        {
            self.ime_cancel_external();
        }
    }
    pub(super) fn ime_finish(&mut self, ctx: &egui::Context) {
        let actual = self.ime_actual_target(ctx);
        if let Some(tag) = &mut self.ime.live
            && matches!(tag.target, Target::Widget(None))
            && matches!(actual, Target::Widget(Some(_)))
        {
            tag.target = actual.clone();
            self.ime.target = Some(actual.clone());
        }
        if let Some(old) = self.ime.focus(actual, false) {
            self.ime_clear(&old);
        }
        if self.ime.exhausted {
            self.edit.cancel_composition();
            self.terminal.preedit = None;
            self.status = "IME session clock exhausted · restart this window".into();
        }
        if self.ime.reset {
            ctx.output_mut(|o| o.ime = None);
            self.ime.reset = false;
            self.ime.metrics.resets += 1;
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
    pub(super) fn ime_deliver(&mut self, tag: &Tag, event: &ImeEvent) -> bool {
        let valid = match &tag.target {
            Target::Editor { .. } => self.ime_editor_target().as_ref() == Some(&tag.target),
            Target::Terminal(id) => self
                .terminal
                .sessions
                .get(self.terminal.active)
                .is_some_and(|s| s.id == *id),
            Target::Widget(_) => true,
        };
        if !valid {
            self.ime.metrics.rejected += 1;
            return false;
        }
        if matches!(tag.target, Target::Editor { .. }) && matches!(event, ImeEvent::Commit(_)) {
            self.edit.enable_ime();
        }
        if matches!(tag.target, Target::Editor { .. })
            && matches!(event, ImeEvent::Enabled | ImeEvent::Preedit(_))
            && self
                .ime
                .live
                .as_ref()
                .is_some_and(|s| s.ticket == tag.ticket)
        {
            self.ime.guard = Some((
                self.doc.render_snapshot(),
                self.edit.anchor,
                self.edit.cursor,
            ));
        }
        true
    }
    pub(super) fn ime_ui_only(&mut self, ctx: &egui::Context) {
        self.ime_begin(ctx);
        let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
        let mut out = vec![];
        let mut target = self.ime_actual_target(ctx);
        for event in events {
            match &event {
                Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => {
                    target = Target::Widget(None);
                    if let Some(old) = self.ime.focus(target.clone(), true) {
                        self.ime_clear(&old);
                        out.push(Event::Ime(ImeEvent::Disabled));
                    }
                }
                Event::Ime(e) => {
                    if !ctx.input(|i| i.focused) {
                        if matches!(e, ImeEvent::Disabled) {
                            let _ = self.ime.accept(target.clone(), e);
                            continue;
                        }
                        self.ime.metrics.rejected += 1;
                        continue;
                    }
                    if self.ime.accept(target.clone(), e).is_none() {
                        continue;
                    }
                }
                _ => {}
            }
            out.push(event);
        }
        ctx.input_mut(|i| i.events = out);
    }
}
pub(super) fn barrier(e: &pointer_input::Ordered) -> bool {
    matches!(
        e,
        pointer_input::Ordered::Ime(_, ImeEvent::Disabled)
            | pointer_input::Ordered::Raw(Event::Ime(ImeEvent::Disabled))
            | pointer_input::Ordered::Terminal(Event::Ime(ImeEvent::Disabled))
            | pointer_input::Ordered::CancelIme(_)
            | pointer_input::Ordered::Pointer(pointer_input::Pointer::Press { .. })
            | pointer_input::Ordered::Terminal(Event::PointerButton { pressed: true, .. })
    ) || matches!(e,pointer_input::Ordered::Raw(e)|pointer_input::Ordered::Terminal(e)if super::input_route::terminal_key(e)||matches!(e,Event::Copy|Event::Cut|Event::Paste(_)|Event::Key{key:Key::Escape,pressed:true,..})||matches!(e,Event::Key{key:Key::A|Key::Z|Key::Y|Key::Home|Key::End|Key::ArrowLeft|Key::ArrowRight|Key::Backspace|Key::Delete|Key::F|Key::H,pressed:true,modifiers,..}if modifiers.ctrl||modifiers.command))
}
pub(super) fn segments(
    events: &std::collections::VecDeque<pointer_input::Ordered>,
) -> Vec<(bool, bool)> {
    let mut out = vec![(false, false)];
    let mut i = 0;
    for e in events {
        if barrier(e) {
            i += 1;
            out.push((false, false));
        }
        match e {
            pointer_input::Ordered::Ime(
                tag,
                egui::ImeEvent::Preedit(_) | egui::ImeEvent::Commit(_),
            ) => match tag.target {
                Target::Editor { .. } => out[i].0 = true,
                Target::Terminal(_) => out[i].1 = true,
                _ => {}
            },
            pointer_input::Ordered::Raw(Event::Ime(ImeEvent::Preedit(_) | ImeEvent::Commit(_))) => {
                out[i].0 = true
            }
            pointer_input::Ordered::Terminal(Event::Ime(
                ImeEvent::Preedit(_) | ImeEvent::Commit(_),
            )) => out[i].1 = true,
            _ => {}
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    fn editor(n: u64) -> Target {
        Target::Editor {
            path: PathBuf::from("/Main.kt"),
            epoch: n,
        }
    }
    #[test]
    fn stale_commit_is_rejected_until_fresh_start_and_new_target_recovers() {
        let mut i = Ime::default();
        i.accept(Target::Terminal(1), &ImeEvent::Preedit("old".into()))
            .unwrap();
        assert!(i.focus(editor(1), true).is_some());
        assert!(
            i.accept(editor(1), &ImeEvent::Commit("old".into()))
                .is_none()
        );
        assert!(
            i.accept(editor(1), &ImeEvent::Preedit("late".into()))
                .is_none()
        );
        i.accept(editor(1), &ImeEvent::Enabled).unwrap();
        i.accept(editor(1), &ImeEvent::Preedit("new".into()))
            .unwrap();
        assert!(
            i.accept(editor(1), &ImeEvent::Commit("new".into()))
                .is_some()
        );
    }
    #[test]
    fn epoch_and_session_identity_not_only_surface_kind_fence_composition() {
        for (a, b) in [
            (editor(1), editor(2)),
            (Target::Terminal(1), Target::Terminal(2)),
            (
                Target::Widget(Some(Id::new("find-query"))),
                Target::Widget(Some(Id::new("replace-query"))),
            ),
        ] {
            let mut i = Ime::default();
            i.accept(a, &ImeEvent::Enabled).unwrap();
            assert!(i.focus(b.clone(), false).is_some());
            assert!(i.accept(b, &ImeEvent::Commit("ghost".into())).is_none());
        }
    }
    #[test]
    fn empty_preedit_before_commit_keeps_session_and_commit_only_stays_supported() {
        let mut i = Ime::default();
        i.accept(editor(1), &ImeEvent::Enabled).unwrap();
        i.accept(editor(1), &ImeEvent::Preedit(String::new()))
            .unwrap();
        assert!(i.accept(editor(1), &ImeEvent::Commit("é".into())).is_some());
        assert!(i.accept(editor(1), &ImeEvent::Commit("é".into())).is_some());
    }
    #[test]
    fn disabled_clears_tombstone_and_counter_exhaustion_does_not_reuse_ticket() {
        let mut i = Ime::default();
        i.accept(editor(1), &ImeEvent::Enabled).unwrap();
        i.cancel();
        i.accept(editor(1), &ImeEvent::Disabled).unwrap();
        assert!(
            i.accept(editor(1), &ImeEvent::Commit("direct".into()))
                .is_some()
        );
        i.next = u64::MAX;
        assert!(i.accept(editor(1), &ImeEvent::Enabled).is_none());
    }
}
