//! Real native-window first-project K2 regression. No APK or fake language events.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::time::Instant;
fn select_source(
    root: &std::path::Path,
    protected: &[(PathBuf, String)],
    choice: Option<&std::path::Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = choice {
        anyhow::ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "smoke source must be project-relative"
        );
        let path = root.join(path).canonicalize()?;
        anyhow::ensure!(
            path.starts_with(root)
                && path.extension().is_some_and(|e| e == "kt")
                && protected.iter().any(|(p, _)| p == &path),
            "smoke source is not a declared project Kotlin file"
        );
        Ok(path)
    } else {
        protected
            .iter()
            .find(|(p, _)| p.ends_with("MainActivity.kt"))
            .map(|(p, _)| p.clone())
            .ok_or_else(|| anyhow::anyhow!("analysis smoke source missing"))
    }
}
pub(super) struct AnalysisSmoke {
    dir: PathBuf,
    source: PathBuf,
    original: String,
    protected: Vec<(PathBuf, String)>,
    step: usize,
    start: Instant,
    next: Instant,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    snapshots: Vec<Value>,
}
impl AnalysisSmoke {
    pub(super) fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !app.root.join(".wex/native").exists(),
            "first-project smoke must not have an APK model"
        );
        assert!(
            !app.root.join(".wex/analysis").exists(),
            "native window must prepare the first model itself"
        );
        let protected = project::source_files(&app.root)
            .into_iter()
            .map(|p| {
                let h = blake3::hash(&std::fs::read(&p).unwrap())
                    .to_hex()
                    .to_string();
                (p, h)
            })
            .collect::<Vec<_>>();
        let choice = std::env::var_os("WEX_ANALYSIS_SMOKE_SOURCE").map(PathBuf::from);
        let source = select_source(&app.root, &protected, choice.as_deref()).unwrap();
        let original = std::fs::read_to_string(&source).unwrap();
        app.open_file(source.clone());
        app.editor_ui.focus_editor = true;
        Self {
            dir,
            source,
            original,
            protected,
            step: 0,
            start: Instant::now(),
            next: Instant::now() + Duration::from_secs(2),
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            snapshots: vec![],
        }
    }
}
fn key(input: &mut egui::RawInput, key: Key, modifiers: egui::Modifiers) {
    for pressed in [true, false] {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
}
fn clean(app: &Workbench) -> bool {
    app.doc
        .path
        .as_ref()
        .is_some_and(|p| app.diagnostic_snapshot.as_ref() == Some(&(p.clone(), app.doc.version)))
        && !app.diagnostics.iter().any(|d| d.severity == Some(1))
}
fn snap(app: &Workbench) -> Value {
    json!({"variant":app.variant,"path":app.doc.path,"version":app.doc.version,"diagnostic_snapshot":app.diagnostic_snapshot,"diagnostics":app.diagnostics,"language_status":app.language_status,"dirty":app.doc.dirty()})
}
impl Workbench {
    pub(super) fn analysis_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.analysis_smoke.take() else {
            return;
        };
        input.focused = true;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
        assert!(
            s.start.elapsed() < Duration::from_secs(350),
            "native analysis smoke step{}: {} / {} / {:?}",
            s.step,
            self.status,
            self.language_status,
            self.diagnostics
        );
        ctx.request_repaint_after(Duration::from_millis(16));
        if let Some((pos, until)) = s.release.take() {
            input.events.push(egui::Event::PointerMoved(pos));
            if Instant::now() < until {
                s.release = Some((pos, until));
            } else {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
            self.analysis_smoke = Some(s);
            return;
        }
        if s.awaiting.is_none()
            && let Some((name, hover)) = s.pending.take()
        {
            if let Some(rect) = self.chrome.hits.get(&name) {
                let pos = rect.center();
                input.events.push(egui::Event::PointerMoved(pos));
                if hover < 3 {
                    s.pending = Some((name, hover + 1));
                } else {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    });
                    s.release = Some((pos, Instant::now() + Duration::from_millis(100)));
                }
            } else {
                s.pending = Some((name, 0));
            }
            self.analysis_smoke = Some(s);
            return;
        }
        if s.awaiting.is_some() || Instant::now() < s.next {
            self.analysis_smoke = Some(s);
            return;
        }
        let mut shot = None;
        match s.step {
            0 => {
                if !clean(self) {
                    self.analysis_smoke = Some(s);
                    return;
                }
                s.snapshots.push(snap(self));
                shot = Some("01-first-project-k2");
            }
            1 => {
                self.editor_ui.focus_editor = true;
                let command = egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                };
                key(input, Key::A, command);
                let broken = s.original.replace(
                    "val debug = BuildConfig.DEBUG",
                    "val analysisMismatch: Int = \"wrong\"\n        val debug = BuildConfig.DEBUG",
                );
                assert_ne!(broken, s.original);
                input.events.push(egui::Event::Paste(broken));
            }
            2 => {
                if self.diagnostic_snapshot.as_ref() != Some(&(s.source.clone(), self.doc.version))
                    || !self.diagnostics.iter().any(|d| d.severity == Some(1))
                {
                    self.analysis_smoke = Some(s);
                    return;
                }
                assert!(self.doc.dirty());
                s.snapshots.push(snap(self));
                shot = Some("02-real-type-error");
            }
            3 => {
                self.editor_ui.focus_editor = true;
                key(
                    input,
                    Key::Z,
                    egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                );
            }
            4 => {
                if *self.doc.rope() != s.original || !clean(self) {
                    self.analysis_smoke = Some(s);
                    return;
                }
                s.snapshots.push(snap(self));
                shot = Some("03-undo-recovery");
            }
            5 => {
                if !self.variant_open {
                    s.pending = Some(("variant".into(), 0));
                    s.next = Instant::now() + Duration::from_millis(800);
                    self.analysis_smoke = Some(s);
                    return;
                }
                s.pending = Some(("variant-release".into(), 0));
            }
            6 => {
                if self.variant != "release" {
                    s.step = 5;
                    s.next = Instant::now() + Duration::from_millis(800);
                    self.analysis_smoke = Some(s);
                    return;
                }
                if self.variant_open
                    || !clean(self)
                    || !self.language_status.contains("· release ·")
                    || !self
                        .model_status
                        .as_ref()
                        .is_some_and(|m| m.phase == language::ModelPhase::Applied)
                {
                    self.analysis_smoke = Some(s);
                    return;
                }
                s.snapshots.push(snap(self));
                shot = Some("04-release-analysis");
            }
            7 => {
                let at = s.original.find("BuildConfig.DEBUG").unwrap() + 2;
                self.edit.collapse(s.original[..at].chars().count());
                self.scroll_to_cursor = true;
                self.editor_ui.focus_editor = true;
                key(input, Key::F12, egui::Modifiers::NONE);
            }
            8 => {
                if !self
                    .doc
                    .path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("BuildConfig.java"))
                {
                    // Retry only after an actual empty definition response. Never
                    // queue another F12 while the first navigation is in flight.
                    if self.doc.path.as_ref() == Some(&s.source)
                        && self.status == "No definition found"
                    {
                        s.step = 7;
                        s.next = Instant::now() + Duration::from_millis(700);
                    }
                    self.analysis_smoke = Some(s);
                    return;
                }
                let path = self.doc.path.clone().unwrap();
                assert!(path.starts_with(self.root.join(".wex/analysis/release")));
                assert!(self.doc.rope().to_string().contains("DEBUG = false"));
                s.snapshots.push(snap(self));
                shot = Some("05-generated-release-definition");
            }
            9 => {
                for (p, h) in &s.protected {
                    assert_eq!(
                        blake3::hash(&std::fs::read(p).unwrap()).to_hex().as_str(),
                        h
                    );
                }
                assert!(!self.root.join(".wex/native").exists());
                for v in ["debug", "release"] {
                    assert!(
                        self.root
                            .join(".wex/analysis")
                            .join(v)
                            .join("current.json")
                            .is_file()
                    );
                }
                let value = json!({"result":"GUI_NATIVE_ANALYSIS_OK","renderer":"wgpu","snapshots":s.snapshots,"screenshots":s.shots,"source_hashes":s.protected,"disk_source_unchanged":true,"apk_built":false,"deployment_invoked":false,"gradle_invoked":false,"generated_definition":self.doc.path});
                std::fs::write(
                    s.dir.join("analysis-window.json"),
                    serde_json::to_vec_pretty(&value).unwrap(),
                )
                .unwrap();
                eprintln!(
                    "GUI_NATIVE_ANALYSIS_OK: first-project K2/R/BuildConfig; unsaved type error/Undo; real release picker/F12 DEBUG=false;5 native GPU frames; no APK/sign/Gradle/deploy"
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => unreachable!(),
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.next = Instant::now() + Duration::from_millis(350);
        self.analysis_smoke = Some(s);
    }
    pub(super) fn analysis_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.analysis_smoke else {
            return;
        };
        if let Some(name) = s.awaiting.clone() {
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let path = s.dir.join(format!("{name}.png"));
                    let bytes = image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect::<Vec<_>>();
                    image::save_buffer(
                        &path,
                        &bytes,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    s.shots
                        .push(json!({"path":path,"width":image.size[0],"height":image.size[1]}));
                    s.awaiting = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_analysis_probe_source_stays_in_declared_project_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("feature")).unwrap();
        for p in ["MainActivity.kt", "feature/Feature.kt", "Other.kt"] {
            std::fs::write(root.join(p), "fun sample()=1").unwrap();
        }
        let protected = vec![
            (root.join("MainActivity.kt"), String::new()),
            (root.join("feature/Feature.kt"), String::new()),
        ];
        assert_eq!(
            select_source(&root, &protected, None).unwrap(),
            root.join("MainActivity.kt")
        );
        assert_eq!(
            select_source(
                &root,
                &protected,
                Some(std::path::Path::new("feature/Feature.kt"))
            )
            .unwrap(),
            root.join("feature/Feature.kt")
        );
        for p in ["../Other.kt", "Other.kt", "/tmp/Outside.kt"] {
            assert!(select_source(&root, &protected, Some(std::path::Path::new(p))).is_err());
        }
    }
    #[test]
    fn empty_default_or_stale_diagnostics_are_not_semantic_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Main.kt"), "val answer=7").unwrap();
        let mut app = Workbench::new(tmp.path().to_owned(), egui::Context::default(), None, false);
        assert!(!clean(&app));
        let path = app.doc.path.clone().unwrap();
        app.diagnostic_snapshot = Some((path.clone(), app.doc.version));
        assert!(clean(&app));
        app.diagnostic_snapshot = Some((path, app.doc.version + 1));
        assert!(!clean(&app));
        app.diagnostic_snapshot = Some((tmp.path().join("Other.kt"), app.doc.version));
        assert!(!clean(&app));
    }
}
