//! Native event order with pointer coordinates captured against the displayed viewport.
use super::edit_ui::{GUTTER, ROW};
use super::*;
use std::{collections::VecDeque, time::Instant};
#[derive(Clone, Copy, Debug)]
pub(super) struct Point {
    pub line: usize,
    pub x: f32,
    pub screen: egui::Pos2,
}
#[derive(Clone, Debug)]
pub(super) enum Pointer {
    Press {
        point: Point,
        shift: bool,
        when: Instant,
    },
    Move(Point),
    Release(Point),
}
#[derive(Clone, Debug)]
pub(super) enum Ordered {
    Raw(egui::Event),
    Terminal(egui::Event),
    Ime(super::ime::Tag, egui::ImeEvent),
    CancelIme(super::ime::Target),
    Pointer(Pointer),
}
#[derive(Default, serde::Serialize)]
pub(super) struct Metrics {
    pub deferred: u64,
    pub resumed: u64,
    pub presses: u64,
    pub releases: u64,
    pub cancelled: u64,
    pub last_selection: Option<(usize, usize)>,
    pub last_deferred_revision: u64,
    pub last_deferred_order: Vec<&'static str>,
}
pub(super) fn push(out: &mut VecDeque<Ordered>, event: Ordered) {
    if matches!(event, Ordered::Pointer(Pointer::Move(_)))
        && matches!(out.back(), Some(Ordered::Pointer(Pointer::Move(_))))
    {
        *out.back_mut().unwrap() = event;
    } else {
        out.push_back(event);
    }
}
pub(super) fn point(origin: egui::Pos2, p: egui::Pos2) -> Point {
    Point {
        line: ((p.y - origin.y) / ROW).floor().max(0.) as usize,
        x: p.x - origin.x - GUTTER,
        screen: p,
    }
}
pub(super) fn capture(
    ctx: &egui::Context,
    origin: egui::Pos2,
    viewport: egui::Rect,
    layer: egui::LayerId,
) -> VecDeque<Ordered> {
    let events = ctx.input(|i| i.events.clone());
    let mut out = VecDeque::new();
    for event in events {
        let point = |p: egui::Pos2| Point {
            line: ((p.y - origin.y) / ROW).floor().max(0.) as usize,
            x: p.x - origin.x - GUTTER,
            screen: p,
        };
        match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            } => {
                if pressed {
                    if viewport.contains(pos) && ctx.layer_id_at(pos) == Some(layer) {
                        push(
                            &mut out,
                            Ordered::Pointer(Pointer::Press {
                                point: point(pos),
                                shift: modifiers.shift,
                                when: Instant::now(),
                            }),
                        );
                    }
                } else {
                    push(&mut out, Ordered::Pointer(Pointer::Release(point(pos))));
                }
            }
            egui::Event::PointerMoved(pos) => {
                push(&mut out, Ordered::Pointer(Pointer::Move(point(pos))))
            }
            e => push(&mut out, Ordered::Raw(e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_tracking_points_coalesce_but_never_cross_input_or_release_barriers() {
        let mut q = VecDeque::new();
        for x in 0..1000 {
            push(
                &mut q,
                Ordered::Pointer(Pointer::Move(Point {
                    line: 0,
                    x: x as f32,
                    screen: egui::Pos2::ZERO,
                })),
            );
        }
        assert_eq!(q.len(), 1);
        push(&mut q, Ordered::Raw(egui::Event::Copy));
        for x in 0..1000 {
            push(
                &mut q,
                Ordered::Pointer(Pointer::Move(Point {
                    line: 1,
                    x: x as f32,
                    screen: egui::Pos2::ZERO,
                })),
            );
        }
        assert_eq!(q.len(), 3);
        push(
            &mut q,
            Ordered::Pointer(Pointer::Release(Point {
                line: 1,
                x: 999.,
                screen: egui::Pos2::ZERO,
            })),
        );
        assert_eq!(q.len(), 4);
    }
}
