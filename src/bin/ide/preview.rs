//! Expressive neighboring preview surface; all compiler/JNI work is in background processes.
use super::*;
use std::time::Instant;
use wexandroidstudio::{
    material::{self as m, ButtonKind as Kind, Icon},
    preview,
};
pub(super) struct RenderedPreview {
    pub report: preview::Report,
    pub image: egui::ColorImage,
}
fn decode(report: preview::Report) -> anyhow::Result<Box<RenderedPreview>> {
    let image = image::load_from_memory(&std::fs::read(&report.output)?)?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(Box::new(RenderedPreview {
        report,
        image: egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
    }))
}
#[derive(Default)]
pub(super) struct PreviewUi {
    pub open: bool,
    pub live: bool,
    pub busy: bool,
    pub stale: bool,
    pub generation: u64,
    pub note: String,
    pub catalog: wexandroidstudio::preview_catalog::Catalog,
    pub selected: String,
    pub dark: Option<bool>,
    pub report: Option<preview::Report>,
    pub texture: Option<egui::TextureHandle>,
    pub task: Option<tokio::task::JoinHandle<()>>,
    signature: Vec<(PathBuf, u64, bool)>,
    changed: Option<Instant>,
    auto_pending: bool,
}
impl Drop for PreviewUi {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}
impl PreviewUi {
    pub(super) fn invalidate(&mut self) {
        self.generation += 1;
        self.stale = true;
        self.busy = false;
        self.auto_pending = true;
        self.changed = Some(Instant::now());
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
    pub fn accepts(&self, id: u64) -> bool {
        self.generation == id
    }
}
impl Workbench {
    fn preview_buffers(&self) -> Vec<(PathBuf, ropey::Rope, i32)> {
        self.parked
            .iter()
            .map(|(p, (d, _))| (p, d))
            .chain(self.doc.path.as_ref().map(|p| (p, &self.doc)))
            .filter(|(p, d)| {
                d.dirty()
                    && p.extension()
                        .is_some_and(|e| e == "kt" || e == "java" || e == "xml")
            })
            .map(|(p, d)| (p.clone(), d.rope().clone(), d.version))
            .collect()
    }
    pub(super) fn toggle_preview(&mut self) {
        self.preview.open = !self.preview.open;
        if self.preview.open {
            if self.preview.report.is_none() || self.preview.stale {
                self.start_preview();
            }
        } else if self.preview.busy {
            self.cancel_preview();
        }
    }
    pub(super) fn cancel_preview(&mut self) {
        self.preview.invalidate();
        self.preview.auto_pending = false;
        self.preview.note = "Cancelled · last preview retained".into();
    }
    pub(super) fn start_preview(&mut self) {
        if self.closing_requested() || self.file_ui.open || self.replace_blocked() {
            return;
        }
        self.preview.invalidate();
        self.preview.auto_pending = false;
        let selection = preview::Selection {
            id: (!self.preview.selected.is_empty()).then(|| self.preview.selected.clone()),
            dark: self.preview.dark,
        };
        self.preview.busy = true;
        self.preview.auto_pending = false;
        self.preview.note = "Compiling and discovering @Preview…".into();
        let id = self.preview.generation;
        let root = self.root.clone();
        let buffers = self.preview_buffers();
        let output = root
            .join(".wex/preview-images")
            .join(format!("{}-{id}.png", std::process::id()));
        let tx = self.tx.clone();
        let ctx = self.context.clone();
        self.preview.task = Some(self.runtime.spawn(async move {
            let buffers = tokio::task::spawn_blocking(move || {
                buffers
                    .into_iter()
                    .map(|(path, text, version)| preview::Buffer {
                        path,
                        text: text.to_string(),
                        version,
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .expect("preview snapshot worker");
            let result = preview::render_selected(
                &root,
                selection,
                buffers,
                &output,
                &|e| {
                    if let RunEvent::Phase(note) = e {
                        let _ = tx.try_send(Update::PreviewPhase(id, note));
                        ctx.request_repaint();
                    }
                },
                &|catalog| {
                    let _ = tx.try_send(Update::PreviewCatalog(id, Box::new(catalog)));
                    ctx.request_repaint();
                },
            )
            .await;
            let result = match result {
                Ok(report) => tokio::task::spawn_blocking(move || decode(report))
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|r| r),
                Err(error) => Err(error),
            }
            .map_err(|e| format!("{e:#}"));
            let _ = tokio::task::spawn_blocking(move || tx.send(Update::PreviewReady(id, result)))
                .await;
            ctx.request_repaint();
        }));
    }
    pub(super) fn preview_catalog(
        &mut self,
        id: u64,
        catalog: wexandroidstudio::preview_catalog::Catalog,
    ) {
        if self.preview.accepts(id) {
            self.preview.catalog = catalog;
        }
    }
    pub(super) fn preview_ready(&mut self, id: u64, result: Result<Box<RenderedPreview>, String>) {
        if !self.preview.accepts(id) {
            if let Ok(ready) = result {
                let _ = std::fs::remove_file(&ready.report.output);
                let _ = std::fs::remove_file(ready.report.output.with_extension("json"));
            }
            return;
        }
        self.preview.task.take();
        self.preview.busy = false;
        match result {
            Ok(ready) => {
                self.preview.catalog = ready.report.catalog.clone();
                self.preview.selected = ready.report.selected_id.clone();
                self.preview.texture = Some(self.context.load_texture(
                    "headless-compose-preview",
                    ready.image,
                    egui::TextureOptions::LINEAR,
                ));
                self.preview.report = Some(ready.report);
                self.preview.stale = false;
                self.preview.note = "Layoutlib · headless · current snapshot".into();
            }
            Err(e) => {
                self.preview.note = e;
                self.preview.stale = true;
            }
        }
    }
    pub(super) fn preview_step(&mut self) {
        let mut signature = self
            .parked
            .iter()
            .map(|(p, (d, _))| (p.clone(), d.revision, d.dirty()))
            .collect::<Vec<_>>();
        if let Some(p) = &self.doc.path {
            signature.push((p.clone(), self.doc.revision, self.doc.dirty()));
        }
        signature.sort_by(|a, b| a.0.cmp(&b.0));
        let changed = signature.iter().any(|(p, r, d)| {
            self.preview
                .signature
                .iter()
                .find(|(old, _, _)| old == p)
                .map_or(*d, |(_, old_r, old_d)| old_r != r || old_d != d)
        }) || self
            .preview
            .signature
            .iter()
            .any(|(p, _, dirty)| *dirty && !signature.iter().any(|(next, _, _)| next == p));
        self.preview.signature = signature;
        if changed {
            self.preview.invalidate();
        }
        if self.preview.open
            && self.preview.live
            && self.preview.auto_pending
            && !self.preview.busy
            && self
                .preview
                .changed
                .is_some_and(|t| t.elapsed() > Duration::from_millis(800))
        {
            self.start_preview();
        }
    }
    pub(super) fn preview_panel(&mut self, ctx: &egui::Context) {
        let target = if self.preview.open {
            (ctx.available_rect().width() * 0.45).clamp(220., 360.)
        } else {
            0.
        };
        let width = self
            .chrome
            .spatial(ctx, "preview-width", target, 0., self.reduced_motion)
            .clamp(0., 360.);
        if width < 2. {
            return;
        }
        egui::SidePanel::right("compose-preview")
            .exact_width(width).resizable(false)
            .frame(egui::Frame::new().fill(m::SURFACE).inner_margin(8))
            .show(ctx, |ui| {
                m::card().inner_margin(12).show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Preview").size(22.).color(m::TEXT));
                        if self.chrome.button(ui,"preview-close","",Icon::Close,Kind::Ghost,true,32.,self.reduced_motion).clicked() { self.toggle_preview(); }
                    });
                    ui.label(RichText::new("Compose · Layoutlib · debug").color(m::PRIMARY).size(12.));
                    let previous=self.preview.selected.clone();
                    let selected=self.preview.catalog.items.iter().find(|i|i.id==previous).map(|i|i.entry.name.as_str()).unwrap_or(if previous.is_empty(){"Choose preview"}else{"Selection missing"});
                    let picker=egui::ComboBox::from_id_salt("preview-entry")
                        .selected_text(selected)
                        .width((width-48.).max(80.))
                        .show_ui(ui,|ui| {
                            let mut viewport=ui.clip_rect();let content=ui.max_rect();viewport.min.x=viewport.min.x.max(content.min.x);viewport.max.x=viewport.max.x.min(content.max.x);
                            self.chrome.hits.insert("preview-catalog-viewport".into(),viewport);
                            for item in &self.preview.catalog.items {
                                let response=ui.add_enabled_ui(item.problem.is_none(),|ui|ui.add_sized([ui.available_width(),40.],egui::Button::selectable(self.preview.selected==item.id,item.label()).wrap_mode(egui::TextWrapMode::Truncate))).inner;
                                self.chrome.hits.insert(format!("preview-entry-{}",item.id),response.rect);
                                if response.clicked(){self.preview.selected=item.id.clone();}
                                if let Some(problem)=&item.problem {response.on_hover_text_passive(problem);}else{response.on_hover_text_passive(format!("{}:{} · {}",item.source_file,item.line,item.origin));}
                            }
                        });
                    self.chrome.hits.insert("preview-picker".into(),picker.response.rect);
                    if previous!=self.preview.selected { self.preview.dark=None; self.start_preview(); }
                    if let Some(item)=self.preview.catalog.items.iter().find(|i|i.id==self.preview.selected) {
                        ui.label(RichText::new(format!("{} · {}:{}",item.origin,item.source_file,item.line)).size(10.).color(m::MUTED));
                    }
                    let unavailable=self.preview.catalog.items.iter().filter(|i|i.problem.is_some()).count();
                    if unavailable>0||!self.preview.catalog.issues.is_empty(){
                        let issues=ui.collapsing(format!("{} unavailable · {} discovery issues",unavailable,self.preview.catalog.issues.len()),|ui|{
                            for item in &self.preview.catalog.items {if let Some(problem)=&item.problem {ui.colored_label(m::WARNING,format!("{}: {problem}",item.entry.name));}}
                            for issue in &self.preview.catalog.issues {ui.colored_label(m::WARNING,format!("{}: {}",issue.member,issue.message));}
                        });
                        self.chrome.hits.insert("preview-issues".into(),issues.header_response.rect);
                    }
                    ui.horizontal_wrapped(|ui| {
                        if self.chrome.button(ui,"preview-refresh","Refresh",Icon::Refresh,Kind::Filled,true,104.,self.reduced_motion).clicked() {self.start_preview();}
                        if self.chrome.button(ui,"preview-cancel","",Icon::Close,Kind::Tonal,self.preview.busy,36.,self.reduced_motion).clicked() {self.cancel_preview();}
                        let mut dark=self.preview.dark.unwrap_or_else(||self.preview.catalog.items.iter().find(|e|e.id==self.preview.selected).is_some_and(|e|e.entry.dark));
                        let d=ui.checkbox(&mut dark,"Dark");self.chrome.hits.insert("preview-dark".into(),d.rect);
                        if d.changed() {self.preview.dark=Some(dark);self.start_preview();}
                        let live=ui.checkbox(&mut self.preview.live,"Live");self.chrome.hits.insert("preview-live".into(),live.rect);
                        if live.changed() && self.preview.live && self.preview.stale {self.preview.auto_pending=true; self.preview.changed=Some(Instant::now());}
                    });
                    if self.preview.stale && self.preview.texture.is_some() {ui.colored_label(m::WARNING,"Source changed · last good preview");}
                    ui.label(RichText::new(self.preview.note.chars().take(140).collect::<String>()).size(11.).color(if self.preview.stale&&!self.preview.busy{m::WARNING}else{m::MUTED})).on_hover_text_passive(&self.preview.note);
                    if self.preview.busy {let (r,_)=ui.allocate_exact_size(egui::vec2(28.,28.),egui::Sense::hover());m::loading(ui,r,self.reduced_motion);}
                    if let Some(texture)=&self.preview.texture {
                        let size=texture.size_vec2(); let ratio=(ui.available_width()/size.x).min(ui.available_height().max(10.)/size.y).min(1.);
                        let image=ui.add(egui::Image::new((texture.id(),size*ratio)).corner_radius(16));self.chrome.hits.insert("preview-image".into(),image.rect);
                    } else {ui.add_space(30.);ui.label("Refresh renders the compiled composable without a device. Unsaved Kotlin/Java/XML buffers stay on disk unchanged.");}
                });
            });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, Workbench) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Main.kt"), "val x=1").unwrap();
        let ctx = egui::Context::default();
        m::install(&ctx);
        let a = Workbench::new(d.path().to_owned(), ctx, None, false);
        (d, a)
    }
    fn last_good(a: &mut Workbench) {
        a.preview.report = Some(preview::Report {
            result: "unit fixture".into(),
            entry: preview::Entry::default(),
            catalog: Default::default(),
            selected_id: String::new(),
            output: a.root.join("unit.png"),
            png_blake3: "last-good".into(),
            source_hashes: Default::default(),
            buffers: vec![],
            renderer: serde_json::json!({}),
            studio_build: "unit".into(),
            native_backend: "unit".into(),
            resource_blake3: "unit".into(),
            compilation_steps: vec![],
            sdk_cache: vec![],
            resource_cache: vec![],
            compile_elapsed_ms: 0,
            compose: None,
            disk_source_unchanged: true,
        });
        a.preview.texture = Some(a.context.load_texture(
            "unit",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::LINEAR,
        ));
    }
    #[test]
    fn catalog_popup_records_a_real_scroll_viewport_and_scrolls_to_offscreen_entries() {
        let (_d, mut a) = app();
        a.reduced_motion = true;
        a.preview.open = true;
        for n in 0..15 {
            a.preview
                .catalog
                .items
                .push(wexandroidstudio::preview_catalog::Item {
                    id: format!("row{n}"),
                    entry: preview::Entry {
                        name: format!("Scene {n}"),
                        composable: format!("example.very_long_namespace.deeply_nested_feature.PreviewShowcaseWithVeryLongFileName.Scene{n}"),
                        ..Default::default()
                    },
                    origin: "@Preview".into(),
                    group: "Group".into(),
                    source_file: "Foo.kt".into(),
                    line: 1,
                    descriptor: String::new(),
                    annotation_path: vec![],
                    parameters: serde_json::Value::Null,
                    problem: None,
                });
        }
        a.preview.selected = "row0".into();
        fn frame(a: &mut Workbench, events: Vec<egui::Event>) {
            let context = a.context.clone();
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900., 900.),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ctx| a.preview_panel(ctx),
            );
        }
        frame(&mut a, vec![]);
        let pos = a.chrome.hits["preview-picker"].center();
        frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            &mut a,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        frame(&mut a, vec![]);
        let view = a.chrome.hits["preview-catalog-viewport"];
        let old = a.chrome.hits["preview-entry-row5"];
        eprintln!("CATALOG_VIEWPORT_BEFORE: view={view:?}; row5={old:?}");
        frame(
            &mut a,
            vec![
                egui::Event::PointerMoved(view.center()),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0., -100.),
                    modifiers: Default::default(),
                },
            ],
        );
        for _ in 0..30 {
            frame(&mut a, vec![]);
        }
        let next_view = a.chrome.hits["preview-catalog-viewport"];
        assert!(
            (next_view.top() - view.top()).abs() < 0.1
                && (next_view.bottom() - view.bottom()).abs() < 0.1,
            "scrolling content max_rect must not shrink the viewport Y range"
        );
        let next = a.chrome.hits["preview-entry-row5"];
        eprintln!("CATALOG_VIEWPORT_AFTER: row5={next:?}");
        assert!(
            next.center().y < old.center().y,
            "popup did not scroll over recorded viewport"
        );
        assert!(
            next.right() <= view.right() + 1.,
            "long labels must not grow outside popup width"
        );
        let pos = next.intersect(view).center();
        frame(&mut a, vec![egui::Event::PointerMoved(pos)]);
        frame(
            &mut a,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        frame(
            &mut a,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert_eq!(a.preview.selected, "row5");
        a.cancel_preview();
    }
    #[test]
    fn stale_catalog_discovery_cannot_replace_current_selection_or_last_good_image() {
        let (_d, mut a) = app();
        last_good(&mut a);
        a.preview.generation = 4;
        a.preview.selected = "annotation:current".into();
        let current = wexandroidstudio::preview_catalog::Catalog {
            backend: "current".into(),
            ..Default::default()
        };
        a.preview_catalog(4, current);
        a.preview_catalog(
            3,
            wexandroidstudio::preview_catalog::Catalog {
                backend: "obsolete".into(),
                ..Default::default()
            },
        );
        assert_eq!(a.preview.catalog.backend, "current");
        assert_eq!(a.preview.selected, "annotation:current");
        assert_eq!(a.preview.report.as_ref().unwrap().png_blake3, "last-good");
    }
    #[test]
    fn catalog_survives_render_failure_and_cancellation_for_a_new_selection() {
        let (_d, mut a) = app();
        last_good(&mut a);
        a.preview.busy = true;
        a.preview.selected = "annotation:new".into();
        a.preview_catalog(
            0,
            wexandroidstudio::preview_catalog::Catalog {
                backend: "compiled".into(),
                ..Default::default()
            },
        );
        a.preview_ready(0, Err("Preview unavailable: parameter provider".into()));
        assert_eq!(a.preview.catalog.backend, "compiled");
        assert_eq!(a.preview.selected, "annotation:new");
        assert!(a.preview.stale && a.preview.texture.is_some());
        a.cancel_preview();
        assert_eq!(a.preview.catalog.backend, "compiled");
    }
    #[test]
    fn late_decoded_result_cannot_replace_current_texture_and_cleans_its_files() {
        let (_d, mut a) = app();
        last_good(&mut a);
        let mut old = a.preview.report.take().unwrap();
        old.png_blake3 = "obsolete".into();
        old.output = a.root.join("obsolete.png");
        std::fs::write(&old.output, "unit data").unwrap();
        std::fs::write(old.output.with_extension("json"), "{}").unwrap();
        let path = old.output.clone();
        last_good(&mut a);
        let texture = a.preview.texture.as_ref().unwrap().id();
        a.preview.generation = 2;
        a.preview_ready(
            1,
            Ok(Box::new(RenderedPreview {
                report: old,
                image: egui::ColorImage::filled([2, 2], egui::Color32::BLUE),
            })),
        );
        assert_eq!(a.preview.report.as_ref().unwrap().png_blake3, "last-good");
        assert_eq!(a.preview.texture.as_ref().unwrap().id(), texture);
        assert!(!path.exists() && !path.with_extension("json").exists());
    }
    #[test]
    fn corrupt_renderer_png_fails_background_decode_instead_of_publishing_a_texture() {
        let (_d, mut a) = app();
        last_good(&mut a);
        let report = a.preview.report.take().unwrap();
        std::fs::write(&report.output, "invalid PNG").unwrap();
        assert!(decode(report).is_err());
    }
    #[test]
    fn expressive_preview_controls_fit_compact_and_wide_workspaces() {
        for width in [760., 1000., 1440.] {
            let (_d, mut a) = app();
            last_good(&mut a);
            a.preview.open = true;
            a.reduced_motion = true;
            let ctx = a.context.clone();
            for _ in 0..4 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 560.),
                        )),
                        ..Default::default()
                    },
                    |ctx| a.draw_shell(ctx),
                );
            }
            for key in [
                "preview-refresh",
                "preview-cancel",
                "preview-dark",
                "preview-live",
                "preview-close",
                "nav-preview",
            ] {
                assert!(
                    ctx.content_rect().contains_rect(a.chrome.hits[key]),
                    "{key} clipped at {width}: {:?}",
                    a.chrome.hits[key]
                );
            }
        }
    }
    #[test]
    fn compiler_failure_and_cancel_keep_last_good_image_and_report() {
        let (_d, mut a) = app();
        last_good(&mut a);
        let texture = a.preview.texture.as_ref().unwrap().id();
        a.preview.busy = true;
        a.preview_ready(0, Err("compiler failed".into()));
        assert!(a.preview.stale && !a.preview.busy);
        assert_eq!(a.preview.report.as_ref().unwrap().png_blake3, "last-good");
        assert_eq!(a.preview.texture.as_ref().unwrap().id(), texture);
        a.cancel_preview();
        assert_eq!(a.preview.texture.as_ref().unwrap().id(), texture);
        assert!(!a.preview.auto_pending);
    }
    #[test]
    fn closing_current_preview_does_not_mark_it_stale_or_queue_a_rebuild() {
        let (_d, mut a) = app();
        last_good(&mut a);
        a.preview.open = true;
        a.toggle_preview();
        assert!(!a.preview.open && !a.preview.stale);
        assert_eq!(a.preview.generation, 0);
        a.toggle_preview();
        assert!(a.preview.open && !a.preview.busy && !a.preview.stale);
    }
    #[test]
    fn invalid_configuration_cancels_old_job_and_keeps_visible_image_stale() {
        let (_d, mut a) = app();
        last_good(&mut a);
        a.preview.busy = true;
        a.start_preview();
        assert!(a.preview.busy && a.preview.stale && a.preview.generation == 1);
        assert!(a.preview.texture.is_some());
        if let Some(task) = a.preview.task.take() {
            task.abort();
        }
        a.preview_ready(1, Err("invalid preview config".into()));
        assert!(!a.preview.busy && a.preview.stale && a.preview.texture.is_some());
        assert!(a.preview.note.contains("invalid preview config"));
        assert!(!a.preview.accepts(0));
    }
    #[test]
    fn opening_clean_document_does_not_cancel_an_active_preview() {
        let (d, mut a) = app();
        a.preview_step();
        a.preview.busy = true;
        let path = d.path().join("Other.kt");
        std::fs::write(&path, "val y=2").unwrap();
        a.open_file_wait(path);
        a.preview_step();
        assert!(a.preview.busy && a.preview.generation == 0);
    }
    #[test]
    fn stale_jobs_cannot_replace_newer_preview() {
        let mut state = PreviewUi::default();
        let old = state.generation;
        state.invalidate();
        assert!(!state.accepts(old));
        assert!(state.accepts(state.generation));
        assert!(state.stale && !state.busy);
    }
    #[test]
    fn cancelling_disables_automatic_retry_but_keeps_last_report() {
        let mut state = PreviewUi::default();
        state.busy = true;
        state.auto_pending = true;
        state.invalidate();
        state.auto_pending = false;
        assert!(!state.busy && !state.auto_pending);
    }
}
