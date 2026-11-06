//! Lazy private font/atlas worker: never locks the window's Fonts or publishes stale jobs.
use crate::editor_mesh::{Cache, Stats};
use eframe::egui::{
    self, Color32, FontDefinitions, Pos2, Rect, Vec2,
    epaint::{
        Galley, Mesh,
        text::{Fonts, LayoutJob},
        textures::TextureOptions,
    },
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
        mpsc,
    },
    thread,
    time::Instant,
};
#[derive(Clone)]
pub struct FontConfig {
    pub definitions: FontDefinitions,
    pub max_texture_side: usize,
    pub pixels_per_point: f32,
    pub alpha: egui::epaint::AlphaFromCoverage,
}
impl FontConfig {
    pub fn current(ctx: &egui::Context) -> Self {
        let pixels_per_point = ctx.pixels_per_point();
        let alpha = ctx.style().visuals.text_alpha_from_coverage;
        ctx.fonts(|f| Self {
            definitions: f.definitions().clone(),
            max_texture_side: f.max_texture_side(),
            pixels_per_point,
            alpha,
        })
    }
    pub fn same(&self, o: &Self) -> bool {
        self.max_texture_side == o.max_texture_side
            && self.pixels_per_point.to_bits() == o.pixels_per_point.to_bits()
            && self.alpha == o.alpha
            && self.definitions.families == o.definitions.families
            && self.definitions.font_data.len() == o.definitions.font_data.len()
            && self.definitions.font_data.iter().all(|(k, v)| {
                o.definitions
                    .font_data
                    .get(k)
                    .is_some_and(|w| Arc::ptr_eq(v, w))
            })
    }
}
pub struct Layout {
    galley: Option<Arc<Galley>>,
    image: Option<Arc<egui::ColorImage>>,
    index: Mutex<Cache>,
    pub elapsed_us: u128,
    pub worker_thread: String,
}
impl Layout {
    pub fn galley(&self) -> &Arc<Galley> {
        self.galley.as_ref().unwrap()
    }
    pub fn image(&self) -> &Arc<egui::ColorImage> {
        self.image.as_ref().unwrap()
    }
    pub fn hit(&self, p: Vec2) -> egui::text::CCursor {
        self.index.lock().unwrap().cursor_from_pos(self.galley(), p)
    }
    pub fn mesh(
        &self,
        pos: Pos2,
        clip: Rect,
        fallback: Color32,
        texture: egui::TextureId,
    ) -> (Mesh, Stats) {
        let (mut m, s) =
            self.index
                .lock()
                .unwrap()
                .mesh(self.galley(), pos, clip, self.image().size, fallback);
        m.texture_id = texture;
        (m, s)
    }
    pub fn upload(&self, ctx: &egui::Context, name: String) -> egui::TextureHandle {
        ctx.load_texture(
            name,
            egui::ImageData::Color(self.image().clone()),
            TextureOptions::LINEAR,
        )
    }
}
impl Drop for Layout {
    fn drop(&mut self) {
        crate::document_retirement::retire(crate::document_retirement::Payload::Layout {
            galley: self.galley.take().unwrap(),
            image: self.image.take().unwrap(),
        });
    }
}
struct Job {
    id: u64,
    generation: u64,
    font: FontConfig,
    make: Box<dyn FnOnce() -> LayoutJob + Send>,
}
pub struct Completed {
    pub id: u64,
    pub generation: u64,
    pub layout: Arc<Layout>,
}
struct Shared {
    queue: Mutex<VecDeque<Job>>,
    changed: Condvar,
    stop: AtomicBool,
    generation: AtomicU64,
    active: AtomicU64,
    completed: AtomicU64,
    superseded: AtomicU64,
}
#[derive(Debug, serde::Serialize)]
pub struct Metrics {
    pub started: bool,
    pub active: u64,
    pub completed: u64,
    pub superseded: u64,
}
pub struct Worker {
    shared: Arc<Shared>,
    rx: Option<mpsc::Receiver<Completed>>,
    started: bool,
    next: u64,
}
impl Default for Worker {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(VecDeque::new()),
                changed: Condvar::new(),
                stop: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                active: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                superseded: AtomicU64::new(0),
            }),
            rx: None,
            started: false,
            next: 0,
        }
    }
}
impl Worker {
    pub fn metrics(&self) -> Metrics {
        Metrics {
            started: self.started,
            active: self.shared.active.load(SeqCst),
            completed: self.shared.completed.load(SeqCst),
            superseded: self.shared.superseded.load(SeqCst),
        }
    }
    pub fn invalidate(&self) -> u64 {
        let n = self.shared.generation.fetch_add(1, SeqCst) + 1;
        self.shared.queue.lock().unwrap().clear();
        self.shared.changed.notify_one();
        n
    }
    pub fn cancel(&self, id: u64) {
        self.shared.queue.lock().unwrap().retain(|j| j.id != id);
    }
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(SeqCst)
    }
    fn start(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<()> {
        if self.started {
            return Ok(());
        }
        let s = self.shared.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        thread::Builder::new()
            .name("wex-long-line-layout".into())
            .spawn(move || {
                let mut fonts: Option<(FontConfig, Fonts)> = None;
                let mut font_generation = 0;
                loop {
                    let job = {
                        let mut q = s.queue.lock().unwrap();
                        while q.is_empty()
                            && !s.stop.load(SeqCst)
                            && s.generation.load(SeqCst) == font_generation
                        {
                            q = s.changed.wait(q).unwrap();
                        }
                        if s.stop.load(SeqCst) {
                            break;
                        }
                        let generation = s.generation.load(SeqCst);
                        if generation != font_generation {
                            drop(q);
                            // Retain rasterized faces/atlas, but release obsolete Galleys
                            // on this worker, even when the new view has only short rows.
                            if let Some((config, f)) = &mut fonts {
                                f.begin_pass(config.max_texture_side, config.alpha);
                                f.begin_pass(config.max_texture_side, config.alpha);
                            }
                            font_generation = generation;
                            continue;
                        }
                        q.pop_front().unwrap()
                    };
                    if job.generation != s.generation.load(SeqCst) {
                        s.superseded.fetch_add(1, SeqCst);
                        continue;
                    }
                    s.active.store(job.id, SeqCst);
                    wake();
                    let started = Instant::now();
                    if fonts.as_ref().is_none_or(|(c, _)| !c.same(&job.font)) {
                        fonts = Some((
                            job.font.clone(),
                            Fonts::new(
                                job.font.max_texture_side,
                                job.font.alpha,
                                job.font.definitions.clone(),
                            ),
                        ));
                    }
                    let f = &mut fonts.as_mut().unwrap().1;
                    f.begin_pass(job.font.max_texture_side, job.font.alpha);
                    let g = f
                        .with_pixels_per_point(job.font.pixels_per_point)
                        .layout_job((job.make)());
                    let mut index = Cache::default();
                    index.prepare(&g);
                    let image = Arc::new(f.image());
                    let _ = f.font_image_delta();
                    let layout = Arc::new(Layout {
                        galley: Some(g),
                        image: Some(image),
                        index: Mutex::new(index),
                        elapsed_us: started.elapsed().as_micros(),
                        worker_thread: format!("{:?}", thread::current().id()),
                    });
                    s.active.store(0, SeqCst);
                    if job.generation != s.generation.load(SeqCst) || s.stop.load(SeqCst) {
                        s.superseded.fetch_add(1, SeqCst);
                        drop(layout);
                        wake();
                        continue;
                    }
                    s.completed.fetch_add(1, SeqCst);
                    if tx
                        .send(Completed {
                            id: job.id,
                            generation: job.generation,
                            layout,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            })?;
        self.rx = Some(rx);
        self.started = true;
        Ok(())
    }
    pub fn request(
        &mut self,
        font: FontConfig,
        make: impl FnOnce() -> LayoutJob + Send + 'static,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<u64> {
        self.start(wake)?;
        self.next += 1;
        let id = self.next;
        let generation = self.generation();
        let mut q = self.shared.queue.lock().unwrap();
        if q.len() >= 64 {
            return Err(std::io::Error::other("Long-line layout queue full · Retry"));
        }
        q.push_back(Job {
            id,
            generation,
            font,
            make: Box::new(make),
        });
        drop(q);
        self.shared.changed.notify_one();
        Ok(id)
    }
    pub fn poll(&self) -> Option<Completed> {
        self.rx.as_ref()?.try_recv().ok()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, SeqCst);
        self.shared.queue.lock().unwrap().clear();
        self.rx.take();
        self.shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn config(ppp: f32) -> FontConfig {
        FontConfig {
            definitions: FontDefinitions::default(),
            max_texture_side: 8192,
            pixels_per_point: ppp,
            alpha: egui::epaint::AlphaFromCoverage::DARK_MODE_DEFAULT,
        }
    }
    fn job(text: String) -> LayoutJob {
        let mut j = LayoutJob::default();
        j.append(
            &text,
            0.,
            egui::TextFormat {
                font_id: egui::FontId::monospace(16.),
                color: Color32::LIGHT_GREEN,
                ..Default::default()
            },
        );
        j.wrap.max_width = f32::INFINITY;
        j
    }
    fn wait(w: &Worker) -> Completed {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = w.poll() {
                return e;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn pixels(image: &egui::ColorImage, g: &egui::epaint::text::Glyph) -> Vec<Color32> {
        let u = g.uv_rect;
        let mut p = vec![];
        for y in u.min[1]..u.max[1] {
            for x in u.min[0]..u.max[0] {
                p.push(image.pixels[y as usize * image.size[0] + x as usize]);
            }
        }
        p
    }
    #[test]
    fn private_worker_atlas_has_exact_stock_positions_and_glyph_pixels_at_fractional_dpi() {
        for ppp in [1., 1.25, 2.] {
            let c = config(ppp);
            let mut f = Fonts::new(c.max_texture_side, c.alpha, c.definitions.clone());
            f.with_pixels_per_point(ppp)
                .layout_job(job("Force a different atlas allocation: QWX😀ё".into()));
            let mut j = job("abc\tЖ😀e\u{301} 012345".repeat(200));
            j.sections[0].format.italics = true;
            j.sections[0].format.background = Color32::DARK_BLUE;
            j.sections[0].format.underline = egui::Stroke::new(1.5_f32, Color32::YELLOW);
            let original = f.with_pixels_per_point(ppp).layout_job(j.clone());
            let image = f.image();
            let mut w = Worker::default();
            w.invalidate();
            w.request(c, move || j, Arc::new(|| {})).unwrap();
            let r = wait(&w);
            assert_ne!(
                r.layout.worker_thread,
                format!("{:?}", thread::current().id())
            );
            assert_eq!(r.layout.galley().size(), original.size());
            for (a, b) in r.layout.galley().rows[0]
                .glyphs
                .iter()
                .zip(&original.rows[0].glyphs)
            {
                assert_eq!(
                    (
                        a.chr,
                        a.pos,
                        a.advance_width,
                        a.uv_rect.offset,
                        a.uv_rect.size
                    ),
                    (
                        b.chr,
                        b.pos,
                        b.advance_width,
                        b.uv_rect.offset,
                        b.uv_rect.size
                    )
                );
                assert_eq!(pixels(r.layout.image(), a), pixels(&image, b));
            }
            let p = Vec2::new(original.size().x - 31., 10.);
            assert_eq!(r.layout.hit(p), original.cursor_from_pos(p));
            let (_, s) = r.layout.mesh(
                Pos2::ZERO,
                Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 40.)),
                Color32::WHITE,
                egui::TextureId::Managed(17),
            );
            assert_eq!(s.newly_indexed_glyphs, 0);
        }
    }
    #[test]
    fn real_in_flight_million_char_shape_is_superseded_without_publishing_old_generation() {
        let mut w = Worker::default();
        w.invalidate();
        let old = w
            .request(config(1.), || job("x".repeat(1000000)), Arc::new(|| {}))
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while w.metrics().active != old {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        let generation = w.invalidate();
        let current = w
            .request(config(1.25), || job("latest Ж😀".into()), Arc::new(|| {}))
            .unwrap();
        loop {
            let r = wait(&w);
            if r.generation != generation {
                continue;
            }
            assert_eq!(r.id, current);
            assert_eq!(r.layout.galley().job.text, "latest Ж😀");
            break;
        }
        assert!(w.metrics().superseded > 0);
        assert_eq!(w.metrics().active, 0);
    }
    #[test]
    fn bounded_queue_reports_retry_and_cancel_frees_capacity_without_joining_worker() {
        let mut w = Worker::default();
        w.invalidate();
        let (tx, rx) = mpsc::channel();
        let first = w
            .request(
                config(1.),
                move || {
                    rx.recv().unwrap();
                    job("gate".into())
                },
                Arc::new(|| {}),
            )
            .unwrap();
        while w.metrics().active != first {
            thread::yield_now();
        }
        let mut ids = vec![];
        for _ in 0..64 {
            ids.push(
                w.request(config(1.), || job("queued".into()), Arc::new(|| {}))
                    .unwrap(),
            );
        }
        assert!(
            w.request(config(1.), || job("overflow".into()), Arc::new(|| {}))
                .unwrap_err()
                .to_string()
                .contains("Retry")
        );
        w.cancel(ids[0]);
        w.request(config(1.), || job("replacement".into()), Arc::new(|| {}))
            .unwrap();
        drop(w);
        tx.send(()).unwrap();
    }
    #[test]
    fn font_configuration_fences_dpi_alpha_size_families_and_face_identity() {
        let a = config(1.);
        assert!(a.same(&a.clone()));
        let mut b = a.clone();
        b.pixels_per_point = 1.25;
        assert!(!a.same(&b));
        b = a.clone();
        b.alpha = egui::epaint::AlphaFromCoverage::LIGHT_MODE_DEFAULT;
        assert!(!a.same(&b));
        b = a.clone();
        b.max_texture_side = 4096;
        assert!(!a.same(&b));
        b = a.clone();
        b.definitions
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .reverse();
        assert!(!a.same(&b));
        b = a.clone();
        let key = b.definitions.font_data.keys().next().unwrap().clone();
        let data = (*b.definitions.font_data[&key]).clone();
        b.definitions.font_data.insert(key, Arc::new(data));
        assert!(!a.same(&b));
    }
}
