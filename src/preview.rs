//! Real headless Layoutlib/Compose rendering over an isolated native project snapshot.
use crate::{build, native, run::RunEvent};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub composable: String,
    pub width_dp: u32,
    pub height_dp: u32,
    pub dark: bool,
    pub font_scale: f32,
    pub locale: String,
    pub show_background: bool,
    pub background_color: u32,
}
impl Default for Entry {
    fn default() -> Self {
        Self {
            name: "Preview".into(),
            composable: String::new(),
            width_dp: 360,
            height_dp: 640,
            dark: false,
            font_scale: 1.,
            locale: "en-US".into(),
            show_background: false,
            background_color: 0,
        }
    }
}
impl Entry {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "invalid preview name"
        );
        ensure!(
            self.composable.len() <= 1024
                && self.composable.split('.').count() >= 3
                && self.composable.split('.').all(|s| !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_$".contains(&c))
                    && !s.as_bytes()[0].is_ascii_digit()),
            "invalid composable entry"
        );
        ensure!(
            (1..=1080).contains(&self.width_dp)
                && (1..=1920).contains(&self.height_dp)
                && self.font_scale.is_finite()
                && (0.5..=3.).contains(&self.font_scale),
            "preview dimensions/font scale outside supported bounds"
        );
        ensure!(
            !self.locale.is_empty()
                && self.locale.len() <= 64
                && self
                    .locale
                    .split('-')
                    .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())),
            "invalid preview locale"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: u32,
    pub entries: Vec<Entry>,
}
impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let bytes = std::fs::read(root.join("wex.preview.json"))
            .context("Add wex.preview.json with compiled composable entries")?;
        ensure!(bytes.len() <= 65536, "preview config exceeds64KiB");
        let c: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            c.schema == 1 && !c.entries.is_empty() && c.entries.len() <= 64,
            "invalid preview config"
        );
        let mut names = std::collections::BTreeSet::new();
        for e in &c.entries {
            e.validate()?;
            ensure!(names.insert(e.name.clone()), "duplicate preview name");
        }
        Ok(c)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Buffer {
    pub path: PathBuf,
    pub text: String,
    pub version: i32,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub result: String,
    pub entry: Entry,
    #[serde(default)]
    pub catalog: crate::preview_catalog::Catalog,
    #[serde(default)]
    pub selected_id: String,
    pub output: PathBuf,
    pub png_blake3: String,
    pub source_hashes: BTreeMap<PathBuf, String>,
    pub buffers: Vec<(PathBuf, i32, String)>,
    pub renderer: Value,
    pub studio_build: String,
    pub native_backend: String,
    pub resource_blake3: String,
    pub compilation_steps: Vec<build::Step>,
    #[serde(default)]
    pub sdk_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub resource_cache: Vec<crate::resource_cache::Record>,
    pub compile_elapsed_ms: u128,
    pub compose: Option<build::ComposeBuild>,
    pub disk_source_unchanged: bool,
}
#[derive(Deserialize)]
struct Tools {
    schema: u32,
    studio: PathBuf,
    studio_build: String,
    java: PathBuf,
    classes: PathBuf,
    source_sha256: String,
    #[serde(default)]
    discovery_source_sha256: String,
    classpath: Vec<PathBuf>,
    files: Vec<ToolFile>,
}
#[derive(Deserialize)]
struct ToolFile {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}
fn tools() -> Result<Tools> {
    tools_checked(None)
}
fn tools_checked(cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<Tools> {
    let repo = &crate::runtime_paths::paths().support;
    crate::runtime_paths::verify_support()?;
    let t: Tools = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("WEX_PREVIEW_TOOLS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    crate::runtime_paths::paths()
                        .tools
                        .join("preview-renderer/tools.json")
                }),
        )
        .context("Run wex prepare-preview first")?,
    )?;
    ensure!(
        t.schema == 1 && t.java.is_file() && t.classes.join("WexPreviewRenderer.class").is_file(),
        "preview renderer installation missing"
    );
    use sha2::Digest;
    let actual = sha2::Sha256::digest(std::fs::read(repo.join("preview/WexPreviewRenderer.java"))?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let discovery = sha2::Sha256::digest(std::fs::read(
        repo.join("preview/WexPreviewDiscovery.java"),
    )?)
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect::<String>();
    ensure!(
        discovery == t.discovery_source_sha256,
        "preview discovery adapter changed; run prepare-preview"
    );
    ensure!(
        actual == t.source_sha256,
        "preview adapter changed; run prepare-preview"
    );
    ensure!(
        !t.files.is_empty() && t.files.len() < 10000,
        "invalid preview tool inventory"
    );
    ensure!(
        t.classpath
            .iter()
            .chain(
                [
                    t.classes.join("WexPreviewRenderer.class"),
                    t.classes.join("WexPreviewDiscovery.class")
                ]
                .iter()
            )
            .all(|p| t.files.iter().any(|f| &f.path == p)),
        "incomplete preview runtime inventory"
    );
    // Cache only unchanged filesystem identities. Unix ctime catches replacements
    // and writes even if mtime was restored; changed files are re-hashed.
    static VERIFIED: std::sync::Mutex<BTreeMap<PathBuf, (String, String)>> =
        std::sync::Mutex::new(BTreeMap::new());
    let mut verified = VERIFIED
        .lock()
        .map_err(|_| anyhow::anyhow!("preview verification interrupted"))?;
    for f in &t.files {
        ensure!(
            !cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)),
            "preview verification cancelled"
        );
        let metadata = std::fs::metadata(&f.path)?;
        let stamp = format!("{}:{:?}", metadata.len(), metadata.modified()?);
        #[cfg(unix)]
        let stamp = {
            use std::os::unix::fs::MetadataExt;
            format!(
                "{stamp}:{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        };
        if verified.get(&f.path) == Some(&(stamp.clone(), f.sha256.clone())) {
            continue;
        }
        ensure!(
            std::fs::metadata(&f.path)?.len() == f.bytes,
            "preview runtime size mismatch: {}",
            f.path.display()
        );
        let mut file = std::fs::File::open(&f.path)?;
        let mut sha = sha2::Sha256::new();
        use std::io::Read;
        let mut chunk = [0u8; 65536];
        loop {
            ensure!(
                !cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)),
                "preview verification cancelled"
            );
            let n = file.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            sha.update(&chunk[..n]);
        }
        ensure!(
            sha.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                == f.sha256,
            "preview runtime hash mismatch: {}",
            f.path.display()
        );
        verified.insert(f.path.clone(), (stamp, f.sha256.clone()));
    }
    Ok(t)
}
/// Tooling stays outside the application lock/classpath/DEX, using the same version mediation.
pub fn prepare_tooling(root: &Path, offline: bool) -> Result<Vec<PathBuf>> {
    static PREPARE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = PREPARE
        .lock()
        .map_err(|_| anyhow::anyhow!("preview preparation interrupted"))?;
    let mut cfg = native::Config::load(root)?;
    ensure!(
        cfg.build.compose.is_some(),
        "Compose build settings required for preview"
    );
    let cache = native::default_cache();
    let lock = native::resolve_project(root, &cache, offline)?;
    let version = lock
        .artifacts
        .iter()
        .find(|a| {
            a.coordinate.group == "androidx.compose.ui" && a.coordinate.artifact == "ui-android"
        })
        .context("native dependency graph has no Compose UI Android runtime")?
        .coordinate
        .version
        .clone();
    let application = cfg.build.application_module.clone();
    cfg.modules
        .iter_mut()
        .find(|m| m.name == application)
        .context("app module missing")?
        .dependencies
        .push(format!("androidx.compose.ui:ui-tooling-android:{version}"));
    cfg.name = "Wex preview tooling".into();
    cfg.build.compose = None;
    let dir = root.join(".wex/preview-tooling");
    std::fs::create_dir_all(&dir)?;
    let data = serde_json::to_vec_pretty(&cfg)?;
    if std::fs::read(dir.join("wex.json")).ok().as_ref() != Some(&data) {
        std::fs::write(dir.join("wex.json"), data)?;
    }
    // This is a generated private tooling project. Its wex.json is rebuilt from
    // the current root graph above, so an existing tooling lock is only a cache
    // input, never an authority after the generated config changes.
    let lock = native::resolve(&dir, &cache, offline, false)?;
    let mut cp = Vec::new();
    for (index, a) in lock.artifacts.iter().enumerate() {
        match a.packaging.as_str() {
            "jar" => cp.push(cache.join(&a.file)),
            "aar" => {
                let p = dir.join("aar").join(index.to_string());
                std::fs::create_dir_all(&p)?;
                let mut z = zip::ZipArchive::new(std::fs::File::open(cache.join(&a.file))?)?;
                if let Ok(mut f) = z.by_name("classes.jar") {
                    let mut bytes = Vec::new();
                    use std::io::Read;
                    f.read_to_end(&mut bytes)?;
                    let dest = p.join("classes.jar");
                    if std::fs::read(&dest).ok().as_ref() != Some(&bytes) {
                        std::fs::write(&dest, bytes)?;
                    }
                    cp.push(dest);
                }
            }
            _ => {}
        }
    }
    Ok(cp)
}
pub async fn prepare(root: &Path, offline: bool) -> Result<()> {
    let repo = &crate::runtime_paths::paths().support;
    crate::runtime_paths::verify_support()?;
    let result = tokio::process::Command::new("python3")
        .arg(repo.join("scripts/prepare-preview.py"))
        .env("WEX_TOOLS_HOME", &crate::runtime_paths::paths().tools)
        .arg("--offline")
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        result.status.success(),
        "Preview tools: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let r = root.to_owned();
    tokio::task::spawn_blocking(move || prepare_tooling(&r, offline)).await??;
    tools()?;
    Ok(())
}
fn copy_snapshot(
    root: &Path,
    dest: &Path,
    buffers: &[Buffer],
) -> Result<BTreeMap<PathBuf, String>> {
    let cfg = native::Config::load(root)?;
    crate::build::ensure_configured_key_not_packaged(root, &cfg)?;
    let mut paths = vec![
        PathBuf::from("wex.preview.json"),
        PathBuf::from("wex.json"),
        PathBuf::from("wex.lock.json"),
        cfg.build.manifest.clone(),
    ];
    for m in cfg.modules {
        paths.extend(m.sources);
        paths.extend(m.resources);
        paths.extend(m.assets);
        paths.push(PathBuf::from(m.name).join("src/main"));
        if let Some(p) = m.manifest {
            paths.push(p);
        }
    }
    let mut hashes = BTreeMap::new();
    let mut size = 0usize;
    for p in paths {
        let src = root.join(&p);
        if !src.exists() {
            continue;
        }
        ensure!(
            src.canonicalize()?.starts_with(root),
            "preview input outside project"
        );
        // Never copy explicitly configured internal caches/VCS paths or our
        // own destination recursively back into the next snapshot.
        let internal = root.join(".wex");
        let vcs = root.join(".git");
        for entry in walkdir::WalkDir::new(src)
            .into_iter()
            .filter_entry(|e| !e.path().starts_with(&internal) && !e.path().starts_with(&vcs))
        {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root)?;
            if hashes.contains_key(rel) {
                continue;
            }
            ensure!(
                !entry.path().is_symlink(),
                "preview symlink input unsupported"
            );
            let bytes = std::fs::read(path)?;
            size += bytes.len();
            ensure!(
                size <= 256 * 1024 * 1024 && hashes.len() < 20000,
                "preview snapshot exceeds limits"
            );
            let file = dest.join(rel);
            std::fs::create_dir_all(file.parent().unwrap())?;
            std::fs::write(file, &bytes)?;
            hashes.insert(rel.to_owned(), blake3::hash(&bytes).to_hex().to_string());
        }
    }
    let mut overlays = std::collections::BTreeSet::new();
    let mut overlay_bytes = 0;
    for b in buffers {
        overlay_bytes += b.text.len();
        ensure!(
            b.text.len() <= 8 * 1024 * 1024 && overlay_bytes <= 64 * 1024 * 1024,
            "preview buffer snapshot exceeds limits"
        );
        let path = b.path.canonicalize()?;
        let rel = path
            .strip_prefix(root)
            .context("preview buffer outside project")?;
        ensure!(overlays.insert(rel.to_owned()), "duplicate preview buffer");
        ensure!(
            hashes.contains_key(rel),
            "preview buffer outside captured native sources/resources"
        );
        std::fs::write(dest.join(rel), &b.text)?;
    }
    Ok(hashes)
}
async fn run_process(mut command: tokio::process::Command, label: &str) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(r: R) -> Result<String> {
        let mut data = Vec::new();
        r.take(2 * 1024 * 1024 + 1).read_to_end(&mut data).await?;
        ensure!(
            data.len() <= 2 * 1024 * 1024,
            "renderer output exceeded2MiB"
        );
        Ok(String::from_utf8_lossy(&data).into_owned())
    }
    let stdout = child.stdout.take().context("renderer stdout")?;
    let stderr = child.stderr.take().context("renderer stderr")?;
    let (_, stderr, status) = tokio::time::timeout(Duration::from_secs(45), async {
        tokio::try_join!(bounded(stdout), bounded(stderr), async {
            Ok::<_, anyhow::Error>(child.wait().await?)
        })
    })
    .await
    .context("preview process timed out")??;
    ensure!(
        status.success(),
        "{label}: {}",
        stderr
            .chars()
            .rev()
            .take(12000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );
    Ok(())
}
struct Cancel(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}
struct Snapshot {
    _cancel: Cancel,
    work: std::sync::Arc<tempfile::TempDir>,
    project: PathBuf,
    root: PathBuf,
    source_hashes: BTreeMap<PathBuf, String>,
    input_fingerprint: String,
    cfg: native::Config,
    built: build::PreviewCompilation,
    dependencies: Vec<PathBuf>,
    tools: Tools,
    buffers: Vec<Buffer>,
}
async fn capture(
    root: &Path,
    buffers: Vec<Buffer>,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Snapshot> {
    let root = root.canonicalize()?;
    let cancel = Cancel(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
        false,
    )));
    let flag = cancel.0.clone();
    event(RunEvent::Phase("Verifying local Layoutlib runtime…".into()));
    let t = tokio::task::spawn_blocking(move || tools_checked(Some(&flag))).await??;
    let r = root.clone();
    let dependencies = tokio::task::spawn_blocking(move || prepare_tooling(&r, true)).await??;
    let jobs = root.join(".wex/preview-jobs");
    std::fs::create_dir_all(&jobs)?;
    let work = std::sync::Arc::new(tempfile::tempdir_in(jobs)?);
    let project = work.path().join("project");
    std::fs::create_dir(&project)?;
    event(RunEvent::Phase(
        "Snapshotting native sources · no disk edits".into(),
    ));
    let r = root.clone();
    let p = project.clone();
    let copy_buffers = buffers.clone();
    let copy_work = work.clone(); // aborted async jobs cannot outlive their snapshot's owner
    let (input_fingerprint, source_hashes) = tokio::task::spawn_blocking(move || {
        let _owner = copy_work;
        let fingerprint = preview_fingerprint(&r, &native::Config::load(&r)?)?;
        Ok::<_, anyhow::Error>((fingerprint, copy_snapshot(&r, &p, &copy_buffers)?))
    })
    .await??;
    let cfg = native::Config::load(&project)?;
    // An existing malformed explicit config is an error, never automatic fallback.
    if project.join("wex.preview.json").exists() {
        Config::load(&project)?;
    }
    let built = build::compile_preview_cached(&project, &root, event).await?;
    Ok(Snapshot {
        _cancel: cancel,
        work,
        project,
        root,
        source_hashes,
        input_fingerprint,
        cfg,
        built,
        dependencies,
        tools: t,
        buffers,
    })
}

fn config_stamp(root: &Path) -> Result<Option<String>> {
    match std::fs::read(root.join("wex.preview.json")) {
        Ok(bytes) => {
            ensure!(bytes.len() <= 65536, "preview config exceeds64KiB");
            Ok(Some(blake3::hash(&bytes).to_hex().to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("reading preview config state"),
    }
}
fn preview_fingerprint(root: &Path, cfg: &native::Config) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(&(
        build::input_fingerprint(root, cfg)?,
        config_stamp(root)?,
    ))?)
    .to_hex()
    .to_string())
}
fn verify_inputs(snapshot: &Snapshot) -> Result<()> {
    ensure!(
        preview_fingerprint(&snapshot.root, &native::Config::load(&snapshot.root)?)?
            == snapshot.input_fingerprint,
        "project input inventory changed while previewing; refresh preview"
    );
    for (path, hash) in &snapshot.source_hashes {
        ensure!(
            blake3::hash(&std::fs::read(snapshot.root.join(path))?)
                .to_hex()
                .as_str()
                == hash,
            "project input changed while previewing; refresh preview"
        );
    }
    Ok(())
}
async fn discover(snapshot: &Snapshot) -> Result<crate::preview_catalog::Catalog> {
    let mut cp = snapshot.built.classpath.clone();
    cp.extend(snapshot.dependencies.clone());
    let input = snapshot.work.path().join("discovery-input.json");
    let output = snapshot.work.path().join("discovery.json");
    std::fs::write(
        &input,
        serde_json::to_vec(
            &json!({"project_classes":snapshot.built.classpath,"classpath":cp,"output":output}),
        )?,
    )?;
    let classpath = std::env::join_paths(
        std::iter::once(&snapshot.tools.classes).chain(snapshot.tools.classpath.iter()),
    )?;
    let mut command = tokio::process::Command::new(&snapshot.tools.java);
    command
        .args(["-Xmx512m", "-cp"])
        .arg(classpath)
        .arg("WexPreviewDiscovery")
        .arg(input)
        .kill_on_drop(true);
    run_process(command, "Compiled preview discovery").await?;
    let data = std::fs::read(output)?;
    ensure!(data.len() <= 2 * 1024 * 1024, "preview catalog exceeds2MiB");
    let explicit = if snapshot.project.join("wex.preview.json").exists() {
        Some(Config::load(&snapshot.project)?)
    } else {
        None
    };
    crate::preview_catalog::Catalog::build(
        explicit,
        serde_json::from_slice(&data)?,
        snapshot.cfg.android.compile_sdk,
    )
}
#[derive(Debug, Serialize)]
pub struct CatalogReport {
    pub result: String,
    pub catalog: crate::preview_catalog::Catalog,
    pub source_hashes: BTreeMap<PathBuf, String>,
    pub compilation_steps: Vec<build::Step>,
    #[serde(default)]
    pub sdk_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub resource_cache: Vec<crate::resource_cache::Record>,
    pub compile_elapsed_ms: u128,
    pub disk_source_unchanged: bool,
}
pub async fn list(
    root: &Path,
    buffers: Vec<Buffer>,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<CatalogReport> {
    let snapshot = capture(root, buffers, event).await?;
    let catalog = discover(&snapshot).await?;
    verify_inputs(&snapshot)?;
    Ok(CatalogReport {
        result: "PREVIEW_CATALOG_OK".into(),
        catalog,
        source_hashes: snapshot.source_hashes.clone(),
        compilation_steps: snapshot.built.steps.clone(),
        sdk_cache: snapshot.built.sdk_cache.clone(),
        resource_cache: snapshot.built.resource_cache.clone(),
        compile_elapsed_ms: snapshot.built.elapsed_ms,
        disk_source_unchanged: true,
    })
}
#[derive(Clone, Default)]
pub struct Selection {
    pub id: Option<String>,
    pub dark: Option<bool>,
}
pub async fn render(
    root: &Path,
    entry: &Entry,
    buffers: Vec<Buffer>,
    output: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Report> {
    entry.validate()?;
    render_inner(
        root,
        Some(entry.clone()),
        Selection::default(),
        buffers,
        output,
        event,
        &|_| {},
    )
    .await
}
pub async fn render_selected(
    root: &Path,
    selection: Selection,
    buffers: Vec<Buffer>,
    output: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    catalog_event: &(dyn Fn(crate::preview_catalog::Catalog) + Send + Sync),
) -> Result<Report> {
    render_inner(root, None, selection, buffers, output, event, catalog_event).await
}
async fn render_inner(
    root: &Path,
    explicit: Option<Entry>,
    selection: Selection,
    buffers: Vec<Buffer>,
    output: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    catalog_event: &(dyn Fn(crate::preview_catalog::Catalog) + Send + Sync),
) -> Result<Report> {
    let snapshot = capture(root, buffers, event).await?;
    event(RunEvent::Phase(
        "Discovering compiled @Preview/multipreview annotations…".into(),
    ));
    let catalog = discover(&snapshot).await?;
    catalog_event(catalog.clone());
    let (mut entry, selected_id) = if let Some(entry) = explicit {
        (entry, "explicit".to_owned())
    } else {
        let item = catalog.select(selection.id.as_deref())?;
        (item.entry.clone(), item.id.clone())
    };
    if let Some(dark) = selection.dark {
        entry.dark = dark;
    }
    entry.validate()?;
    verify_inputs(&snapshot)?;
    let Snapshot {
        _cancel,
        work,
        project,
        root,
        source_hashes,
        input_fingerprint,
        cfg,
        built,
        dependencies,
        tools: t,
        buffers,
    } = snapshot;
    let mut cp = dependencies;
    cp.extend(built.classpath.clone());
    let compiler = build::prepare_tools(&project, true).await?;
    let stdlib = compiler
        .into_iter()
        .find(|p| {
            p.file_name().is_some_and(|n| {
                n == format!("kotlin-stdlib-{}.jar", cfg.build.kotlin_version).as_str()
            })
        })
        .context("matching preview host stdlib missing")?;
    let png = work.path().join("render.png");
    let receipt = work.path().join("render.json");
    let input = work.path().join("input.json");
    let sdk = std::env::var_os("ANDROID_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Android/Sdk")
        });
    std::fs::write(
        &input,
        serde_json::to_vec(
            &json!({"resource_package":if cfg.build.namespace.is_empty(){&cfg.android.application_id}else{&cfg.build.namespace},"studio":t.studio,"platform":sdk.join(format!("platforms/android-{}",cfg.android.compile_sdk)),"classpath":cp,"build":{"apk":{"path":built.resources,"application_id":cfg.android.application_id}},"width":entry.width_dp*2,"height":entry.height_dp*2,"dark":entry.dark,"font_scale":entry.font_scale,"locale":entry.locale,"show_background":entry.show_background,"background_color":entry.background_color,"api":cfg.android.compile_sdk,"min_sdk":cfg.android.min_sdk,"composable":entry.composable,"output":png,"receipt":receipt}),
        )?,
    )?;
    let classpath = std::env::join_paths([t.classes, stdlib].into_iter().chain(t.classpath))?;
    event(RunEvent::Phase(
        "Rendering real Compose · headless Layoutlib".into(),
    ));
    let mut command = tokio::process::Command::new(t.java);
    command
        .arg("-Djava.awt.headless=true")
        .arg("-Xmx1024m")
        .arg("-cp")
        .arg(classpath)
        .arg("WexPreviewRenderer")
        .arg(input)
        .kill_on_drop(true);
    run_process(command, "Layoutlib").await?;
    let renderer: Value = serde_json::from_slice(&std::fs::read(receipt)?)?;
    ensure!(
        renderer["result"] == "HEADLESS_COMPOSE_LAYOUTLIB_OK"
            && renderer["headless"] == true
            && renderer["compose_groups"].as_u64().is_some_and(|n| n > 0),
        "Layoutlib produced no genuine Compose scene"
    );
    let bytes = std::fs::read(png)?;
    ensure!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() <= 24 * 1024 * 1024,
        "invalid preview PNG"
    );
    ensure!(
        preview_fingerprint(&root, &native::Config::load(&root)?)? == input_fingerprint,
        "project input inventory changed while rendering; refresh preview"
    );
    ensure!(
        bytes.len() >= 24
            && u32::from_be_bytes(bytes[16..20].try_into()?) == entry.width_dp * 2
            && u32::from_be_bytes(bytes[20..24].try_into()?) == entry.height_dp * 2
            && renderer["width"].as_u64() == Some((entry.width_dp * 2) as u64)
            && renderer["height"].as_u64() == Some((entry.height_dp * 2) as u64),
        "preview dimensions differ from requested scene"
    );
    for (path, hash) in &source_hashes {
        ensure!(
            blake3::hash(&std::fs::read(root.join(path))?)
                .to_hex()
                .as_str()
                == hash,
            "project input changed while rendering; refresh preview"
        );
    }
    if let Some(p) = output.parent() {
        std::fs::create_dir_all(p)?;
    }
    let report = Report {
        result: "PREVIEW_OK".into(),
        entry: entry.clone(),
        catalog,
        selected_id,
        output: output.to_owned(),
        png_blake3: blake3::hash(&bytes).to_hex().to_string(),
        source_hashes,
        buffers: buffers
            .into_iter()
            .map(|b| {
                (
                    b.path,
                    b.version,
                    blake3::hash(b.text.as_bytes()).to_hex().to_string(),
                )
            })
            .collect(),
        renderer,
        studio_build: t.studio_build,
        native_backend: "wex-native-compile-v1".into(),
        resource_blake3: built.resource_blake3,
        compilation_steps: built.steps,
        sdk_cache: built.sdk_cache,
        resource_cache: built.resource_cache,
        compile_elapsed_ms: built.elapsed_ms,
        compose: built.compose,
        disk_source_unchanged: true,
    };
    let mut file =
        tempfile::NamedTempFile::new_in(output.parent().context("preview output parent")?)?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.persist(output).map_err(|e| e.error)?;
    std::fs::write(
        output.with_extension("json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_presence_and_edits_change_preview_stamp_without_affecting_native_config() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(config_stamp(d.path()).unwrap(), None);
        std::fs::write(d.path().join("wex.preview.json"), b"{}").unwrap();
        let a = config_stamp(d.path()).unwrap();
        assert!(a.is_some());
        std::fs::write(d.path().join("wex.preview.json"), b"{\"schema\":1}").unwrap();
        assert_ne!(config_stamp(d.path()).unwrap(), a);
        std::fs::remove_file(d.path().join("wex.preview.json")).unwrap();
        assert_eq!(config_stamp(d.path()).unwrap(), None);
    }
    #[test]
    fn locale_and_small_explicit_viewports_are_validated() {
        let mut e = Entry {
            composable: "example.FileKt.Preview".into(),
            width_dp: 50,
            height_dp: 50,
            ..Default::default()
        };
        assert!(e.validate().is_ok());
        for locale in ["", "../outside", "en--US", "en_US"] {
            e.locale = locale.into();
            assert!(e.validate().is_err());
        }
        e.locale = "ar-EG".into();
        assert!(e.validate().is_ok());
        e.width_dp = 0;
        assert!(e.validate().is_err());
    }
    #[test]
    fn entry_limits_and_names() {
        let mut e = Entry {
            composable: "dev.wex.FooKt.Preview".into(),
            ..Default::default()
        };
        assert!(e.validate().is_ok());
        for bad in ["bad", "foo..Preview", "foo.Bar.fn();", "foo.1Bar.fn"] {
            e.composable = bad.into();
            assert!(e.validate().is_err());
        }
        e.composable = "dev.wex.FooKt.Preview".into();
        e.font_scale = f32::NAN;
        assert!(e.validate().is_err());
    }
    #[test]
    fn snapshot_overlays_dirty_buffers_without_writing_disk() {
        let r = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        let config = include_str!("../tests/fixtures/compose-android/wex.json");
        std::fs::write(r.path().join("wex.json"), config).unwrap();
        std::fs::write(r.path().join("wex.lock.json"), "{}").unwrap();
        let p = r.path().join("app/src/main/kotlin/Main.kt");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "original").unwrap();
        let b = Buffer {
            path: p.clone(),
            text: "unsaved".into(),
            version: 7,
        };
        let h = copy_snapshot(r.path(), d.path(), &[b]).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(d.path().join("app/src/main/kotlin/Main.kt")).unwrap(),
            "unsaved"
        );
        assert!(h.contains_key(Path::new("app/src/main/kotlin/Main.kt")));
    }
    #[test]
    fn config_rejects_duplicate_entries_and_unknown_fields() {
        let root = tempfile::tempdir().unwrap();
        let entry = serde_json::to_value(Entry {
            composable: "dev.wex.FooKt.Preview".into(),
            ..Default::default()
        })
        .unwrap();
        for invalid in [
            json!({"schema":1,"entries":[entry.clone(),entry.clone()]}),
            json!({"schema":2,"entries":[entry.clone()]}),
            json!({"schema":1,"entries":[entry.clone()],"other":true}),
        ] {
            std::fs::write(
                root.path().join("wex.preview.json"),
                serde_json::to_vec(&invalid).unwrap(),
            )
            .unwrap();
            assert!(Config::load(root.path()).is_err());
        }
    }
    #[test]
    fn snapshot_rejects_duplicate_or_uncaptured_buffers() {
        let root = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wex.json"),
            include_str!("../tests/fixtures/compose-android/wex.json"),
        )
        .unwrap();
        let path = root.path().join("app/src/main/kotlin/Main.kt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original").unwrap();
        let b = Buffer {
            path: path.clone(),
            text: "unsaved".into(),
            version: 1,
        };
        assert!(
            copy_snapshot(root.path(), dest.path(), &[b.clone(), b])
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        let outside = root.path().join("ignored.kt");
        std::fs::write(&outside, "outside").unwrap();
        assert!(
            copy_snapshot(
                root.path(),
                dest.path(),
                &[Buffer {
                    path: outside,
                    text: "x".into(),
                    version: 1
                }]
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }
    #[test]
    fn oversized_overlay_is_rejected_without_disk_mutation() {
        let root = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wex.json"),
            include_str!("../tests/fixtures/compose-android/wex.json"),
        )
        .unwrap();
        let path = root.path().join("app/src/main/kotlin/Main.kt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original").unwrap();
        let b = Buffer {
            path: path.clone(),
            text: "x".repeat(8 * 1024 * 1024 + 1),
            version: 1,
        };
        assert!(
            copy_snapshot(root.path(), dest.path(), &[b])
                .unwrap_err()
                .to_string()
                .contains("limits")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }
    #[test]
    fn internal_output_source_is_excluded_from_its_own_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/compose-android/wex.json"))
                .unwrap();
        cfg["modules"][0]["sources"] = json!(["src", ".wex"]);
        std::fs::write(
            root.path().join("wex.json"),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/Main.kt"), "val x=1").unwrap();
        let dest = root.path().join(".wex/preview-jobs/snapshot");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(root.path().join(".wex/ignored.kt"), "cache").unwrap();
        let h = copy_snapshot(root.path(), &dest, &[]).unwrap();
        assert!(h.contains_key(Path::new("src/Main.kt")));
        assert!(!dest.join(".wex").exists());
        assert!(h.keys().all(|p| !p.starts_with(".wex")));
    }
    #[test]
    fn packaged_release_key_is_rejected_before_snapshot_copy() {
        let root = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let mut cfg: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/compose-android/wex.json"))
                .unwrap();
        cfg["build"]["release_signing"] = json!({"keystore":"app/src/main/res/raw/private.jks","key_alias":"release","store_password_env":"WEX_MISSING_STORE","key_password_env":"WEX_MISSING_KEY","certificate_sha256":"a".repeat(64)});
        std::fs::write(
            root.path().join("wex.json"),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        let key = root.path().join("app/src/main/res/raw/private.jks");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "private fixture key").unwrap();
        assert!(
            copy_snapshot(root.path(), dest.path(), &[])
                .unwrap_err()
                .to_string()
                .contains("would be packaged")
        );
        assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());
    }
}
