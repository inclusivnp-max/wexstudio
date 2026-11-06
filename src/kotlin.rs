//! Actual JetBrains K2 LSP, importing the cached Android graph as workspace.json.
pub use crate::k2_cache::Record as IndexCacheRecord;
use crate::{editor::Document, lsp::LspClient, project::ProjectGraph};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub message: String,
    #[serde(default)]
    pub severity: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub insert_text: Option<String>,
    #[serde(default)]
    pub text_edit: Option<Value>,
    #[serde(default)]
    pub additional_text_edits: Vec<Value>,
    #[serde(default)]
    pub insert_text_format: Option<u32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
pub fn file_uri(path: &Path) -> Result<String> {
    Ok(Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("invalid file path: {}", path.display()))?
        .to_string())
}
pub fn uri_path(uri: &str) -> Result<PathBuf> {
    Url::parse(uri)?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("definition is not a local file URI"))
}
pub fn default_server() -> PathBuf {
    std::env::var_os("WEX_KOTLIN_LSP")
        .map(|p| crate::runtime_paths::absolute(&PathBuf::from(p)))
        .unwrap_or_else(|| {
            crate::runtime_paths::paths()
                .tools
                .join("tools/kotlin-lsp/kotlin-server-263.6379.0/bin/intellij-server")
        })
}
fn java_home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("JAVA_HOME") {
        return PathBuf::from(path)
            .canonicalize()
            .context("Kotlin workspace JDK directory missing");
    }
    #[cfg(unix)]
    {
        let java = std::fs::canonicalize("/usr/bin/java")?;
        Ok(java
            .parent()
            .and_then(Path::parent)
            .context("missing Java home")?
            .to_owned())
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("set JAVA_HOME for the Kotlin workspace SDK")
    }
}
pub fn workspace_model(graph: &ProjectGraph, jdk: &Path) -> Result<Value> {
    let mut libraries = BTreeMap::new();
    let mut modules = Vec::new();
    for m in &graph.modules {
        let mut deps = vec![
            json!({"type":"moduleSource"}),
            json!({"type":"sdk","name":"Wex JDK","kind":"JavaSDK"}),
        ];
        for name in &m.module_dependencies {
            deps.push(json!({"type":"module","name":name,"scope":"compile","isExported":true}));
        }
        for path in m.boot_classpath.iter().chain(&m.classpath) {
            if !path.is_file() {
                continue;
            } // R.jar can be absent until resource generation.
            let name = format!("Wex: {}", path.display());
            deps.push(json!({"type":"library","name":name,"scope":"compile"}));
            libraries.insert(
                name.clone(),
                json!({"name":name,"type":"COMPILE","roots":[{"path":path}]}),
            );
        }
        let mut content = Vec::new();
        for (path, _generated) in m
            .source_roots
            .iter()
            .map(|p| (p, false))
            .chain(m.generated_source_roots.iter().map(|p| (p, true)))
        {
            content.push(json!({"path":path,"sourceRoots":[{"path":path,"type":"java-source"}]}));
        }
        modules.push(json!({"name":m.name,"dependencies":deps,"contentRoots":content}));
    }
    Ok(
        json!({"modules":modules,"libraries":libraries.into_values().collect::<Vec<_>>(),"sdks":[{"name":"Wex JDK","type":"JavaSDK","homePath":jdk,"version":null,"additionalData":""}]}),
    )
}
fn compose_settings(
    config: &crate::native::Config,
    module: &crate::project::Module,
    info: &crate::build::ComposeBuild,
) -> Value {
    // Kotlin LSP's root kotlinSettings uses the JPS J{...} compiler-argument format,
    // not arbitrary module facets or -Xplugin command-line text.
    let compiler = json!({"jvmTarget":config.build.jvm_target.to_string(),"pluginOptions":[format!("plugin:androidx.compose.compiler.plugins.kotlin:sourceInformation={}",info.source_information)],"pluginClasspaths":[info.plugin_jar]});
    json!({"name":"Kotlin","sourceRoots":module.source_roots.iter().chain(&module.generated_source_roots).collect::<Vec<_>>(),"configFileItems":[],"module":module.name,"useProjectSettings":false,"implementedModuleNames":[],"dependsOnModuleNames":[],"additionalVisibleModuleNames":[],"productionOutputPath":null,"testOutputPath":null,"sourceSetNames":[],"isTestModule":false,"externalProjectId":"","isHmppEnabled":false,"pureKotlinSourceFolders":[],"kind":"default","compilerArguments":format!("J{compiler}"),"additionalArguments":null,"scriptTemplates":null,"scriptTemplatesClasspath":null,"copyJsLibraryFiles":false,"outputDirectoryForJsLibraryFiles":null,"targetPlatform":null,"externalSystemRunTasks":[],"version":5,"flushNeeded":false})
}
pub fn export_workspace(root: &Path, graph: &ProjectGraph) -> Result<PathBuf> {
    let workspace = root.join(".wex/kotlin-workspace");
    export_workspace_to(root, graph, &workspace)?;
    Ok(workspace)
}
fn export_workspace_to(root: &Path, graph: &ProjectGraph, workspace: &Path) -> Result<()> {
    let data = workspace_bytes(root, graph)?;
    write_workspace(workspace, &data)
}
fn workspace_bytes(root: &Path, graph: &ProjectGraph) -> Result<Vec<u8>> {
    ensure!(
        graph.root == root.canonicalize()?,
        "graph belongs to a different project"
    );
    let mut model = workspace_model(graph, &java_home()?)?;
    if root.join("wex.json").is_file() {
        let config = crate::native::Config::load(root)?;
        if config.build.compose.is_some() {
            let info = if graph.model_api == crate::analysis::MODEL_API {
                crate::analysis::compose_for_graph(root, graph)?
                    .context("Compose analysis configuration missing")?
            } else {
                let report: crate::build::BuildReport = serde_json::from_slice(&std::fs::read(
                    root.join(".wex/native")
                        .join(&graph.selected_variant)
                        .join("current.json"),
                )?)?;
                report
                    .compose
                    .context("Compose build configuration missing")?
            };
            ensure!(
                info.compiler_version == config.build.kotlin_version
                    && blake3::hash(&std::fs::read(&info.plugin_jar)?)
                        .to_hex()
                        .as_str()
                        == info.plugin_blake3,
                "Compose analysis plugin differs from verified native compiler"
            );
            model["kotlinSettings"] = json!(
                graph
                    .modules
                    .iter()
                    .map(|module| compose_settings(&config, module, &info))
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(serde_json::to_vec_pretty(&model)?)
}
fn write_workspace(workspace: &Path, data: &[u8]) -> Result<()> {
    std::fs::create_dir_all(workspace)?;
    let path = workspace.join("workspace.json");
    if std::fs::read(&path).ok().as_deref() != Some(data) {
        let tmp = workspace.join("workspace.json.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(tmp, &path)?;
    }
    Ok(())
}
fn credential_env(root: &Path) -> Vec<String> {
    crate::native::Config::load(root)
        .ok()
        .and_then(|c| c.build.release_signing)
        .map(|s| vec![s.store_password_env, s.key_password_env])
        .unwrap_or_default()
}
pub struct KotlinSession {
    pub client: LspClient,
    pub capabilities: Value,
    pub workspace: PathBuf,
    pub index_cache: IndexCacheRecord,
    _owned: Option<tempfile::TempDir>,
    _system: Option<tempfile::TempDir>,
    index: Option<crate::k2_cache::Slot>,
    index_ready: bool,
}
impl KotlinSession {
    pub async fn start(root: &Path, graph: &ProjectGraph, server: &Path) -> Result<Self> {
        ensure!(
            server.is_file(),
            "Kotlin server missing: run scripts/install-kotlin-lsp.py or set WEX_KOTLIN_LSP"
        );
        let workspace = export_workspace(root, graph)?;
        let server = server.canonicalize()?;
        let args = vec![
            "--stdio".into(),
            "--system-path".into(),
            std::env::var_os("WEX_K2_SYSTEM_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(".wex/k2-system"))
                .to_string_lossy()
                .into_owned(),
            "--log-level".into(),
            "INFO".into(),
        ];
        let mut client =
            LspClient::spawn_filtered(&server, &args, &workspace, &credential_env(root))?;
        client.set_request_timeout(Duration::from_secs(180));
        let capabilities = client.initialize(&file_uri(&workspace)?).await?;
        Ok(Self {
            client,
            capabilities,
            index_cache: IndexCacheRecord {
                status: "legacy".into(),
                key: None,
                workspace: workspace.clone(),
                system: PathBuf::from(&args[2]),
                reused_files: 0,
                detail: None,
            },
            workspace,
            _owned: None,
            _system: None,
            index: None,
            index_ready: false,
        })
    }
    /// A stopped, verified profile may be reused. Live or unsealed profiles are
    /// NEVER shared: nonblocking leases fall back to private candidate directories.
    pub async fn start_isolated(root: &Path, graph: &ProjectGraph, server: &Path) -> Result<Self> {
        ensure!(server.is_file(), "Kotlin server missing");
        let canonical = root.canonicalize()?;
        let root = canonical.as_path();
        let server = server.canonicalize()?;
        let data = workspace_bytes(root, graph)?;
        let removed = credential_env(root);
        let base = std::env::var_os("WEX_K2_SYSTEM_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".wex/k2-profiles"));
        let cache = base.join("wex-index-cache/k2-profile-v1");
        let g = graph.clone();
        let s = server.clone();
        let jdk = java_home()?;
        let bytes = data.clone();
        let env = removed.clone();
        let choice = tokio::task::spawn_blocking(move || {
            let inputs = crate::k2_cache::Inputs::capture(&g, &s, &jdk, &bytes, &env)?;
            crate::k2_cache::Slot::acquire(&cache, inputs)
        })
        .await?;
        let (mut index, record, owned, system) = if let Ok(Some((index, record))) = choice {
            (Some(index), record, None, None)
        } else {
            let detail = choice.as_ref().err().map(|e| format!("{e:#}"));
            let status = if choice.is_ok() {
                "busy"
            } else {
                "independent"
            };
            let (owned, system, workspace, path) = Self::private_directories(root)?;
            (
                None,
                IndexCacheRecord {
                    status: status.into(),
                    key: None,
                    workspace,
                    system: path,
                    reused_files: 0,
                    detail,
                },
                Some(owned),
                system,
            )
        };
        write_workspace(&record.workspace, &data)?;
        let args = vec![
            "--stdio".into(),
            "--system-path".into(),
            record.system.to_string_lossy().into_owned(),
            "--log-level".into(),
            "INFO".into(),
        ];
        if let Some(index) = &mut index {
            index.mark_launched();
        }
        let mut client =
            match LspClient::spawn_filtered(&server, &args, &record.workspace, &removed) {
                Ok(client) => client,
                Err(error) => {
                    if let Some(index) = &mut index {
                        index.launch_failed();
                    }
                    return Err(error);
                }
            };
        client.set_request_timeout(Duration::from_secs(180));
        let capabilities = client.initialize(&file_uri(&record.workspace)?).await?;
        Ok(Self {
            client,
            capabilities,
            workspace: record.workspace.clone(),
            index_cache: record,
            _owned: owned,
            _system: system,
            index,
            index_ready: false,
        })
    }
    fn private_directories(
        root: &Path,
    ) -> Result<(
        tempfile::TempDir,
        Option<tempfile::TempDir>,
        PathBuf,
        PathBuf,
    )> {
        let parent = root.join(".wex/k2-sessions");
        std::fs::create_dir_all(&parent)?;
        let owned = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(&parent)?;
        let workspace = owned.path().join("workspace");
        let system = if let Some(base) = std::env::var_os("WEX_K2_SYSTEM_DIR") {
            std::fs::create_dir_all(&base)?;
            Some(
                tempfile::Builder::new()
                    .prefix("session-")
                    .tempdir_in(base)?,
            )
        } else {
            None
        };
        let path = system
            .as_ref()
            .map(|d| d.path().canonicalize())
            .transpose()?
            .unwrap_or_else(|| owned.path().join("system"));
        Ok((owned, system, workspace, path))
    }
    /// Called only after real workspace import AND current diagnostics completed.
    pub(crate) fn index_checkpoint(&mut self) {
        self.index_ready = true;
    }
    pub async fn shutdown(&mut self) -> Result<()> {
        let response = self.client.shutdown().await;
        let normal_exit = self.client.child_exited_successfully();
        let clean = response.is_ok() && normal_exit && self.index_ready;
        if let Some(index) = self.index.take() {
            let retained = tokio::task::spawn_blocking(move || index.seal(clean)).await??;
            if !retained {
                self.index_cache.detail = Some(format!(
                    "Profile not retained: genuine checkpoint={}, shutdown reply={}, child exited0={normal_exit}",
                    self.index_ready,
                    response.is_ok()
                ));
            }
        }
        response
    }
    pub async fn open(&self, path: &Path, text: &str, version: i32) -> Result<()> {
        self.client.notify("textDocument/didOpen",json!({"textDocument":{"uri":file_uri(path)?,"languageId":if path.extension().is_some_and(|e|e=="java"){"java"}else{"kotlin"},"version":version,"text":text}})).await
    }
    pub async fn change(&self, path: &Path, text: &str, version: i32) -> Result<()> {
        self.client.notify("textDocument/didChange",json!({"textDocument":{"uri":file_uri(path)?,"version":version},"contentChanges":[{"text":text}]})).await
    }
    pub async fn close(&self, path: &Path) -> Result<()> {
        self.client
            .notify(
                "textDocument/didClose",
                json!({"textDocument":{"uri":file_uri(path)?}}),
            )
            .await
    }
    pub async fn saved(&self, path: &Path) -> Result<()> {
        self.client
            .notify(
                "textDocument/didSave",
                json!({"textDocument":{"uri":file_uri(path)?}}),
            )
            .await
    }
    pub async fn completion(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<CompletionItem>> {
        let result=self.client.request("textDocument/completion",json!({"textDocument":{"uri":file_uri(path)?},"position":{"line":line,"character":character},"context":{"triggerKind":1}})).await?;
        let items = if result.is_array() {
            result
        } else {
            result.get("items").cloned().unwrap_or(json!([]))
        };
        Ok(serde_json::from_value(items)?)
    }
    pub async fn definition(&self, path: &Path, line: u32, character: u32) -> Result<Value> {
        self.client.request("textDocument/definition",json!({"textDocument":{"uri":file_uri(path)?},"position":{"line":line,"character":character}})).await
    }
    pub async fn prepare_rename(&self, path: &Path, line: u32, character: u32) -> Result<Value> {
        self.client.request("textDocument/prepareRename",json!({"textDocument":{"uri":file_uri(path)?},"position":{"line":line,"character":character}})).await
    }
    pub async fn rename(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        name: &str,
    ) -> Result<Value> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 1024,
            "enter a new symbol name"
        );
        self.client.request("textDocument/rename",json!({"textDocument":{"uri":file_uri(path)?},"position":{"line":line,"character":character},"newName":name})).await
    }
    pub async fn references(&self, path: &Path, line: u32, character: u32) -> Result<Value> {
        self.client.request("textDocument/references",json!({"textDocument":{"uri":file_uri(path)?},"position":{"line":line,"character":character},"context":{"includeDeclaration":true}})).await
    }
    pub(crate) async fn begin_formatting(
        &self,
        path: &Path,
        range: Option<&Range>,
    ) -> Result<crate::lsp::PendingResponse> {
        let (method, params) = crate::formatting::rpc_request(&self.capabilities, path, range)?;
        self.client.begin_request(method, params).await
    }
    pub async fn format(&self, path: &Path, range: Option<&Range>) -> Result<Value> {
        self.begin_formatting(path, range).await?.wait().await
    }
    pub(crate) async fn begin_pull_diagnostics(
        &self,
        path: &Path,
    ) -> Result<crate::lsp::PendingResponse> {
        self.client
            .begin_request(
                "textDocument/diagnostic",
                json!({"textDocument":{"uri":file_uri(path)?}}),
            )
            .await
    }
    pub async fn pull_diagnostics(&self, path: &Path) -> Result<Value> {
        self.client
            .request(
                "textDocument/diagnostic",
                json!({"textDocument":{"uri":file_uri(path)?}}),
            )
            .await
    }
}
/// Apply plain-text LSP edits in reverse document order, validating UTF-16 ranges.
/// Extra auto-import edits are handled along with the primary edit, atomically validated first.
pub fn apply_completion(doc: &mut Document, cursor: usize, item: &CompletionItem) -> Result<usize> {
    ensure!(
        item.insert_text_format.unwrap_or(1) == 1,
        "server returned a snippet despite snippetSupport=false"
    );
    let primary = if let Some(edit) = &item.text_edit {
        let range = edit
            .get("range")
            .or_else(|| edit.get("replace"))
            .context("completion has no edit range")?;
        let range: Range = serde_json::from_value(range.clone())?;
        let start = doc.char_at_lsp(range.start.line, range.start.character)?;
        let end = doc.char_at_lsp(range.end.line, range.end.character)?;
        (
            start,
            end,
            edit["newText"]
                .as_str()
                .context("completion has no newText")?
                .to_owned(),
        )
    } else {
        (
            cursor,
            cursor,
            item.insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone()),
        )
    };
    let mut edits = vec![primary.clone()];
    for edit in &item.additional_text_edits {
        let range: Range = serde_json::from_value(edit["range"].clone())?;
        edits.push((
            doc.char_at_lsp(range.start.line, range.start.character)?,
            doc.char_at_lsp(range.end.line, range.end.character)?,
            edit["newText"]
                .as_str()
                .context("missing extra edit text")?
                .to_owned(),
        ));
    }
    edits.sort_by_key(|e| e.0);
    for w in edits.windows(2) {
        ensure!(w[0].1 <= w[1].0, "overlapping completion edits");
    }
    let delta: isize = edits
        .iter()
        .filter(|e| e.0 < primary.0)
        .map(|e| e.2.chars().count() as isize - (e.1 - e.0) as isize)
        .sum();
    for (start, end, text) in edits.into_iter().rev() {
        doc.edit(start..end, &text)?;
    }
    Ok((primary.0 as isize + delta) as usize + primary.2.chars().count())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compose_settings_use_real_module_roots_and_jps_compiler_plugin_format() {
        let config: crate::native::Config =
            serde_json::from_str(include_str!("../tests/fixtures/compose-android/wex.json"))
                .unwrap();
        let module = crate::project::Module {
            name: ":shared".into(),
            source_roots: vec![PathBuf::from("/project/shared/src")],
            generated_source_roots: vec![PathBuf::from("/project/generated")],
            ..Default::default()
        };
        let plugin = crate::build::ComposeBuild {
            compiler_version: "2.4.0".into(),
            plugin_jar: PathBuf::from("/cache/compose compiler.jar"),
            plugin_blake3: String::new(),
            source_information: true,
            strong_skipping: true,
            reports: true,
            modules: vec!["shared".into()],
        };
        let settings = compose_settings(&config, &module, &plugin);
        assert_eq!(settings["module"], ":shared");
        assert_eq!(settings["sourceRoots"].as_array().unwrap().len(), 2);
        let args = settings["compilerArguments"].as_str().unwrap();
        assert!(args.starts_with("J{"));
        let args: Value = serde_json::from_str(&args[1..]).unwrap();
        assert_eq!(args["jvmTarget"], "17");
        assert_eq!(args["pluginClasspaths"][0], "/cache/compose compiler.jar");
        assert_eq!(settings["useProjectSettings"], false);
        assert!(!args.to_string().contains("ComposableFunction0"));
    }
    #[test]
    fn unicode_file_uri_roundtrip() {
        let path = Path::new("/tmp/проект a/Main.kt");
        assert_eq!(uri_path(&file_uri(path).unwrap()).unwrap(), path);
    }
    #[test]
    fn completion_replaces_utf16_range_and_adds_import() {
        let mut d = Document::new("//😀\nfun x() { Gre() }\n").unwrap();
        let item:CompletionItem=serde_json::from_value(json!({"label":"Greeter","textEdit":{"range":{"start":{"line":1,"character":10},"end":{"line":1,"character":13}},"newText":"Greeter"},"additionalTextEdits":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},"newText":"import p.Greeter\n"}]})).unwrap();
        let cursor = apply_completion(&mut d, 0, &item).unwrap();
        assert_eq!(
            d.rope().to_string(),
            "//😀\nimport p.Greeter\nfun x() { Greeter() }\n"
        );
        assert_eq!(d.rope().char(cursor), '(');
    }
}

/// End-to-end semantic probe on the checked-in Android integration app.
/// All broken/completion buffers are sent as didChange; disk sources stay unchanged.
async fn diagnostics(
    session: &mut KotlinSession,
    path: &Path,
    pull: bool,
    min_version: i32,
) -> Result<Value> {
    if pull {
        return session.pull_diagnostics(path).await;
    }
    let uri = file_uri(path)?;
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(event) = session.client.events.recv().await {
            if event["method"] == "textDocument/publishDiagnostics"
                && event["params"]["uri"] == uri
                && event["params"]["version"]
                    .as_i64()
                    .is_none_or(|v| v >= min_version as i64)
            {
                return Ok(json!({"items":event["params"]["diagnostics"]}));
            }
        }
        anyhow::bail!("diagnostics event stream closed")
    })
    .await
    .context("waiting for Kotlin diagnostics")?
}

pub async fn check_android(
    root: &Path,
    path: &Path,
    server: &Path,
    variant: &str,
) -> Result<Value> {
    if root.join("wex.json").is_file() && crate::native::Config::load(root)?.build.compose.is_some()
    {
        return check_compose(root, path, server, variant).await;
    }

    let sync = if root.join("wex.json").is_file() {
        let prepared = crate::analysis::prepare(root, variant, true, false, &|_| {}).await?;
        crate::gradle::SyncResult {
            cache_hit: prepared.cache_hit,
            elapsed_ms: prepared.elapsed_ms,
            graph: prepared.graph,
        }
    } else {
        crate::gradle::sync_project(
            root,
            &crate::gradle::SyncOptions {
                variant: variant.into(),
                ..Default::default()
            },
        )
        .await?
    };
    let mut session = KotlinSession::start(root, &sync.graph, server).await?;
    let original = std::fs::read_to_string(path)?;
    session.open(path, &original, 1).await?;
    let probe = original.replace("greeter.greeting(title)", "greeter.gre");
    ensure!(
        probe != original,
        "semantic probe requires greeter.greeting(title) in the input"
    );
    let pos = probe.find("greeter.gre").unwrap() + "greeter.gre".len();
    let c = probe[..pos].chars().count();
    let doc = Document::new(&probe)?;
    let (line, character) = doc.lsp_position(c)?;
    session.change(path, &probe, 2).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut labels = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let items = session.completion(path, line, character).await?;
        labels = items.iter().map(|c| c.label.clone()).collect();
        if labels.iter().any(|s| s.starts_with("greeting")) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    ensure!(
        labels.iter().any(|s| s.starts_with("greeting")),
        "K2 completion did not resolve Greeter members; labels: {labels:?}"
    );
    session.change(path, &original, 3).await?;
    let doc = Document::new(&original)?;
    let at = original
        .find("Greeter()")
        .context("missing Greeter constructor")?;
    let (line, character) = doc.lsp_position(original[..at].chars().count() + 2)?;
    let definition = session.definition(path, line, character).await?;
    ensure!(
        definition.to_string().contains("Greeter.kt"),
        "K2 definition did not resolve source module: {definition}"
    );
    let r_at = original
        .find("R.string.app_name")
        .context("missing Android R reference")?;
    let (rl, rc) = doc.lsp_position(original[..r_at].chars().count())?;
    let r_definition = session.definition(path, rl, rc).await?;
    let bc_at = original
        .find("BuildConfig.DEBUG")
        .context("missing BuildConfig reference")?;
    let (bl, bc) = doc.lsp_position(original[..bc_at].chars().count() + 2)?;
    let build_config_definition = session.definition(path, bl, bc).await?;
    // Pull diagnostics if advertised; otherwise wait for pushed, URI-matched reports.
    let pull = !session.capabilities["capabilities"]["diagnosticProvider"].is_null();
    let clean = diagnostics(&mut session, path, pull, 3).await?;
    let errors = clean["items"]
        .as_array()
        .context("missing diagnostic items")?
        .iter()
        .filter(|d| d["severity"] == 1)
        .collect::<Vec<_>>();
    ensure!(
        errors.is_empty(),
        "K2 Android references are unresolved: {errors:?}"
    );
    ensure!(
        !r_definition.is_null() && r_definition != json!([]),
        "K2 R definition is missing"
    );
    ensure!(
        build_config_definition.to_string().contains("BuildConfig"),
        "K2 BuildConfig definition is missing"
    );
    let broken = original.replace(
        "val debug = BuildConfig.DEBUG",
        "val broken: Int = \"wrong\"\n        val debug = BuildConfig.DEBUG",
    );
    session.change(path, &broken, 4).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut bad = Value::Null;
    while tokio::time::Instant::now() < deadline {
        bad = diagnostics(&mut session, path, pull, 4).await?;
        if bad["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["severity"] == 1))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    ensure!(
        bad["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["severity"] == 1)),
        "K2 did not report an introduced type mismatch"
    );
    session.change(path, &original, 5).await?;
    let recovered = diagnostics(&mut session, path, pull, 5).await?;
    ensure!(
        !recovered["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["severity"] == 1)),
        "K2 errors persisted after restoring the buffer: {recovered}"
    );
    session.close(path).await?;
    session.shutdown().await?;
    Ok(
        json!({"variant":variant,"server":server,"server_info":session.capabilities["serverInfo"],"cache_hit":sync.cache_hit,"analysis_model_api":sync.graph.model_api,"completion_labels":labels,"definition":definition,"r_definition":r_definition,"build_config_definition":build_config_definition,"clean_diagnostics":clean,"introduced_error_diagnostics":bad,"recovered_diagnostics":recovered,"disk_source_unchanged":std::fs::read_to_string(path)?==original,"result":"K2_ANDROID_OK"}),
    )
}

/// Semantic end-to-end probe for the native two-module Compose fixture.
/// Buffer edits exercise K2 without writing test mutations to disk.
async fn check_compose(root: &Path, path: &Path, server: &Path, variant: &str) -> Result<Value> {
    let graph = crate::analysis::prepare(root, variant, true, false, &|_| {})
        .await?
        .graph;
    let shared = crate::project::source_files(root)
        .into_iter()
        .find(|p| p.ends_with("CounterCard.kt"))
        .context("Compose probe requires CounterCard.kt")?;
    let original = std::fs::read_to_string(path)?;
    let shared_text = std::fs::read_to_string(&shared)?;
    let mut session = KotlinSession::start(root, &graph, server).await?;
    session.open(path, &original, 1).await?;
    session.open(&shared, &shared_text, 1).await?;
    let probe = original.replace("CounterCard { count ->", "CounterCa { count ->");
    ensure!(
        probe != original,
        "Compose probe requires CounterCard callback call"
    );
    let pos = probe.find("CounterCa { count ->").unwrap() + "CounterCa".len();
    let doc = Document::new(&probe)?;
    let (line, character) = doc.lsp_position(probe[..pos].chars().count())?;
    session.change(path, &probe, 2).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut labels = Vec::new();
    while tokio::time::Instant::now() < deadline {
        labels = session
            .completion(path, line, character)
            .await?
            .into_iter()
            .map(|c| c.label)
            .collect();
        if labels.iter().any(|s: &String| s.starts_with("CounterCard")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    ensure!(
        labels.iter().any(|s| s.starts_with("CounterCard")),
        "K2 did not complete shared composable: {labels:?}"
    );
    session.change(path, &original, 3).await?;
    let doc = Document::new(&original)?;
    let at = original.find("CounterCard { count ->").unwrap();
    let (line, character) = doc.lsp_position(original[..at].chars().count() + 2)?;
    let definition = session.definition(path, line, character).await?;
    ensure!(
        definition.to_string().contains("CounterCard.kt"),
        "K2 composable definition missing: {definition}"
    );
    let doc = Document::new(&shared_text)?;
    let at = shared_text
        .find("CounterLabel.value")
        .context("Compose probe requires Java CounterLabel")?;
    let (line, character) = doc.lsp_position(shared_text[..at].chars().count() + 2)?;
    let java_deadline = tokio::time::Instant::now() + Duration::from_secs(35);
    let mut java_definition = Value::Null;
    while tokio::time::Instant::now() < java_deadline {
        java_definition = session.definition(&shared, line, character).await?;
        if java_definition.to_string().contains("CounterLabel.java") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    ensure!(
        java_definition.to_string().contains("CounterLabel.java"),
        "K2 Java source definition missing: {java_definition}"
    );
    let build_config_definition = if let Some(at) = original.find("BuildConfig.DEBUG") {
        let doc = Document::new(&original)?;
        let (line, character) = doc.lsp_position(original[..at].chars().count() + 2)?;
        let value = session.definition(path, line, character).await?;
        ensure!(
            value.to_string().contains("BuildConfig"),
            "K2 selected-variant BuildConfig definition missing"
        );
        value
    } else {
        Value::Null
    };
    let pull = !session.capabilities["capabilities"]["diagnosticProvider"].is_null();
    async fn clean(
        session: &mut KotlinSession,
        path: &Path,
        pull: bool,
        version: i32,
    ) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            let d = diagnostics(session, path, pull, version).await?;
            if d["items"]
                .as_array()
                .is_some_and(|a| a.iter().all(|d| d["severity"] != 1))
            {
                return Ok(d);
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "K2 Compose source errors: {d}"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    let clean_main = clean(&mut session, path, pull, 3).await?;
    let clean_shared = clean(&mut session, &shared, pull, 1).await?;
    let broken = original.replace(
        "        setContent {",
        "        val wexSemanticError: Int = \"wrong type\"\n        setContent {",
    );
    ensure!(broken != original, "Compose probe requires setContent call");
    session.change(path, &broken, 4).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut bad = Value::Null;
    while tokio::time::Instant::now() < deadline {
        bad = diagnostics(&mut session, path, pull, 4).await?;
        if bad["items"].as_array().is_some_and(|a| {
            a.iter().any(|d| {
                d["severity"] == 1
                    && d["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("Int") && m.contains("String"))
            })
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    ensure!(
        bad["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["severity"] == 1
                && d["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("Int") && m.contains("String")))),
        "K2 did not report Compose probe Int/String mismatch: {bad}"
    );
    session.change(path, &original, 5).await?;
    let recovered = clean(&mut session, path, pull, 5).await?;
    let misuse = format!("{original}\nfun wexInvalidComposableCall() {{ CounterCard {{ }} }}\n");
    session.change(path, &misuse, 6).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut compose_error = Value::Null;
    while tokio::time::Instant::now() < deadline {
        compose_error = diagnostics(&mut session, path, pull, 6).await?;
        if compose_error["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["code"] == "COMPOSABLE_INVOCATION"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    ensure!(
        compose_error["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|d| d["code"] == "COMPOSABLE_INVOCATION")),
        "Compose FIR checker did not reject invalid call: {compose_error}"
    );
    session.change(path, &original, 7).await?;
    let final_recovered = clean(&mut session, path, pull, 7).await?;
    session.close(path).await?;
    session.close(&shared).await?;
    session.shutdown().await?;
    ensure!(
        std::fs::read_to_string(path)? == original
            && std::fs::read_to_string(&shared)? == shared_text,
        "K2 probe changed disk sources"
    );
    Ok(
        json!({"result":"K2_COMPOSE_OK","variant":variant,"server":server,"server_info":session.capabilities["serverInfo"],"completion_labels":labels,"composable_definition":definition,"java_definition":java_definition,"build_config_definition":build_config_definition,"clean_main":clean_main,"clean_shared":clean_shared,"introduced_error":bad,"recovered":recovered,"invalid_composable_call":compose_error,"final_recovered":final_recovered,"disk_source_unchanged":true,"native_graph":true}),
    )
}

/// Validate a server's WorkspaceEdit against the exact open-buffer version before editing.
/// Resource operations and edits to other files require a separate multi-file UI workflow.
pub fn apply_workspace_edit(
    doc: &mut Document,
    uri: &str,
    workspace: &Value,
    cursor: usize,
) -> Result<usize> {
    let mut list = Vec::new();
    if let Some(changes) = workspace.get("changes").and_then(Value::as_object) {
        for (target, edits) in changes {
            ensure!(target == uri, "completion attempted to edit another file");
            list.extend(
                edits
                    .as_array()
                    .context("invalid workspace edits")?
                    .iter()
                    .cloned(),
            );
        }
    }
    if let Some(changes) = workspace.get("documentChanges").and_then(Value::as_array) {
        for edit in changes {
            ensure!(
                edit.get("kind").is_none(),
                "resource operation requires multi-file review"
            );
            ensure!(
                edit["textDocument"]["uri"] == uri,
                "completion attempted to edit another file"
            );
            if let Some(version) = edit["textDocument"]["version"].as_i64() {
                ensure!(version == doc.version as i64, "stale server edit version");
            }
            list.extend(
                edit["edits"]
                    .as_array()
                    .context("missing TextDocumentEdit edits")?
                    .iter()
                    .cloned(),
            );
        }
    }
    ensure!(!list.is_empty(), "empty workspace edit");
    let mut edits = Vec::new();
    for edit in list {
        let range: Range = serde_json::from_value(edit["range"].clone())?;
        let start = doc.char_at_lsp(range.start.line, range.start.character)?;
        let end = doc.char_at_lsp(range.end.line, range.end.character)?;
        ensure!(start <= end, "reversed server edit");
        edits.push((
            start,
            end,
            edit["newText"]
                .as_str()
                .context("missing newText")?
                .to_owned(),
        ));
    }
    edits.sort_by_key(|e| e.0);
    for w in edits.windows(2) {
        ensure!(w[0].1 <= w[1].0, "overlapping server edits");
    }
    ensure!(
        doc.version.checked_add(edits.len() as i32).is_some(),
        "document version exhausted"
    );
    let mut new_cursor = cursor as isize;
    for (start, end, text) in &edits {
        if *end <= cursor {
            new_cursor += text.chars().count() as isize - (*end - *start) as isize;
        } else if *start <= cursor {
            new_cursor = *start as isize + text.chars().count() as isize;
        }
    }
    for (start, end, text) in edits.into_iter().rev() {
        doc.edit(start..end, &text)?;
    }
    Ok((new_cursor.max(0) as usize).min(doc.rope().len_chars()))
}

#[cfg(test)]
mod workspace_edit_tests {
    use super::*;
    #[test]
    fn server_edit_updates_the_open_rope() {
        let mut d = Document::new("fun x() { greeter.gre }\n").unwrap();
        let uri = "file:///tmp/Main.kt";
        let edit = json!({"changes":{uri:[{"range":{"start":{"line":0,"character":18},"end":{"line":0,"character":21}},"newText":"greeting()"}]}});
        apply_workspace_edit(&mut d, uri, &edit, 20).unwrap();
        assert_eq!(d.rope().to_string(), "fun x() { greeter.greeting() }\n");
    }
    #[test]
    fn stale_or_foreign_edits_do_not_mutate() {
        let mut d = Document::new("val x = 1\n").unwrap();
        let uri = "file:///tmp/Main.kt";
        for edit in [
            json!({"changes":{"file:///tmp/Other.kt":[]}}),
            json!({"documentChanges":[{"textDocument":{"uri":uri,"version":999},"edits":[]}]}),
        ] {
            assert!(apply_workspace_edit(&mut d, uri, &edit, 0).is_err());
            assert_eq!(d.rope().to_string(), "val x = 1\n");
            assert_eq!(d.version, 1);
        }
    }
    #[test]
    fn all_ranges_are_validated_before_any_edit() {
        let mut d = Document::new("val x = 1\n").unwrap();
        let uri = "file:///tmp/Main.kt";
        let edit = json!({"changes":{uri:[{"range":{"start":{"line":0,"character":8},"end":{"line":0,"character":9}},"newText":"2"},{"range":{"start":{"line":9,"character":0},"end":{"line":9,"character":1}},"newText":"bad"}]}});
        assert!(apply_workspace_edit(&mut d, uri, &edit, 0).is_err());
        assert!(!d.dirty());
        assert_eq!(d.version, 1);
    }
}
