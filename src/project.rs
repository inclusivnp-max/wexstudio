//! Project discovery and content-addressed Gradle graph cache. Discovery != Gradle sync.
use crate::cache::DiskCache;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectGraph {
    pub schema: u32,
    pub root: PathBuf,
    pub fingerprint: String,
    pub modules: Vec<Module>,
    pub selected_variant: String,
    #[serde(default)]
    pub model_api: String,
    #[serde(default)]
    pub model_version: String,
    #[serde(default)]
    pub dependency_stamps: Vec<FileStamp>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Module {
    pub name: String,
    pub source_roots: Vec<PathBuf>,
    pub classpath: Vec<PathBuf>,
    pub variants: Vec<String>,
    #[serde(default)]
    pub directory: PathBuf,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub project_type: String,
    #[serde(default)]
    pub resource_roots: Vec<PathBuf>,
    #[serde(default)]
    pub generated_source_roots: Vec<PathBuf>,
    #[serde(default)]
    pub generated_resource_roots: Vec<PathBuf>,
    #[serde(default)]
    pub boot_classpath: Vec<PathBuf>,
    #[serde(default)]
    pub classes_roots: Vec<PathBuf>,
    #[serde(default)]
    pub module_dependencies: Vec<String>,
    #[serde(default)]
    pub selected_variant: String,
    #[serde(default)]
    pub assemble_task: String,
    #[serde(default)]
    pub application_id: String,
    #[serde(default)]
    pub apk_listing_file: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileStamp {
    pub path: PathBuf,
    pub length: u64,
    pub modified_nanos: u128,
}
impl FileStamp {
    pub fn read(path: &Path) -> Result<Self> {
        let m = std::fs::metadata(path)?;
        ensure!(
            m.is_file(),
            "classpath entry is not a file: {}",
            path.display()
        );
        Ok(Self {
            path: path.to_owned(),
            length: m.len(),
            modified_nanos: m
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
        })
    }
}
fn ignored(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0
        && [".git", ".gradle", ".wex", "build", "target", "node_modules"]
            .iter()
            .any(|v| entry.file_name() == *v)
}
static DISCOVERIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn discovery_calls() -> u64 {
    DISCOVERIES.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn source_files(root: &Path) -> Vec<PathBuf> {
    DISCOVERIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut files = walkdir::WalkDir::new(root)
        .max_depth(20)
        .into_iter()
        .filter_entry(|e| !ignored(e))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("kt" | "kts" | "java" | "xml")
            )
        })
        .take(10000)
        .collect::<Vec<_>>();
    files.sort();
    files
}
/// Independent tree inventory, including empty folders/config/text; compiler source scope is unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplorerInventory {
    pub entries: Vec<ExplorerEntry>,
    pub package_roots: Vec<(PathBuf, String)>,
    pub partial: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub directory: bool,
}
pub fn explorer_entries(root: &Path) -> ExplorerInventory {
    explorer_entries_until(root, || false)
}
pub fn explorer_entries_until(root: &Path, cancel: impl Fn() -> bool) -> ExplorerInventory {
    DISCOVERIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut result = ExplorerInventory::default();
    if let Ok(config) = crate::native::Config::load(root) {
        for module in &config.modules {
            for source in &module.sources {
                result.package_roots.push((
                    root.join(source),
                    if module.namespace.is_empty() {
                        config.build.namespace.clone()
                    } else {
                        module.namespace.clone()
                    },
                ));
            }
        }
    }
    for entry in walkdir::WalkDir::new(root)
        .max_depth(20)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !ignored(e))
    {
        if cancel() {
            result.error = Some("Project scan cancelled".into());
            return result;
        }
        let e = match entry {
            Ok(e) => e,
            Err(e) => {
                result.error = Some(e.to_string());
                continue;
            }
        };
        if e.depth() == 0 {
            continue;
        }
        if result.entries.len() == 10000 {
            result.partial = true;
            break;
        }
        if e.depth() == 20 && e.file_type().is_dir() {
            result.partial = true;
        }
        if e.file_type().is_file() || e.file_type().is_dir() {
            result.entries.push(ExplorerEntry {
                path: e.path().to_owned(),
                directory: e.file_type().is_dir(),
            });
        }
    }
    result
}
impl ExplorerInventory {
    pub fn sources(&self) -> Vec<PathBuf> {
        let mut files = self
            .entries
            .iter()
            .filter(|e| {
                !e.directory
                    && matches!(
                        e.path.extension().and_then(|e| e.to_str()),
                        Some("kt" | "kts" | "java" | "xml")
                    )
            })
            .map(|e| e.path.clone())
            .collect::<Vec<_>>();
        files.sort();
        files
    }
}
/// Includes buildSrc/build-logic, version catalogs, wrapper and dependency lockfiles.
pub fn fingerprint(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .max_depth(30)
        .into_iter()
        .filter_entry(|e| !ignored(e))
    {
        let e = entry?;
        if !e.file_type().is_file() {
            continue;
        }
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy();
        let rel = p.strip_prefix(root)?;
        let in_logic = rel
            .components()
            .any(|c| c.as_os_str() == "buildSrc" || c.as_os_str() == "build-logic");
        if in_logic
            || name.ends_with(".gradle")
            || name.ends_with(".gradle.kts")
            || name.ends_with(".versions.toml")
            || name.ends_with(".lockfile")
            || name == "gradle.properties"
            || name == "local.properties"
            || name == "gradle-wrapper.properties"
            || name == "gradle-wrapper.jar"
        {
            files.push(p.to_owned());
        }
    }
    files.sort();
    let mut h = blake3::Hasher::new();
    for p in files {
        let rel = p.strip_prefix(root)?.to_string_lossy();
        let data = std::fs::read(&p)?;
        h.update(&(rel.len() as u64).to_le_bytes());
        h.update(rel.as_bytes());
        h.update(&(data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    // Environment and global Gradle properties also affect resolved SDK/classpaths.
    for name in [
        "JAVA_HOME",
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "GRADLE_USER_HOME",
    ] {
        h.update(name.as_bytes());
        h.update(
            std::env::var_os(name)
                .unwrap_or_default()
                .as_encoded_bytes(),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        let gradle = std::env::var_os("GRADLE_USER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".gradle"));
        for name in ["gradle.properties", "init.gradle", "init.gradle.kts"] {
            if let Ok(data) = std::fs::read(gradle.join(name)) {
                h.update(name.as_bytes());
                h.update(&data);
            }
        }
        if gradle.join("init.d").exists() {
            for e in walkdir::WalkDir::new(gradle.join("init.d")).sort_by_file_name() {
                let e = e?;
                if e.file_type().is_file() {
                    h.update(e.path().as_os_str().as_encoded_bytes());
                    h.update(&std::fs::read(e.path())?);
                }
            }
        }
    }
    h.update(b"wex-android-v2-cache-abi-4");
    Ok(h.finalize().to_hex().to_string())
}
pub fn cache_graph(cache: &DiskCache, graph: &ProjectGraph) -> Result<()> {
    ensure!(graph.schema == 2, "unsupported graph schema");
    cache.put(
        &format!("gradle:graph:{}", graph.selected_variant),
        &serde_json::to_vec(graph)?,
    )
}
pub fn cached_graph(cache: &DiskCache, root: &Path) -> Result<Option<ProjectGraph>> {
    cached_variant(cache, root, "debug")
}
pub fn cached_variant(
    cache: &DiskCache,
    root: &Path,
    variant: &str,
) -> Result<Option<ProjectGraph>> {
    let Some(bytes) = cache.get(&format!("gradle:graph:{variant}"))? else {
        return Ok(None);
    };
    // Corrupted or outdated data is a cache miss, not an editor startup failure.
    let Ok(graph) = serde_json::from_slice::<ProjectGraph>(&bytes) else {
        return Ok(None);
    };
    if graph.schema != 2
        || graph.root != root.canonicalize()?
        || graph.selected_variant != variant
        || graph.fingerprint != fingerprint(root)?
    {
        return Ok(None);
    }
    if graph
        .dependency_stamps
        .iter()
        .any(|s| !matches!(FileStamp::read(&s.path),Ok(actual) if actual==*s))
    {
        return Ok(None);
    }
    Ok(Some(graph))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_ignores_code_but_detects_gradle() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::write(p.join("build.gradle.kts"), "plugins {}").unwrap();
        let a = fingerprint(p).unwrap();
        std::fs::write(p.join("Main.kt"), "fun main() {}").unwrap();
        assert_eq!(a, fingerprint(p).unwrap());
        std::fs::write(p.join("build.gradle.kts"), "plugins { kotlin(\"jvm\") }").unwrap();
        assert_ne!(a, fingerprint(p).unwrap());
    }
    #[test]
    fn version_catalog_and_build_logic_invalidate() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("buildSrc")).unwrap();
        let a = fingerprint(d.path()).unwrap();
        std::fs::write(d.path().join("buildSrc/Logic.kt"), "val x = 1").unwrap();
        assert_ne!(a, fingerprint(d.path()).unwrap());
    }
    #[test]
    fn graph_stale_after_build_change() {
        let d = tempfile::tempdir().unwrap();
        let c = DiskCache::open(&d.path().join(".wex")).unwrap();
        let root = d.path().canonicalize().unwrap();
        let g = ProjectGraph {
            schema: 2,
            root: root.clone(),
            fingerprint: fingerprint(&root).unwrap(),
            modules: vec![],
            selected_variant: "debug".into(),
            model_api: String::new(),
            model_version: String::new(),
            dependency_stamps: vec![],
        };
        cache_graph(&c, &g).unwrap();
        assert_eq!(cached_graph(&c, &root).unwrap(), Some(g));
        std::fs::write(root.join("settings.gradle.kts"), "include(\":app\")").unwrap();
        assert!(cached_graph(&c, &root).unwrap().is_none());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    fn graph(root: &Path, variant: &str) -> ProjectGraph {
        ProjectGraph {
            schema: 2,
            root: root.canonicalize().unwrap(),
            fingerprint: fingerprint(root).unwrap(),
            modules: vec![],
            selected_variant: variant.into(),
            model_api: String::new(),
            model_version: String::new(),
            dependency_stamps: vec![],
        }
    }
    #[test]
    fn corrupted_record_is_a_miss() {
        let d = tempfile::tempdir().unwrap();
        let c = DiskCache::open(&d.path().join(".wex")).unwrap();
        c.put("gradle:graph:debug", b"not json").unwrap();
        assert!(cached_graph(&c, d.path()).unwrap().is_none());
    }
    #[test]
    fn variants_have_independent_cache_keys() {
        let d = tempfile::tempdir().unwrap();
        let c = DiskCache::open(&d.path().join(".wex")).unwrap();
        cache_graph(&c, &graph(d.path(), "release")).unwrap();
        assert!(cached_graph(&c, d.path()).unwrap().is_none());
        assert!(cached_variant(&c, d.path(), "release").unwrap().is_some());
    }
    #[test]
    fn missing_classpath_invalidates_cache() {
        let d = tempfile::tempdir().unwrap();
        let dep = d.path().join("dependency.jar");
        std::fs::write(&dep, b"fixture").unwrap();
        let c = DiskCache::open(&d.path().join(".wex")).unwrap();
        let mut g = graph(d.path(), "debug");
        g.dependency_stamps.push(FileStamp::read(&dep).unwrap());
        cache_graph(&c, &g).unwrap();
        assert!(cached_graph(&c, d.path()).unwrap().is_some());
        std::fs::remove_file(dep).unwrap();
        assert!(cached_graph(&c, d.path()).unwrap().is_none());
    }
}

#[cfg(test)]
mod explorer_tests {
    use super::*;
    #[test]
    fn tree_includes_empty_folders_and_config_without_broadening_compiler_sources() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("empty")).unwrap();
        std::fs::create_dir(d.path().join(".wex")).unwrap();
        std::fs::write(d.path().join(".wex/hidden.kt"), "x").unwrap();
        std::fs::write(d.path().join("wex.json"), "{}").unwrap();
        std::fs::write(d.path().join("A.kt"), "val a=1").unwrap();
        let tree = explorer_entries(d.path());
        assert!(!tree.partial && tree.error.is_none());
        assert_eq!(tree.entries.len(), 3);
        assert!(
            tree.entries
                .iter()
                .any(|e| e.directory && e.path.ends_with("empty"))
        );
        assert_eq!(tree.sources(), source_files(d.path()));
        assert_eq!(tree.sources(), vec![d.path().join("A.kt")]);
    }
}
