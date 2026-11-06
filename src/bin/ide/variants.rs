//! APK target selection is separate from credentials; environment values never enter UI state.
use super::*;
use egui::Rect;
use wexandroidstudio::material::{self as m, ButtonKind as Kind, Icon};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    schema: u32,
    variant: String,
}
pub(super) fn load(root: &std::path::Path) -> String {
    let value = (|| -> anyhow::Result<Choice> {
        let bytes = std::fs::read(root.join(".wex/apk-variant.json"))?;
        anyhow::ensure!(bytes.len() < 65536, "variant preference too large");
        Ok(serde_json::from_slice(&bytes)?)
    })();
    value
        .ok()
        .filter(|c| c.schema == 1 && matches!(c.variant.as_str(), "debug" | "release"))
        .map(|c| c.variant)
        .unwrap_or_else(|| "debug".into())
}
impl Workbench {
    pub(super) fn select_variant(&mut self, variant: &str) {
        if self.file_ui.open
            || self.running
            || self.building
            || self.debug.connection.is_some()
            || self.debug.busy
            || self.refactor.dialog
            || !matches!(variant, "debug" | "release")
        {
            return;
        }
        if self.variant == variant {
            self.variant_open = false;
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let folder = self.root.join(".wex");
            std::fs::create_dir_all(&folder)?;
            let mut file = tempfile::NamedTempFile::new_in(&folder)?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec_pretty(&Choice {
                schema: 1,
                variant: variant.into(),
            })?)?;
            file.persist(folder.join("apk-variant.json"))?;
            Ok(())
        })();
        if let Err(e) = result {
            self.status = format!("Variant preference: {e}");
            return;
        }
        self.variant = variant.into();
        self.variant_open = false;
        self.status = if self.root.join("wex.json").is_file() {
            format!("Selected {} · Preparing K2 analysis model", self.variant)
        } else {
            format!("Selected {} · Build to refresh variant model", self.variant)
        };
        self.connect_language(false);
    }
    pub(super) fn variant_title(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let enabled =
            !self.running && !self.building && self.debug.connection.is_none() && !self.debug.busy;
        let response = ui
            .interact(
                rect,
                ui.make_persistent_id("variant-picker"),
                if enabled {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                },
            )
            .on_hover_text_passive("Choose APK build variant · debug/release");
        self.chrome.hits.insert("variant".into(), rect);
        if response.hovered() || response.has_focus() {
            ui.painter().rect_filled(rect, 12, m::PRIMARY_CONTAINER);
        }
        if enabled && response.clicked() {
            self.variant_open = true;
        }
    }
    pub(super) fn variant_dialog(&mut self, ctx: &egui::Context) {
        if !self.variant_open {
            return;
        }
        let response=egui::Modal::new(egui::Id::new("apk-variant-dialog")).frame(m::card().fill(m::HIGH).inner_margin(24)).show(ctx,|ui| {
            ui.set_width(420.);ui.heading("APK build variant");ui.label("Build, Run and K2 use the selected variant. Compose preview currently uses debug.");
            ui.add_space(12.);ui.horizontal(|ui| {
                for (key,label) in [("debug","Debug"),("release","Release")] {
                    if self.chrome.button(ui,&format!("variant-{key}"),label,if key=="debug"{Icon::Debug}else{Icon::Build},if self.variant==key{Kind::Filled}else{Kind::Tonal},true,160.,self.reduced_motion).clicked(){self.select_variant(key);}
                }
            });
            ui.add_space(12.);
            match wexandroidstudio::native::Config::load(&self.root) {
                Ok(c)=>if let Some(s)=c.build.release_signing {ui.label("Release key configured · passwords read from environment only");ui.label(format!("Alias: {}",s.key_alias));ui.label(format!("Certificate: {}…",s.certificate_sha256.chars().take(20).collect::<String>())).on_hover_text_passive(s.certificate_sha256);}else{ui.colored_label(m::WARNING,"Release needs build.release_signing in wex.json; debug key is never substituted.");},
                Err(_)=>{ui.label("Open a native wex.json project to configure release signing.");}
            }
            if let Some(report)=&self.last_build && report.variant==self.variant {
                ui.add_space(12.);ui.label(format!("Last {} APK · {} bytes",report.variant,report.bytes));ui.label(report.apk.path.display().to_string());
                if self.chrome.button(ui,"copy-apk-path","Copy APK path",Icon::Copy,Kind::Tonal,true,190.,self.reduced_motion).clicked(){ctx.copy_text(report.apk.path.display().to_string());}
            }
            ui.add_space(12.);if self.chrome.button(ui,"variant-close","Done",Icon::Check,Kind::Filled,true,104.,self.reduced_motion).clicked(){self.variant_open=false;}
        });
        if response.should_close() {
            self.variant_open = false;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val x=1").unwrap();
        let c = egui::Context::default();
        m::install(&c);
        let a = Workbench::new(d.path().to_owned(), c, None, false);
        (d, a)
    }
    #[test]
    fn preference_is_strict_and_unknown_variant_defaults_to_debug() {
        let (d, _a) = app();
        std::fs::create_dir_all(d.path().join(".wex")).unwrap();
        for data in [
            r#"{"schema":1,"variant":"other"}"#,
            r#"{"schema":2,"variant":"release"}"#,
            r#"{"schema":1,"variant":"release","password":"bad"}"#,
        ] {
            std::fs::write(d.path().join(".wex/apk-variant.json"), data).unwrap();
            assert_eq!(load(d.path()), "debug");
        }
    }
    #[test]
    fn selection_persists_but_is_pinned_during_build_or_run() {
        let (d, mut a) = app();
        a.select_variant("release");
        assert_eq!(load(d.path()), "release");
        a.building = true;
        a.select_variant("debug");
        assert_eq!(a.variant, "release");
        a.building = false;
        a.running = true;
        a.select_variant("debug");
        assert_eq!(a.variant, "release");
    }
    #[test]
    fn failed_preference_write_does_not_partially_switch_variant() {
        let (d, mut a) = app();
        std::fs::write(d.path().join(".wex"), "blocked").unwrap();
        a.select_variant("release");
        assert_eq!(a.variant, "debug");
        assert!(a.status.contains("Variant preference"));
    }
    #[test]
    fn release_debug_shortcut_is_rejected_before_saving_or_selecting_device() {
        let (_d, mut a) = app();
        a.variant = "release".into();
        a.start_debug();
        assert!(a.status.contains("Debug requires debug variant"));
        assert!(!a.running && a.debug.connection.is_none());
    }
    #[test]
    fn expressive_variant_controls_fit_compact_and_wide_windows() {
        for width in [760., 1000., 1440.] {
            let (_d, mut a) = app();
            a.variant_open = true;
            a.reduced_motion = true;
            let ctx = a.context.clone();
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 560.),
                        )),
                        ..Default::default()
                    },
                    |c| a.draw_shell(c),
                );
            }
            for key in [
                "variant",
                "variant-debug",
                "variant-release",
                "variant-close",
            ] {
                assert!(
                    ctx.content_rect().contains_rect(a.chrome.hits[key]),
                    "{key} clipped at {width}"
                );
            }
        }
    }
}
