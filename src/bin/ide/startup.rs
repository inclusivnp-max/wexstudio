//! Native first-frame policy: prefer Vulkan on Linux, retry full supported set only
//! after a genuine GPU initialization error BEFORE the app creator ever ran.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone)]
pub(super) struct Trace {
    started: std::time::Instant,
    marks: Arc<std::sync::Mutex<Vec<(String, u128)>>>,
    pub attempts: Arc<std::sync::atomic::AtomicUsize>,
    pub adapter: Arc<std::sync::Mutex<Option<String>>>,
}
impl Default for Trace {
    fn default() -> Self {
        Self::new(std::time::Instant::now())
    }
}
impl Trace {
    pub fn new(started: std::time::Instant) -> Self {
        Self {
            started,
            marks: Default::default(),
            attempts: Default::default(),
            adapter: Default::default(),
        }
    }
    pub fn mark(&self, label: &str) {
        self.marks
            .lock()
            .unwrap()
            .push((label.into(), self.started.elapsed().as_micros()));
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({"marks_us":*self.marks.lock().unwrap(),"gpu_attempts":self.attempts.load(Ordering::Relaxed),"adapter":*self.adapter.lock().unwrap()})
    }
}
pub(super) fn run(
    options: eframe::NativeOptions,
    trace: Trace,
    create: impl Fn(&eframe::CreationContext<'_>) -> eframe::Result<Box<dyn eframe::App>> + 'static,
) -> eframe::Result {
    let explicit = wgpu::Backends::from_env().is_some();
    let prefer = cfg!(target_os = "linux") && !explicit;
    // The real host retry test requests GL first; the second attempt remains actual default wgpu.
    let retry_test = std::env::args().nth(1).as_deref() == Some("--startup-retry-smoke");
    let created = Arc::new(AtomicBool::new(false));
    for attempt in 0..if prefer || retry_test { 2 } else { 1 } {
        let mut opts = options.clone();
        if attempt == 0
            && (prefer || retry_test)
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) =
                &mut opts.wgpu_options.wgpu_setup
        {
            setup.instance_descriptor.backends = if retry_test {
                wgpu::Backends::GL
            } else {
                wgpu::Backends::VULKAN
            };
        }
        trace.attempts.fetch_add(1, Ordering::Relaxed);
        trace.mark("gpu_attempt_start");
        let started = created.clone();
        let traced = trace.clone();
        let factory = &create;
        let result = eframe::run_native(
            "WexAndroidStudio",
            opts,
            Box::new(move |cc| {
                started.store(true, Ordering::Relaxed);
                traced.mark("gpu_ready");
                if let Some(state) = &cc.wgpu_render_state {
                    *traced.adapter.lock().unwrap() =
                        Some(format!("{:?}", state.adapter.get_info()));
                }
                Ok(factory(cc)?)
            }),
        );
        match result {
            Err(eframe::Error::Wgpu(ref e))
                if attempt == 0
                    && (prefer || retry_test)
                    && !explicit
                    && !created.load(Ordering::Relaxed) =>
            {
                eprintln!("GPU_INITIALIZATION_RETRY: {e}");
                trace.mark("gpu_initialization_error");
            }
            other => return other,
        }
    }
    unreachable!("last GPU attempt returns its result")
}
