//! Existing completed native tapes traverse the real new closing controls as input.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
pub(super) struct Exit {
    start: std::time::Instant,
    next: std::time::Instant,
    release: Option<(Pos2, std::time::Instant)>,
}
impl Workbench {
    pub(super) fn legacy_close_input(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
        event: bool,
    ) -> bool {
        let mode = std::env::args().nth(2).unwrap_or_default();
        if self.legacy_exit.is_none()
            && event
            && mode.starts_with("--")
            && mode.ends_with("-smoke")
            && !matches!(
                mode.as_str(),
                "--window-close-smoke" | "--debug-close-smoke"
            )
        {
            self.legacy_exit = Some(Exit {
                start: std::time::Instant::now(),
                next: std::time::Instant::now(),
                release: None,
            });
            eprintln!(
                "LEGACY_TAPE_COMPLETED_CLOSE_FLOW_BEGIN: {mode}; original tape assertions/deadline unchanged"
            );
        }
        let Some(mut s) = self.legacy_exit.take() else {
            return false;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(15),
            "completed tape closing exceeded15s: {}",
            self.status
        );
        input.focused = true;
        input.modifiers = Modifiers::NONE;
        input
            .events
            .retain(|e| matches!(e, Event::Screenshot { .. }));
        ctx.request_repaint_after(Duration::from_millis(16));
        if let Some((pos, end)) = s.release.take() {
            input.events.push(Event::PointerMoved(pos));
            if std::time::Instant::now() < end {
                s.release = Some((pos, end));
            } else {
                input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
                s.next = std::time::Instant::now() + Duration::from_millis(100);
            }
        } else if std::time::Instant::now() >= s.next && !self.close_ui.allow {
            let button = match self.close_ui.phase() {
                Some(window_close::Phase::Choices) => Some("window-close-discard-text"),
                Some(window_close::Phase::Backup) => Some("window-close-backup"),
                Some(window_close::Phase::Failed) => {
                    panic!("native closing check failed: {}", self.status)
                }
                _ => None,
            };
            if let Some(name) = button
                && let Some(rect) = self.chrome.hits.get(name)
            {
                let pos = rect.center();
                assert!(ctx.content_rect().contains(pos));
                input.events.extend([
                    Event::PointerMoved(pos),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ]);
                s.release = Some((pos, std::time::Instant::now() + Duration::from_millis(90)));
                eprintln!("LEGACY_CLOSE_REAL_POINTER: {name}");
            } else if self.close_ui.requested && !self.close_ui.open && self.close_other_modal() {
                for pressed in [true, false] {
                    input.events.push(Event::Key {
                        key: Key::Escape,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    });
                }
                s.next = std::time::Instant::now() + Duration::from_millis(180);
            }
        }
        self.legacy_exit = Some(s);
        true
    }
}
