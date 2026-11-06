//! Model refresh UI is separate from current K2 diagnostics and editor history.
use super::*;
use wexandroidstudio::{
    language::{ModelPhase, ModelStatus},
    material as m,
};
fn accepts(current: Option<&ModelStatus>, next: &ModelStatus) -> bool {
    let Some(old) = current else {
        return true;
    };
    if next.revision != old.revision {
        return next.revision > old.revision;
    }
    if next.attempt != old.attempt {
        return next.attempt > old.attempt;
    }
    !matches!(old.phase, ModelPhase::Applied | ModelPhase::Failed) || next.phase == old.phase
}
impl Workbench {
    pub(super) fn model_update(&mut self, mut next: ModelStatus) {
        if !accepts(self.model_status.as_ref(), &next) {
            return;
        }
        let invalidate = self.model_status.as_ref().is_some_and(|old| {
            old.revision != next.revision
                || (old.attempt != next.attempt
                    && matches!(old.phase, ModelPhase::Applied | ModelPhase::Failed))
        });
        if invalidate {
            self.cancel_semantic_open();
        }
        if invalidate || next.phase == ModelPhase::Applied {
            self.completion_pending = false;
            self.completions.clear();
            self.refactor.id = self
                .refactor
                .id
                .checked_add(1)
                .expect("semantic request epoch exhausted");
            self.refactor.busy = false;
            self.refactor.plan = None;
            self.refactor.capture = None;
            self.refactor.usages.clear();
            if self.refactor.dialog {
                self.refactor.dialog = false;
                self.refactor.error = "Model changed; request a new rename preview".into();
            }
        }
        if next.phase == ModelPhase::Applied {
            self.diagnostic_snapshot = None;
            self.diagnostics.clear();
        }
        next.detail = next.detail.chars().take(4096).collect();
        self.model_status = Some(next);
    }
    pub(super) fn model_brief(&self) -> &str {
        match self.model_status.as_ref().map(|s| &s.phase) {
            Some(ModelPhase::Waiting | ModelPhase::Preparing | ModelPhase::Warming)
                if self.model_status.as_ref().is_some_and(|s| s.retained) =>
            {
                "Updating model · previous K2 retained"
            }
            Some(ModelPhase::Waiting | ModelPhase::Preparing | ModelPhase::Warming) => {
                "Preparing model → K2"
            }
            Some(ModelPhase::Failed) if self.model_status.as_ref().is_some_and(|s| s.retained) => {
                "Model error · previous K2 retained"
            }
            Some(ModelPhase::Failed) => "K2 unavailable · retry model",
            _ => "",
        }
    }
    pub(super) fn model_tip(&self) -> String {
        if let Some(s) = &self.model_status {
            format!(
                "{}\n{}\n{}",
                self.language_status,
                self.model_brief(),
                s.detail
            )
        } else {
            self.language_status.clone()
        }
    }
    pub(super) fn retry_model(&mut self) {
        if self.language.as_ref().is_some_and(|c| c.task.is_finished()) {
            self.connect_language(false);
            return;
        }
        if let Some(c) = &self.language
            && c.requests
                .try_send(Request::Refresh {
                    force: false,
                    variant: Some(self.variant.clone()),
                })
                .is_err()
        {
            self.status = "Model request queue busy · retry".into();
        }
    }
    pub(super) fn model_card(&mut self, ui: &mut egui::Ui) {
        if !self.native_language {
            return;
        }
        m::card().fill(m::HIGH).inner_margin(16).show(ui,|ui| {
            ui.heading("Project model");
            if let Some(s)=&self.model_status {
                ui.colored_label(if s.phase==ModelPhase::Failed{m::WARNING}else{m::MUTED},format!("{:?} · change{} / attempt{}",s.phase,s.revision,s.attempt));
                ui.label(&s.detail);
                if s.retained {ui.label("Your current K2 session and unsaved tabs stay available while this updates.");}
            } else {ui.label("Watching config, dependency locks, manifests and resources in the background.");}
            if self.model_status.as_ref().is_some_and(|s|s.phase==ModelPhase::Failed) && self.chrome.button(ui,"model-retry-settings","Retry model",m::Icon::Refresh,m::ButtonKind::Tonal,true,160.,self.reduced_motion).clicked(){self.retry_model();}
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn status(revision: u64, attempt: u64, phase: ModelPhase) -> ModelStatus {
        ModelStatus {
            revision,
            attempt,
            phase,
            detail: "test".into(),
            retained: true,
        }
    }
    #[test]
    fn stale_progress_cannot_replace_a_newer_epoch_or_terminal_state() {
        let old = status(4, 2, ModelPhase::Applied);
        assert!(!accepts(Some(&old), &status(3, 9, ModelPhase::Failed)));
        assert!(!accepts(Some(&old), &status(4, 1, ModelPhase::Preparing)));
        assert!(!accepts(Some(&old), &status(4, 2, ModelPhase::Warming)));
        assert!(accepts(Some(&old), &status(5, 0, ModelPhase::Waiting)));
        assert!(accepts(Some(&old), &status(4, 3, ModelPhase::Preparing)));
    }
    #[test]
    fn refresh_error_does_not_mutate_dirty_buffer_selection_or_diagnostics() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val answer=1").unwrap();
        let mut app = Workbench::new(d.path().to_owned(), egui::Context::default(), None, false);
        app.doc.edit(0..0, "// unsaved\n").unwrap();
        app.edit.collapse(3);
        app.diagnostic_snapshot = Some((app.doc.path.clone().unwrap(), app.doc.version));
        let before = app.doc.rope().clone();
        let selected = app.edit.cursor;
        let diagnostic = app.diagnostic_snapshot.clone();
        app.model_update(status(2, 1, ModelPhase::Failed));
        assert_eq!(*app.doc.rope(), before);
        assert!(app.doc.dirty());
        assert_eq!(app.edit.cursor, selected);
        assert_eq!(app.diagnostic_snapshot, diagnostic);
    }
}
