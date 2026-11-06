use super::{MotionToken, Spring};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
use std::collections::HashMap;
pub const SURFACE: Color32 = Color32::from_rgb(18, 16, 23);
pub const LOW: Color32 = Color32::from_rgb(25, 23, 32);
pub const CONTAINER: Color32 = Color32::from_rgb(33, 30, 41);
pub const HIGH: Color32 = Color32::from_rgb(45, 41, 55);
pub const PRIMARY: Color32 = Color32::from_rgb(211, 191, 255);
pub const ON_PRIMARY: Color32 = Color32::from_rgb(54, 29, 108);
pub const PRIMARY_CONTAINER: Color32 = Color32::from_rgb(78, 57, 126);
pub const ON_CONTAINER: Color32 = Color32::from_rgb(236, 222, 255);
pub const TEXT: Color32 = Color32::from_rgb(235, 229, 241);
pub const MUTED: Color32 = Color32::from_rgb(180, 173, 192);
pub const OUTLINE: Color32 = Color32::from_rgb(74, 67, 87);
pub const SUCCESS: Color32 = Color32::from_rgb(159, 225, 199);
pub const ERROR: Color32 = Color32::from_rgb(255, 180, 171);
pub const WARNING: Color32 = Color32::from_rgb(243, 207, 153);
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0., 1.);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
        channel(a.a(), b.a()),
    )
}
pub fn install(ctx: &egui::Context) {
    crate::material_symbols::install(ctx);
    let mut s = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();
    v.panel_fill = SURFACE;
    v.window_fill = HIGH;
    v.extreme_bg_color = LOW;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = PRIMARY_CONTAINER;
    v.selection.stroke = Stroke::new(1.0_f32, ON_CONTAINER);
    v.window_corner_radius = CornerRadius::same(24);
    v.window_stroke = Stroke::new(1.0_f32, OUTLINE);
    v.error_fg_color = ERROR;
    v.warn_fg_color = WARNING;
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.noninteractive,
        &mut v.widgets.open,
    ] {
        w.bg_fill = CONTAINER;
        w.weak_bg_fill = CONTAINER;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.corner_radius = CornerRadius::same(12);
    }
    for w in [&mut v.widgets.hovered, &mut v.widgets.active] {
        w.bg_fill = PRIMARY_CONTAINER;
        w.weak_bg_fill = PRIMARY_CONTAINER;
        w.fg_stroke = Stroke::new(1.0_f32, ON_CONTAINER);
        w.bg_stroke = Stroke::new(1.0_f32, PRIMARY);
        w.corner_radius = CornerRadius::same(12);
    }
    s.visuals = v;
    s.spacing.item_spacing = egui::vec2(8., 6.);
    s.spacing.button_padding = egui::vec2(14., 10.);
    s.spacing.interact_size = egui::vec2(40., 40.);
    s.spacing.scroll.bar_width = 5.;
    s.animation_time = 0.;
    for (style, size) in [
        (egui::TextStyle::Heading, 24.),
        (egui::TextStyle::Body, 14.),
        (egui::TextStyle::Button, 14.),
        (egui::TextStyle::Small, 12.),
    ] {
        s.text_styles.insert(style, FontId::proportional(size));
    }
    ctx.set_style(s);
}
struct Entry {
    spring: Spring,
    target: f32,
    last_frame: u64,
    interruptions: u64,
}
#[derive(Default)]
pub struct Motion {
    entries: HashMap<String, Entry>,
    pub interruptions: u64,
}
impl Motion {
    pub fn value(
        &mut self,
        ctx: &egui::Context,
        key: impl Into<String>,
        target: f32,
        initial: f32,
        token: MotionToken,
        reduced: bool,
    ) -> f32 {
        let frame = ctx.cumulative_frame_nr();
        let e = self.entries.entry(key.into()).or_insert(Entry {
            spring: Spring {
                value: initial,
                velocity: 0.,
            },
            target,
            last_frame: u64::MAX,
            interruptions: 0,
        });
        if (e.target - target).abs() > 0.001 && e.spring.moving(e.target) {
            self.interruptions += 1;
            e.interruptions += 1;
        }
        e.target = target;
        if e.last_frame != frame || reduced {
            e.spring
                .step_with(target, ctx.input(|i| i.stable_dt), reduced, token);
            e.last_frame = frame;
        }
        if e.spring.moving(target) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        e.spring.value
    }
    pub fn prune(&mut self, frame: u64) {
        if frame.is_multiple_of(240) {
            self.entries
                .retain(|_, e| frame.saturating_sub(e.last_frame) < 600);
        }
    }
    pub fn interrupted_keys(&self) -> Vec<(String, u64)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.interruptions > 0)
            .map(|(k, e)| (k.clone(), e.interruptions))
            .collect();
        out.sort();
        out
    }
    pub fn settle_all(&mut self) {
        for e in self.entries.values_mut() {
            e.spring.value = e.target;
            e.spring.velocity = 0.;
        }
    }
    pub fn moving(&self) -> bool {
        self.entries.values().any(|e| e.spring.moving(e.target))
    }
    pub fn snapshot(&self) -> Vec<(String, f32, f32, f32)> {
        let mut v: Vec<_> = self
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.spring.value, e.spring.velocity, e.target))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Git,
    Terminal,
    Add,
    Paste,
    Stop,
    Debug,
    StepOver,
    StepInto,
    StepOut,
    Play,
    Build,
    Save,
    Copy,
    Phone,
    Chevron,
    Refresh,
    Code,
    Folder,
    Search,
    Settings,
    Close,
    More,
    Problems,
    Log,
    Panel,
    Check,
    Collapse,
    Expand,
    Pause,
    File,
    Motion,
}
pub fn icon(ui: &Ui, rect: Rect, kind: Icon, color: Color32) {
    use crate::material_symbols as s;
    let glyph = match kind {
        Icon::Git => s::SCHEMA,
        Icon::Terminal => s::TERMINAL,
        Icon::Add => s::ADD,
        Icon::Paste => s::CONTENT_PASTE,
        Icon::Stop => s::STOP,
        Icon::Debug => s::BUG_REPORT,
        Icon::StepOver => s::STEP_OVER,
        Icon::StepInto => s::STEP_INTO,
        Icon::StepOut => s::STEP_OUT,
        Icon::Play => s::PLAY_ARROW,
        Icon::Build => s::BUILD,
        Icon::Save => s::SAVE,
        Icon::Copy => s::CONTENT_COPY,
        Icon::Phone => s::SMARTPHONE,
        Icon::Chevron => s::CHEVRON_RIGHT,
        Icon::Refresh => s::REFRESH,
        Icon::Code => s::CODE,
        Icon::Folder => s::FOLDER,
        Icon::Search => s::SEARCH,
        Icon::Settings => s::SETTINGS,
        Icon::Close => s::CLOSE,
        Icon::More => s::MORE_HORIZ,
        Icon::Problems => s::WARNING,
        Icon::Log => s::ARTICLE,
        Icon::Panel => s::DOCK_TO_BOTTOM,
        Icon::Check => s::CHECK,
        Icon::Collapse => s::EXPAND_LESS,
        Icon::Expand => s::EXPAND_MORE,
        Icon::Pause => s::PAUSE,
        Icon::File => s::DESCRIPTION,
        Icon::Motion => s::ANIMATION,
    };
    s::paint(ui, rect, glyph, color, false);
}

#[derive(Clone, Copy)]
pub enum ButtonKind {
    Filled,
    Tonal,
    Outline,
    Ghost,
    Danger,
}
pub struct Button<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub kind: ButtonKind,
    pub enabled: bool,
    pub width: f32,
}
pub fn button(ui: &mut Ui, m: &mut Motion, b: Button<'_>, reduced: bool) -> Response {
    let size = Vec2::new(b.width, 40.);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(
        rect,
        ui.id().with(b.key),
        if b.enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            b.enabled,
            if b.label.is_empty() { b.key } else { b.label },
        )
    });
    let press = m.value(
        ui.ctx(),
        format!("{}:press", b.key),
        if response.is_pointer_button_down_on() {
            1.
        } else {
            0.
        },
        0.,
        MotionToken::FastSpatial,
        reduced,
    );
    let hover = m
        .value(
            ui.ctx(),
            format!("{}:hover", b.key),
            if response.hovered() && b.enabled {
                1.
            } else {
                0.
            },
            0.,
            MotionToken::Effects,
            reduced,
        )
        .clamp(0., 1.);
    let (base, text) = if !b.enabled {
        (CONTAINER, MUTED)
    } else {
        match b.kind {
            ButtonKind::Filled => (PRIMARY, ON_PRIMARY),
            ButtonKind::Tonal => (PRIMARY_CONTAINER, ON_CONTAINER),
            ButtonKind::Outline | ButtonKind::Ghost => (SURFACE, TEXT),
            ButtonKind::Danger => (Color32::from_rgb(97, 38, 42), ERROR),
        }
    };
    let fill = if matches!(b.kind, ButtonKind::Ghost) {
        Color32::from_rgba_unmultiplied(text.r(), text.g(), text.b(), (20. * hover).round() as u8)
    } else {
        mix(base, text, 0.08 * hover)
    };
    let shape = rect.shrink((press * 1.5).max(-0.5));
    ui.painter().rect_filled(
        shape,
        CornerRadius::same((20. - 8. * press).clamp(10., 24.) as u8),
        fill,
    );
    if matches!(b.kind, ButtonKind::Outline) {
        ui.painter().rect_stroke(
            shape,
            CornerRadius::same(20),
            Stroke::new(1.0_f32, OUTLINE),
            StrokeKind::Inside,
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.),
            CornerRadius::same(22),
            Stroke::new(2.0_f32, PRIMARY),
            StrokeKind::Outside,
        );
    }
    if b.label.is_empty() {
        if let Some(i) = b.icon {
            icon(
                ui,
                Rect::from_center_size(rect.center(), Vec2::splat(22.)),
                i,
                text,
            );
        }
    } else {
        let icon_width = if b.icon.is_some() { 26. } else { 0. };
        let g = ui
            .painter()
            .layout_no_wrap(b.label.into(), FontId::proportional(14.), text);
        let total = (g.size().x + icon_width).min(rect.width() - 20.);
        let x = rect.center().x - total * 0.5;
        if let Some(i) = b.icon {
            icon(
                ui,
                Rect::from_center_size(egui::pos2(x + 10., rect.center().y), Vec2::splat(20.)),
                i,
                text,
            );
        }
        let clip = Rect::from_min_max(
            egui::pos2(x + icon_width, rect.top()),
            rect.right_bottom() - egui::vec2(10., 0.),
        );
        ui.painter().with_clip_rect(clip).galley(
            egui::pos2(x + icon_width, rect.center().y - g.size().y / 2.),
            g,
            text,
        );
    }
    response
}
pub fn badge(ui: &mut Ui, label: &str, color: Color32) -> Response {
    egui::Frame::new()
        .fill(mix(CONTAINER, color, 0.12))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(11.).color(color))
        })
        .inner
}
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(LOW)
        .corner_radius(22)
        .inner_margin(egui::Margin::same(14))
}
pub fn loading(ui: &Ui, rect: Rect, reduced: bool) {
    let t = if reduced {
        0.
    } else {
        ui.input(|i| i.time) as f32
    };
    for n in 0..3 {
        let phase = (t * 5. - n as f32 * 0.8).sin();
        let r = if reduced {
            2.5
        } else {
            2.5 + phase.abs() * 1.4
        };
        ui.painter().circle_filled(
            rect.center() + egui::vec2((n as f32 - 1.) * 10., 0.),
            r,
            PRIMARY,
        );
    }
    if !reduced {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
}
pub fn label_clipped(ui: &Ui, rect: Rect, text: &str, size: f32, color: Color32) {
    ui.painter().with_clip_rect(rect).text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(size),
        color,
    );
}

pub fn switch(
    ui: &mut Ui,
    motion: &mut Motion,
    key: &str,
    value: &mut bool,
    label: &str,
    reduced: bool,
) -> Response {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.), Sense::hover());
    let response = ui.interact(r, ui.id().with(key), Sense::click());
    if response.clicked() {
        *value = !*value;
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *value, label)
    });
    let t = motion.value(
        ui.ctx(),
        format!("{key}:thumb"),
        if *value { 1. } else { 0. },
        if *value { 1. } else { 0. },
        MotionToken::FastSpatial,
        reduced,
    );
    let effect = motion
        .value(
            ui.ctx(),
            format!("{key}:fill"),
            if *value { 1. } else { 0. },
            if *value { 1. } else { 0. },
            MotionToken::Effects,
            reduced,
        )
        .clamp(0., 1.);
    let track = Rect::from_center_size(r.left_center() + egui::vec2(26., 0.), egui::vec2(52., 32.));
    ui.painter()
        .rect_filled(track, 16, mix(CONTAINER, PRIMARY, effect));
    ui.painter().rect_stroke(
        track,
        16,
        Stroke::new(1.5_f32, mix(OUTLINE, PRIMARY, effect)),
        StrokeKind::Inside,
    );
    ui.painter().circle_filled(
        track.left_center() + egui::vec2(16. + 20. * t, 0.),
        8. + 4. * effect,
        mix(MUTED, ON_PRIMARY, effect),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            track.expand(3.),
            19,
            Stroke::new(2.0_f32, PRIMARY),
            StrokeKind::Outside,
        );
    }
    label_clipped(
        ui,
        Rect::from_min_max(r.min + egui::vec2(68., 0.), r.max),
        label,
        14.,
        TEXT,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn motion_retargets_without_reset_and_reduced_snaps_every_channel() {
        let ctx = egui::Context::default();
        let mut m = Motion::default();
        let mut before = 0.;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |ctx| {
                before = m.value(ctx, "x", 100., 0., MotionToken::DefaultSpatial, false);
            });
        }
        let velocity = m.snapshot()[0].2;
        assert!(before > 0. && before < 100. && velocity > 0.);
        let _ = ctx.run(Default::default(), |ctx| {
            let after = m.value(ctx, "x", 0., 0., MotionToken::DefaultSpatial, false);
            assert!(after > 0.);
        });
        assert_eq!(m.interruptions, 1);
        let _ = ctx.run(Default::default(), |ctx| {
            assert_eq!(
                m.value(ctx, "x", 40., 0., MotionToken::DefaultSpatial, true),
                40.
            );
        });
        assert_eq!(m.snapshot()[0].2, 0.);
        assert!(!m.moving());
    }
    #[test]
    fn color_effects_clamp_overshoot() {
        assert_eq!(mix(SURFACE, PRIMARY, 2.), PRIMARY);
        assert_eq!(mix(SURFACE, PRIMARY, -1.), SURFACE);
    }
    #[test]
    fn custom_button_supports_keyboard_activation() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut motion = Motion::default();
        let mut id = None;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = button(
                    ui,
                    &mut motion,
                    Button {
                        key: "test",
                        label: "Run",
                        icon: Some(Icon::Play),
                        kind: ButtonKind::Filled,
                        enabled: true,
                        width: 88.,
                    },
                    true,
                );
                id = Some(r.id);
                r.request_focus();
            });
        });
        assert!(ctx.memory(|m| m.has_focus(id.unwrap())));
        let mut clicked = false;
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    clicked = button(
                        ui,
                        &mut motion,
                        Button {
                            key: "test",
                            label: "Run",
                            icon: Some(Icon::Play),
                            kind: ButtonKind::Filled,
                            enabled: true,
                            width: 88.,
                        },
                        true,
                    )
                    .clicked();
                });
            },
        );
        assert!(clicked, "focused expressive button should respond to Enter");
    }
}

/// Read-only text tooltips must not capture the next click on underlying controls.
pub fn passive_tooltip(response: &Response, text: impl Into<egui::WidgetText>) {
    if !response.enabled() || !egui::Tooltip::should_show_tooltip(response, false) {
        return;
    }
    passive_tooltip_content(response, text);
}

/// Text-only hover help is click-through; interactive hover UIs remain explicit.
pub trait PassiveTooltip: Sized {
    fn on_hover_text_passive(self, text: impl Into<egui::WidgetText>) -> Self;
    fn on_disabled_hover_text_passive(self, text: impl Into<egui::WidgetText>) -> Self;
}
impl PassiveTooltip for Response {
    fn on_hover_text_passive(self, text: impl Into<egui::WidgetText>) -> Self {
        passive_tooltip(&self, text);
        self
    }
    fn on_disabled_hover_text_passive(self, text: impl Into<egui::WidgetText>) -> Self {
        if !self.enabled() && egui::Tooltip::should_show_tooltip(&self, false) {
            passive_tooltip_content(&self, text);
        }
        self
    }
}
fn passive_tooltip_content(response: &Response, text: impl Into<egui::WidgetText>) {
    let ctx = &response.ctx;
    let width = ctx
        .style()
        .spacing
        .tooltip_width
        .min((ctx.content_rect().width() - 24.).max(80.));
    // A hover-sense Area still registers widget rectangles in egui's top-layer
    // hit test, even with interactable(false). Paint read-only help directly:
    // no Area, no child widget and therefore no pointer-occluding hit rectangle.
    let style = ctx.style();
    let galley = text.into().into_galley_impl(
        ctx,
        &style,
        egui::text::TextWrapping {
            max_width: width,
            max_rows: 20,
            ..Default::default()
        },
        egui::TextStyle::Body.into(),
        egui::Align::Min,
    );
    let size = galley.size() + egui::vec2(16., 12.);
    let screen = ctx.content_rect().shrink(6.);
    let mut pos = response.rect.left_bottom() + egui::vec2(0., 4.);
    if pos.y + size.y > screen.bottom() {
        pos.y = response.rect.top() - size.y - 4.;
    }
    pos.x = pos
        .x
        .clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    pos.y = pos
        .y
        .clamp(screen.top(), (screen.bottom() - size.y).max(screen.top()));
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            response.id.with("wex-passive-tooltip"),
        ))
        .with_clip_rect(screen);
    let rect = Rect::from_min_size(pos, size);
    painter.rect_filled(rect, 12, HIGH);
    painter.rect_stroke(rect, 12, Stroke::new(1_f32, OUTLINE), StrokeKind::Inside);
    painter.galley(pos + egui::vec2(8., 6.), galley, TEXT);
}

#[cfg(test)]
mod passive_tooltip_tests {
    use super::*;
    #[test]
    fn plain_tooltip_overlay_keeps_first_underlying_click() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.style_mut(|s| {
            s.interaction.tooltip_delay = 0.;
            s.interaction.tooltip_grace_time = 0.;
            s.interaction.show_tooltips_only_when_still = false;
            s.spacing.tooltip_width = 240.;
        });
        let p = egui::pos2(150., 95.);
        let mut time = 0.;
        let mut clicks = 0;
        let mut frame = |events: Vec<egui::Event>| {
            time += 0.1;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(500., 500.),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let r = ui.put(
                            Rect::from_min_size(egui::pos2(50., 75.), egui::vec2(200., 40.)),
                            egui::Button::new("target"),
                        );
                        if r.clicked() {
                            clicks += 1;
                        }
                        let mut anchor = r;
                        anchor.rect =
                            Rect::from_min_max(egui::pos2(50., 40.), egui::pos2(300., 70.));
                        passive_tooltip_content(
                            &anchor,
                            "ordinary non-interactive path text ".repeat(30),
                        );
                    });
                },
            );
        };
        for _ in 0..3 {
            frame(vec![]);
        }
        frame(vec![egui::Event::PointerMoved(p)]);
        for _ in 0..5 {
            frame(vec![]);
        }
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert_eq!(
            clicks, 1,
            "a read-only tooltip painted over a control must not consume its first click"
        );
    }
}
