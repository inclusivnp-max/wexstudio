//! Local WEXdesktop Material Symbols Rounded subsets; not editor text fallbacks.
pub const TERMINAL: char = '\u{eb8e}';
pub const ADD: char = '\u{e145}';
pub const CONTENT_PASTE: char = '\u{e14f}';
pub const STOP: char = '\u{e047}';
pub const BUG_REPORT: char = '\u{e868}';
pub const STEP_OVER: char = '\u{f6ff}';
pub const STEP_INTO: char = '\u{f701}';
pub const STEP_OUT: char = '\u{f700}';
pub const PLAY_ARROW: char = '\u{e037}';
pub const BUILD: char = '\u{e869}';
pub const SAVE: char = '\u{e161}';
pub const CONTENT_COPY: char = '\u{e14d}';
pub const SMARTPHONE: char = '\u{e0d4}';
pub const CHEVRON_RIGHT: char = '\u{e409}';
pub const REFRESH: char = '\u{e5d5}';
pub const CODE: char = '\u{e86f}';
pub const FOLDER: char = '\u{e2c7}';
pub const FOLDER_OPEN: char = '\u{e2c8}';
pub const SEARCH: char = '\u{e8b6}';
pub const SETTINGS: char = '\u{e8b8}';
pub const CLOSE: char = '\u{e14c}';
pub const MORE_HORIZ: char = '\u{e5d3}';
pub const WARNING: char = '\u{e002}';
pub const ARTICLE: char = '\u{ef42}';
pub const DOCK_TO_BOTTOM: char = '\u{f7e6}';
pub const CHECK: char = '\u{e5ca}';
pub const EXPAND_LESS: char = '\u{e5ce}';
pub const EXPAND_MORE: char = '\u{e5cf}';
pub const PAUSE: char = '\u{e034}';
pub const DESCRIPTION: char = '\u{e873}';
pub const ANIMATION: char = '\u{e71c}';
pub const ANDROID: char = '\u{e859}';
pub const FOLDER_ZIP: char = '\u{eb2c}';
pub const WIDGETS: char = '\u{e1bd}';
pub const DATA_OBJECT: char = '\u{ead3}';
pub const COFFEE: char = '\u{efef}';
pub const STORAGE: char = '\u{e1db}';
pub const INTEGRATION_INSTRUCTIONS: char = '\u{ef54}';
pub const IMAGE: char = '\u{e251}';
pub const MOVIE: char = '\u{e02c}';
pub const AUDIOTRACK: char = '\u{e3a1}';
pub const TUNE: char = '\u{e429}';
pub const KEY: char = '\u{e73c}';
pub const VERIFIED_USER: char = '\u{e8e8}';
pub const MEMORY: char = '\u{e322}';
pub const TEXT_SNIPPET: char = '\u{f1c6}';
pub const DATABASE: char = '\u{f20e}';
pub const DEVELOPER_BOARD: char = '\u{e30d}';
pub const LANGUAGE: char = '\u{e894}';
pub const CSS: char = '\u{eb93}';
pub const HTML: char = '\u{eb7e}';
pub const JAVASCRIPT: char = '\u{eb7c}';
pub const PETS: char = '\u{e91d}';
pub const DIAMOND: char = '\u{ead5}';
pub const SCHEMA: char = '\u{e4fd}';
pub const MENU_BOOK: char = '\u{ea19}';
pub const TABLE_CHART: char = '\u{e265}';
pub const SLIDESHOW: char = '\u{e41b}';
pub const TEXT_FIELDS: char = '\u{e262}';
pub const INVENTORY_2: char = '\u{e1a1}';
pub const DIFFERENCE: char = '\u{eb7d}';

#[cfg(feature = "desktop")]
pub fn install(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (
            "wex-symbols-filled",
            include_bytes!("../assets/icons/material-rounded-filled.ttf").as_slice(),
        ),
        (
            "wex-symbols-outlined",
            include_bytes!("../assets/icons/material-rounded-outlined.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
        fonts
            .families
            .insert(FontFamily::Name(name.into()), vec![name.into()]);
    }
    ctx.set_fonts(fonts);
}
#[cfg(feature = "desktop")]
pub fn paint(
    ui: &eframe::egui::Ui,
    rect: eframe::egui::Rect,
    glyph: char,
    color: eframe::egui::Color32,
    filled: bool,
) {
    use eframe::egui::{Align2, FontFamily, FontId};
    let family = if filled {
        "wex-symbols-filled"
    } else {
        "wex-symbols-outlined"
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(
            rect.width().min(rect.height()),
            FontFamily::Name(family.into()),
        ),
        color,
    );
}
#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;
    use ab_glyph::{Font, FontRef};
    #[test]
    fn every_imported_symbol_has_real_outline_in_both_fonts() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/icons/manifest.json")).unwrap();
        for bytes in [
            include_bytes!("../assets/icons/material-rounded-filled.ttf").as_slice(),
            include_bytes!("../assets/icons/material-rounded-outlined.ttf").as_slice(),
        ] {
            let font = FontRef::try_from_slice(bytes).unwrap();
            for cp in manifest["symbols"].as_object().unwrap().values() {
                let glyph = font.glyph_id(char::from_u32(cp.as_u64().unwrap() as u32).unwrap());
                assert_ne!(glyph.0, 0);
                assert!(font.outline(glyph).is_some());
            }
        }
    }
    #[test]
    fn icon_install_keeps_default_source_font_families_exactly_unchanged() {
        use eframe::egui::{Context, FontDefinitions, FontFamily};
        let ctx = Context::default();
        install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            let actual = ctx.fonts(|f| f.definitions().clone());
            let original = FontDefinitions::default();
            for family in [FontFamily::Monospace, FontFamily::Proportional] {
                assert_eq!(actual.families[&family], original.families[&family]);
            }
            assert_eq!(actual.font_data.len(), original.font_data.len() + 2);
        });
    }
}
