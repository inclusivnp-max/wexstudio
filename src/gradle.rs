//! Android Tooling API bridge with a disk-cache fast path that never launches Java.
use crate::{
    cache::DiskCache,
    project::{self, FileStamp, Module, ProjectGraph},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub variant: String,
    pub force: bool,
    pub offline: bool,
    pub bridge: PathBuf,
}
impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            variant: "debug".into(),
            force: false,
            offline: false,
            bridge: std::env::var_os("WEX_GRADLE_BRIDGE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    crate::runtime_paths::paths()
                        .support
                        .join("scripts/gradle-bridge.sh")
                }),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncResult {
    pub cache_hit: bool,
    pub elapsed_ms: u128,
    pub graph: ProjectGraph,
}
#[derive(Deserialize)]
struct BridgeResult {
    modules: Vec<Module>,
    model_api: String,
    model_version: String,
}
pub async fn sync_project(root: &Path, options: &SyncOptions) -> Result<SyncResult> {
    let start = Instant::now();
    let root = root.canonicalize()?;
    ensure!(
        options
            .variant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !options.variant.is_empty(),
        "invalid Android variant"
    );
    let r = root.clone();
    let variant = options.variant.clone();
    let force = options.force;
    let cached = tokio::task::spawn_blocking(move || -> Result<Option<ProjectGraph>> {
        if force {
            return Ok(None);
        }
        let cache = DiskCache::open(&r.join(".wex/index"))?;
        project::cached_variant(&cache, &r, &variant)
    })
    .await??;
    if let Some(graph) = cached {
        return Ok(SyncResult {
            cache_hit: true,
            elapsed_ms: start.elapsed().as_millis(),
            graph,
        });
    }
    let r = root.clone();
    let before = tokio::task::spawn_blocking(move || project::fingerprint(&r)).await??;
    let installation = cached_distribution(&root)?
        .context("Gradle bridge requires an already installed project distribution")?;
    let mut command = tokio::process::Command::new(&options.bridge);
    command.env("WEX_TOOLS_HOME", &crate::runtime_paths::paths().tools);
    command
        .env("WEX_CACHED_GRADLE", installation)
        .arg(&root)
        .arg(&options.variant)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    if options.offline {
        command.arg("--offline");
    }
    let mut child = command
        .spawn()
        .context("starting Gradle Tooling API bridge")?;
    let mut stdout = child.stdout.take().context("missing bridge stdout")?;
    let (status, bytes) = tokio::time::timeout(Duration::from_secs(300), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "Android model response too large"
        );
        Ok::<_, anyhow::Error>((child.wait().await?, bytes))
    })
    .await
    .context("Gradle model sync timed out")??;
    ensure!(
        status.success(),
        "Gradle Tooling API bridge exited with {status}"
    );
    let result: BridgeResult =
        serde_json::from_slice(&bytes).context("invalid Android model JSON from bridge")?;
    ensure!(!result.modules.is_empty(), "no Android modules returned");
    let r = root.clone();
    let after = tokio::task::spawn_blocking(move || project::fingerprint(&r)).await??;
    ensure!(
        before == after,
        "Gradle inputs changed during sync; repeat synchronization"
    );
    let mut stamps = Vec::new();
    for module in &result.modules {
        ensure!(
            module.selected_variant == options.variant,
            "bridge returned wrong variant"
        );
        for file in module.boot_classpath.iter().chain(&module.classpath) {
            // Generated project outputs may not exist before the first build. External artifacts must.
            if !file.starts_with(&root) {
                stamps.push(
                    FileStamp::read(file)
                        .with_context(|| format!("invalid dependency {}", file.display()))?,
                );
            }
        }
    }
    stamps.sort_by(|a, b| a.path.cmp(&b.path));
    stamps.dedup_by(|a, b| a.path == b.path);
    let graph = ProjectGraph {
        schema: 2,
        root: root.clone(),
        fingerprint: after,
        modules: result.modules,
        selected_variant: options.variant.clone(),
        model_api: result.model_api,
        model_version: result.model_version,
        dependency_stamps: stamps,
    };
    let g = graph.clone();
    tokio::task::spawn_blocking(move || {
        let cache = DiskCache::open(&root.join(".wex/index"))?;
        project::cache_graph(&cache, &g)
    })
    .await??;
    Ok(SyncResult {
        cache_hit: false,
        elapsed_ms: start.elapsed().as_millis(),
        graph,
    })
}

/// Never provision Gradle: compatibility only uses an already unpacked wrapper distribution.
pub fn cached_distribution(root: &Path) -> Result<Option<PathBuf>> {
    let properties = root.join("gradle/wrapper/gradle-wrapper.properties");
    if !properties.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(properties)?;
    let raw = text
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .find(|(k, _)| k.trim() == "distributionUrl")
        .context("wrapper distributionUrl missing")?
        .1
        .trim()
        .replace("\\:", ":");
    let url = url::Url::parse(&raw)?;
    let file = url
        .path_segments()
        .and_then(|mut p| p.next_back())
        .context("invalid Gradle distribution URL")?;
    let name = file
        .strip_suffix(".zip")
        .context("expected Gradle distribution ZIP")?;
    let version = name
        .strip_prefix("gradle-")
        .and_then(|s| s.strip_suffix("-bin").or_else(|| s.strip_suffix("-all")))
        .context("unsupported Gradle distribution name")?;
    ensure!(
        !version.is_empty()
            && version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
        "invalid Gradle version"
    );
    let home = std::env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".gradle")
        });
    let distributions = home.join("wrapper/dists").join(name);
    if let Ok(dirs) = std::fs::read_dir(distributions) {
        for d in dirs.flatten() {
            let install = d.path().join(format!("gradle-{version}"));
            if install.join("bin/gradle").is_file() {
                return Ok(Some(install));
            }
        }
    }
    anyhow::bail!(
        "Gradle {version} is not already installed; distribution download is disabled. Use wex.json/native resolve"
    )
}

#[cfg(test)]
mod provisioning_tests {
    use super::*;
    #[test]
    fn absent_distribution_is_an_error_not_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("gradle/wrapper");
        std::fs::create_dir_all(&wrapper).unwrap();
        std::fs::write(wrapper.join("gradle-wrapper.properties"),"distributionUrl=https\\://services.gradle.org/distributions/gradle-0.0.0-wex-no-download-bin.zip\n").unwrap();
        assert!(
            cached_distribution(dir.path())
                .unwrap_err()
                .to_string()
                .contains("download is disabled")
        );
    }
}
