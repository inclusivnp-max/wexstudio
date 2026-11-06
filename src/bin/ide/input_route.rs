//! Reserve source-owned keyboard events before other widgets/global shortcuts see them.
use super::*;
use egui::{Event, Key, Modifiers, PointerButton};
use std::collections::VecDeque;
#[derive(Clone, Copy, PartialEq)]
enum Owner {
    Editor,
    Find,
    ProjectSearch,
    Terminal,
    Other,
}
fn command(m: Modifiers) -> bool {
    m.matches_exact(Modifiers::COMMAND)
        || m.matches_exact(Modifiers::CTRL)
        // Native smoke input represents the platform command key with both
        // egui bits set. Treat that equivalent pair as command-only, while
        // still rejecting Shift/Alt combinations.
        || (m.ctrl && m.command && !m.shift && !m.alt)
}
fn command_shift(m: Modifiers) -> bool {
    m.matches_exact(Modifiers::COMMAND | Modifiers::SHIFT)
        || m.matches_exact(Modifiers::CTRL | Modifiers::SHIFT)
        || (m.ctrl && m.command && m.shift && !m.alt)
}
pub(super) fn terminal_key(e: &Event) -> bool {
    matches!(e,Event::Key{key:Key::F12,pressed:true,modifiers,..}if modifiers.matches_exact(Modifiers::ALT))
        || matches!(e,Event::Key{key:Key::T,pressed:true,modifiers,..}if command_shift(*modifiers))
}
pub(super) fn project_key(e: &Event) -> bool {
    matches!(e,Event::Key{key:Key::F,pressed:true,modifiers,..}if command_shift(*modifiers))
}
pub(super) fn git_key(e: &Event) -> bool {
    matches!(e,Event::Key{key:Key::G,pressed:true,modifiers,..}if command_shift(*modifiers))
}
fn app_key(e: &Event) -> bool {
    matches!(e,Event::Key{key,pressed:true,modifiers,..}if format_ui::shortcut(*key,*modifiers))
        || terminal_key(e)
        || project_key(e)
        || git_key(e)
        || matches!(e,Event::Key {key,pressed:true,modifiers,..}if (command(*modifiers)&&matches!(key,Key::S|Key::B|Key::F|Key::H))||(command_shift(*modifiers)&&*key==Key::S)||(*key==Key::F3&&(modifiers.matches_exact(Modifiers::NONE)||modifiers.matches_exact(Modifiers::SHIFT)))||(matches!(key,Key::F5|Key::F6|Key::F9|Key::F10|Key::F11)&&modifiers.matches_exact(Modifiers::NONE))||(matches!(key,Key::F5|Key::F6|Key::F10|Key::F11|Key::F12)&&modifiers.matches_exact(Modifiers::SHIFT)))
}
impl Workbench {
    pub(super) fn dispatch_editor_frame(&mut self, ctx: &egui::Context) {
        self.ime_begin(ctx);
        if !ctx.input(|i| i.focused) {
            self.ime_cancel_external();
            self.ime_ui_only(ctx);
            self.terminal.preedit = None;
            self.terminal.stop_drag();
            self.cancel_deferred_input();
            return;
        }
        let frame = ctx.cumulative_frame_nr();
        if self.editor_ui.frame_handled == Some(frame) {
            return;
        }
        if self.editor_blocked() {
            self.ime_ui_only(ctx);
            return;
        }
        let viewport = self.editor_ui.geometry.viewport;
        let origin = self.editor_ui.geometry.origin;
        let layer = self.editor_ui.pointer_layer;
        let has_doc = self.doc.path.is_some();
        // The gutter is an editor-owned control. Use the displayed geometry itself,
        // not the previous frame's hit map: the smoke/native pointer can arrive
        // before this frame repaints its passive gutter rectangles.
        let gutter_hit = |p: egui::Pos2| has_doc && viewport.contains(p) && p.x < origin.x + 22.;
        let editor_hit = |p: egui::Pos2| {
            has_doc
                && viewport.contains(p)
                && (layer.is_some_and(|l| ctx.layer_id_at(p) == Some(l)) || gutter_hit(p))
        };
        let terminal_focus = self.terminal.visible
            && (self.terminal.focus || ctx.memory(|m| m.has_focus(self.terminal.id())));
        let code_focus = has_doc
            && !terminal_focus
            && (self
                .editor_ui
                .input_id
                .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
                || self.editor_ui.focus_editor);
        let project_focus = !terminal_focus
            && self.psearch.open
            && (self.psearch.focus
                || !self.psearch.handoff.is_empty()
                || ctx.memory(|m| {
                    m.focused()
                        .is_some_and(|id| id == egui::Id::new("project-search-query"))
                }));
        let find_focus = !project_focus
            && !terminal_focus
            && self.editor_ui.find.open
            && (self.editor_ui.find.focus
                || !self.editor_ui.find.handoff.is_empty()
                || ctx.memory(|m| m.has_focus(egui::Id::new("find-query"))));
        let presses = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        ..
                    } => Some(*pos),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let term_wheel = ctx.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, Event::MouseWheel { .. }))
        }) && ctx
            .pointer_hover_pos()
            .is_some_and(|p| self.terminal.hit(ctx, p));
        let owns = (!project_focus && !find_focus && code_focus)
            || terminal_focus
            || presses
                .iter()
                .any(|p| editor_hit(*p) || self.terminal.hit(ctx, *p))
            || self.editor_ui.deferred_input.is_some()
            || term_wheel
            || ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| terminal_key(e) || project_key(e) || git_key(e))
            });
        if !owns && !find_focus && !project_focus {
            self.ime_ui_only(ctx);
            return;
        }
        let source_gesture =
            self.editor_ui.drag_active() || self.editor_ui.deferred_input.is_some();
        let terminal_gesture = self.terminal.dragging();
        let lease = presses
            .last()
            .map_or(source_gesture || terminal_gesture, |p| {
                editor_hit(*p) || self.terminal.hit(ctx, *p)
            });
        let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
        let mut kept = vec![];
        let mut ordered = VecDeque::new();
        let mut find = vec![];
        let mut project_events = vec![];
        let mut project_command = false;
        let mut find_command = false;
        let mut cancel_deferred = false;
        let mut owner = if terminal_focus {
            Owner::Terminal
        } else if project_focus {
            Owner::ProjectSearch
        } else if find_focus {
            Owner::Find
        } else if code_focus {
            Owner::Editor
        } else {
            Owner::Other
        };
        let editor_target = self.ime_editor_target();
        let mut terminal_target = self.ime_terminal_target();
        let mut next_terminal = self.terminal.next_id();
        let mut terminal_open = self.tab == 5 && self.chrome.dock_open;
        let mut virtual_session = !self.terminal.sessions.is_empty();
        let mut widget_target = ctx.memory(|m| m.focused());
        let target_of =
            |owner: Owner, term: &super::ime::Target, widget: Option<egui::Id>| match owner {
                Owner::Editor => editor_target
                    .clone()
                    .unwrap_or(super::ime::Target::Widget(None)),
                Owner::Terminal => term.clone(),
                Owner::ProjectSearch => {
                    super::ime::Target::Widget(Some(egui::Id::new("project-search-query")))
                }
                Owner::Find => super::ime::Target::Widget(Some(egui::Id::new("find-query"))),
                Owner::Other => super::ime::Target::Widget(widget),
            };
        for event in events {
            let mut forecast = owner;
            let mut force = false;
            if let Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                ..
            } = &event
            {
                force = true;
                forecast = if editor_hit(*pos) {
                    Owner::Editor
                } else if self.terminal.hit(ctx, *pos) {
                    Owner::Terminal
                } else if self.psearch.open
                    && self
                        .psearch
                        .hits
                        .get("query")
                        .is_some_and(|r| r.contains(*pos))
                {
                    Owner::ProjectSearch
                } else if self.editor_ui.find.open
                    && self
                        .editor_ui
                        .hits
                        .get("find-query")
                        .is_some_and(|r| r.contains(*pos))
                {
                    Owner::Find
                } else {
                    Owner::Other
                };
                if forecast == Owner::Other {
                    widget_target = if self.editor_ui.find.open
                        && self
                            .editor_ui
                            .hits
                            .get("replace-query")
                            .is_some_and(|r| r.contains(*pos))
                    {
                        Some(egui::Id::new("replace-query"))
                    } else {
                        None
                    };
                }
            } else if matches!(&event,Event::Key{key:Key::F12,pressed:true,modifiers,..}if modifiers.matches_exact(Modifiers::ALT))
            {
                terminal_open = !terminal_open;
                if terminal_open && !virtual_session {
                    terminal_target = super::ime::Target::Terminal(next_terminal);
                    next_terminal = next_terminal.saturating_add(1);
                    virtual_session = true;
                }
                forecast = if terminal_open {
                    Owner::Terminal
                } else if has_doc {
                    Owner::Editor
                } else {
                    Owner::Other
                };
            } else if matches!(&event,Event::Key{key:Key::T,pressed:true,modifiers,..}if command_shift(*modifiers))
            {
                forecast = Owner::Terminal;
                terminal_target = super::ime::Target::Terminal(next_terminal);
                next_terminal = next_terminal.saturating_add(1);
                virtual_session = true;
                terminal_open = true;
            } else if git_key(&event) {
                forecast = Owner::Other;
                force = true;
            } else if project_key(&event) {
                forecast = Owner::ProjectSearch;
                force = true;
            } else if owner != Owner::Terminal
                && matches!(&event,Event::Key{key:Key::F|Key::H,modifiers,..}if command(*modifiers))
            {
                // Keep ownership through the matching key release. RawInput
                // tapes commonly emit Ctrl+F press, release, then Text in one
                // frame; dropping ownership on release would write the query
                // into the source Rope before the Find widget is created.
                forecast = Owner::Find;
                owner = Owner::Find;
                force = matches!(&event, Event::Key { pressed: true, .. });
                if force {
                    find_command = true;
                }
            }
            if matches!(
                &event,
                Event::Copy
                    | Event::Cut
                    | Event::Paste(_)
                    | Event::Key {
                        key: Key::Escape,
                        pressed: true,
                        ..
                    }
            ) || matches!(&event,Event::Key{key:Key::A|Key::Z|Key::Y|Key::Home|Key::End|Key::ArrowLeft|Key::ArrowRight|Key::Backspace|Key::Delete,pressed:true,modifiers,..}if modifiers.command||modifiers.ctrl)
            {
                force = true;
            }
            let target = target_of(forecast, &terminal_target, widget_target);
            if let Some(old) = self.ime.focus(target.clone(), force) {
                if matches!(old,super::ime::Target::Widget(Some(id))if id==egui::Id::new("find-query"))
                {
                    self.editor_ui
                        .find
                        .handoff
                        .retain(|e| !matches!(e, Event::Ime(_)));
                    find.push(Event::Ime(egui::ImeEvent::Disabled));
                } else if matches!(old,super::ime::Target::Widget(Some(id))if id==egui::Id::new("project-search-query"))
                {
                    self.psearch.handoff.retain(|e| !matches!(e, Event::Ime(_)));
                    project_events.push(Event::Ime(egui::ImeEvent::Disabled));
                } else if matches!(old, super::ime::Target::Widget(_)) {
                    kept.push(Event::Ime(egui::ImeEvent::Disabled));
                }
                ordered.push_back(pointer_input::Ordered::CancelIme(old));
            }
            if let Event::Ime(ime) = event {
                if let Some(tag) = self.ime.accept(target, &ime) {
                    match owner {
                        Owner::Editor | Owner::Terminal => {
                            ordered.push_back(pointer_input::Ordered::Ime(tag, ime))
                        }
                        Owner::ProjectSearch => project_events.push(Event::Ime(ime)),
                        Owner::Find => find.push(Event::Ime(ime)),
                        Owner::Other => kept.push(Event::Ime(ime)),
                    }
                }
                continue;
            }

            match event {
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers,
                } => {
                    if editor_hit(pos) {
                        owner = Owner::Editor;
                        pointer_input::push(
                            &mut ordered,
                            pointer_input::Ordered::Pointer(pointer_input::Pointer::Press {
                                point: pointer_input::point(origin, pos),
                                shift: modifiers.shift,
                                when: std::time::Instant::now(),
                            }),
                        );
                    } else if self.terminal.hit(ctx, pos) {
                        owner = Owner::Terminal;
                        cancel_deferred = true;
                        ordered.push_back(pointer_input::Ordered::Terminal(Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: true,
                            modifiers,
                        }));
                    } else {
                        owner = if self.psearch.open
                            && self
                                .psearch
                                .hits
                                .get("query")
                                .is_some_and(|r| r.contains(pos))
                        {
                            Owner::ProjectSearch
                        } else if self.editor_ui.find.open
                            && self
                                .editor_ui
                                .hits
                                .get("find-query")
                                .is_some_and(|r| r.contains(pos))
                        {
                            Owner::Find
                        } else {
                            Owner::Other
                        };
                    }
                    kept.push(Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    });
                }
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers,
                } => {
                    if owner == Owner::Terminal || terminal_gesture {
                        ordered.push_back(pointer_input::Ordered::Terminal(Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: false,
                            modifiers,
                        }));
                    } else if owns {
                        pointer_input::push(
                            &mut ordered,
                            pointer_input::Ordered::Pointer(pointer_input::Pointer::Release(
                                pointer_input::point(origin, pos),
                            )),
                        );
                    }
                    kept.push(Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers,
                    });
                }
                Event::PointerMoved(pos) => {
                    if owner == Owner::Terminal || terminal_gesture {
                        ordered
                            .push_back(pointer_input::Ordered::Terminal(Event::PointerMoved(pos)));
                    } else if owner == Owner::Editor || source_gesture {
                        pointer_input::push(
                            &mut ordered,
                            pointer_input::Ordered::Pointer(pointer_input::Pointer::Move(
                                pointer_input::point(origin, pos),
                            )),
                        );
                    }
                    kept.push(Event::PointerMoved(pos));
                }
                e @ Event::MouseWheel { .. } if term_wheel => {
                    ordered.push_back(pointer_input::Ordered::Terminal(e))
                }
                e if owner == Owner::Terminal
                    && matches!(
                        &e,
                        Event::Key { .. }
                            | Event::Text(_)
                            | Event::Paste(_)
                            | Event::Copy
                            | Event::Cut
                            | Event::Ime(_)
                    ) =>
                {
                    if project_key(&e) {
                        project_command = true;
                    }
                    owner = forecast;
                    ordered.push_back(pointer_input::Ordered::Terminal(e))
                }
                e if matches!(&e,Event::Key{key:Key::F|Key::H,modifiers,..}if command(*modifiers)) =>
                {
                    // Consume both halves of the command before the generic
                    // app-key branch. The following Text event in this same
                    // frame must be queued for Find, never for the editor.
                    owner = Owner::Find;
                    if matches!(&e, Event::Key { pressed: true, .. }) {
                        find_command = true;
                        self.start_find(matches!(&e, Event::Key { key: Key::H, .. }));
                    }
                }
                e if app_key(&e) => {
                    if !owns {
                        kept.push(e);
                        continue;
                    }
                    if git_key(&e) {
                        owner = Owner::Other;
                    } else if project_key(&e) {
                        owner = Owner::ProjectSearch;
                        project_command = true;
                    } else if terminal_key(&e) {
                        owner = forecast;
                    } else if matches!(&e,Event::Key{key:Key::F|Key::H,modifiers,..}if command(*modifiers))
                    {
                        owner = Owner::Find;
                        find_command = true;
                    }
                    pointer_input::push(&mut ordered, pointer_input::Ordered::Raw(e));
                }
                e if owner == Owner::Find
                    && !owns
                    && matches!(
                        &e,
                        Event::Key {
                            key: Key::Enter | Key::Escape,
                            ..
                        }
                    ) =>
                {
                    kept.push(e)
                }
                e if matches!(
                    &e,
                    Event::Key { .. }
                        | Event::Text(_)
                        | Event::Paste(_)
                        | Event::Copy
                        | Event::Cut
                        | Event::Ime(_)
                ) =>
                {
                    match owner {
                        Owner::Editor => {
                            pointer_input::push(&mut ordered, pointer_input::Ordered::Raw(e))
                        }
                        Owner::ProjectSearch => project_events.push(e),
                        Owner::Find => find.push(e),
                        Owner::Terminal => ordered.push_back(pointer_input::Ordered::Terminal(e)),
                        Owner::Other => kept.push(e),
                    }
                }
                e => kept.push(e),
            }
        }
        if lease {
            kept.retain(|e| {
                !matches!(
                    e,
                    Event::PointerButton {
                        button: PointerButton::Primary,
                        ..
                    } | Event::PointerMoved(_)
                )
            });
        }
        ctx.input_mut(|i| i.events = kept);
        if !find.is_empty() {
            if find.iter().any(|e| {
                matches!(
                    e,
                    Event::Text(_)
                        | Event::Paste(_)
                        | Event::Cut
                        | Event::Ime(_)
                        | Event::Key {
                            key: Key::Backspace | Key::Delete | Key::Z | Key::Y,
                            pressed: true,
                            ..
                        }
                )
            }) {
                self.editor_ui.find.submitted_for_handoff();
            }
            if find_command {
                // start_find() marks the newly opened Find field ready for the
                // events queued in this frame. Do not clear that admission here:
                // doing so strands the same-frame query until the smoke timeout.
                self.editor_ui.find.handoff_ready = true;
            }
        }
        self.editor_ui.find.handoff.extend(find);
        self.psearch.handoff.extend(project_events);
        if project_command {
            self.psearch.handoff_ready = false;
        }
        if owns {
            if cancel_deferred {
                self.cancel_deferred_input();
            }
            self.editor_ui.frame_editor_focus = code_focus;
            self.editor_ui.frame_handled = Some(frame);
            self.editor_ui.frame_events = Some(ordered);
            self.input(ctx);
            if project_command {
                self.psearch.handoff_ready = false;
            }
            if owner == Owner::Other || owner == Owner::ProjectSearch {
                self.editor_ui.frame_editor_focus = false;
                self.editor_ui.focus_editor = false;
            }
            if owner != Owner::Terminal {
                self.terminal.focus = false;
                self.terminal.stop_drag();
            }
            if lease {
                self.editor_ui.leased_pointer =
                    Some(ctx.input_mut(|i| std::mem::take(&mut i.pointer)));
            }
        }
    }
    pub(super) fn finish_editor_frame(&mut self, ctx: &egui::Context) {
        if let Some(pointer) = self.editor_ui.leased_pointer.take() {
            ctx.input_mut(|i| i.pointer = pointer);
        }
        if self.terminal.focus && self.terminal.visible {
            ctx.memory_mut(|m| m.request_focus(self.terminal.id()));
        }
    }
    pub(super) fn editor_command(&mut self, ctx: &egui::Context, key: Key, m: Modifiers) -> bool {
        if format_ui::shortcut(key, m) {
            self.start_formatting();
            return true;
        }
        if m.matches_exact(Modifiers::ALT) && key == Key::F12 {
            self.toggle_terminal();
            return true;
        }
        if command_shift(m) && key == Key::T {
            self.new_terminal();
            return true;
        }
        if command_shift(m) && key == Key::G {
            self.show_git(None);
            return true;
        }
        if command_shift(m) && key == Key::F {
            self.show_project_search();
            return true;
        }
        if command_shift(m) && key == Key::S {
            self.save_before(save_ui::After::None);
            return true;
        }
        if command(m) {
            match key {
                Key::S => self.save_document(),
                Key::B => self.build(),
                Key::F => self.start_find(false),
                Key::H => self.start_find(true),
                _ => return false,
            }
            return true;
        }
        if m.matches_exact(Modifiers::SHIFT) {
            match key {
                Key::F10 => self.run(),
                Key::F5 => self.stop_debug(),
                Key::F11 => self.debug_control("stepOut"),
                Key::F3 => self.find_next(true),
                _ => return false,
            }
            return true;
        }
        if m.matches_exact(Modifiers::NONE) {
            match key {
                Key::F9 => {
                    if let Some(path) = self.doc.path.clone() {
                        self.toggle_breakpoint(
                            path,
                            self.doc.rope().char_to_line(self.edit.cursor) as u32 + 1,
                        );
                    }
                }
                Key::F5 => {
                    if self.debug.connection.is_some() {
                        self.debug_control("continue")
                    } else {
                        self.start_debug()
                    }
                }
                Key::F6 => self.debug_control("pause"),
                Key::F10 => self.debug_control("next"),
                Key::F11 => self.debug_control("stepIn"),
                Key::F3 => self.find_next(false),
                Key::Escape
                    if self.open_ui.pending.is_some()
                        && !self.editor_ui.find.open
                        && self.completions.is_empty()
                        && self.edit.composition.is_none() =>
                {
                    self.cancel_document_open();
                    self.status = "File opening cancelled · current tab retained".into();
                }
                _ => return false,
            }
            ctx.request_repaint();
            return true;
        }
        false
    }
}
