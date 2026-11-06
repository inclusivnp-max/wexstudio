//! Material3 project terminal; the UI owns only bounded visible PTY snapshots.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Sense};
use std::sync::{Arc, atomic::Ordering};
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    terminal::{self, Frame, Handle, Shell, State},
};
#[derive(Clone)]
struct Selection {
    frame: Arc<Frame>,
    a: (u16, u16),
    b: (u16, u16),
}
pub(super) struct Session {
    pub id: u64,
    pub handle: Handle,
    pub frame: Arc<Frame>,
    selection: Option<Selection>,
    drag: bool,
    size: (u16, u16),
}
impl Session {
    fn displayed(&self) -> &Arc<Frame> {
        self.selection.as_ref().map_or(&self.frame, |s| &s.frame)
    }
}
pub(super) struct TerminalUi {
    pub sessions: Vec<Session>,
    pub active: usize,
    next: u64,
    pub focus: bool,
    pub rect: Rect,
    pub layer: Option<egui::LayerId>,
    pub visible: bool,
    pub retiring: Vec<terminal::Cleanup>,
    pub error: Option<String>,
    pub preedit: Option<String>,
    cell: egui::Vec2,
}
impl Default for TerminalUi {
    fn default() -> Self {
        Self {
            sessions: vec![],
            active: 0,
            next: 1,
            focus: false,
            rect: Rect::NOTHING,
            layer: None,
            visible: false,
            retiring: vec![],
            error: None,
            preedit: None,
            cell: egui::vec2(8.4, 20.),
        }
    }
}
impl TerminalUi {
    pub(super) fn next_id(&self) -> u64 {
        self.next
    }
    pub fn id(&self) -> egui::Id {
        egui::Id::new((
            "terminal-input",
            self.sessions.get(self.active).map(|s| s.id),
        ))
    }
    pub fn hit(&self, ctx: &egui::Context, p: Pos2) -> bool {
        self.visible
            && self.rect.contains(p)
            && self.layer.is_some_and(|l| ctx.layer_id_at(p) == Some(l))
    }
    pub fn dragging(&self) -> bool {
        self.sessions.get(self.active).is_some_and(|s| s.drag)
    }
    pub fn poll(&mut self, visible: bool) {
        self.visible = visible;
        self.retiring.retain(|c| !c.done());
        for (i, s) in self.sessions.iter_mut().enumerate() {
            s.handle
                .visible
                .store(visible && i == self.active, Ordering::Release);
            if let Some(f) = s.handle.snapshot() {
                s.frame = f;
            }
        }
        if self.sessions.is_empty() {
            self.rect = Rect::NOTHING;
            self.layer = None;
        }
        if !visible {
            self.focus = false;
            self.preedit = None;
        }
    }
    fn send(&mut self, b: Vec<u8>) {
        if let Some(s) = self.sessions.get(self.active)
            && let Err(e) = s.handle.input(b)
        {
            self.error = Some(e.to_string());
        }
    }
    pub(super) fn cell_position(&self, row: u16, col: u16) -> Pos2 {
        self.rect.min
            + egui::vec2(
                col as f32 * self.cell.x,
                row as f32 * self.cell.y + self.cell.y / 2.,
            )
    }
    pub(super) fn selected_text(&self) -> Option<String> {
        self.sessions
            .get(self.active)
            .and_then(|s| s.selection.as_ref())
            .map(|s| s.frame.selected(s.a, s.b))
    }
    fn point(&self, p: Pos2) -> (u16, u16) {
        let Some(s) = self.sessions.get(self.active) else {
            return (0, 0);
        };
        let f = s.displayed();
        let row = (((p.y - self.rect.top()) / self.cell.y).floor().max(0.) as u16).min(f.rows - 1);
        let x = ((p.x - self.rect.left()) / self.cell.x).max(0.);
        let mut col = (x.round() as u16).min(f.cols);
        if f.cell(row, col).is_some_and(|c| c.is_wide_continuation()) {
            col = if x < (col as f32) {
                col.saturating_sub(1)
            } else {
                (col + 1).min(f.cols)
            };
        }
        (row, col)
    }
    fn copy(&self, ctx: &egui::Context) {
        if let Some(s) = self
            .sessions
            .get(self.active)
            .and_then(|s| s.selection.as_ref())
        {
            let text = s.frame.selected(s.a, s.b);
            if !text.is_empty() {
                ctx.copy_text(text);
            }
        }
    }
    fn input_event(&mut self, ctx: &egui::Context, event: Event, ime_frame: bool) {
        match event {
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                ..
            } => {
                let p = self.point(pos);
                if let Some(s) = self.sessions.get_mut(self.active) {
                    if pressed {
                        self.focus = true;
                        self.preedit = None;
                        s.selection = Some(Selection {
                            frame: s.displayed().clone(),
                            a: p,
                            b: p,
                        });
                        s.drag = true;
                    } else {
                        if let Some(a) = &mut s.selection {
                            a.b = p;
                            if a.a == a.b {
                                s.selection = None;
                            }
                        }
                        s.drag = false;
                    }
                }
            }
            Event::PointerMoved(pos) => {
                let p = self.point(pos);
                if let Some(s) = self.sessions.get_mut(self.active)
                    && s.drag
                    && let Some(a) = &mut s.selection
                {
                    a.b = p;
                }
            }
            Event::MouseWheel { delta, .. } => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    s.selection = None;
                    let rows = (delta.y / 12.).round() as isize;
                    if rows != 0
                        && let Err(e) = s.handle.scroll(rows)
                    {
                        self.error = Some(e.to_string());
                    }
                }
            }
            Event::Copy => {
                if self.selected_text().is_some_and(|s| !s.is_empty())
                    || ctx.input(|i| i.modifiers.shift)
                {
                    self.copy(ctx);
                } else {
                    self.send(vec![3]);
                }
            }
            Event::Cut => self.send(vec![24]),
            Event::Paste(s) => {
                match paste(
                    &s,
                    self.sessions
                        .get(self.active)
                        .is_some_and(|s| s.frame.bracketed_paste),
                ) {
                    Ok(b) => {
                        self.clear_selection();
                        self.send(b);
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Event::Text(s) if !ime_frame && self.preedit.is_none() => {
                self.clear_selection();
                let mut b = vec![];
                if ctx.input(|i| i.modifiers.alt) {
                    b.push(27);
                }
                b.extend(s.as_bytes());
                self.send(b);
            }
            Event::Ime(ime) => match ime {
                egui::ImeEvent::Enabled => self.preedit = None,
                egui::ImeEvent::Preedit(s) => self.preedit = Some(s),
                egui::ImeEvent::Commit(s) => {
                    self.preedit = None;
                    self.clear_selection();
                    self.send(s.into_bytes());
                }
                egui::ImeEvent::Disabled => self.preedit = None,
            },
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if !ime_frame && self.preedit.is_none() => {
                if key == Key::PageUp && modifiers.shift {
                    if let Some(s) = self.sessions.get_mut(self.active) {
                        s.selection = None;
                        let _ = s.handle.scroll(s.frame.rows as isize);
                    }
                } else if key == Key::PageDown && modifiers.shift {
                    if let Some(s) = self.sessions.get_mut(self.active) {
                        s.selection = None;
                        let _ = s.handle.scroll(-(s.frame.rows as isize));
                    }
                } else if let Some(b) = key_bytes(
                    key,
                    modifiers,
                    self.sessions
                        .get(self.active)
                        .is_some_and(|s| s.frame.application_cursor),
                ) {
                    self.clear_selection();
                    self.send(b);
                }
            }
            _ => {}
        }
    }
    pub(super) fn stop_drag(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.active) {
            s.drag = false;
        }
    }
    fn clear_selection(&mut self) {
        if let Some(s) = self.sessions.get_mut(self.active) {
            s.selection = None;
            s.drag = false;
        }
    }
    fn close(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        let s = self.sessions.remove(index);
        self.retiring.push(s.handle.cleanup());
        drop(s);
        self.active = self.active.min(self.sessions.len().saturating_sub(1));
        self.preedit = None;
        self.focus = !self.sessions.is_empty();
    }
}
pub(super) fn paste(s: &str, bracketed: bool) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        s.len() <= terminal::MAX_INPUT - 12,
        "Terminal paste exceeds 64KiB; nothing sent"
    );
    let text = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut b = vec![];
    if bracketed {
        b.extend_from_slice(b"\x1b[200~");
    }
    b.extend(
        text.bytes()
            .filter(|b| *b >= 32 || matches!(b, b'\n' | b'\t')),
    );
    if bracketed {
        b.extend_from_slice(b"\x1b[201~");
    }
    Ok(b)
}
fn key_bytes(k: Key, m: Modifiers, app: bool) -> Option<Vec<u8>> {
    let modifier = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl || m.command);
    if m.ctrl || m.command {
        let name = k.name().as_bytes();
        if name.len() == 1 && name[0].is_ascii_alphabetic() {
            let mut b = vec![name[0].to_ascii_uppercase() - b'A' + 1];
            if m.alt {
                b.insert(0, 27);
            }
            return Some(b);
        }
        let c = match k {
            Key::Space | Key::Num2 => Some(0),
            Key::OpenBracket => Some(27),
            Key::Backslash => Some(28),
            Key::CloseBracket => Some(29),
            Key::Num6 => Some(30),
            Key::Minus => Some(31),
            _ => None,
        };
        if let Some(c) = c {
            return Some(vec![c]);
        }
    }
    let suffix = match k {
        Key::ArrowUp => Some('A'),
        Key::ArrowDown => Some('B'),
        Key::ArrowRight => Some('C'),
        Key::ArrowLeft => Some('D'),
        Key::Home => Some('H'),
        Key::End => Some('F'),
        _ => None,
    };
    if let Some(c) = suffix {
        return Some(if modifier != 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes()
        } else {
            format!("\x1b{}{c}", if app { 'O' } else { '[' }).into_bytes()
        });
    }
    let n = match k {
        Key::Insert => Some(2),
        Key::Delete => Some(3),
        Key::PageUp => Some(5),
        Key::PageDown => Some(6),
        Key::F5 => Some(15),
        Key::F6 => Some(17),
        Key::F7 => Some(18),
        Key::F8 => Some(19),
        Key::F9 => Some(20),
        Key::F10 => Some(21),
        Key::F11 => Some(23),
        Key::F12 => Some(24),
        _ => None,
    };
    if let Some(n) = n {
        return Some(
            if modifier == 1 {
                format!("\x1b[{n}~")
            } else {
                format!("\x1b[{n};{modifier}~")
            }
            .into_bytes(),
        );
    }
    let mut b = match k {
        Key::Enter => vec![13],
        Key::Backspace => vec![127],
        Key::Escape => vec![27],
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![9],
        Key::F1 | Key::F2 | Key::F3 | Key::F4 => {
            let c = match k {
                Key::F1 => 'P',
                Key::F2 => 'Q',
                Key::F3 => 'R',
                _ => 'S',
            };
            if modifier == 1 {
                format!("\x1bO{c}")
            } else {
                format!("\x1b[1;{modifier}{c}")
            }
            .into_bytes()
        }
        _ => return None,
    };
    if m.alt {
        b.insert(0, 27);
    }
    Some(b)
}
fn ansi(c: vt100::Color, default: Color32, bold: bool) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(mut n) => {
            if bold && n < 8 {
                n += 8;
            }
            let basic = [
                0x202026, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xcba6f7, 0x94e2d5, 0xcdd6f4,
                0x585b70, 0xf5a6ba, 0xb4edaa, 0xffe8b6, 0x9ec5ff, 0xd6b5ff, 0xa6eee2, 0xffffff,
            ];
            if n < 16 {
                let rgb = basic[n as usize];
                Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
            } else if n < 232 {
                let n = n - 16;
                let v = |x| if x == 0 { 0 } else { 55 + 40 * x };
                Color32::from_rgb(v(n / 36), v(n / 6 % 6), v(n % 6))
            } else {
                Color32::from_gray(8 + 10 * (n - 232))
            }
        }
    }
}
impl Workbench {
    pub(super) fn new_terminal(&mut self) {
        if self.terminal.sessions.len() + self.terminal.retiring.len() >= 4 {
            self.terminal.error =
                Some("Four terminal sessions are active or closing; close one first".into());
            return;
        }
        let ctx = self.context.clone();
        let wake: terminal::Wake = Arc::new(move || ctx.request_repaint());
        let shell = if cfg!(test) || self.terminal_smoke.is_some() || self.ime_smoke.is_some() {
            Shell::fixture(self.root.clone())
        } else {
            Shell::project(self.root.clone())
        };
        match Handle::start(shell, 12, 80, wake) {
            Ok(handle) => {
                let frame = handle.snapshot().unwrap_or_else(|| handle.initial());
                let id = self.terminal.next;
                self.terminal.next += 1;
                self.terminal.sessions.push(Session {
                    id,
                    handle,
                    frame,
                    selection: None,
                    drag: false,
                    size: (12, 80),
                });
                self.terminal.active = self.terminal.sessions.len() - 1;
                self.terminal.error = None;
                self.tab = 5;
                self.chrome.dock_open = true;
                self.terminal.visible = true;
                self.terminal.focus = true;
                self.editor_ui.focus_editor = false;
                self.editor_ui.frame_editor_focus = false;
            }
            Err(e) => self.terminal.error = Some(format!("Terminal: {e:#}")),
        }
    }
    pub(super) fn toggle_terminal(&mut self) {
        if self.tab == 5 && self.chrome.dock_open {
            self.chrome.dock_open = false;
            self.terminal.focus = false;
            self.editor_ui.focus_editor = true;
            self.editor_ui.frame_editor_focus = true;
        } else {
            self.tab = 5;
            self.chrome.dock_open = true;
            if self.terminal.sessions.is_empty() {
                self.new_terminal();
            } else {
                self.terminal.focus = true;
                self.editor_ui.frame_editor_focus = false;
                self.editor_ui.focus_editor = false;
            }
        }
    }
    pub(super) fn terminal_event(&mut self, ctx: &egui::Context, event: Event, ime_frame: bool) {
        if super::input_route::git_key(&event) {
            self.show_git(None);
            return;
        }
        if super::input_route::project_key(&event) {
            self.show_project_search();
            return;
        }
        if matches!(&event,Event::Key{key:Key::F12,pressed:true,modifiers,..}if modifiers.matches_exact(Modifiers::ALT))
        {
            self.toggle_terminal();
            return;
        }
        if matches!(&event,Event::Key{key:Key::T,pressed:true,modifiers,..}if modifiers.matches_exact(Modifiers::COMMAND|Modifiers::SHIFT)||modifiers.matches_exact(Modifiers::CTRL|Modifiers::SHIFT))
        {
            self.new_terminal();
            return;
        }
        if matches!(&event, Event::PointerButton { pressed: true, .. }) {
            self.edit.cancel_composition();
        }
        self.terminal.input_event(ctx, event, ime_frame);
        if self.terminal.focus {
            self.editor_ui.focus_editor = false;
            self.editor_ui.frame_editor_focus = false;
        }
    }
    pub(super) fn terminal_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal_wrapped(|ui| {
            let mut activate = None;
            for (i, s) in self.terminal.sessions.iter().enumerate() {
                let text = format!(
                    "{} · {}",
                    i + 1,
                    if matches!(s.frame.state, State::Exited { .. } | State::Failed(_)) {
                        "Exited"
                    } else {
                        "Shell"
                    }
                );
                if self
                    .chrome
                    .button(
                        ui,
                        &format!("terminal-tab-{}", s.id),
                        &text,
                        Icon::Terminal,
                        if i == self.terminal.active {
                            Kind::Tonal
                        } else {
                            Kind::Ghost
                        },
                        true,
                        98.,
                        self.reduced_motion,
                    )
                    .clicked()
                {
                    activate = Some(i);
                }
            }
            if let Some(i) = activate {
                self.terminal.active = i;
                self.terminal.focus = true;
                self.terminal.preedit = None;
            }
            if self
                .chrome
                .button(
                    ui,
                    "terminal-new",
                    "",
                    Icon::Add,
                    Kind::Ghost,
                    true,
                    44.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("New shell · Ctrl+Shift+T")
                .clicked()
            {
                self.new_terminal();
            }
            if self
                .chrome
                .button(
                    ui,
                    "terminal-close",
                    "",
                    Icon::Close,
                    Kind::Ghost,
                    !self.terminal.sessions.is_empty(),
                    40.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Close shell and its PTY session jobs")
                .clicked()
            {
                self.terminal.close(self.terminal.active);
            }
            if self
                .chrome
                .button(
                    ui,
                    "terminal-copy",
                    "",
                    Icon::Copy,
                    Kind::Ghost,
                    true,
                    40.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Copy selection · Ctrl+Shift+C")
                .clicked()
            {
                self.terminal.copy(ctx);
            }
            if self
                .chrome
                .button(
                    ui,
                    "terminal-paste",
                    "",
                    Icon::Paste,
                    Kind::Ghost,
                    !self.terminal.sessions.is_empty(),
                    40.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Paste · Ctrl+Shift+V")
                .clicked()
            {
                self.terminal.focus = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            if self
                .chrome
                .button(
                    ui,
                    "terminal-interrupt",
                    "",
                    Icon::Stop,
                    Kind::Ghost,
                    !self.terminal.sessions.is_empty(),
                    40.,
                    self.reduced_motion,
                )
                .on_hover_text_passive("Interrupt foreground command · Ctrl+C")
                .clicked()
            {
                self.terminal.send(vec![3]);
            }
        });
        if let Some(e) = &self.terminal.error {
            ui.label(RichText::new(e).small().color(m::ERROR));
        }
        let Some(s) = self.terminal.sessions.get(self.terminal.active) else {
            ui.label(
                RichText::new("Open a project shell · commands run in this project's directory")
                    .color(m::MUTED),
            );
            return;
        };
        let status = match &s.frame.state {
            State::Starting => "Starting shell…".into(),
            State::Running => format!(
                "PID {} · {} × {} · {}",
                s.frame.pid.unwrap_or(0),
                s.frame.cols,
                s.frame.rows,
                self.root.display()
            ),
            State::Stopping => "Closing PTY session…".into(),
            State::Exited { code, signal } => {
                format!("Exited · code {code:?} · signal {signal:?} · + opens a new shell")
            }
            State::Failed(e) => format!("Shell error · {e} · + retries"),
        };
        ui.add(egui::Label::new(RichText::new(&status).small().color(m::MUTED)).truncate())
            .on_hover_text_passive(status);
        let font = FontId::monospace(14.);
        self.terminal.cell = egui::vec2(ctx.fonts_mut(|f| f.glyph_width(&font, 'M')), 20.);
        let available = ui.available_size().max(egui::vec2(80., 40.));
        let rows = ((available.y / 20.).floor() as u16).clamp(2, terminal::MAX_ROWS);
        let cols =
            ((available.x / self.terminal.cell.x).floor() as u16).clamp(8, terminal::MAX_COLS);
        let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
        self.terminal.rect = rect;
        self.terminal.layer = Some(ui.layer_id());
        self.chrome.hits.insert("terminal-grid".into(), rect);
        let id = self.terminal.id();
        let response = ui
            .interact(rect, id, Sense::click_and_drag())
            .union(response);
        if self.terminal.focus {
            response.request_focus();
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        let Some(s) = self.terminal.sessions.get_mut(self.terminal.active) else {
            return;
        };
        if s.size != (rows, cols)
            && !matches!(s.frame.state, State::Exited { .. } | State::Failed(_))
        {
            if let Err(e) = s.handle.resize(rows, cols) {
                self.terminal.error = Some(e.to_string());
            } else {
                s.size = (rows, cols);
            }
        }
        let f = s.displayed();
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 8, m::SURFACE);
        for r in 0..f.rows.min(rows) {
            for c in 0..f.cols.min(cols) {
                let cell = f.cell(r, c).unwrap();
                if cell.is_wide_continuation() {
                    continue;
                }
                let pos = rect.min + egui::vec2(c as f32 * self.terminal.cell.x, r as f32 * 20.);
                let width = self.terminal.cell.x * if cell.is_wide() { 2. } else { 1. };
                let cr = Rect::from_min_size(pos, egui::vec2(width, 20.));
                let (mut fg, mut bg) = (
                    ansi(cell.fgcolor(), m::TEXT, cell.bold()),
                    ansi(cell.bgcolor(), m::SURFACE, false),
                );
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                painter.rect_filled(cr, 0, bg);
                if let Some(sel) = &s.selection {
                    let (a, b) = if sel.a <= sel.b {
                        (sel.a, sel.b)
                    } else {
                        (sel.b, sel.a)
                    };
                    if (r, c) >= a && (r, c) < b {
                        painter.rect_filled(cr, 0, m::PRIMARY_CONTAINER);
                    }
                }
                if !cell.is_wide_continuation() && cell.has_contents() {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        cell.contents(),
                        0.,
                        TextFormat {
                            font_id: font.clone(),
                            color: if cell.dim() {
                                fg.gamma_multiply(0.65)
                            } else {
                                fg
                            },
                            italics: cell.italic(),
                            ..Default::default()
                        },
                    );
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    painter.galley(pos + egui::vec2(0., 2.), g, fg);
                    if cell.underline() {
                        painter.line_segment(
                            [cr.left_bottom(), cr.right_bottom()],
                            egui::Stroke::new(1.0_f32, fg),
                        );
                    }
                }
            }
        }
        let cursor = rect.min
            + egui::vec2(
                f.cursor.1 as f32 * self.terminal.cell.x,
                f.cursor.0 as f32 * 20.,
            );
        let caret = Rect::from_min_size(cursor, egui::vec2(self.terminal.cell.x, 20.));
        if self.terminal.focus
            && !f.hide_cursor
            && f.scrollback == 0
            && !matches!(f.state, State::Exited { .. } | State::Failed(_))
        {
            if self.reduced_motion || ctx.input(|i| i.time) % 1. < 0.6 {
                painter.rect_stroke(
                    caret,
                    0,
                    egui::Stroke::new(1.2_f32, m::PRIMARY),
                    egui::StrokeKind::Inside,
                );
            }
            ui.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect,
                    cursor_rect: caret,
                })
            });
            if let Some(pre) = &self.terminal.preedit {
                let mut chars = pre.chars();
                let mut visible: String = chars.by_ref().take(128).collect();
                if chars.next().is_some() {
                    visible.push('…');
                }
                let galley = painter.layout_no_wrap(visible, font, m::ON_CONTAINER);
                painter.rect_filled(
                    Rect::from_min_size(
                        cursor,
                        egui::vec2(galley.size().x.max(self.terminal.cell.x), 20.),
                    ),
                    2,
                    m::PRIMARY_CONTAINER,
                );
                painter.galley(cursor, galley, m::ON_CONTAINER);
            }
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_keys_are_not_editor_shortcuts() {
        let m = Modifiers::CTRL | Modifiers::COMMAND;
        assert_eq!(key_bytes(Key::S, m, false), Some(vec![19]));
        assert_eq!(key_bytes(Key::C, m, false), Some(vec![3]));
        assert_eq!(
            key_bytes(Key::ArrowUp, Modifiers::NONE, true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(key_bytes(Key::ArrowRight, m, false).unwrap(), b"\x1b[1;5C");
        assert_eq!(
            key_bytes(Key::F9, Modifiers::NONE, false).unwrap(),
            b"\x1b[20~"
        );
    }
    #[test]
    fn bracketed_paste_removes_escape_end_marker_and_bounds_input() {
        assert_eq!(
            paste("a\r\nb\x1b[201~\0", true).unwrap(),
            b"\x1b[200~a\nb[201~\x1b[201~"
        );
        assert!(paste(&"x".repeat(terminal::MAX_INPUT), true).is_err());
        assert_eq!(paste("日本", false).unwrap(), "日本".as_bytes());
    }
    fn routed(a: &mut Workbench, events: Vec<Event>) -> egui::FullOutput {
        let ctx = a.context.clone();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900., 700.))),
                events,
                ..Default::default()
            },
            |ctx| {
                a.terminal.poll(true);
                a.editor_updates(ctx);
                a.semantic_shortcuts(ctx);
                a.editor_shortcuts(ctx);
                a.debug_shortcuts(ctx);
                if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::S)) {
                    a.save_document();
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.allocate_ui(egui::vec2(ui.available_width(), 280.), |ui| {
                        a.terminal_panel(ui, ctx)
                    });
                    a.editor(ui, ctx);
                });
                a.finish_editor_frame(ctx);
                a.ime_finish(ctx);
            },
        )
    }
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val x=1\n").unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let mut a = Workbench::new(d.path().to_owned(), ctx, None, false);
        a.new_terminal();
        routed(&mut a, vec![]);
        (d, a)
    }
    fn ready(a: &mut Workbench, text: &str) {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            routed(a, vec![]);
            if a.terminal.sessions[a.terminal.active]
                .frame
                .text()
                .lines()
                .any(|l| l == text)
                || text == "wex$"
                    && a.terminal.sessions[a.terminal.active]
                        .frame
                        .text()
                        .contains(text)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < end,
                "PTY output not received: {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn press(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn terminal_focus_consumes_save_debug_and_typing_without_editing_source() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        let before = a.doc.content_hash();
        routed(
            &mut a,
            vec![
                Event::Key {
                    key: Key::S,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::CTRL | Modifiers::COMMAND,
                },
                Event::Key {
                    key: Key::Q,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::CTRL | Modifiers::COMMAND,
                },
                Event::Key {
                    key: Key::F9,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
                Event::Key {
                    key: Key::U,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::CTRL | Modifiers::COMMAND,
                },
                Event::Text("printf 'TERMINAL_ONLY\\n'".into()),
                Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        ready(&mut a, "TERMINAL_ONLY");
        assert_eq!(a.doc.content_hash(), before);
        assert!(!a.save_ui.saver.worker_started());
        assert!(a.debug.breakpoints.is_empty());
    }
    #[test]
    fn mixed_source_terminal_source_events_preserve_each_surface_order() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        a.terminal.focus = false;
        a.editor_ui.focus_editor = true;
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        routed(&mut a, vec![]);
        let term = a.terminal.rect.center();
        let source = a.editor_ui.geometry.rows[&0].0 + egui::vec2(1., 11.5);
        routed(
            &mut a,
            vec![
                Event::Paste("// before\n".into()),
                press(term, true),
                press(term, false),
                Event::Text("printf 'MIXED_TERM\\n'".into()),
                Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
                press(source, true),
                press(source, false),
                Event::Paste("// after\n".into()),
            ],
        );
        ready(&mut a, "MIXED_TERM");
        assert_eq!(a.doc.rope().to_string(), "// after\n// before\nval x=1\n");
        assert!(!a.save_ui.saver.worker_started());
    }
    #[test]
    fn copy_selection_uses_actual_output_command_and_does_not_interrupt() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        a.terminal.send(b"printf 'COPY_ONLY\\n'\r".to_vec());
        ready(&mut a, "COPY_ONLY");
        let f = a.terminal.sessions[0].frame.clone();
        let r = (0..f.rows)
            .find(|r| f.selected((*r, 0), (*r, f.cols)) == "COPY_ONLY")
            .unwrap();
        a.terminal.sessions[0].selection = Some(Selection {
            frame: f.clone(),
            a: (r, 0),
            b: (r, 9),
        });
        let out = routed(&mut a, vec![Event::Copy]);
        assert!(
            out.platform_output
                .commands
                .iter()
                .any(|c| matches!(c,egui::OutputCommand::CopyText(s)if s=="COPY_ONLY"))
        );
        assert_eq!(a.terminal.sessions[0].frame.input_bytes, f.input_bytes);
    }
    #[test]
    fn terminal_remains_interactive_after_last_source_tab_closes() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        a.doc = Document::new("").unwrap();
        a.tabs.clear();
        routed(
            &mut a,
            vec![
                Event::Text("printf 'NO_SOURCE_TAB\\n'".into()),
                Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        ready(&mut a, "NO_SOURCE_TAB");
        assert!(a.doc.path.is_none());
        assert_eq!(a.doc.rope().len_chars(), 0);
    }
    #[test]
    fn ime_handoff_regression_late_terminal_commit_does_not_enter_source_or_drop_fresh_text() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        routed(
            &mut a,
            vec![
                Event::Ime(egui::ImeEvent::Enabled),
                Event::Ime(egui::ImeEvent::Preedit("候補".into())),
            ],
        );
        let source = a.editor_ui.geometry.rows[&0].0 + egui::vec2(1., 11.5);
        routed(
            &mut a,
            vec![
                press(source, true),
                press(source, false),
                Event::Ime(egui::ImeEvent::Commit("候補".into())),
                Event::Text("fresh".into()),
            ],
        );
        let actual = a.doc.rope().to_string();
        let cleanup = a.terminal.sessions[0].handle.cleanup();
        a.terminal.sessions[0].handle.stop();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !cleanup.done() {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            actual, "freshval x=1\n",
            "late IME must not cross surfaces or suppress subsequent genuine Text"
        );
    }
    #[test]
    fn ime_handoff_regression_alt_f12_close_then_text_enters_editor_in_same_frame() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        routed(
            &mut a,
            vec![
                Event::Key {
                    key: Key::F12,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::ALT,
                },
                Event::Text("// source\n".into()),
            ],
        );
        assert!(!a.chrome.dock_open);
        let actual = a.doc.rope().to_string();
        let cleanup = a.terminal.sessions[0].handle.cleanup();
        a.terminal.sessions[0].handle.stop();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !cleanup.done() {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            actual, "// source\nval x=1\n",
            "closing terminal must route subsequent Text to source, not hidden PTY"
        );
    }
    fn start_source_ime(a: &mut Workbench) {
        a.terminal.focus = false;
        a.editor_ui.focus_editor = true;
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        routed(
            a,
            vec![
                Event::Ime(egui::ImeEvent::Enabled),
                Event::Ime(egui::ImeEvent::Preedit("候補".into())),
            ],
        );
    }
    fn type_event(text: &str) -> Event {
        Event::Text(text.into())
    }
    fn stop(a: &mut Workbench) {
        let cs = a
            .terminal
            .sessions
            .iter()
            .map(|s| {
                s.handle.stop();
                s.handle.cleanup()
            })
            .collect::<Vec<_>>();
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while cs.iter().any(|c| !c.done()) {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn ime_source_to_terminal_drops_late_commit_but_writes_new_plain_text() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        start_source_ime(&mut a);
        let before = a.terminal.sessions[0].frame.input_bytes;
        let p = a.terminal.rect.center();
        routed(
            &mut a,
            vec![
                press(p, true),
                press(p, false),
                Event::Ime(egui::ImeEvent::Commit("候補".into())),
                type_event("fresh"),
            ],
        );
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.terminal.sessions[0]
            .handle
            .snapshot()
            .unwrap()
            .input_bytes
            < before + 5
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            a.terminal.sessions[0]
                .handle
                .snapshot()
                .unwrap()
                .input_bytes,
            before + 5
        );
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        stop(&mut a);
    }
    #[test]
    fn genuine_commit_before_click_is_kept_and_later_terminal_text_is_not_suppressed() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        start_source_ime(&mut a);
        let p = a.terminal.rect.center();
        routed(
            &mut a,
            vec![
                Event::Ime(egui::ImeEvent::Commit("源".into())),
                press(p, true),
                press(p, false),
                type_event("fresh"),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), "源val x=1\n");
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !a.terminal.sessions[0]
            .handle
            .snapshot()
            .unwrap()
            .text()
            .contains("wex$ fresh")
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        stop(&mut a);
    }
    #[test]
    fn ime_doc_activation_fences_old_commit_and_new_start_recovers_with_one_undo() {
        let (d, mut a) = app();
        ready(&mut a, "wex$");
        start_source_ime(&mut a);
        let first = a.doc.path.clone().unwrap();
        let p = d.path().join("B.kt");
        std::fs::write(&p, "val b=2\n").unwrap();
        let b = Document::open(&p).unwrap();
        let p = b.path.clone().unwrap();
        a.parked.insert(p.clone(), (b, EditState::default()));
        a.tabs.push(p.clone());
        a.open_file(p);
        routed(
            &mut a,
            vec![Event::Ime(egui::ImeEvent::Commit("old".into()))],
        );
        assert_eq!(a.doc.rope().to_string(), "val b=2\n");
        a.open_file(first);
        routed(
            &mut a,
            vec![
                Event::Ime(egui::ImeEvent::Enabled),
                Event::Ime(egui::ImeEvent::Preedit("新".into())),
                Event::Ime(egui::ImeEvent::Commit("新".into())),
                Event::Ime(egui::ImeEvent::Disabled),
                type_event("X"),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), "新Xval x=1\n");
        a.edit.undo(&mut a.doc, false).unwrap();
        assert_eq!(a.doc.rope().to_string(), "新val x=1\n");
        a.edit.undo(&mut a.doc, false).unwrap();
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        stop(&mut a);
    }
    #[test]
    fn ime_exact_state_guard_rejects_replacement_even_with_same_path_epoch_and_revision() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        start_source_ime(&mut a);
        let path = a.doc.path.clone();
        a.doc = Document::new("replacement").unwrap();
        a.doc.path = path;
        routed(
            &mut a,
            vec![Event::Ime(egui::ImeEvent::Commit("ghost".into()))],
        );
        assert_eq!(a.doc.rope().to_string(), "replacement");
        stop(&mut a);
    }
    #[test]
    fn ime_new_terminal_session_never_receives_old_session_commit() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        routed(
            &mut a,
            vec![Event::Ime(egui::ImeEvent::Preedit("old".into()))],
        );
        let first = a.terminal.sessions[0].frame.input_bytes;
        a.new_terminal();
        ready(&mut a, "wex$");
        let second = a.terminal.sessions[1].frame.input_bytes;
        routed(
            &mut a,
            vec![Event::Ime(egui::ImeEvent::Commit("old".into()))],
        );
        assert_eq!(
            a.terminal.sessions[0]
                .handle
                .snapshot()
                .unwrap()
                .input_bytes,
            first
        );
        assert_eq!(
            a.terminal.sessions[1]
                .handle
                .snapshot()
                .unwrap()
                .input_bytes,
            second
        );
        routed(
            &mut a,
            vec![
                Event::Ime(egui::ImeEvent::Enabled),
                Event::Ime(egui::ImeEvent::Commit("新".into())),
            ],
        );
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.terminal.sessions[1]
            .handle
            .snapshot()
            .unwrap()
            .input_bytes
            < second + 3
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            a.terminal.sessions[0]
                .handle
                .snapshot()
                .unwrap()
                .input_bytes,
            first
        );
        stop(&mut a);
    }
    #[test]
    fn ime_window_focus_loss_cancels_candidate_without_suppressing_fresh_terminal_text() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        routed(
            &mut a,
            vec![Event::Ime(egui::ImeEvent::Preedit("old".into()))],
        );
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                focused: false,
                ..Default::default()
            },
            |ctx| a.semantic_shortcuts(ctx),
        );
        assert!(a.terminal.preedit.is_none());
        routed(
            &mut a,
            vec![
                Event::Ime(egui::ImeEvent::Commit("old".into())),
                type_event("fresh"),
            ],
        );
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !a.terminal.sessions[0]
            .handle
            .snapshot()
            .unwrap()
            .text()
            .contains("wex$ fresh")
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        stop(&mut a);
    }
    #[test]
    fn ime_source_to_find_late_commit_does_not_replace_query_and_fresh_text_survives() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        a.reduced_motion = true;
        start_source_ime(&mut a);
        routed(
            &mut a,
            vec![
                Event::Key {
                    key: Key::F,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::CTRL | Modifiers::COMMAND,
                },
                Event::Ime(egui::ImeEvent::Commit("old".into())),
                type_event("query"),
            ],
        );
        let ctx = a.context.clone();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| a.find_bar(ui, ctx));
            a.ime_finish(ctx);
        });
        assert_eq!(a.editor_ui.find.options.query, "query");
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        stop(&mut a);
    }
    #[test]
    fn ime_disabled_while_unfocused_allows_genuine_commit_only_after_return() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        start_source_ime(&mut a);
        let ctx = a.context.clone();
        let _ = ctx.run(
            egui::RawInput {
                focused: false,
                events: vec![Event::Ime(egui::ImeEvent::Disabled)],
                ..Default::default()
            },
            |ctx| a.semantic_shortcuts(ctx),
        );
        let p = a.editor_ui.geometry.viewport.min + egui::vec2(1., 11.5);
        routed(
            &mut a,
            vec![
                Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
                Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                },
                Event::Ime(egui::ImeEvent::Commit("新".into())),
            ],
        );
        assert_eq!(a.doc.rope().to_string(), "新val x=1\n");
        stop(&mut a);
    }
    #[test]
    fn two_terminal_toggles_use_forecast_state_not_previous_frame_dock_state() {
        let (_d, mut a) = app();
        ready(&mut a, "wex$");
        let toggle = || Event::Key {
            key: Key::F12,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::ALT,
        };
        routed(&mut a, vec![toggle(), toggle(), type_event("fresh")]);
        assert!(a.chrome.dock_open);
        assert_eq!(a.doc.rope().to_string(), "val x=1\n");
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !a.terminal.sessions[0]
            .handle
            .snapshot()
            .unwrap()
            .text()
            .contains("wex$ fresh")
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        stop(&mut a);
    }
    #[test]
    fn opening_then_new_session_in_one_frame_tags_ime_with_actual_new_id() {
        let (d, mut a) = app();
        ready(&mut a, "wex$");
        stop(&mut a);
        a.terminal = Default::default();
        a.tab = 0;
        a.chrome.dock_open = true;
        a.editor_ui.focus_editor = true;
        let id = a.editor_ui.input_id.unwrap();
        a.context.memory_mut(|m| m.request_focus(id));
        routed(
            &mut a,
            vec![
                Event::Key {
                    key: Key::F12,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::ALT,
                },
                Event::Key {
                    key: Key::T,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
                },
                Event::Ime(egui::ImeEvent::Enabled),
                Event::Ime(egui::ImeEvent::Commit("新".into())),
            ],
        );
        assert_eq!(a.terminal.sessions.len(), 2);
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while a.terminal.sessions[1]
            .handle
            .snapshot()
            .unwrap()
            .input_bytes
            < 3
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            a.terminal.sessions[0]
                .handle
                .snapshot()
                .unwrap()
                .input_bytes,
            0
        );
        stop(&mut a);
        drop(d);
    }
}
