//! Honest module compilation receipts, separate from actual executed tool steps.
use super::*;
use wexandroidstudio::{compile_cache::Record, material as m};
fn summary(records: &[Record]) -> String {
    let hits = records.iter().filter(|r| r.state == "hit").count();
    format!("{hits} / {} modules reused", records.len())
}
impl Workbench {
    pub(super) fn compilation_summary(&self, records: &[Record]) -> String {
        summary(records)
    }
    pub(super) fn compile_cache_panel(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.last_build else {
            return;
        };
        if report.compilation_cache.is_empty() {
            return;
        }
        let response = m::card().fill(m::HIGH).inner_margin(12).show(ui, |ui| {
            ui.colored_label(
                m::PRIMARY,
                format!(
                    "Last successful build · {}",
                    summary(&report.compilation_cache)
                ),
            );
            for r in &report.dex_cache {
                ui.label(
                    egui::RichText::new(format!(
                        "DEX · {}",
                        if r.state == "hit" {
                            "Reused"
                        } else if r.state == "miss" {
                            "Regenerated"
                        } else {
                            "Uncached"
                        }
                    ))
                    .small()
                    .color(m::MUTED),
                );
            }
            for r in &report.sdk_cache {
                ui.colored_label(
                    m::MUTED,
                    format!(
                        "Android Java image · {}",
                        match r.state.as_str() {
                            "hit" => "Reused",
                            "miss" => "Regenerated",
                            _ => "Uncached",
                        }
                    ),
                )
                .on_hover_text_passive(&r.detail);
            }
            if !report.resource_cache.is_empty() {
                let reused: u64 = report.resource_cache.iter().map(|r| r.reused as u64).sum();
                let files: u64 = report.resource_cache.iter().map(|r| r.files as u64).sum();
                ui.colored_label(
                    m::MUTED,
                    format!("Resources · {reused} / {files} files reused"),
                );
            }
            ui.horizontal_wrapped(|ui| {
                for r in &report.compilation_cache {
                    let (label, color) = match r.state.as_str() {
                        "hit" => ("Reused", m::PRIMARY),
                        "miss" => ("Recompiled", m::TEXT),
                        _ => ("Uncached", m::MUTED),
                    };
                    m::card().fill(m::CONTAINER).inner_margin(6).show(ui, |ui| {
                        ui.colored_label(color, format!("{} · {label}", r.module))
                            .on_hover_text_passive(format!("{}\n{} files", r.detail, r.files));
                    });
                }
            });
        });
        self.chrome
            .hits
            .insert("compile-cache-summary".into(), response.response.rect);
        ui.add_space(8.);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(state: &str) -> Record {
        Record {
            module: "app".into(),
            key: String::new(),
            state: state.into(),
            detail: String::new(),
            files: 1,
        }
    }
    #[test]
    fn receipts_count_hits_not_skipped_or_compiled_modules() {
        assert_eq!(
            summary(&[record("hit"), record("miss"), record("disabled")]),
            "1 / 3 modules reused"
        );
        assert_eq!(
            summary(&[record("miss"), record("miss")]),
            "0 / 2 modules reused"
        );
        assert_eq!(
            summary(&[record("hit"), record("hit")]),
            "2 / 2 modules reused"
        );
    }
}
