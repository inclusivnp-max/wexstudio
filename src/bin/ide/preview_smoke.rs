//! Real native-window preview regression. No device, emulator image or disk source edit.
use super::*;
use egui::{PointerButton, Pos2};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};
pub(super) struct PreviewSmoke {
    dir: PathBuf,
    source: PathBuf,
    original: String,
    changed: String,
    protected: BTreeMap<PathBuf, String>,
    start: Instant,
    since: Instant,
    step: usize,
    pending: Option<(String, u8)>,
    release: Option<(Pos2, Instant)>,
    awaiting: Option<String>,
    shots: Vec<Value>,
    snapshots: BTreeMap<String, Value>,
    last_good: String,
    error_generation: u64,
    catalog_check: bool,
    case_source: PathBuf,
    case_original: String,
    chosen_id: String,
    scroll_until: Option<Instant>,
}
impl PreviewSmoke {
    pub fn new(dir: PathBuf, app: &mut Workbench) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("preview-window.json"));
        let source = app
            .files
            .iter()
            .find(|p| p.ends_with("CounterCard.kt"))
            .unwrap()
            .clone();
        let original = std::fs::read_to_string(&source).unwrap();
        assert!(original.contains("Wex Compose"));
        let changed = original.replace("Wex Compose", "Wex Snapshot");
        let protected = walkdir::WalkDir::new(&app.root)
            .into_iter()
            .filter_entry(|e| e.file_name() != ".wex")
            .map(Result::unwrap)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (
                    e.path().to_owned(),
                    blake3::hash(&std::fs::read(e.path()).unwrap())
                        .to_hex()
                        .to_string(),
                )
            })
            .collect();
        app.open_file(source.clone());
        app.edit.collapse(0);
        app.scroll_to_cursor = true;
        let catalog_check = std::env::args().nth(2).as_deref() == Some("--preview-catalog-smoke");
        let case_source = app
            .root
            .join("app/src/main/kotlin/dev/wex/compose/PreviewCases.kt");
        let case_original = if catalog_check {
            assert!(!app.root.join("wex.preview.json").exists());
            std::fs::read_to_string(&case_source).unwrap()
        } else {
            String::new()
        };
        Self {
            catalog_check,
            case_source,
            case_original,
            chosen_id: String::new(),
            scroll_until: None,
            dir,
            source,
            original,
            changed,
            protected,
            start: Instant::now(),
            since: Instant::now(),
            step: 0,
            pending: None,
            release: None,
            awaiting: None,
            shots: vec![],
            snapshots: BTreeMap::new(),
            last_good: String::new(),
            error_generation: 0,
        }
    }
    fn record(&mut self, app: &Workbench, label: &str) {
        let report = app.preview.report.as_ref().unwrap();
        let path = self.dir.join(format!("{label}-render.png"));
        std::fs::copy(&report.output, &path).unwrap();
        std::fs::write(
            path.with_extension("json"),
            serde_json::to_vec_pretty(report).unwrap(),
        )
        .unwrap();
        self.snapshots.insert(label.into(),json!({"report":report,"copied_image":path,"generation":app.preview.generation,"dirty":app.doc.dirty(),"version":app.doc.version,"stale":app.preview.stale}));
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
fn paste(app: &mut Workbench, input: &mut egui::RawInput, text: String) {
    app.editor_ui.focus_editor = true;
    key(
        input,
        Key::A,
        egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        },
    );
    input.events.push(egui::Event::Paste(text));
}
impl Workbench {
    pub(super) fn preview_smoke_input(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let Some(mut s) = self.preview_smoke.take() else {
            return;
        };
        assert!(
            s.start.elapsed() < Duration::from_secs(if s.catalog_check { 650 } else { 420 }),
            "preview timeout at {}: {}",
            s.step,
            self.preview.note
        );
        input.focused = true;
        input.modifiers = egui::Modifiers::NONE;
        input
            .events
            .retain(|e| matches!(e, egui::Event::Screenshot { .. }));
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
            self.preview_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if s.awaiting.is_none()
            && let Some((name, hover)) = s.pending.take()
        {
            if s.scroll_until.is_some_and(|until| Instant::now() < until) {
                s.pending = Some((name, 0));
                self.preview_smoke = Some(s);
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            let rect = *self
                .chrome
                .hits
                .get(&name)
                .unwrap_or_else(|| panic!("missing preview control {name}"));
            if name.starts_with("preview-entry-")
                && let Some(view) = self.chrome.hits.get("preview-catalog-viewport")
                && !(view.top()..=view.bottom()).contains(&rect.center().y)
            {
                input.events.push(egui::Event::PointerMoved(view.center()));
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(
                        0.,
                        if rect.center().y > view.center().y {
                            -100.
                        } else {
                            100.
                        },
                    ),
                    modifiers: Default::default(),
                });
                s.scroll_until = Some(Instant::now() + Duration::from_millis(250));
                s.pending = Some((name, 0));
                self.preview_smoke = Some(s);
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }
            let pos = if name.starts_with("preview-entry-") {
                rect.intersect(self.chrome.hits["preview-catalog-viewport"])
                    .center()
            } else {
                rect.center()
            };
            input.events.push(egui::Event::PointerMoved(pos));
            if hover < 2 {
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
            self.preview_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if s.awaiting.is_some() || s.start.elapsed() < Duration::from_secs(2) {
            self.preview_smoke = Some(s);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        macro_rules! wait {
            ($condition:expr) => {
                if !$condition {
                    assert!(
                        s.since.elapsed() < Duration::from_secs(100),
                        "preview step {}: {}",
                        s.step,
                        self.preview.note
                    );
                    self.preview_smoke = Some(s);
                    ctx.request_repaint_after(Duration::from_millis(30));
                    return;
                }
            };
        }
        let mut click = None;
        let mut dynamic_click = None;
        let mut shot = None;
        match s.step {
            0 => {
                click = Some("nav-preview");
            }
            1 => {
                wait!(
                    self.preview.open
                        && !self.preview.busy
                        && self.preview.report.is_some()
                        && !self.preview.stale
                );
                s.record(self, "01-original");
                s.last_good = self.preview.report.as_ref().unwrap().png_blake3.clone();
                shot = Some("01-original-window");
            }
            2 => {
                paste(self, input, s.changed.clone());
            }
            3 => {
                wait!(
                    self.doc.dirty()
                        && self.doc.rope().slice(..) == s.changed.as_str()
                        && self.preview.stale
                );
                click = Some("preview-live");
            }
            4 => {
                wait!(
                    self.preview.live
                        && !self.preview.busy
                        && !self.preview.stale
                        && self
                            .preview
                            .report
                            .as_ref()
                            .is_some_and(|r| !r.buffers.is_empty() && r.png_blake3 != s.last_good)
                );
                assert_eq!(self.doc.path.as_ref(), Some(&s.source));
                s.record(self, "02-unsaved");
                s.last_good = self.preview.report.as_ref().unwrap().png_blake3.clone();
                shot = Some("02-unsaved-window");
            }
            5 => {
                s.error_generation = self.preview.generation;
                paste(self, input, format!("{}\nfun broken( =\n", s.changed));
            }
            6 => {
                wait!(
                    self.preview.generation > s.error_generation + 1
                        && !self.preview.busy
                        && self.preview.stale
                        && self.preview.note.contains("failed")
                );
                assert_eq!(
                    self.preview.report.as_ref().unwrap().png_blake3,
                    s.last_good
                );
                assert!(self.preview.texture.is_some());
                s.snapshots.insert("error".into(),json!({"note":self.preview.note,"retained_png":s.last_good,"stale":self.preview.stale}));
                shot = Some("03-error-retains-window");
            }
            7 => {
                paste(self, input, s.changed.clone());
            }
            8 => {
                wait!(self.preview.busy && self.preview.note.contains("Kotlin K2"));
                click = Some("preview-cancel");
            }
            9 => {
                wait!(!self.preview.busy && self.preview.note.starts_with("Cancelled"));
                assert_eq!(
                    self.preview.report.as_ref().unwrap().png_blake3,
                    s.last_good
                );
                s.snapshots.insert("cancel".into(),json!({"note":self.preview.note,"generation":self.preview.generation,"retained_png":s.last_good}));
                shot = Some("04-cancelled-window");
                click = Some("preview-live");
            }
            10 => {
                wait!(!self.preview.live);
                // Restore three real editor transactions through its normal undo path.
                self.editor_ui.focus_editor = true;
                for _ in 0..3 {
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
            }
            11 => {
                wait!(!self.doc.dirty() && self.doc.rope().slice(..) == s.original.as_str());
                click = Some("preview-dark");
            }
            12 => {
                wait!(
                    !self.preview.busy
                        && !self.preview.stale
                        && self
                            .preview
                            .report
                            .as_ref()
                            .is_some_and(|r| r.entry.dark && r.renderer["night"] == true)
                );
                s.record(self, "05-dark");
                assert_ne!(
                    self.preview.report.as_ref().unwrap().png_blake3,
                    s.last_good
                );
                shot = Some("05-dark-window");
            }
            13 => {
                click = Some("preview-close");
            }
            14 => {
                wait!(!self.preview.open && !self.preview.busy);
                click = Some("nav-preview");
            }
            15 => {
                wait!(self.preview.open && !self.preview.busy && !self.preview.stale);
                shot = Some("06-reopened-window");
            }
            16 if s.catalog_check => {
                click = Some("preview-picker");
            }
            17 if s.catalog_check => {
                let item = self
                    .preview
                    .catalog
                    .items
                    .iter()
                    .find(|i| {
                        i.entry.name == "MetaNight" && i.entry.composable.ends_with(".MultiScene")
                    })
                    .unwrap();
                s.chosen_id = item.id.clone();
                let hit = format!("preview-entry-{}", item.id);
                wait!(self.chrome.hits.contains_key(&hit));
                dynamic_click = Some(hit);
            }
            18 if s.catalog_check => {
                wait!(
                    !self.preview.busy
                        && !self.preview.stale
                        && self.preview.report.as_ref().is_some_and(|r| r.selected_id
                            == s.chosen_id
                            && r.renderer["locale"] == "fr-FR")
                );
                assert!(self.preview.report.as_ref().unwrap().entry.dark);
                assert_eq!(self.preview.report.as_ref().unwrap().entry.font_scale, 1.25);
                s.record(self, "07-annotation-night");
                shot = Some("07-annotation-night-window");
            }
            19 if s.catalog_check => {
                click = Some("preview-picker");
            }
            20 if s.catalog_check => {
                let item = self
                    .preview
                    .catalog
                    .items
                    .iter()
                    .find(|i| i.entry.name == "Landscape")
                    .unwrap();
                s.chosen_id = item.id.clone();
                let hit = format!("preview-entry-{}", item.id);
                wait!(self.chrome.hits.contains_key(&hit));
                dynamic_click = Some(hit);
            }
            21 if s.catalog_check => {
                wait!(
                    !self.preview.busy
                        && !self.preview.stale
                        && self.preview.report.as_ref().is_some_and(|r| r.selected_id
                            == s.chosen_id
                            && r.entry.width_dp == 640)
                );
                assert_eq!(
                    self.preview.report.as_ref().unwrap().renderer["height"],
                    480
                );
                s.record(self, "08-annotation-landscape");
                shot = Some("08-annotation-landscape-window");
                if !self.preview.live {
                    click = Some("preview-live");
                }
            }
            22 if s.catalog_check => {
                wait!(self.preview.live);
                self.open_file(s.case_source.clone());
                wait!(self.doc.path.as_ref() == Some(&s.case_source));
                self.edit.collapse(0);
                self.scroll_to_cursor = true;
                paste(
                    self,
                    input,
                    s.case_original
                        .replace("widthDp = HEIGHT * 2", "widthDp = 180"),
                );
            }
            23 if s.catalog_check => {
                wait!(
                    self.doc.dirty()
                        && !self.preview.busy
                        && !self.preview.stale
                        && self
                            .preview
                            .report
                            .as_ref()
                            .is_some_and(|r| r.selected_id == s.chosen_id
                                && r.entry.width_dp == 180
                                && !r.buffers.is_empty())
                );
                assert_eq!(self.preview.report.as_ref().unwrap().renderer["width"], 360);
                s.record(self, "09-unsaved-annotation");
                shot = Some("09-unsaved-annotation-window");
            }
            24 if s.catalog_check => {
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
            25 if s.catalog_check => {
                wait!(
                    !self.doc.dirty()
                        && !self.preview.busy
                        && !self.preview.stale
                        && self
                            .preview
                            .report
                            .as_ref()
                            .is_some_and(|r| r.entry.width_dp == 640)
                );
                assert_eq!(self.preview.catalog.items.len(), 15);
                assert_eq!(
                    self.preview
                        .catalog
                        .items
                        .iter()
                        .filter(|i| i.problem.is_some())
                        .count(),
                    3
                );
                click = Some("preview-issues");
            }
            26 if s.catalog_check => {
                shot = Some("10-unavailable-scenes-window");
            }
            16 | 27 => {
                assert!(!self.running && !self.building && self.last_run.is_none());
                for (p, h) in &s.protected {
                    assert_eq!(
                        blake3::hash(&std::fs::read(p).unwrap()).to_hex().as_str(),
                        h
                    );
                }
                let jobs = self.root.join(".wex/preview-jobs");
                wait!(std::fs::read_dir(jobs).unwrap().next().is_none());
                let result = if s.catalog_check {
                    "GUI_HEADLESS_PREVIEW_CATALOG_OK"
                } else {
                    "GUI_HEADLESS_PREVIEW_OK"
                };
                let report = json!({"result":result,"catalog":self.preview.catalog,"selected_id":self.preview.selected,"snapshots":s.snapshots,"screenshots":s.shots,"disk_source_unchanged":true,"source_hashes":s.protected,"deployment_invoked":false,"emulator_used":false,"snapshot_jobs_cleaned":true,"input":"native winit/wgpu; real RawInput buttons + Ctrl+A/Paste; live unsaved compile + actual Layoutlib","elapsed_ms":s.start.elapsed().as_millis()});
                std::fs::write(
                    s.dir.join("preview-window.json"),
                    serde_json::to_vec_pretty(&report).unwrap(),
                )
                .unwrap();
                if s.catalog_check {
                    eprintln!(
                        "GUI_HEADLESS_PREVIEW_CATALOG_OK: 15 compiled annotations; real dropdown night/French/landscape; unsaved annotation width + stable selection; 3 unavailable; original live/error/Cancel/reopen; 10 GPU screenshots; no config/device/Gradle/disk edits"
                    );
                } else {
                    eprintln!(
                        "GUI_HEADLESS_PREVIEW_OK: original/unsaved/dark Compose; live debounce; compiler error retains image; cancel/reopen; 6 GPU screenshots; disk unchanged; no device/Gradle"
                    );
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        if let Some(name) = dynamic_click {
            s.pending = Some((name, 0));
        }
        if let Some(name) = click {
            s.pending = Some((name.into(), 0));
        }
        if let Some(name) = shot {
            s.awaiting = Some(name.into());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        s.step += 1;
        s.since = Instant::now();
        self.preview_smoke = Some(s);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub(super) fn preview_smoke_capture(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.preview_smoke else {
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
