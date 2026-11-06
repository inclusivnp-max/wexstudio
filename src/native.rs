//! Wex configuration and a deterministic Maven POM resolver, independent of Gradle/Maven.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

pub const RESOLVER_ABI: u32 = 3;

const ANDROID_MEDIATION: &str =
    "android-jvm-module-v1/highest-reachable-fixed-v2/project-bom-pins-v1";

fn is_false(v: &bool) -> bool {
    !*v
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: u32,
    pub name: String,
    pub repositories: Vec<String>,
    pub android: Android,
    pub modules: Vec<Module>,
    #[serde(default)]
    pub build: BuildSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub android_variants: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Android {
    pub compile_sdk: u32,
    pub min_sdk: u32,
    pub target_sdk: u32,
    pub application_id: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub name: String,
    pub sources: Vec<PathBuf>,
    #[serde(default)]
    pub resources: Vec<PathBuf>,
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub manifest: Option<PathBuf>,
    #[serde(default)]
    pub assets: Vec<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildSettings {
    pub application_module: String,
    pub namespace: String,
    pub manifest: PathBuf,
    pub build_tools: String,
    pub kotlin_version: String,
    pub jvm_target: u32,
    pub version_code: u32,
    pub version_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose: Option<ComposeSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_signing: Option<crate::signing::Config>,
}
impl Default for BuildSettings {
    fn default() -> Self {
        Self {
            application_module: "app".into(),
            namespace: String::new(),
            manifest: "app/src/main/AndroidManifest.xml".into(),
            build_tools: "36.0.0".into(),
            kotlin_version: "2.4.0".into(),
            jvm_target: 17,
            version_code: 1,
            version_name: "1.0".into(),
            compose: None,
            release_signing: None,
        }
    }
}
/// Presence enables the matching Kotlin Compose compiler in every native module.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComposeSettings {
    pub source_information: bool,
    pub reports: bool,
    pub strong_skipping: bool,
}
impl Default for ComposeSettings {
    fn default() -> Self {
        Self {
            source_information: true,
            reports: false,
            strong_skipping: true,
        }
    }
}
pub fn default_cache() -> PathBuf {
    std::env::var_os("WEX_MAVEN_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/wex/maven")
        })
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Coordinate {
    pub group: String,
    pub artifact: String,
    pub version: String,
}
impl Coordinate {
    pub fn parse(s: &str) -> Result<Self> {
        let p: Vec<_> = s.split(':').collect();
        ensure!(p.len() == 3, "expected group:artifact:version: {s}");
        let c = Self {
            group: p[0].into(),
            artifact: p[1].into(),
            version: p[2].into(),
        };
        c.validate()?;
        Ok(c)
    }
    fn validate(&self) -> Result<()> {
        for p in [&self.group, &self.artifact, &self.version] {
            ensure!(
                !p.is_empty()
                    && !p.contains("..")
                    && p.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                "invalid Maven coordinate component: {p}"
            );
        }
        ensure!(
            !matches!(self.version.as_str(), "LATEST" | "RELEASE")
                && !self.version.ends_with("SNAPSHOT"),
            "dependency version must be immutable/pinned"
        );
        Ok(())
    }
    fn ga(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
    fn text(&self) -> String {
        format!("{}:{}:{}", self.group, self.artifact, self.version)
    }
    fn path(&self, ext: &str) -> String {
        format!(
            "{}/{}/{}/{}-{}.{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.artifact,
            self.version,
            ext
        )
    }
}
fn parse_constraint(pin: &str) -> Result<(String, String)> {
    let parts = pin.split(':').collect::<Vec<_>>();
    ensure!(
        parts.len() == 3,
        "constraint must be group:artifact:version"
    );
    let artifact = if parts[1] == "*" {
        "wildcard"
    } else {
        parts[1]
    };
    let c = Coordinate::parse(&format!("{}:{artifact}:{}", parts[0], parts[2]))?;
    Ok((format!("{}:{}", c.group, parts[1]), c.version))
}
fn constraint_pin<'a>(pins: &'a BTreeMap<String, String>, ga: &str) -> Option<&'a String> {
    pins.get(ga).or_else(|| {
        ga.rsplit_once(':')
            .and_then(|(group, _)| pins.get(&format!("{group}:*")))
    })
}
impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        Self::from_bytes(&std::fs::read(root.join("wex.json"))?)
    }
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= 1024 * 1024, "config exceeds 1 MiB");
        let c: Self = serde_json::from_slice(bytes)?;
        ensure!(c.schema == 1, "unsupported Wex schema");
        ensure!(
            !c.name.is_empty() && !c.repositories.is_empty() && !c.modules.is_empty(),
            "empty project configuration"
        );
        ensure!(
            c.android.min_sdk <= c.android.target_sdk
                && c.android.target_sdk <= c.android.compile_sdk,
            "invalid Android SDK levels"
        );
        ensure!(
            c.android.application_id.split('.').count() >= 2
                && c.android
                    .application_id
                    .split('.')
                    .all(|s| !s.is_empty()
                        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')),
            "invalid application ID"
        );
        for platform in &c.platforms {
            Coordinate::parse(platform)?;
        }
        let mut constraints = BTreeMap::new();
        for pin in &c.constraints {
            let (ga, version) = parse_constraint(pin)?;
            if let Some(old) = constraints.insert(ga.clone(), version.clone()) {
                ensure!(old == version, "conflicting project constraints for {ga}");
            }
        }
        if let Some(signing) = &c.build.release_signing {
            signing.validate()?;
        }
        let mut names = BTreeSet::new();
        for m in &c.modules {
            ensure!(
                !m.name.is_empty() && names.insert(m.name.clone()),
                "duplicate/empty module"
            );
            for p in m
                .sources
                .iter()
                .chain(&m.resources)
                .chain(&m.assets)
                .chain(m.manifest.iter())
            {
                ensure!(
                    !p.is_absolute()
                        && p.components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "module path must stay within project"
                );
            }
            for d in &m.dependencies {
                Coordinate::parse(d)?;
            }
        }
        for m in &c.modules {
            for p in &m.projects {
                ensure!(
                    names.contains(p) && p != &m.name,
                    "invalid project dependency {p}"
                );
            }
        }
        for repo in &c.repositories {
            validate_repo(repo)?;
        }
        // Reject project cycles rather than silently generating an invalid compile order.
        fn visit(
            name: &str,
            c: &Config,
            active: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
        ) -> Result<()> {
            if done.contains(name) {
                return Ok(());
            }
            ensure!(
                active.insert(name.into()),
                "project dependency cycle at {name}"
            );
            for p in &c.modules.iter().find(|m| m.name == name).unwrap().projects {
                visit(p, c, active, done)?;
            }
            active.remove(name);
            done.insert(name.into());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for m in &c.modules {
            visit(&m.name, &c, &mut BTreeSet::new(), &mut done)?;
        }
        Ok(c)
    }
}
fn validate_repo(repo: &str) -> Result<()> {
    let u = url::Url::parse(repo)?;
    ensure!(
        (u.scheme() == "https" && u.host_str().is_some())
            || (u.scheme() == "file" && u.to_file_path().is_ok()),
        "repository must be HTTPS or local file URL"
    );
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "invalid repository URL"
    );
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockedFile {
    pub repository: String,
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub coordinate: Coordinate,
    pub packaging: String,
    pub file: String,
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lock {
    pub schema: u32,
    #[serde(default)]
    pub resolver_abi: u32,
    pub config_blake3: String,
    #[serde(default)]
    pub module_scopes: BTreeMap<String, BTreeSet<String>>,
    pub artifacts: Vec<Artifact>,
    pub files: Vec<LockedFile>,
    pub mediation: String,
}
#[derive(Clone, Default)]
struct Pom {
    packaging: String,
    properties: BTreeMap<String, String>,
    managed: BTreeMap<String, Dep>,
    dependencies: Vec<Dep>,
    module_metadata: bool,
}
#[derive(Clone)]
struct Dep {
    coordinate: Coordinate,
    scope: String,
    kind: String,
    optional: bool,
    exclusions: BTreeSet<String>,
}
struct Resolver {
    config: Config,
    cache: PathBuf,
    offline: bool,
    files: BTreeMap<String, LockedFile>,
    poms: BTreeMap<String, Pom>,
    active: BTreeSet<String>,
}
impl Resolver {
    fn file(&mut self, c: &Coordinate, ext: &str) -> Result<PathBuf> {
        c.validate()?;
        ensure!(
            ["pom", "jar", "aar", "module"].contains(&ext),
            "unsupported Maven artifact type {ext}"
        );
        let rel = c.path(ext);
        for repo in self.config.repositories.clone() {
            let prefix = blake3::hash(repo.as_bytes()).to_hex()[..16].to_string();
            let dest = self.cache.join(prefix).join(&rel);
            if !dest.exists() {
                let u = url::Url::parse(&repo)?;
                if self.offline && u.scheme() != "file" {
                    continue;
                }
                std::fs::create_dir_all(dest.parent().unwrap())?;
                let mut temp = tempfile::NamedTempFile::new_in(dest.parent().unwrap())?;
                if u.scheme() == "file" {
                    let source = u
                        .to_file_path()
                        .map_err(|_| anyhow::anyhow!("invalid file repository"))?
                        .join(&rel);
                    if !source.is_file() {
                        continue;
                    }
                    std::io::copy(&mut std::fs::File::open(source)?, &mut temp)?;
                } else {
                    let url = format!("{}/{}", repo.trim_end_matches('/'), rel);
                    // Transport only; dependency semantics and cache/lock ownership live in Rust.
                    let out = std::process::Command::new("curl")
                        .args([
                            "--silent",
                            "--show-error",
                            "--location",
                            "--proto",
                            "=https",
                            "--proto-redir",
                            "=https",
                            "--connect-timeout",
                            "10",
                            "--max-time",
                            "60",
                            "--max-filesize",
                            "268435456",
                            "--output",
                        ])
                        .arg(temp.path())
                        .args(["--write-out", "%{http_code}", &url])
                        .output()
                        .context("HTTPS repository transport")?;
                    let status = String::from_utf8_lossy(&out.stdout);
                    if status == "404" {
                        continue;
                    }
                    ensure!(
                        out.status.success() && status == "200",
                        "repository fetch failed for {url}: HTTP {status}: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                ensure!(
                    temp.as_file().metadata()?.len() <= 256 * 1024 * 1024,
                    "artifact exceeds 256 MiB"
                );
                temp.persist(&dest).map_err(|e| e.error)?;
            }
            let bytes = std::fs::read(&dest)?;
            ensure!(bytes.len() <= 256 * 1024 * 1024, "artifact exceeds 256 MiB");
            let path = dest
                .strip_prefix(&self.cache)?
                .to_string_lossy()
                .into_owned();
            self.files.insert(
                path.clone(),
                LockedFile {
                    repository: repo,
                    path,
                    bytes: bytes.len() as u64,
                    blake3: blake3::hash(&bytes).to_hex().to_string(),
                },
            );
            return Ok(dest);
        }
        bail!(
            "Maven artifact missing{}: {} ({ext})",
            if self.offline {
                " in offline cache"
            } else {
                " from repositories"
            },
            c.text()
        )
    }
    fn pom(&mut self, c: &Coordinate) -> Result<Pom> {
        let key = c.text();
        if let Some(p) = self.poms.get(&key) {
            return Ok(p.clone());
        }
        ensure!(
            self.active.len() < 64 && self.active.insert(key.clone()),
            "POM parent/BOM cycle or depth limit: {key}"
        );
        let path = self.file(c, "pom")?;
        let text = std::fs::read_to_string(path)?;
        ensure!(text.len() <= 4 * 1024 * 1024, "POM exceeds 4 MiB");
        let doc = roxmltree::Document::parse(&text)?;
        let root = doc.root_element();
        ensure!(root.tag_name().name() == "project", "invalid POM root");
        if let Some(profiles) = child(root, "profiles") {
            for profile in profiles.children().filter(|n| n.has_tag_name("profile")) {
                ensure!(
                    [
                        "dependencies",
                        "dependencyManagement",
                        "properties",
                        "repositories"
                    ]
                    .iter()
                    .all(|name| child(profile, name).is_none()),
                    "Dependency-affecting POM profiles require explicit support: {key}"
                );
            }
        }
        let mut pom = if let Some(p) = child(root, "parent") {
            self.pom(&Coordinate {
                group: value(p, "groupId"),
                artifact: value(p, "artifactId"),
                version: value(p, "version"),
            })?
        } else {
            Pom::default()
        };
        pom.module_metadata = text.contains("published-with-gradle-metadata");
        pom.packaging = value(root, "packaging");
        if pom.packaging.is_empty() {
            pom.packaging = "jar".into();
        }
        if let Some(props) = child(root, "properties") {
            for p in props.children().filter(|n| n.is_element()) {
                pom.properties.insert(
                    p.tag_name().name().into(),
                    p.text().unwrap_or("").trim().into(),
                );
            }
        }
        for (name, v) in [
            ("groupId", &c.group),
            ("artifactId", &c.artifact),
            ("version", &c.version),
        ] {
            pom.properties.insert(format!("project.{name}"), v.clone());
            pom.properties.insert(format!("pom.{name}"), v.clone());
        }
        if let Some(dm) = child(root, "dependencyManagement").and_then(|n| child(n, "dependencies"))
        {
            let mut direct = vec![];
            for n in dm.children().filter(|n| n.has_tag_name("dependency")) {
                let dep = parse_dep(n, &pom.properties, &pom.managed)?;
                if dep.scope == "import" {
                    ensure!(dep.kind == "pom", "BOM import requires type=pom");
                    let bom = self.pom(&dep.coordinate)?;
                    for (ga, d) in bom.managed {
                        pom.managed.entry(ga).or_insert(d);
                    }
                } else {
                    direct.push(dep);
                }
            }
            for d in direct {
                pom.managed.insert(d.coordinate.ga(), d);
            }
        }
        if let Some(deps) = child(root, "dependencies") {
            for n in deps.children().filter(|n| n.has_tag_name("dependency")) {
                let d = parse_dep(n, &pom.properties, &pom.managed)?;
                if let Some(old) = pom
                    .dependencies
                    .iter_mut()
                    .find(|old| old.coordinate.ga() == d.coordinate.ga())
                {
                    *old = d;
                } else {
                    pom.dependencies.push(d);
                }
            }
        }
        self.active.remove(&key);
        self.poms.insert(key, pom.clone());
        Ok(pom)
    }
}
fn child<'a, 'input>(
    n: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    n.children().find(|n| n.has_tag_name(name))
}
fn value(n: roxmltree::Node<'_, '_>, name: &str) -> String {
    child(n, name)
        .and_then(|n| n.text())
        .unwrap_or("")
        .trim()
        .into()
}
fn expand(s: &str, props: &BTreeMap<String, String>) -> Result<String> {
    let mut s = s.to_owned();
    for _ in 0..16 {
        let Some(start) = s.find("${") else {
            return Ok(s);
        };
        let end = s[start..].find('}').context("unterminated POM property")? + start;
        let key = &s[start + 2..end];
        let v = props
            .get(key)
            .with_context(|| format!("unresolved POM property {key}"))?;
        s.replace_range(start..=end, v);
    }
    bail!("cyclic POM property")
}
fn parse_dep(
    n: roxmltree::Node<'_, '_>,
    props: &BTreeMap<String, String>,
    managed: &BTreeMap<String, Dep>,
) -> Result<Dep> {
    let group = expand(&value(n, "groupId"), props)?;
    let artifact = expand(&value(n, "artifactId"), props)?;
    let ga = format!("{group}:{artifact}");
    let management = managed.get(&ga);
    let v = value(n, "version");
    let v = if v.is_empty() {
        management
            .map(|d| d.coordinate.version.clone())
            .context("dependency has no version/management")?
    } else {
        expand(&v, props)?
    };
    // Maven [1.2.3] is an exact pin; ranges/dynamic selections are deliberately errors.
    let version = if v.starts_with('[') && v.ends_with(']') && !v.contains(',') {
        v[1..v.len() - 1].into()
    } else {
        v
    };
    let coordinate = Coordinate {
        group,
        artifact,
        version,
    };
    coordinate.validate()?;
    ensure!(
        value(n, "classifier").is_empty(),
        "Maven classifiers require explicit support: {ga}"
    );
    let scope = value(n, "scope");
    let scope = if scope.is_empty() {
        "compile".into()
    } else {
        scope
    };
    ensure!(
        ["compile", "runtime", "test", "provided", "system", "import"].contains(&scope.as_str()),
        "unknown dependency scope {scope}"
    );
    let kind = value(n, "type");
    let kind = if kind.is_empty() { "auto".into() } else { kind };
    let mut exclusions = management.map(|d| d.exclusions.clone()).unwrap_or_default();
    if let Some(e) = child(n, "exclusions") {
        for x in e.children().filter(|n| n.has_tag_name("exclusion")) {
            exclusions.insert(format!(
                "{}:{}",
                value(x, "groupId"),
                value(x, "artifactId")
            ));
        }
    }
    Ok(Dep {
        coordinate,
        scope,
        kind,
        optional: value(n, "optional") == "true",
        exclusions,
    })
}
fn excluded(set: &BTreeSet<String>, c: &Coordinate) -> bool {
    set.contains(&c.ga())
        || set.contains("*:*")
        || set.contains(&format!("{}:*", c.group))
        || set.contains(&format!("*:{}", c.artifact))
}
fn record_requirement(
    choices: &mut BTreeMap<String, String>,
    forced: &BTreeMap<String, String>,
    ga: &str,
    version: &str,
) -> Result<()> {
    if let Some(pin) = constraint_pin(forced, ga) {
        choices.insert(ga.into(), pin.clone());
        return Ok(());
    }
    if let Some(current) = choices.get(ga)
        && crate::version::compare(version, current)
            .with_context(|| format!("comparing dependency versions for {ga}"))?
            != std::cmp::Ordering::Greater
    {
        return Ok(());
    }
    ensure!(
        choices.len() < 8192,
        "dependency version requirement limit exceeded"
    );
    choices.insert(ga.into(), version.into());
    Ok(())
}
fn verify_lock_files(lock: &Lock, cache: &Path) -> Result<()> {
    for f in &lock.files {
        ensure!(
            Path::new(&f.path)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid lock cache path"
        );
        let data = std::fs::read(cache.join(&f.path))
            .with_context(|| format!("locked artifact missing: {}", f.path))?;
        ensure!(
            data.len() as u64 == f.bytes && blake3::hash(&data).to_hex().as_str() == f.blake3,
            "locked artifact hash mismatch: {}",
            f.path
        );
    }
    Ok(())
}
/// Normal IDE sync: reuse an intact matching lock, otherwise recompute from repositories/cache.
/// Schema2 migration validates old bytes first; explicit --locked remains read-only/strict.
pub fn resolve_project(root: &Path, cache: &Path, offline: bool) -> Result<Lock> {
    let hash = blake3::hash(&std::fs::read(root.join("wex.json"))?)
        .to_hex()
        .to_string();
    let old = std::fs::read(root.join("wex.lock.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Lock>(&b).ok());
    if let Some(lock) = old.filter(|l| l.schema == 1 && l.config_blake3 == hash) {
        if lock.resolver_abi == RESOLVER_ABI {
            return resolve(root, cache, offline, true);
        }
        if lock.resolver_abi == 2 {
            verify_lock_files(&lock, cache)?;
        }
    }
    resolve(root, cache, offline, false)
}
/// Builds still require an existing matching lock. Only an intact legacy schema2
/// lock is upgraded offline, before source fingerprinting and tool execution.
pub fn resolve_for_build(root: &Path, cache: &Path) -> Result<Lock> {
    let old: Lock = serde_json::from_slice(&std::fs::read(root.join("wex.lock.json"))?)?;
    let hash = blake3::hash(&std::fs::read(root.join("wex.json"))?)
        .to_hex()
        .to_string();
    ensure!(
        old.config_blake3 == hash,
        "Wex config changed; regenerate lock"
    );
    if old.schema == 1 && old.resolver_abi == 2 {
        verify_lock_files(&old, cache)?;
        resolve(root, cache, true, false)
    } else {
        resolve(root, cache, true, true)
    }
}
type ScopeContext = (String, Vec<String>);
fn trace_scopes(
    config: &Config,
    edges: &BTreeMap<ScopeContext, Vec<ScopeContext>>,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut scopes = BTreeMap::new();
    for m in &config.modules {
        let mut todo = m
            .dependencies
            .iter()
            .map(|p| Ok((Coordinate::parse(p)?.ga(), vec![])))
            .collect::<Result<Vec<_>>>()?;
        let mut visited = BTreeSet::new();
        let mut scope = BTreeSet::new();
        while let Some(ctx) = todo.pop() {
            if !visited.insert(ctx.clone()) {
                continue;
            }
            scope.insert(ctx.0.clone());
            todo.extend(
                edges
                    .get(&ctx)
                    .context("resolved exclusion context missing")?
                    .iter()
                    .cloned(),
            );
        }
        scopes.insert(m.name.clone(), scope);
    }
    Ok(scopes)
}
/// Resolve fixed POM dependencies (legacy nearest/first) or published Android/JVM
/// module variants with highest currently reachable fixed requirements and explicit pins.
/// --locked is read-only and verifies every cached byte without launching Java.
pub fn resolve(root: &Path, cache: &Path, offline: bool, locked: bool) -> Result<Lock> {
    let config_bytes = std::fs::read(root.join("wex.json"))?;
    let config = Config::from_bytes(&config_bytes)?;
    let config_blake3 = blake3::hash(&config_bytes).to_hex().to_string();
    let lock_path = root.join("wex.lock.json");
    if locked {
        let lock: Lock = serde_json::from_slice(&std::fs::read(&lock_path)?)?;
        ensure!(
            lock.schema == 1
                && lock.resolver_abi == RESOLVER_ABI
                && lock.config_blake3 == config_blake3,
            "Wex config changed; regenerate lock"
        );
        ensure!(
            !config.android_variants || lock.mediation == ANDROID_MEDIATION,
            "Android resolver selection changed; regenerate lock"
        );
        ensure!(
            lock.module_scopes.keys().cloned().collect::<BTreeSet<_>>()
                == config.modules.iter().map(|m| m.name.clone()).collect(),
            "lock module scopes missing; regenerate lock"
        );
        verify_lock_files(&lock, cache)?;
        return Ok(lock);
    }
    // Explicit project constraints align legacy/KMP artifact families without
    // stripping duplicate classes or silently changing nearest-first mediation.
    let explicit_constraints = config
        .constraints
        .iter()
        .map(|s| parse_constraint(s))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut r = Resolver {
        config: config.clone(),
        cache: cache.into(),
        offline,
        files: BTreeMap::new(),
        poms: BTreeMap::new(),
        active: BTreeSet::new(),
    };
    let mut constraints = BTreeMap::new();
    for pin in &config.platforms {
        let pom = r.pom(&Coordinate::parse(pin)?)?;
        ensure!(
            pom.packaging == "pom",
            "project platform must use POM packaging: {pin}"
        );
        // Maven import ordering: first platform wins; exact project pins override.
        for (ga, dep) in pom.managed {
            constraints.entry(ga).or_insert(dep.coordinate.version);
        }
    }
    for (pattern, version) in &explicit_constraints {
        if let Some(group) = pattern.strip_suffix('*') {
            for (ga, current) in &mut constraints {
                if ga.starts_with(group) && !explicit_constraints.contains_key(ga) {
                    current.clone_from(version);
                }
            }
        }
    }
    constraints.extend(explicit_constraints);
    let platform_files = r.files.clone();
    let mut choices = BTreeMap::<String, String>::new();
    if config.android_variants {
        for m in &config.modules {
            for pin in &m.dependencies {
                let c = Coordinate::parse(pin)?;
                record_requirement(&mut choices, &constraints, &c.ga(), &c.version)?;
            }
        }
    }
    let mut passes = 0;
    let mut seen_choices = BTreeSet::new();
    let (mut artifacts, module_scopes) = loop {
        passes += 1;
        ensure!(
            passes <= 32,
            "Android version mediation did not converge in32 passes"
        );
        r.files = platform_files.clone();
        r.poms.clear();
        r.active.clear();
        ensure!(
            seen_choices.insert(choices.clone()),
            "Android version mediation cycle; add an explicit project constraint"
        );
        // Recompute from the currently reachable graph, not obsolete selected versions.
        let mut next_choices = BTreeMap::new();
        let mut queue = VecDeque::new();
        for m in &config.modules {
            for d in &m.dependencies {
                queue.push_back((Coordinate::parse(d)?, BTreeSet::new()));
            }
        }
        let mut chosen = BTreeSet::new();
        let mut traversed = BTreeSet::new();
        let mut artifacts = vec![];
        let mut context_edges = BTreeMap::<ScopeContext, Vec<ScopeContext>>::new();
        while let Some((mut c, exclusions)) = queue.pop_front() {
            if config.android_variants {
                record_requirement(&mut next_choices, &constraints, &c.ga(), &c.version)?;
            }
            if let Some(version) =
                constraint_pin(&constraints, &c.ga()).or_else(|| choices.get(&c.ga()))
            {
                c.version.clone_from(version);
            }
            ensure!(
                chosen.len() < 2048 && traversed.len() < 8192,
                "dependency graph exceeds limits"
            );
            // Distinct exclusion paths must still be expanded even if an artifact was selected before.
            let first = chosen.insert(c.ga());
            let selected = artifacts
                .iter()
                .find(|a: &&Artifact| a.coordinate.ga() == c.ga())
                .map(|a| a.coordinate.clone())
                .unwrap_or(c.clone());
            let context = (
                selected.ga(),
                exclusions.iter().cloned().collect::<Vec<_>>(),
            );
            if !traversed.insert(context.clone()) {
                continue;
            }
            let mut pom = r.pom(&selected)?;
            let mut binary_metadata = None;
            let mut selected_variant = None;
            if config.android_variants && pom.module_metadata {
                let metadata = r.file(&selected, "module")?;
                let variant = crate::module_metadata::select(
                    &std::fs::read(metadata)?,
                    &selected,
                    config.build.jvm_target,
                    &constraints,
                )
                .with_context(|| {
                    format!("selecting Android module variant for {}", selected.text())
                })?;
                selected_variant = Some(variant.name.clone());
                for (ga, version) in &variant.requirements {
                    record_requirement(&mut next_choices, &constraints, ga, version)?;
                }
                if let Some(alias) = variant.alias {
                    pom.packaging = "pom".into();
                    pom.dependencies = vec![Dep {
                        coordinate: alias,
                        scope: "runtime".into(),
                        kind: "auto".into(),
                        optional: false,
                        exclusions: BTreeSet::new(),
                    }];
                } else {
                    if let Some(binary) = variant.binary {
                        pom.packaging = binary.kind.clone();
                        binary_metadata = Some(binary);
                    } else {
                        pom.packaging = "pom".into();
                    }
                    pom.dependencies = variant
                        .dependencies
                        .into_iter()
                        .map(|d| Dep {
                            coordinate: d.coordinate,
                            scope: "runtime".into(),
                            kind: "auto".into(),
                            optional: false,
                            exclusions: d.exclusions,
                        })
                        .collect();
                }
            }
            ensure!(
                ["jar", "aar", "pom"].contains(&pom.packaging.as_str()),
                "unsupported packaging {}",
                pom.packaging
            );
            let mut deps = vec![];
            let mut children = vec![];
            for d in pom.dependencies {
                if d.optional
                    || !["compile", "runtime"].contains(&d.scope.as_str())
                    || excluded(&exclusions, &d.coordinate)
                {
                    continue;
                }
                ensure!(
                    ["auto", "jar", "aar", "pom"].contains(&d.kind.as_str()),
                    "unsupported dependency type {}",
                    d.kind
                );
                deps.push(d.coordinate.text());
                let mut next = exclusions.clone();
                next.extend(d.exclusions);
                children.push((d.coordinate.ga(), next.iter().cloned().collect()));
                queue.push_back((d.coordinate, next));
            }
            context_edges.insert(context, children);
            if first {
                let file = if pom.packaging == "pom" {
                    String::new()
                } else {
                    let path = r.file(&selected, &pom.packaging)?;
                    if let Some(metadata) = binary_metadata {
                        use sha2::Digest;
                        let bytes = std::fs::read(&path)?;
                        ensure!(
                            metadata.size.is_none_or(|n| n == bytes.len() as u64),
                            "module binary size mismatch: {}",
                            selected.text()
                        );
                        if let Some(expected) = metadata.sha256 {
                            ensure!(
                                sha2::Sha256::digest(&bytes)
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<String>()
                                    .eq_ignore_ascii_case(&expected),
                                "module binary checksum mismatch: {}",
                                selected.text()
                            );
                        }
                    }
                    path.strip_prefix(cache)?.to_string_lossy().into_owned()
                };
                artifacts.push(Artifact {
                    coordinate: selected,
                    variant: selected_variant,
                    packaging: pom.packaging,
                    file,
                    dependencies: deps,
                });
            } else if let Some(a) = artifacts
                .iter_mut()
                .find(|a| a.coordinate.ga() == selected.ga())
            {
                a.dependencies.extend(deps);
            }
        }
        if config.android_variants && next_choices != choices {
            choices = next_choices;
            continue;
        }
        let module_scopes = trace_scopes(&config, &context_edges)?;
        break (artifacts, module_scopes);
    };
    // Convert requested edges to selected versions; no dangling optional/excluded edges.
    let selected: BTreeMap<_, _> = artifacts
        .iter()
        .map(|a| (a.coordinate.ga(), a.coordinate.text()))
        .collect();
    for a in &mut artifacts {
        a.dependencies = a
            .dependencies
            .iter()
            .filter_map(|d| {
                Coordinate::parse(d)
                    .ok()
                    .and_then(|c| selected.get(&c.ga()).cloned())
            })
            .collect();
        a.dependencies.sort();
        a.dependencies.dedup();
    }
    let lock = Lock {
        schema: 1,
        resolver_abi: RESOLVER_ABI,
        config_blake3,
        module_scopes,
        artifacts,
        files: r.files.into_values().collect(),
        mediation: if config.android_variants { ANDROID_MEDIATION } else if constraints.is_empty() { "nearest-first-declaration (POM compile+runtime; fixed versions)" } else { "nearest-first-declaration + enforced project constraints (POM compile+runtime; fixed versions)" }.into(),
    };
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    std::io::Write::write_all(&mut temp, &serde_json::to_vec_pretty(&lock)?)?;
    ensure!(
        blake3::hash(&std::fs::read(root.join("wex.json"))?)
            .to_hex()
            .as_str()
            == lock.config_blake3,
        "config changed while resolving; lock not published"
    );
    temp.persist(lock_path).map_err(|e| e.error)?;
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn artifact(repo: &Path, name: &str, body: &str, packaging: &str) {
        let c = Coordinate::parse(name).unwrap();
        let p = repo.join(c.path("pom"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p,format!("<project><groupId>{}</groupId><artifactId>{}</artifactId><version>{}</version><packaging>{packaging}</packaging>{body}</project>",c.group,c.artifact,c.version)).unwrap();
        if packaging != "pom" {
            std::fs::write(repo.join(c.path(packaging)), format!("artifact:{name}")).unwrap();
        }
    }
    fn dep(a: &str, v: &str, extra: &str) -> String {
        format!(
            "<dependency><groupId>dev.test</groupId><artifactId>{a}</artifactId><version>{v}</version>{extra}</dependency>"
        )
    }
    fn config(root: &Path, repo: &Path, deps: Vec<&str>) {
        let c = Config {
            build: BuildSettings::default(),
            constraints: vec![],
            platforms: vec![],
            android_variants: false,
            schema: 1,
            name: "test".into(),
            repositories: vec![url::Url::from_directory_path(repo).unwrap().to_string()],
            android: Android {
                compile_sdk: 36,
                min_sdk: 24,
                target_sdk: 36,
                application_id: "dev.wex.test".into(),
            },
            modules: vec![Module {
                name: "app".into(),
                sources: vec!["src".into()],
                resources: vec![],
                projects: vec![],
                namespace: String::new(),
                manifest: None,
                assets: vec![],
                dependencies: deps.into_iter().map(String::from).collect(),
            }],
        };
        std::fs::write(root.join("wex.json"), serde_json::to_vec(&c).unwrap()).unwrap();
    }
    fn android_mode(root: &Path) {
        let path = root.join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.android_variants = true;
        std::fs::write(path, serde_json::to_vec(&c).unwrap()).unwrap();
    }
    #[test]
    fn offline_allows_local_repository_without_network_or_existing_cache() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(repo.path(), "dev.test:local:1", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:local:1"]);
        let l = resolve(root.path(), cache.path(), true, false).unwrap();
        assert_eq!(l.artifacts.len(), 1);
        assert!(cache.path().join(&l.artifacts[0].file).is_file());
        assert_eq!(
            l.module_scopes["app"],
            BTreeSet::from(["dev.test:local".into()])
        );
    }
    #[test]
    fn module_scopes_preserve_exclusions_even_when_another_root_requires_the_artifact() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(repo.path(), "dev.test:leaf:1", "", "jar");
        artifact(
            repo.path(),
            "dev.test:core:1",
            &format!("<dependencies>{}</dependencies>", dep("leaf", "1", "")),
            "jar",
        );
        let exclude = "<exclusions><exclusion><groupId>dev.test</groupId><artifactId>leaf</artifactId></exclusion></exclusions>";
        artifact(
            repo.path(),
            "dev.test:bridge:1",
            &format!("<dependencies>{}</dependencies>", dep("core", "1", exclude)),
            "jar",
        );
        config(root.path(), repo.path(), vec!["dev.test:bridge:1"]);
        let mut c = Config::load(root.path()).unwrap();
        c.modules.push(Module {
            name: "shared".into(),
            sources: vec!["shared".into()],
            dependencies: vec!["dev.test:core:1".into()],
            ..Default::default()
        });
        std::fs::write(
            root.path().join("wex.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        let l = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(
            l.module_scopes["app"],
            BTreeSet::from(["dev.test:bridge".into(), "dev.test:core".into()])
        );
        assert_eq!(
            l.module_scopes["shared"],
            BTreeSet::from(["dev.test:core".into(), "dev.test:leaf".into()])
        );
        assert!(
            l.artifacts
                .iter()
                .find(|a| a.coordinate.artifact == "core")
                .unwrap()
                .dependencies
                .iter()
                .any(|d| d.contains("leaf"))
        );
        assert_eq!(
            resolve(root.path(), cache.path(), true, true)
                .unwrap()
                .module_scopes,
            l.module_scopes
        );
    }
    #[test]
    fn legacy_lock_upgrade_is_offline_and_tamper_checked_while_locked_stays_read_only() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(repo.path(), "dev.test:a:1", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        let mut old = resolve(root.path(), cache.path(), false, false).unwrap();
        old.resolver_abi = 2;
        old.module_scopes.clear();
        let bytes = serde_json::to_vec(&old).unwrap();
        let path = root.path().join("wex.lock.json");
        std::fs::write(&path, &bytes).unwrap();
        assert!(resolve(root.path(), cache.path(), true, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let fresh = resolve_for_build(root.path(), cache.path()).unwrap();
        assert_eq!(fresh.resolver_abi, RESOLVER_ABI);
        assert!(fresh.module_scopes["app"].contains("dev.test:a"));
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(cache.path().join(&old.artifacts[0].file), b"tampered").unwrap();
        assert!(resolve_for_build(root.path(), cache.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn android_lock_rejects_an_old_selection_algorithm_without_touching_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(repo.path(), "dev.test:a:1", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        android_mode(root.path());
        let mut lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.mediation, ANDROID_MEDIATION);
        lock.mediation = "legacy-selection".into();
        let bytes = serde_json::to_vec(&lock).unwrap();
        std::fs::write(root.path().join("wex.lock.json"), &bytes).unwrap();
        assert!(
            resolve(root.path(), cache.path(), true, true)
                .unwrap_err()
                .to_string()
                .contains("selection changed")
        );
        assert_eq!(
            std::fs::read(root.path().join("wex.lock.json")).unwrap(),
            bytes
        );
    }
    #[test]
    fn highest_reachable_requirements_replace_old_edges_and_prune_old_files() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for (name, deps) in [
            ("a:1", dep("startup", "1", "")),
            ("b:1", dep("deep", "1", "")),
            ("deep:1", dep("startup", "2", "")),
            ("startup:1", dep("foo", "9", "")),
            ("startup:2", dep("foo", "1", "")),
            ("foo:1", String::new()),
            ("foo:9", String::new()),
        ] {
            artifact(
                repo.path(),
                &format!("dev.test:{name}"),
                &format!("<dependencies>{deps}</dependencies>"),
                "jar",
            );
        }
        config(
            root.path(),
            repo.path(),
            vec!["dev.test:a:1", "dev.test:b:1"],
        );
        let legacy = resolve(root.path(), cache.path(), false, false).unwrap();
        assert!(
            legacy
                .artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:startup:1")
        );
        android_mode(root.path());
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:startup:2")
        );
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:foo:1")
        );
        assert!(
            !lock
                .artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:foo:9")
        );
        assert!(
            !lock
                .files
                .iter()
                .any(|f| f.path.contains("startup/1/") || f.path.contains("foo/9/"))
        );
        assert!(resolve(root.path(), cache.path(), true, true).is_ok());
    }
    #[test]
    fn mediation_cycle_is_bounded_and_does_not_replace_previous_lock() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // Version2 is required only by version1's obsolete self edge. No fixed point exists.
        artifact(
            repo.path(),
            "dev.test:a:1",
            &format!("<dependencies>{}</dependencies>", dep("a", "2", "")),
            "jar",
        );
        artifact(repo.path(), "dev.test:a:2", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        let old = resolve(root.path(), cache.path(), false, false).unwrap();
        let before = std::fs::read(root.path().join("wex.lock.json")).unwrap();
        assert!(!old.files.is_empty());
        android_mode(root.path());
        assert!(
            resolve(root.path(), cache.path(), false, false)
                .unwrap_err()
                .to_string()
                .contains("mediation cycle")
        );
        assert_eq!(
            std::fs::read(root.path().join("wex.lock.json")).unwrap(),
            before
        );
    }
    #[test]
    fn android_module_alias_selects_android_binary_and_locks_metadata() {
        use serde_json::json;
        use sha2::Digest;
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let marker = "<!-- published-with-gradle-metadata -->";
        artifact(repo.path(), "dev.test:root:1", marker, "jar");
        artifact(repo.path(), "dev.test:android:1", marker, "jar");
        let root_metadata = json!({"formatVersion":"1.1","component":{"group":"dev.test","module":"root","version":"1"},"variants":[
          {"name":"android","attributes":{"org.gradle.category":"library","org.gradle.usage":"java-runtime","org.gradle.jvm.environment":"android"},"available-at":{"group":"dev.test","module":"android","version":"1","url":"../../android/1/android-1.module"}},
          {"name":"jvm","attributes":{"org.gradle.category":"library","org.gradle.usage":"java-runtime","org.gradle.jvm.environment":"standard-jvm"},"available-at":{"group":"dev.test","module":"jvm","version":"1","url":"../../jvm/1/jvm-1.module"}}]});
        std::fs::write(
            repo.path()
                .join(Coordinate::parse("dev.test:root:1").unwrap().path("module")),
            serde_json::to_vec(&root_metadata).unwrap(),
        )
        .unwrap();
        let bytes = std::fs::read(
            repo.path()
                .join(Coordinate::parse("dev.test:android:1").unwrap().path("jar")),
        )
        .unwrap();
        let hash = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut target = json!({"formatVersion":"1.1","component":{"group":"dev.test","module":"root","version":"1","url":"../../root/1/root-1.module"},"variants":[{"name":"runtime","attributes":{"org.gradle.usage":"java-runtime","org.gradle.jvm.environment":"android"},"files":[{"url":"android-1.jar","size":bytes.len(),"sha256":hash}]}]});
        let metadata = repo.path().join(
            Coordinate::parse("dev.test:android:1")
                .unwrap()
                .path("module"),
        );
        std::fs::write(&metadata, serde_json::to_vec(&target).unwrap()).unwrap();
        config(root.path(), repo.path(), vec!["dev.test:root:1"]);
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.android_variants = true;
        std::fs::write(path, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.artifacts.len(), 2);
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.artifact == "android"
                    && a.variant.as_deref() == Some("runtime"))
        );
        assert!(
            lock.artifacts
                .iter()
                .find(|a| a.coordinate.artifact == "root")
                .unwrap()
                .file
                .is_empty()
        );
        assert_eq!(
            lock.files
                .iter()
                .filter(|f| f.path.ends_with(".module"))
                .count(),
            2
        );
        assert!(resolve(root.path(), cache.path(), true, true).is_ok());
        let before = std::fs::read(root.path().join("wex.lock.json")).unwrap();
        target["variants"][0]["files"][0]["sha256"] = json!("0".repeat(64));
        std::fs::write(metadata, serde_json::to_vec(&target).unwrap()).unwrap();
        let fresh = tempfile::tempdir().unwrap();
        assert!(
            resolve(root.path(), fresh.path(), false, false)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        assert_eq!(
            std::fs::read(root.path().join("wex.lock.json")).unwrap(),
            before
        );
    }
    #[test]
    fn metadata_requirement_aligns_a_dependency_reached_from_another_branch() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(
            repo.path(),
            "dev.test:producer:2",
            "<!-- published-with-gradle-metadata -->",
            "jar",
        );
        artifact(
            repo.path(),
            "dev.test:wrapper:1",
            &format!("<dependencies>{}</dependencies>", dep("peer", "1", "")),
            "jar",
        );
        artifact(repo.path(), "dev.test:peer:1", "", "jar");
        artifact(repo.path(), "dev.test:peer:2", "", "jar");
        let metadata = json!({"formatVersion":"1.1","component":{"group":"dev.test","module":"producer","version":"2"},"variants":[{"name":"runtime","attributes":{"org.gradle.usage":"java-runtime"},"dependencyConstraints":[{"group":"dev.test","module":"peer","version":{"requires":"2"}}],"files":[{"url":"producer-2.jar"}]}]});
        std::fs::write(
            repo.path().join(
                Coordinate::parse("dev.test:producer:2")
                    .unwrap()
                    .path("module"),
            ),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        config(
            root.path(),
            repo.path(),
            vec!["dev.test:producer:2", "dev.test:wrapper:1"],
        );
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.android_variants = true;
        std::fs::write(path, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:peer:2")
        );
    }
    #[test]
    fn group_constraints_align_transitives_with_exact_project_override() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(
            repo.path(),
            "dev.test:a:1",
            &format!("<dependencies>{}</dependencies>", dep("b", "1", "")),
            "jar",
        );
        artifact(repo.path(), "dev.test:b:2", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        let p = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        c.constraints = vec!["dev.test:*:2".into(), "dev.test:a:1".into()];
        std::fs::write(p, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:a:1")
        );
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:b:2")
        );
    }
    #[test]
    fn project_bom_aligns_transitives_and_explicit_constraints_override_it() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(
            repo.path(),
            "dev.test:a:1",
            &format!("<dependencies>{}</dependencies>", dep("b", "1", "")),
            "jar",
        );
        artifact(repo.path(), "dev.test:b:2", "", "jar");
        artifact(repo.path(), "dev.test:b:3", "", "jar");
        artifact(
            repo.path(),
            "dev.test:bom:1",
            &format!(
                "<dependencyManagement><dependencies>{}{}</dependencies></dependencyManagement>",
                dep("b", "2", ""),
                dep("unused", "99", "")
            ),
            "pom",
        );
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.platforms = vec!["dev.test:bom:1".into()];
        std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.artifacts.len(), 2);
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:b:2")
        );
        assert!(lock.files.iter().any(|f| f.path.ends_with("bom-1.pom")));
        assert!(resolve(root.path(), cache.path(), true, true).is_ok());
        c.constraints = vec!["dev.test:b:3".into()];
        std::fs::write(path, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:b:3")
        );
    }
    #[test]
    fn jar_is_not_accepted_as_a_project_platform() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(repo.path(), "dev.test:notbom:1", "", "jar");
        config(root.path(), repo.path(), vec![]);
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.platforms = vec!["dev.test:notbom:1".into()];
        std::fs::write(path, serde_json::to_vec(&c).unwrap()).unwrap();
        assert!(
            resolve(root.path(), cache.path(), false, false)
                .unwrap_err()
                .to_string()
                .contains("POM packaging")
        );
    }
    #[test]
    fn constraints_override_reachable_versions_but_do_not_introduce_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(
            repo.path(),
            "dev.test:a:1",
            &format!("<dependencies>{}</dependencies>", dep("b", "1", "")),
            "jar",
        );
        artifact(repo.path(), "dev.test:b:2", "", "jar");
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.constraints = vec!["dev.test:b:2".into(), "dev.test:unused:99".into()];
        std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.artifacts.len(), 2);
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:b:2")
        );
        assert!(
            lock.artifacts
                .iter()
                .find(|a| a.coordinate.artifact == "a")
                .unwrap()
                .dependencies
                .contains(&"dev.test:b:2".into())
        );
        assert!(resolve(root.path(), cache.path(), true, true).is_ok());
    }
    #[test]
    fn conflicting_constraints_fail_before_repository_access() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        config(root.path(), repo.path(), vec![]);
        let path = root.path().join("wex.json");
        let mut c: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        c.constraints = vec!["dev.test:b:1".into(), "dev.test:b:2".into()];
        std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
        assert!(
            Config::load(root.path())
                .unwrap_err()
                .to_string()
                .contains("conflicting project constraints")
        );
    }
    #[test]
    fn nearest_direct_pin_optional_scope_exclusions_and_locked_integrity() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let body = format!(
            "<dependencies>{}{}{}{}{}</dependencies>",
            dep("b", "2", ""),
            dep("optional", "1", "<optional>true</optional>"),
            dep("test", "1", "<scope>test</scope>"),
            dep("excluded", "1", ""),
            dep("runtime", "1", "<scope>runtime</scope>")
        );
        artifact(repo.path(), "dev.test:a:1", &body, "aar");
        artifact(repo.path(), "dev.test:b:1", "", "jar");
        artifact(repo.path(), "dev.test:runtime:1", "", "jar");
        artifact(
            repo.path(),
            "dev.test:wrapper:1",
            &format!(
                "<dependencies>{}</dependencies>",
                dep(
                    "a",
                    "1",
                    "<exclusions><exclusion><groupId>dev.test</groupId><artifactId>excluded</artifactId></exclusion></exclusions>"
                )
            ),
            "jar",
        );
        config(
            root.path(),
            repo.path(),
            vec!["dev.test:wrapper:1", "dev.test:b:1"],
        );
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.artifacts.len(), 4);
        assert!(
            lock.artifacts
                .iter()
                .any(|a| a.coordinate.text() == "dev.test:b:1")
        );
        assert!(
            lock.artifacts
                .iter()
                .all(|a| !a.coordinate.text().contains(":b:2"))
        );
        assert!(resolve(root.path(), cache.path(), true, true).is_ok());
        std::fs::write(cache.path().join(&lock.files[0].path), b"tampered").unwrap();
        assert!(
            resolve(root.path(), cache.path(), true, true)
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
    }
    #[test]
    fn parent_properties_dependency_management_and_bom_import() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        artifact(
            repo.path(),
            "dev.test:parent:1",
            "<properties><v>3</v></properties>",
            "pom",
        );
        artifact(
            repo.path(),
            "dev.test:bom:1",
            &format!(
                "<dependencyManagement><dependencies>{}</dependencies></dependencyManagement>",
                dep("leaf", "3", "")
            ),
            "pom",
        );
        artifact(repo.path(), "dev.test:leaf:3", "", "jar");
        artifact(
            repo.path(),
            "dev.test:app:1",
            &format!(
                "<parent><groupId>dev.test</groupId><artifactId>parent</artifactId><version>1</version></parent><dependencyManagement><dependencies>{}</dependencies></dependencyManagement><dependencies><dependency><groupId>dev.test</groupId><artifactId>leaf</artifactId></dependency></dependencies>",
                dep("bom", "1", "<scope>import</scope><type>pom</type>")
            ),
            "jar",
        );
        config(root.path(), repo.path(), vec!["dev.test:app:1"]);
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        assert_eq!(lock.artifacts.len(), 2);
        assert!(lock.files.iter().any(|f| f.path.ends_with("bom-1.pom")));
        assert_eq!(
            expand("${v}", &BTreeMap::from([("v".into(), "3".into())])).unwrap(),
            "3"
        );
    }
    #[test]
    fn offline_missing_config_changes_and_pom_cycles_are_errors() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        assert!(
            resolve(root.path(), cache.path(), true, false)
                .unwrap_err()
                .to_string()
                .contains("offline cache")
        );
        artifact(
            repo.path(),
            "dev.test:a:1",
            "<parent><groupId>dev.test</groupId><artifactId>a</artifactId><version>1</version></parent>",
            "jar",
        );
        assert!(
            resolve(root.path(), cache.path(), false, false)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
        assert!(!root.path().join("wex.lock.json").exists());
    }
    #[test]
    fn rejects_dynamic_coordinates_and_unknown_config_fields() {
        for c in [
            "dev.test:a:1.+",
            "../x:a:1",
            "dev.test:a:LATEST",
            "dev.test:a:1-SNAPSHOT",
            "dev.test:a:[1,2]",
            "dev.test:a:1:classifier",
        ] {
            assert!(Coordinate::parse(c).is_err(), "{c}");
        }
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        config(root.path(), repo.path(), vec![]);
        let path = root.path().join("wex.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["gradle"] = "download".into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(Config::load(root.path()).is_err());
        v.as_object_mut().unwrap().remove("gradle");
        v["modules"][0]["sources"] = serde_json::json!(["../outside"]);
        std::fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(Config::load(root.path()).is_err());
    }
    #[test]
    fn unsupported_dependency_profile_is_not_silently_ignored() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        artifact(
            repo.path(),
            "dev.test:a:1",
            "<profiles><profile><id>x</id><dependencies/></profile></profiles>",
            "jar",
        );
        assert!(
            resolve(root.path(), cache.path(), false, false)
                .unwrap_err()
                .to_string()
                .contains("profiles")
        );
    }
    #[test]
    fn unchanged_config_locked_resolve_is_read_only_and_repeatable() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        config(root.path(), repo.path(), vec!["dev.test:a:1"]);
        artifact(repo.path(), "dev.test:a:1", "", "jar");
        let lock = resolve(root.path(), cache.path(), false, false).unwrap();
        let path = root.path().join("wex.lock.json");
        let before = std::fs::read(&path).unwrap();
        let again = resolve(root.path(), cache.path(), true, true).unwrap();
        assert_eq!(lock.config_blake3, again.config_blake3);
        assert_eq!(before, std::fs::read(&path).unwrap());
        let mut c = Config::load(root.path()).unwrap();
        c.name = "changed".into();
        std::fs::write(
            root.path().join("wex.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        assert!(
            resolve(root.path(), cache.path(), true, true)
                .unwrap_err()
                .to_string()
                .contains("config changed")
        );
    }
}

#[cfg(test)]
mod edge_exclusions_tests {
    use super::*;
    #[test]
    fn wildcard_edge_exclusions_keep_the_dependency_itself() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join("dev/test/a/1")).unwrap();
        std::fs::create_dir_all(repo.join("dev/test/b/1")).unwrap();
        std::fs::write(repo.join("dev/test/a/1/a-1.pom"),"<project><dependencies><dependency><groupId>dev.test</groupId><artifactId>b</artifactId><version>1</version><exclusions><exclusion><groupId>*</groupId><artifactId>*</artifactId></exclusion></exclusions></dependency></dependencies></project>").unwrap();
        std::fs::write(repo.join("dev/test/a/1/a-1.jar"), b"a").unwrap();
        std::fs::write(repo.join("dev/test/b/1/b-1.pom"), "<project/>").unwrap();
        std::fs::write(repo.join("dev/test/b/1/b-1.jar"), b"b").unwrap();
        let c = Config {
            schema: 1,
            name: "edge".into(),
            repositories: vec![url::Url::from_directory_path(&repo).unwrap().to_string()],
            android: Android {
                compile_sdk: 36,
                min_sdk: 26,
                target_sdk: 36,
                application_id: "dev.wex.test".into(),
            },
            build: BuildSettings::default(),
            constraints: vec![],
            platforms: vec![],
            android_variants: false,
            modules: vec![Module {
                name: "app".into(),
                dependencies: vec!["dev.test:a:1".into()],
                ..Default::default()
            }],
        };
        std::fs::write(d.path().join("wex.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        let l = resolve(d.path(), &d.path().join("cache"), false, false).unwrap();
        assert_eq!(l.artifacts.len(), 2);
        assert!(l.artifacts.iter().any(|a| a.coordinate.artifact == "b"));
    }
}
