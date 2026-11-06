//! Independent native K2 model: real AAPT/Javac symbols, no app compilation/DEX/signing.
use crate::{build, cache::DiskCache, native, project::ProjectGraph, run::RunEvent};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub const MODEL_API: &str = "wex-native-analysis-3";
const ABI: &str = MODEL_API;
const GENERATOR_REVISION: &[u8] = b"resource-files-v8";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileHash {
    pub path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub schema: u32,
    pub backend: String,
    pub root: PathBuf,
    pub variant: String,
    pub fingerprint: String,
    pub model_key: String,
    pub graph_blake3: String,
    pub generation: PathBuf,
    pub contexts: Vec<build::AnalysisContext>,
    pub outputs: Vec<FileHash>,
    pub compiler_inputs: Vec<PathBuf>,
    pub compose: Option<build::ComposeBuild>,
    pub generation_steps: Vec<build::Step>,
    #[serde(default)]
    pub generation_sdk_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub generation_resource_cache: Vec<crate::resource_cache::Record>,
}
#[derive(Debug, Serialize)]
pub struct Prepared {
    pub result: String,
    pub cache_hit: bool,
    pub elapsed_ms: u128,
    pub executed_generation_steps: usize,
    pub graph: ProjectGraph,
    pub model: Model,
}
fn variant(v: &str) -> Result<()> {
    ensure!(
        matches!(v, "debug" | "release"),
        "analysis variant must be debug or release"
    );
    Ok(())
}
fn cache(root: &Path) -> Result<DiskCache> {
    DiskCache::open(&root.join(".wex/analysis/models.lmdb"))
}
pub(crate) fn hash(path: &Path) -> Result<String> {
    static HASHES: std::sync::Mutex<BTreeMap<PathBuf, (String, String)>> =
        std::sync::Mutex::new(BTreeMap::new());
    let m = std::fs::metadata(path)?;
    ensure!(
        m.is_file() && m.len() <= 512 * 1024 * 1024,
        "invalid/oversized analysis file"
    );
    let stamp = format!("{}:{:?}", m.len(), m.modified()?);
    #[cfg(unix)]
    let stamp = {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{stamp}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            m.ctime(),
            m.ctime_nsec()
        )
    };
    let mut memo = HASHES
        .lock()
        .map_err(|_| anyhow::anyhow!("analysis hash cache interrupted"))?;
    if let Some((old, h)) = memo.get(path)
        && *old == stamp
    {
        return Ok(h.clone());
    }
    let mut file = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut b = [0; 65536];
    loop {
        let n = file.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    let value = h.finalize().to_hex().to_string();
    if memo.len() >= 16384 {
        memo.clear();
    }
    memo.insert(path.to_owned(), (stamp, value.clone()));
    Ok(value)
}
fn within(root: &Path, p: &Path) -> Result<PathBuf> {
    let path = root.join(p).canonicalize()?;
    ensure!(path.starts_with(root), "analysis input outside project");
    Ok(path)
}
fn source_roots(root: &Path, cfg: &native::Config) -> Result<()> {
    for m in &cfg.modules {
        for p in &m.sources {
            ensure!(
                within(root, p)?.is_dir(),
                "analysis source root is not a directory"
            );
        }
    }
    Ok(())
}
/// User source contents are intentionally absent: syntax/type errors must not block K2.
fn fingerprint(root: &Path, cfg: &native::Config, compiler: &[PathBuf], v: &str) -> Result<String> {
    variant(v)?;
    source_roots(root, cfg)?;
    let mut paths = vec![
        root.join("wex.json"),
        root.join("wex.lock.json"),
        within(root, &cfg.build.manifest)?,
    ];
    for m in &cfg.modules {
        if let Some(p) = &m.manifest {
            paths.push(within(root, p)?);
        }
        for p in &m.resources {
            let dir = within(root, p)?;
            for entry in walkdir::WalkDir::new(dir) {
                let entry = entry?;
                ensure!(
                    !entry.path().is_symlink(),
                    "analysis resource symlink unsupported"
                );
                if entry.file_type().is_file() {
                    paths.push(entry.into_path());
                }
            }
        }
    }
    let lock: native::Lock = serde_json::from_slice(&std::fs::read(root.join("wex.lock.json"))?)?;
    for f in &lock.files {
        ensure!(
            Path::new(&f.path)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid analysis dependency path"
        );
        let p = native::default_cache().join(&f.path);
        ensure!(
            p.metadata()?.len() == f.bytes && hash(&p)? == f.blake3,
            "analysis dependency checksum mismatch: {}",
            f.path
        );
        paths.push(p);
    }
    paths.extend(build::analysis_sdk_inputs(cfg)?);
    paths.extend(compiler.iter().cloned());
    paths.sort();
    paths.dedup();
    ensure!(paths.len() <= 30000, "analysis input inventory limit");
    let mut h = blake3::Hasher::new();
    h.update(ABI.as_bytes());
    h.update(GENERATOR_REVISION);
    h.update(build::analysis_sdk_context(cfg)?.as_bytes());
    h.update(root.as_os_str().as_encoded_bytes());
    h.update(v.as_bytes());
    for p in paths {
        h.update(p.as_os_str().as_encoded_bytes());
        h.update(&[0]);
        h.update(hash(&p)?.as_bytes());
        h.update(&[0]);
    }
    Ok(h.finalize().to_hex().to_string())
}
fn read_model(root: &Path, v: &str) -> Result<Model> {
    variant(v)?;
    let p = root.join(".wex/analysis").join(v).join("current.json");
    let bytes = std::fs::read(p)?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "analysis receipt limit");
    Ok(serde_json::from_slice(&bytes)?)
}
fn receipt_key(v: &str, fp: &str) -> String {
    format!("receipt:{ABI}:{v}:{fp}")
}
fn model_bytes(model: &Model) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(model)?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "analysis receipt limit");
    Ok(bytes)
}
fn index_model(root: &Path, model: &Model, bytes: &[u8]) -> Result<()> {
    let db = cache(root)?;
    let key = receipt_key(&model.variant, &model.fingerprint);
    if db.get(&key)?.as_deref() != Some(bytes) {
        db.put(&key, bytes)?;
    }
    Ok(())
}
/// Historical receipts are only locators. They must pass precisely the same
/// current-input/graph/context/output validation as the most recent pointer.
fn cached_model(root: &Path, v: &str, fp: &str) -> Option<(Model, ProjectGraph, bool)> {
    // A present current pointer is authoritative for this project state.  Do not
    // silently resurrect an older indexed receipt when the pointer is malformed
    // or fails coverage/output validation: that would turn a tampered context
    // inventory into a false cache hit.  The historical index is a migration and
    // recovery source only when the current pointer is genuinely absent.
    let current = root.join(".wex/analysis").join(v).join("current.json");
    if current.is_file() {
        let model = read_model(root, v).ok()?;
        return validate(root, v, fp, &model)
            .ok()
            .map(|graph| (model, graph, false));
    }
    let model = cache(root)
        .and_then(|db| {
            let bytes = db
                .get(&receipt_key(v, fp))?
                .context("analysis receipt missing")?;
            ensure!(bytes.len() <= 4 * 1024 * 1024, "analysis receipt limit");
            Ok(serde_json::from_slice(&bytes)?)
        })
        .ok()?;
    validate(root, v, fp, &model)
        .ok()
        .map(|graph| (model, graph, true))
}
fn validate(root: &Path, v: &str, fp: &str, model: &Model) -> Result<ProjectGraph> {
    ensure!(
        model.schema == 2,
        "stale analysis model schema: {}",
        model.schema
    );
    ensure!(
        model.backend == ABI,
        "stale analysis model backend: {}",
        model.backend
    );
    ensure!(
        model.root == root,
        "stale analysis model root: {} != {}",
        model.root.display(),
        root.display()
    );
    ensure!(
        model.variant == v,
        "stale analysis model variant: {} != {}",
        model.variant,
        v
    );
    ensure!(
        model.fingerprint == fp,
        "stale analysis model fingerprint: {} != {}",
        model.fingerprint,
        fp
    );
    let parent = root
        .join(".wex/analysis")
        .join(v)
        .join("generations")
        .canonicalize()?;
    let generation = model.generation.canonicalize()?;
    ensure!(
        !model.generation.is_symlink()
            && generation.starts_with(root)
            && generation.parent() == Some(parent.as_path())
            && !model.outputs.is_empty()
            && model.outputs.len() <= 30000,
        "invalid analysis generation"
    );
    let mut inventory = std::collections::BTreeSet::new();
    for f in &model.outputs {
        ensure!(
            inventory.insert(f.path.clone()),
            "duplicate analysis output"
        );
        ensure!(
            !f.path.is_absolute()
                && f.path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid analysis output path"
        );
        let p = generation.join(&f.path);
        ensure!(
            p.canonicalize()?.starts_with(&generation),
            "analysis output escaped generation"
        );
        ensure!(
            p.metadata()?.len() == f.bytes && hash(&p)? == f.blake3,
            "analysis generated output changed: {}",
            f.path.display()
        );
    }
    let mut observed = std::collections::BTreeSet::new();
    for entry in walkdir::WalkDir::new(&generation) {
        let entry = entry?;
        ensure!(!entry.file_type().is_symlink(), "analysis output symlink");
        if entry.file_type().is_file() {
            ensure!(observed.len() < 30000, "analysis output inventory limit");
            observed.insert(entry.path().strip_prefix(&generation)?.to_owned());
        }
    }
    ensure!(observed == inventory, "analysis output inventory changed");
    let data = cache(root)?
        .get(&model.model_key)?
        .context("analysis LMDB model missing")?;
    ensure!(
        data.len() <= 4 * 1024 * 1024
            && blake3::hash(&data).to_hex().as_str() == model.graph_blake3,
        "analysis LMDB hash mismatch"
    );
    let graph: ProjectGraph = serde_json::from_slice(&data)?;
    ensure!(
        graph.root == root
            && graph.selected_variant == v
            && graph.fingerprint == fp
            && graph.model_api == ABI,
        "analysis graph identity mismatch"
    );
    ensure!(
        graph.modules.iter().all(|m| m.selected_variant == v
            && m.classpath
                .iter()
                .chain(&m.boot_classpath)
                .all(|p| p.is_file())
            && m.source_roots
                .iter()
                .chain(&m.generated_source_roots)
                .chain(&m.classes_roots)
                .all(|p| p.is_dir())),
        "analysis graph files missing"
    );
    validate_contexts(root, &generation, &model.contexts, &graph)?;
    if let Some(info) = &model.compose {
        ensure!(
            hash(&info.plugin_jar)? == info.plugin_blake3,
            "Compose analysis plugin changed"
        );
    }
    Ok(graph)
}
fn validate_contexts(
    root: &Path,
    generation: &Path,
    contexts: &[build::AnalysisContext],
    graph: &ProjectGraph,
) -> Result<()> {
    use std::collections::BTreeSet;
    let cfg = native::Config::load(root)?;
    let roots = crate::module_scope::analysis_roots(&cfg)?;
    ensure!(
        contexts.len() == roots.len(),
        "analysis context coverage count mismatch"
    );
    let mut assigned = BTreeMap::new();
    for (i, (context, expected)) in contexts.iter().zip(roots).enumerate() {
        ensure!(
            context.root_module == expected
                && context.closure == crate::module_scope::project_order(&cfg, &expected)?,
            "analysis context root/closure mismatch"
        );
        let directory = if i == 0 {
            generation.to_owned()
        } else {
            generation.join("contexts").join(&expected)
        };
        ensure!(
            context.directory == directory && directory.canonicalize()? == directory,
            "analysis context directory mismatch"
        );
        let modules = context
            .closure
            .iter()
            .filter(|name| !assigned.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        ensure!(
            !modules.is_empty() && context.modules == modules,
            "analysis context module assignment mismatch"
        );
        for name in &modules {
            ensure!(
                assigned.insert(name.clone(), directory.clone()).is_none(),
                "duplicate analysis assignment"
            );
        }
    }
    let expected = cfg
        .modules
        .iter()
        .map(|m| m.name.clone())
        .collect::<BTreeSet<_>>();
    let actual = graph
        .modules
        .iter()
        .map(|m| m.name.trim_start_matches(':').to_owned())
        .collect::<BTreeSet<_>>();
    ensure!(
        expected == actual && actual.len() == graph.modules.len() && assigned.len() == actual.len(),
        "analysis graph omits/duplicates declared modules"
    );
    let app_ns = if cfg.build.namespace.is_empty() {
        &cfg.android.application_id
    } else {
        &cfg.build.namespace
    };
    for module in &graph.modules {
        let name = module.name.trim_start_matches(':');
        let config = cfg
            .modules
            .iter()
            .find(|m| m.name == name)
            .context("analysis module config missing")?;
        let directory = &assigned[name];
        let application = name == cfg.build.application_module;
        ensure!(
            module.name == format!(":{name}")
                && module.namespace
                    == if application {
                        app_ns
                    } else {
                        &config.namespace
                    }
                    .as_str()
                && module.project_type
                    == if application {
                        "APPLICATION"
                    } else {
                        "LIBRARY"
                    }
                && module.application_id
                    == if application {
                        cfg.android.application_id.as_str()
                    } else {
                        ""
                    },
            "analysis module identity/type mismatch"
        );
        let sources = config
            .sources
            .iter()
            .map(|p| within(root, p))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            module.source_roots == sources
                && module.resource_roots
                    == config
                        .resources
                        .iter()
                        .map(|p| root.join(p))
                        .collect::<Vec<_>>()
                && module.module_dependencies
                    == config
                        .projects
                        .iter()
                        .map(|p| format!(":{p}"))
                        .collect::<Vec<_>>(),
            "analysis module source/dependency mapping mismatch"
        );
        ensure!(
            module.generated_source_roots == [directory.join("module-generated").join(name)]
                && module.classes_roots == [directory.join("module-generated-classes").join(name)]
                && module.classpath.last()
                    == Some(&directory.join("symbols").join(format!("{name}.jar"))),
            "analysis module generated context mismatch"
        );
    }
    Ok(())
}
pub fn compose_for_graph(root: &Path, graph: &ProjectGraph) -> Result<Option<build::ComposeBuild>> {
    let root = root.canonicalize()?;
    let cfg = native::Config::load(&root)?;
    let m = read_model(&root, &graph.selected_variant)?;
    let fp = fingerprint(&root, &cfg, &m.compiler_inputs, &graph.selected_variant)?;
    ensure!(
        validate(&root, &graph.selected_variant, &fp, &m)? == *graph,
        "analysis model superseded; sync again"
    );
    Ok(m.compose)
}
struct Guard(PathBuf);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn guard(path: PathBuf) -> Result<Guard> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let guard = Guard(path);
                writeln!(file, "{}", std::process::id())?;
                return Ok(guard);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "analysis preparation busy; inspect preparing.lock if previous process exited"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}
fn prune(work: &Path, graph: &ProjectGraph) -> Result<Vec<FileHash>> {
    let keep = graph
        .modules
        .iter()
        .flat_map(|m| {
            m.classpath
                .iter()
                .chain(&m.generated_source_roots)
                .chain(&m.classes_roots)
        })
        .filter(|p| p.starts_with(work))
        .cloned()
        .collect::<Vec<_>>();
    let mut paths = walkdir::WalkDir::new(work)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort_by_key(|p| std::cmp::Reverse(p.depth()));
    let mut outputs = vec![];
    for entry in paths {
        let p = entry.path();
        if p == work {
            continue;
        }
        if entry.file_type().is_file() {
            if keep.iter().any(|k| p == k || p.starts_with(k)) {
                outputs.push(FileHash {
                    path: p.strip_prefix(work)?.to_owned(),
                    bytes: p.metadata()?.len(),
                    blake3: hash(p)?,
                });
            } else {
                std::fs::remove_file(p)?;
            }
        } else if entry.file_type().is_dir()
            && !keep.iter().any(|k| p == k || k.starts_with(p))
            && std::fs::read_dir(p)?.next().is_none()
        {
            std::fs::remove_dir(p)?;
        }
    }
    outputs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(outputs)
}
fn publish(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("analysis parent")?)?;
    tmp.write_all(data)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub async fn prepare(
    root: &Path,
    v: &str,
    offline: bool,
    force: bool,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Prepared> {
    let start = Instant::now();
    variant(v)?;
    let root = root.canonicalize()?;
    let parent = root.join(".wex/analysis").join(v);
    std::fs::create_dir_all(&parent)?;
    let _guard = guard(parent.join("preparing.lock")).await?;
    // Capture configuration after acquiring the preparation lease, not before
    // waiting behind a different preparation of a potentially different tree.
    let config_bytes = std::fs::read(root.join("wex.json"))?;
    let config_hash = blake3::hash(&config_bytes).to_hex().to_string();
    let cfg = native::Config::from_bytes(&config_bytes)?;
    source_roots(&root, &cfg)?;
    event(RunEvent::Phase(
        "Resolving native analysis dependencies · no Gradle".into(),
    ));
    let r = root.clone();
    tokio::task::spawn_blocking(move || {
        native::resolve_project(&r, &native::default_cache(), offline)
    })
    .await??;
    let compiler = build::prepare_tools(&root, offline).await?;
    let r = root.clone();
    let c = cfg.clone();
    let cp = compiler.clone();
    let vv = v.to_owned();
    let fp = tokio::task::spawn_blocking(move || fingerprint(&r, &c, &cp, &vv)).await??;
    if !force && let Some((model, graph, historical)) = cached_model(&root, v, &fp) {
        ensure!(
            hash(&root.join("wex.json"))? == config_hash
                && fingerprint(&root, &native::Config::load(&root)?, &compiler, v)? == fp,
            "analysis inputs changed; cached model not published"
        );
        let bytes = model_bytes(&model)?;
        if historical {
            publish(&parent.join("current.json"), &bytes)?;
        } else {
            // Migrate a valid pre-index current receipt without regenerating it.
            index_model(&root, &model, &bytes)?;
        }
        event(RunEvent::Phase(
            if historical {
                "ANALYSIS_CACHE_HIT · restored prior Android model · no tool process"
            } else {
                "ANALYSIS_CACHE_HIT · Android symbols/model · no tool process"
            }
            .into(),
        ));
        return Ok(Prepared {
            result: "NATIVE_ANALYSIS_OK".into(),
            cache_hit: true,
            executed_generation_steps: 0,
            elapsed_ms: start.elapsed().as_millis(),
            graph,
            model,
        });
    }
    event(RunEvent::Phase(
        "Generating Android R/BuildConfig for K2 · no APK".into(),
    ));
    let mut compiled = build::compile_analysis(&root, v, offline, event).await?;
    ensure!(
        compiled.config_blake3 == config_hash,
        "config changed between analysis preparations"
    );
    ensure!(
        fingerprint(&root, &native::Config::load(&root)?, &compiler, v)? == fp,
        "analysis inputs changed; model not published"
    );
    if let Some(info) = &compiled.compose {
        ensure!(
            hash(&info.plugin_jar)? == info.plugin_blake3,
            "Compose analysis plugin changed while preparing"
        );
    }
    compiled.graph.fingerprint = fp.clone();
    compiled.graph.model_api = ABI.into();
    let outputs = prune(compiled.work.path(), &compiled.graph)?;
    let data = serde_json::to_vec(&compiled.graph)?;
    ensure!(data.len() <= 4 * 1024 * 1024, "analysis graph limit");
    let key = format!("model:{v}:{}", blake3::hash(&data).to_hex());
    cache(&root)?.put(&key, &data)?;
    let model = Model {
        schema: 2,
        backend: ABI.into(),
        root: root.clone(),
        variant: v.into(),
        fingerprint: fp,
        model_key: key,
        graph_blake3: blake3::hash(&data).to_hex().to_string(),
        generation: compiled.work.path().to_owned(),
        contexts: compiled.contexts,
        outputs,
        compiler_inputs: compiler,
        compose: compiled.compose,
        generation_steps: compiled.steps,
        generation_sdk_cache: compiled.sdk_cache,
        generation_resource_cache: compiled.resource_cache,
    };
    // A successful SDK command is not sufficient: verify the complete new graph,
    // inventory and immutable LMDB record before publishing its pointer too.
    validate(&root, v, &model.fingerprint, &model)?;
    let bytes = model_bytes(&model)?;
    index_model(&root, &model, &bytes)?;
    // The indexed receipt now owns this immutable generation. Even if updating
    // current.json fails, keep it reusable and preserve the previous pointer.
    let _generation = compiled.work.keep();
    publish(&parent.join("current.json"), &bytes)?;
    event(RunEvent::Phase(
        "NATIVE_ANALYSIS_OK · real Android symbols · source errors allowed · no APK/sign/install"
            .into(),
    ));
    Ok(Prepared {
        result: "NATIVE_ANALYSIS_OK".into(),
        cache_hit: false,
        executed_generation_steps: model.generation_steps.len(),
        elapsed_ms: start.elapsed().as_millis(),
        graph: compiled.graph,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Model, ProjectGraph) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let generation = root.join(".wex/analysis/debug/generations/one");
        std::fs::create_dir_all(generation.join("module-generated/app")).unwrap();
        std::fs::create_dir_all(generation.join("symbols")).unwrap();
        std::fs::create_dir_all(generation.join("module-generated-classes/app")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("wex.json"), serde_json::to_vec(&serde_json::json!({"schema":1,"name":"unit model","repositories":["https://repo.maven.apache.org/maven2"],"android":{"compile_sdk":36,"min_sdk":26,"target_sdk":36,"application_id":"dev.test"},"build":{"namespace":"dev.test"},"modules":[{"name":"app","sources":["src"],"dependencies":[]}]})).unwrap()).unwrap();
        std::fs::write(generation.join("symbols/app.jar"), b"real-test-bytes").unwrap();
        let mut graph = ProjectGraph {
            schema: 2,
            root: root.clone(),
            selected_variant: "debug".into(),
            fingerprint: "fingerprint".into(),
            model_api: ABI.into(),
            model_version: "1".into(),
            dependency_stamps: vec![],
            modules: vec![],
        };
        graph.modules.push(crate::project::Module {
            name: ":app".into(),
            selected_variant: "debug".into(),
            source_roots: vec![root.join("src")],
            namespace: "dev.test".into(),
            project_type: "APPLICATION".into(),
            application_id: "dev.test".into(),
            generated_source_roots: vec![generation.join("module-generated/app")],
            classpath: vec![generation.join("symbols/app.jar")],
            classes_roots: vec![generation.join("module-generated-classes/app")],
            ..Default::default()
        });
        let bytes = serde_json::to_vec(&graph).unwrap();
        cache(&root).unwrap().put("model", &bytes).unwrap();
        let model = Model {
            schema: 2,
            backend: ABI.into(),
            root: root.clone(),
            variant: "debug".into(),
            fingerprint: "fingerprint".into(),
            model_key: "model".into(),
            graph_blake3: blake3::hash(&bytes).to_hex().to_string(),
            generation: generation.clone(),
            contexts: vec![build::AnalysisContext {
                root_module: "app".into(),
                modules: vec!["app".into()],
                closure: vec!["app".into()],
                directory: generation.clone(),
            }],
            outputs: vec![FileHash {
                path: "symbols/app.jar".into(),
                bytes: 15,
                blake3: hash(&generation.join("symbols/app.jar")).unwrap(),
            }],
            compiler_inputs: vec![],
            compose: None,
            generation_steps: vec![],
            generation_sdk_cache: vec![],
            generation_resource_cache: vec![],
        };
        (tmp, model, graph)
    }
    #[test]
    fn cached_model_is_exact_root_variant_graph_and_inventory() {
        let (tmp, model, graph) = fixture();
        assert_eq!(
            validate(tmp.path(), "debug", "fingerprint", &model).unwrap(),
            graph
        );
        assert!(validate(tmp.path(), "release", "fingerprint", &model).is_err());
        assert!(validate(tmp.path(), "debug", "changed", &model).is_err());
        let mut bad = model.clone();
        bad.root = tmp.path().join("other");
        assert!(validate(tmp.path(), "debug", "fingerprint", &bad).is_err());
    }
    #[test]
    fn context_omission_duplicate_assignment_and_foreign_directory_invalidate_model() {
        let (tmp, model, _) = fixture();
        let mut missing = model.clone();
        missing.contexts.clear();
        assert!(validate(tmp.path(), "debug", "fingerprint", &missing).is_err());
        let mut duplicate = model.clone();
        duplicate.contexts[0].modules.push("app".into());
        assert!(validate(tmp.path(), "debug", "fingerprint", &duplicate).is_err());
        let mut foreign = model.clone();
        foreign.contexts[0].directory = tmp.path().to_owned();
        assert!(validate(tmp.path(), "debug", "fingerprint", &foreign).is_err());
    }
    #[test]
    fn context_cannot_change_its_dependency_closure_or_claim_another_root() {
        let (tmp, model, _) = fixture();
        for (root, closure) in [("other", vec!["app".into()]), ("app", vec!["other".into()])] {
            let mut bad = model.clone();
            bad.contexts[0].root_module = root.into();
            bad.contexts[0].closure = closure;
            assert!(validate(tmp.path(), "debug", "fingerprint", &bad).is_err());
        }
    }
    #[test]
    fn configuration_added_module_cannot_hit_an_incomplete_cached_graph() {
        let (tmp, model, _) = fixture();
        let path = tmp.path().join("wex.json");
        let mut cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cfg["modules"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"name":"unused","sources":["src"],"dependencies":[]}));
        std::fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        assert!(validate(tmp.path(), "debug", "fingerprint", &model).is_err());
    }
    #[test]
    fn generated_symbol_tamper_and_unrecorded_java_injection_invalidate_cache() {
        let (tmp, model, _) = fixture();
        std::fs::write(
            model.generation.join("symbols/app.jar"),
            b"tampered-binary!",
        )
        .unwrap();
        assert!(validate(tmp.path(), "debug", "fingerprint", &model).is_err());
        std::fs::write(model.generation.join("symbols/app.jar"), b"real-test-bytes").unwrap();
        std::fs::write(
            model.generation.join("module-generated/app/Extra.java"),
            b"class Extra{}",
        )
        .unwrap();
        assert!(validate(tmp.path(), "debug", "fingerprint", &model).is_err());
    }
    #[test]
    fn missing_or_corrupt_lmdb_record_is_not_a_ready_model() {
        let (tmp, mut model, _) = fixture();
        model.model_key = "missing".into();
        assert!(validate(tmp.path(), "debug", "fingerprint", &model).is_err());
        model.model_key = "model".into();
        cache(tmp.path()).unwrap().put("model", b"{}").unwrap();
        assert!(validate(tmp.path(), "debug", "fingerprint", &model).is_err());
    }
    #[test]
    fn output_paths_and_duplicate_receipts_cannot_bypass_inventory() {
        let (tmp, model, _) = fixture();
        for path in ["../symbols.jar", "/tmp/symbols.jar"] {
            let mut m = model.clone();
            m.outputs[0].path = path.into();
            assert!(validate(tmp.path(), "debug", "fingerprint", &m).is_err());
        }
        let mut m = model.clone();
        m.outputs.push(m.outputs[0].clone());
        assert!(validate(tmp.path(), "debug", "fingerprint", &m).is_err());
    }
    #[test]
    fn pruning_preserves_empty_declared_roots_and_only_referenced_files() {
        let (_tmp, model, graph) = fixture();
        std::fs::create_dir_all(model.generation.join("transient/jdk")).unwrap();
        std::fs::write(model.generation.join("transient/jdk/test"), b"temp").unwrap();
        let files = prune(&model.generation, &graph).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("symbols/app.jar"));
        assert!(model.generation.join("module-generated/app").is_dir());
        assert!(!model.generation.join("transient").exists());
    }
    #[tokio::test]
    async fn preparation_guard_serializes_and_cleans_on_cancellation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("preparing.lock");
        let first = guard(path.clone()).await.unwrap();
        let pending = tokio::spawn(guard(path.clone()));
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!pending.is_finished());
        drop(first);
        let second = pending.await.unwrap().unwrap();
        assert!(path.is_file());
        drop(second);
        assert!(!path.exists());
        let task = tokio::spawn(async move {
            let _held = guard(path).await.unwrap();
            std::future::pending::<()>().await;
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        task.abort();
        let _ = task.await;
        assert!(!tmp.path().join("preparing.lock").exists());
    }
    #[test]
    fn same_length_file_edit_does_not_reuse_previous_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("input");
        std::fs::write(&p, b"one").unwrap();
        let before = hash(&p).unwrap();
        std::fs::write(&p, b"two").unwrap();
        assert_ne!(before, hash(&p).unwrap());
    }
    fn remember(root: &Path, model: &Model) {
        index_model(root, model, &model_bytes(model).unwrap()).unwrap();
    }
    #[test]
    fn historical_receipt_requires_an_absent_current_pointer() {
        let (tmp, model, graph) = fixture();
        remember(tmp.path(), &model);
        let pointer = tmp.path().join(".wex/analysis/debug/current.json");
        let mut different = model.clone();
        different.fingerprint = "another tree".into();

        // A missing pointer can be recovered from the verified historical index.
        let _ = std::fs::remove_file(&pointer);
        let (found, actual, historical) = cached_model(tmp.path(), "debug", "fingerprint").unwrap();
        assert!(historical);
        assert_eq!(found.generation, model.generation);
        assert_eq!(actual, graph);

        // Once a pointer exists, even malformed or stale bytes are an explicit
        // invalidation. Do not resurrect an older generation behind the caller's
        // back; prepare() must rebuild and publish a fresh pointer.
        for contents in [
            Some(b"{broken".to_vec()),
            Some(model_bytes(&different).unwrap()),
        ] {
            std::fs::write(&pointer, contents.unwrap()).unwrap();
            assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
        }
    }
    #[test]
    fn current_pointer_remains_preferred_and_old_receipts_are_compatible() {
        let (tmp, model, graph) = fixture();
        let pointer = tmp.path().join(".wex/analysis/debug/current.json");
        std::fs::write(pointer, model_bytes(&model).unwrap()).unwrap();
        let (_, actual, historical) = cached_model(tmp.path(), "debug", "fingerprint").unwrap();
        assert!(!historical);
        assert_eq!(actual, graph);
        // A pre-index valid pointer can be migrated through the exact bytes that
        // passed validation. It must not require a new generation or new ABI.
        remember(tmp.path(), &model);
        assert!(
            cache(tmp.path())
                .unwrap()
                .get(&receipt_key("debug", "fingerprint"))
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn repeated_index_backfill_does_not_rewrite_unchanged_lmdb_pages() {
        let (tmp, model, _) = fixture();
        remember(tmp.path(), &model);
        let data = tmp.path().join(".wex/analysis/models.lmdb/data.mdb");
        let before = std::fs::read(&data).unwrap();
        remember(tmp.path(), &model);
        assert_eq!(before, std::fs::read(data).unwrap());
    }
    #[test]
    fn indexed_metadata_does_not_override_root_variant_graph_or_context_validation() {
        let (tmp, model, _) = fixture();
        for i in 0..8 {
            let mut bad = model.clone();
            match i {
                0 => bad.root = tmp.path().join("foreign"),
                1 => bad.variant = "release".into(),
                2 => bad.fingerprint = "different".into(),
                3 => bad.graph_blake3 = "invalid".into(),
                4 => bad.contexts.clear(),
                5 => bad.outputs.clear(),
                6 => bad.model_key = "missing graph".into(),
                7 => bad.schema = 99,
                _ => unreachable!(),
            }
            cache(tmp.path())
                .unwrap()
                .put(
                    &receipt_key("debug", "fingerprint"),
                    &model_bytes(&bad).unwrap(),
                )
                .unwrap();
            assert!(
                cached_model(tmp.path(), "debug", "fingerprint").is_none(),
                "case {i}"
            );
        }
    }
    #[test]
    fn corrupt_or_oversized_index_record_is_a_miss_not_a_model() {
        let (tmp, _, _) = fixture();
        for bytes in [b"{malformed".to_vec(), vec![b' '; 4 * 1024 * 1024 + 1]] {
            cache(tmp.path())
                .unwrap()
                .put(&receipt_key("debug", "fingerprint"), &bytes)
                .unwrap();
            assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
        }
    }
    #[test]
    fn historical_generation_tamper_injection_and_removal_are_misses() {
        let (tmp, model, _) = fixture();
        remember(tmp.path(), &model);
        let jar = model.generation.join("symbols/app.jar");
        std::fs::write(&jar, b"tampered-binary!").unwrap();
        assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
        std::fs::write(jar, b"real-test-bytes").unwrap();
        let extra = model.generation.join("module-generated/app/Extra.java");
        std::fs::write(&extra, b"class Extra {}").unwrap();
        assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
        std::fs::remove_file(extra).unwrap();
        assert!(cached_model(tmp.path(), "debug", "fingerprint").is_some());
        std::fs::remove_dir_all(&model.generation).unwrap();
        assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
    }
    #[test]
    fn historical_cache_does_not_cross_variants_or_input_fingerprints() {
        let (tmp, model, _) = fixture();
        remember(tmp.path(), &model);
        assert!(cached_model(tmp.path(), "release", "fingerprint").is_none());
        assert!(cached_model(tmp.path(), "debug", "other").is_none());
    }
    #[test]
    fn historical_context_validation_uses_actual_declared_modules() {
        let (tmp, model, _) = fixture();
        remember(tmp.path(), &model);
        let path = tmp.path().join("wex.json");
        let mut cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        cfg["modules"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"name":"new","sources":["src"],"dependencies":[]}));
        std::fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        assert!(cached_model(tmp.path(), "debug", "fingerprint").is_none());
    }
    #[test]
    fn receipt_replacement_selects_latest_verified_generation_without_deleting_old() {
        let (tmp, model, graph) = fixture();
        remember(tmp.path(), &model);
        let newer = model.generation.with_file_name("forced");
        for dir in [
            "module-generated/app",
            "module-generated-classes/app",
            "symbols",
        ] {
            std::fs::create_dir_all(newer.join(dir)).unwrap();
        }
        std::fs::copy(
            model.generation.join("symbols/app.jar"),
            newer.join("symbols/app.jar"),
        )
        .unwrap();
        let old = model.generation.to_string_lossy();
        let replacement = newer.to_string_lossy();
        let mut updated: Model = serde_json::from_str(
            &serde_json::to_string(&model)
                .unwrap()
                .replace(old.as_ref(), replacement.as_ref()),
        )
        .unwrap();
        let graph: ProjectGraph = serde_json::from_str(
            &serde_json::to_string(&graph)
                .unwrap()
                .replace(old.as_ref(), replacement.as_ref()),
        )
        .unwrap();
        let data = serde_json::to_vec(&graph).unwrap();
        updated.model_key = "new graph".into();
        updated.graph_blake3 = blake3::hash(&data).to_hex().to_string();
        cache(tmp.path())
            .unwrap()
            .put(&updated.model_key, &data)
            .unwrap();
        validate(tmp.path(), "debug", "fingerprint", &updated).unwrap();
        remember(tmp.path(), &updated);
        let (actual, _, historical) = cached_model(tmp.path(), "debug", "fingerprint").unwrap();
        assert!(historical);
        assert_eq!(actual.generation, newer);
        assert!(model.generation.is_dir());
    }
}
