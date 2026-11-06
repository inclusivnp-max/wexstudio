//! Optional system CJK fallback is loaded on a worker after the first UI update.
//! Latin/monospace defaults are always available; no font download or executable bloat.
use eframe::egui;
use std::sync::{Arc, OnceLock, mpsc};
type Font = Arc<egui::FontData>;
#[derive(Default)]
pub struct FontFallback {
    rx: Option<mpsc::Receiver<Option<Font>>>,
    started: bool,
    pub ready: bool,
    pub installed: bool,
}
impl FontFallback {
    pub fn poll(&mut self, ctx: &egui::Context) {
        if !self.started {
            self.started = true;
            let (tx, rx) = mpsc::sync_channel(1);
            self.rx = Some(rx);
            let wake = ctx.clone();
            std::thread::spawn(move || {
                static CJK: OnceLock<Option<Font>> = OnceLock::new();
                let font = CJK
                    .get_or_init(|| {
                        [
                            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
                            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                        ]
                        .iter()
                        .find_map(|p| std::fs::read(p).ok().and_then(valid_font))
                    })
                    .clone();
                let _ = tx.send(font);
                wake.request_repaint();
            });
            return;
        }
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(font) => {
                    self.ready = true;
                    self.rx = None;
                    if let Some(font) = font {
                        // Add the late fallback to the installed font set; rebuilding
                        // defaults would discard the shell's named icon families.
                        let mut fonts = ctx.fonts(|f| f.definitions().clone());
                        fonts.font_data.insert("wex-system-cjk".into(), font);
                        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                        {
                            fonts
                                .families
                                .entry(family)
                                .or_default()
                                .push("wex-system-cjk".into());
                        }
                        ctx.set_fonts(fonts);
                        self.installed = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.ready = true;
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

// The process-lifetime OnceLock already retains the optional font. A borrowed
// static face avoids egui cloning the 19 MiB TTC for each font implementation.
fn valid_font(bytes: Vec<u8>) -> Option<Font> {
    if bytes.len() > 32 * 1024 * 1024 || ab_glyph::FontRef::try_from_slice(&bytes).is_err() {
        return None;
    }
    Some(Arc::new(egui::FontData::from_static(Box::leak(
        bytes.into_boxed_slice(),
    ))))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn async_cjk_fallback_keeps_named_icons_and_exact_text_fallback_order() {
        let ctx = egui::Context::default();
        crate::material_symbols::install(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let before = ctx.fonts(|f| f.definitions().clone());
        let font = before.font_data.values().next().unwrap().clone();
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Some(font)).unwrap();
        let mut fallback = FontFallback {
            started: true,
            rx: Some(rx),
            ..Default::default()
        };
        fallback.poll(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            let after = ctx.fonts(|f| f.definitions().clone());
            for name in ["wex-symbols-filled", "wex-symbols-outlined"] {
                let family = egui::FontFamily::Name(name.into());
                assert!(
                    after.families.contains_key(&family),
                    "async CJK replaced {name}"
                );
                assert_eq!(after.families[&family], before.families[&family]);
            }
            for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
                let mut expected = before.families[&family].clone();
                expected.push("wex-system-cjk".into());
                assert_eq!(after.families[&family], expected);
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                let rect = egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::splat(24.));
                for filled in [false, true] {
                    crate::material_symbols::paint(
                        ui,
                        rect,
                        crate::material_symbols::FOLDER,
                        egui::Color32::WHITE,
                        filled,
                    );
                }
            });
        });
        assert!(fallback.ready && fallback.installed);
    }
    #[test]
    fn malformed_optional_font_is_rejected_off_thread() {
        assert!(valid_font(b"bad font bytes".to_vec()).is_none());
        assert!(valid_font(vec![0; 33 * 1024 * 1024]).is_none());
    }
}
