//! Native Android build orchestration. No Gradle, AGP, Maven process, wrapper or daemon.
use crate::{
    manifest,
    native::{self, Config},
    project::{Module, ProjectGraph},
    run::{ApkArtifact, RunEvent},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed_ms: u128,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildReport {
    pub backend: String,
    pub module: String,
    pub variant: String,
    pub apk: ApkArtifact,
    pub blake3: String,
    pub bytes: u64,
    pub config_blake3: String,
    pub input_blake3: String,
    pub kotlin_files: usize,
    pub java_files: usize,
    pub dependency_artifacts: usize,
    pub build_dir: PathBuf,
    pub graph_file: PathBuf,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose: Option<ComposeBuild>,
    pub elapsed_ms: u128,
    #[serde(default)]
    pub compilation_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub dex_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub sdk_cache: Vec<crate::compile_cache::Record>,
    #[serde(default)]
    pub resource_cache: Vec<crate::resource_cache::Record>,
    #[serde(default = "legacy_debuggable")]
    pub debuggable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<crate::signing::Info>,
}
fn legacy_debuggable() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComposeBuild {
    pub compiler_version: String,
    pub plugin_jar: PathBuf,
    pub plugin_blake3: String,
    pub source_information: bool,
    pub strong_skipping: bool,
    pub reports: bool,
    pub modules: Vec<String>,
}
fn compose_plugin_filename(version: &str) -> String {
    format!("kotlin-compose-compiler-plugin-embeddable-{version}.jar")
}
/// Compiler-plugin inputs are independent of the app dependency classpath/DEX.
fn compose_args(
    plugin: &Path,
    settings: &native::ComposeSettings,
    module: &str,
    work: &Path,
) -> Vec<OsString> {
    let mut args = vec![os(format!("-Xplugin={}", plugin.display()))];
    let mut option = |key: &str, value: String| {
        args.push(os("-P"));
        args.push(os(format!(
            "plugin:androidx.compose.compiler.plugins.kotlin:{key}={value}"
        )));
    };
    option("sourceInformation", settings.source_information.to_string());
    if !settings.strong_skipping {
        option("featureFlag", "-StrongSkipping".into());
    }
    if settings.reports {
        let folder = work.join("compose").join(module);
        option(
            "reportsDestination",
            folder.join("reports").to_string_lossy().into_owned(),
        );
        option(
            "metricsDestination",
            folder.join("metrics").to_string_lossy().into_owned(),
        );
    }
    args
}
#[derive(Clone)]
pub(crate) struct Tools {
    sdk: PathBuf,
    bt: PathBuf,
    jdk: PathBuf,
    compiler: Vec<PathBuf>,
    secret_env: Vec<String>,
}
fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}
fn namespace(s: &str) -> bool {
    s.split('.').count() > 1
        && s.split('.').all(|s| {
            !s.is_empty()
                && s.as_bytes()[0].is_ascii_alphabetic()
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        })
}
fn tools(config: &Config, apk: bool) -> Result<Tools> {
    let sdk = std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Android/Sdk"))
        .canonicalize()
        .context("Android SDK directory missing")?;
    ensure!(
        config
            .build
            .build_tools
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid build-tools version"
    );
    let bt = sdk.join("build-tools").join(&config.build.build_tools);
    for n in if apk {
        vec!["aapt2", "d8", "zipalign", "apksigner"]
    } else {
        vec!["aapt2"]
    } {
        ensure!(
            bt.join(n).is_file(),
            "SDK tool missing: {}",
            bt.join(n).display()
        );
    }
    // Resolve environment tool roots once, before child cwd changes. Android SDK
    // already does this above; Java tools/JAVA_HOME must bind the same real JDK.
    let jdk = std::env::var_os("JAVA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/jvm/java-21-openjdk"))
        .canonicalize()
        .context("JDK directory missing")?;
    for n in if apk {
        vec!["java", "javac", "jar", "keytool", "jmod", "jlink"]
    } else {
        vec!["java", "javac", "jar", "jmod", "jlink"]
    } {
        ensure!(jdk.join("bin").join(n).is_file(), "JDK tool missing: {n}");
    }
    ensure!(
        sdk.join(format!(
            "platforms/android-{}/android.jar",
            config.android.compile_sdk
        ))
        .is_file(),
        "Android platform missing"
    );
    ensure!(
        [8, 11, 17].contains(&config.build.jvm_target),
        "supported JVM targets: 8,11,17"
    );
    Ok(Tools {
        sdk,
        bt,
        jdk,
        compiler: vec![],
        secret_env: config
            .build
            .release_signing
            .as_ref()
            .map(|s| vec![s.store_password_env.clone(), s.key_password_env.clone()])
            .unwrap_or_default(),
    })
}
pub(crate) fn toolchain_directory(config: &Config) -> Result<PathBuf> {
    let version = &config.build.kotlin_version;
    native::Coordinate::parse(&format!(
        "org.jetbrains.kotlin:kotlin-compiler-embeddable:{version}"
    ))?;
    Ok(home()
        .join(".cache/wex/toolchains")
        .join(if config.build.compose.is_some() {
            format!("kotlin-{version}-compose")
        } else {
            format!("kotlin-{version}")
        })
        .join("scopes-v3"))
}
pub async fn prepare_tools(root: &Path, offline: bool) -> Result<Vec<PathBuf>> {
    let config = Config::load(root)?;
    let version = &config.build.kotlin_version;
    let dir = toolchain_directory(&config)?;
    std::fs::create_dir_all(&dir)?;
    let cfg = Config {
        schema: 1,
        constraints: vec![],
        platforms: vec![],
        android_variants: false,
        name: format!("Wex Kotlin compiler {version}"),
        repositories: vec!["https://repo.maven.apache.org/maven2".into()],
        android: native::Android {
            compile_sdk: 36,
            min_sdk: 26,
            target_sdk: 36,
            application_id: "dev.wex.toolchain".into(),
        },
        build: native::BuildSettings::default(),
        modules: vec![native::Module {
            name: "compiler".into(),
            sources: vec![],
            dependencies: {
                let mut deps = vec![format!(
                    "org.jetbrains.kotlin:kotlin-compiler-embeddable:{version}"
                )];
                if config.build.compose.is_some() {
                    deps.push(format!(
                        "org.jetbrains.kotlin:kotlin-compose-compiler-plugin-embeddable:{version}"
                    ));
                }
                deps
            },
            ..Default::default()
        }],
    };
    let path = dir.join("wex.json");
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    if std::fs::read(&path).ok().as_ref() != Some(&bytes) {
        write_atomic(&path, &bytes)?;
    }
    let cache = native::default_cache();
    let lock = tokio::task::spawn_blocking(move || native::resolve_project(&dir, &cache, offline))
        .await??;
    Ok(lock
        .artifacts
        .iter()
        .filter(|a| a.packaging == "jar")
        .map(|a| native::default_cache().join(&a.file))
        .collect())
}
/// Copy only immediate R/BuildConfig class files: a parent namespace must not
/// accidentally copy a child's unrelated namespace through recursive directories.
#[allow(clippy::too_many_arguments)]
fn compile_key(
    root: &Path,
    t: &Tools,
    config: &Config,
    variant: &str,
    module: &str,
    source: &[PathBuf],
    cp: &[PathBuf],
    system_image: &Path,
) -> Result<String> {
    use crate::compile_cache::Input;
    let mut inputs = vec![];
    for p in [
        t.jdk.join("bin"),
        t.jdk.join("release"),
        t.jdk.join("lib/modules"),
        t.jdk.join("lib/jrt-fs.jar"),
        t.jdk.join("lib/server/libjvm.so"),
        t.jdk.join("lib/libjava.so"),
        t.jdk.join("lib/libjli.so"),
        t.jdk.join("lib/libzip.so"),
        t.jdk.join("lib/libjimage.so"),
        t.jdk.join("lib/libnet.so"),
        t.jdk.join("conf/security/java.security"),
    ] {
        inputs.push(Input {
            label: format!("jdk:{}", p.display()),
            path: p,
            semantic_jar: false,
        });
    }
    for (i, p) in t.compiler.iter().enumerate() {
        inputs.push(Input {
            label: format!("compiler:{i}:{}", p.display()),
            path: p.clone(),
            semantic_jar: false,
        });
    }
    for p in source {
        inputs.push(Input {
            label: format!("source:{}", p.strip_prefix(root)?.display()),
            path: p.clone(),
            semantic_jar: false,
        });
    }
    for (i, p) in cp.iter().enumerate() {
        inputs.push(Input {
            label: format!("classpath:{i}"),
            path: p.clone(),
            semantic_jar: p.extension().is_some_and(|x| x == "jar"),
        });
    }
    let mut seed = serde_json::to_vec(
        &serde_json::json!({"root":root,"variant":variant,"module":module,"jvm_target":config.build.jvm_target,"kotlin":config.build.kotlin_version,"compose":config.build.compose,"flags":"K2 -no-stdlib -no-reflect -no-jdk;Javac -g -proc:none UTF8;scoped owned symbols/resource-files-v8"}),
    )?;
    inputs.push(Input {
        label: "effective-android-java-image".into(),
        path: system_image.into(),
        semantic_jar: false,
    });
    compiler_env_seed(t, &mut seed);
    crate::compile_cache::fingerprint(&seed, &inputs)
}
/// Resolve exactly the PATH search directory used by the SDK child. Relative and
/// empty entries mean the child's project cwd, never the IDE/CLI caller's cwd.
fn d8_java(root: &Path, path: &std::ffi::OsStr) -> Result<PathBuf> {
    ensure!(root.is_absolute(), "D8 working directory must be absolute");
    std::env::split_paths(path)
        .map(|p| root.join(p).join("java"))
        .find(|p| {
            if !p.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
        .context("D8 java missing")?
        .canonicalize()
        .context("D8 java canonical path")
}
fn dex_key(
    root: &Path,
    t: &Tools,
    variant: &str,
    min_sdk: u32,
    jars: &[PathBuf],
    android: &Path,
    core: &Path,
) -> Result<String> {
    use crate::compile_cache::Input;
    ensure!(
        !std::env::vars_os().any(|(name, _)| {
            let name = name.to_string_lossy();
            (name == "BASH_ENV" || name == "ENV" || name.starts_with("BASH_FUNC_"))
                && !t.secret_env.iter().any(|s| s == name.as_ref())
        }),
        "dynamic shell launcher environment; running uncached D8"
    );
    let mut inputs = vec![];
    for name in ["d8", "d8.real"] {
        let p = t.bt.join(name);
        if name == "d8" || p.exists() {
            inputs.push(Input {
                label: name.into(),
                path: p.canonicalize()?,
                semantic_jar: false,
            });
        }
    }
    // SDK d8 resolves its own d8.jar; reject unsupported layouts rather than reuse
    // a key which omits the compiler. A fingerprint failure still runs real D8.
    for name in ["lib", "lib64"] {
        let p = t.bt.join(name);
        if p.exists() {
            inputs.push(Input {
                label: format!("d8:{name}"),
                path: p.canonicalize()?,
                semantic_jar: false,
            });
        }
    }
    ensure!(
        t.bt.join("lib/d8.jar").is_file() && !t.bt.join("d8.jar").exists(),
        "unsupported D8 JAR layout"
    );
    for (i, p) in [android, core]
        .into_iter()
        .chain(jars.iter().map(PathBuf::as_path))
        .enumerate()
    {
        inputs.push(Input {
            label: format!("ordered-lib-program:{i}"),
            path: p.into(),
            semantic_jar: true,
        });
    }
    // SDK script uses PATH's java, not JAVA_HOME. Bind both the configured JDK
    // and the actual resolved launcher/runtime, including private wrapper backing.
    ensure!(
        !t.secret_env.iter().any(|s| s == "PATH"),
        "DEX cache requires non-credential PATH"
    );
    let java = d8_java(root, &std::env::var_os("PATH").context("D8 PATH missing")?)?;
    let runtime = java
        .parent()
        .and_then(Path::parent)
        .context("D8 Java runtime")?;
    for (scope, jdk) in [("configured", t.jdk.as_path()), ("effective", runtime)] {
        for p in sdk_jdk_inputs(jdk)? {
            inputs.push(Input {
                label: format!("{scope}:{}", p.display()),
                path: p,
                semantic_jar: false,
            });
        }
    }
    let mut seed = serde_json::to_vec(
        &serde_json::json!({"abi":crate::dex_cache::ABI,"launcher_cwd":root,"java_lookup":"project-cwd-v2","variant":variant,"min_api":min_sdk,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"flags":"D8 --debug/--release --min-api --lib android --lib core; ordered program JARs; directory output"}),
    )?;
    compiler_env_seed(t, &mut seed);
    crate::compile_cache::fingerprint(&seed, &inputs)
}
fn sdk_jdk_inputs(jdk: &Path) -> Result<Vec<PathBuf>> {
    let mut inputs = files(&jdk.join("bin"), &[])?;
    for p in [
        "release",
        "lib/modules",
        "lib/jrt-fs.jar",
        "lib/server/libjvm.so",
        "lib/libjava.so",
        "lib/libjli.so",
        "lib/libzip.so",
        "lib/libjimage.so",
        "lib/libnet.so",
        "conf/security/java.security",
    ] {
        inputs.push(jdk.join(p));
    }
    Ok(inputs)
}
fn sdk_image_key(t: &Tools, core: &Path) -> Result<String> {
    use crate::compile_cache::Input;
    let mut inputs = vec![Input {
        label: format!("core:{}", core.display()),
        path: core.into(),
        semantic_jar: false,
    }];
    for p in sdk_jdk_inputs(&t.jdk)? {
        inputs.push(Input {
            label: format!("jdk:{}", p.display()),
            path: p,
            semantic_jar: false,
        });
    }
    let mut seed = serde_json::to_vec(
        &serde_json::json!({"abi":crate::sdk_cache::ABI,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"descriptor":"java.base exports all SDK core packages; javac --system none --patch-module", "jmod":"create --target-platform --module-version","jlink":"--add-modules java.base --disable-plugin system-modules; copy jrt-fs.jar"}),
    )?;
    compiler_env_seed(t, &mut seed);
    crate::compile_cache::fingerprint(&seed, &inputs)
}
pub(crate) fn analysis_sdk_context(config: &Config) -> Result<String> {
    let t = tools(config, false)?;
    let mut seed = format!(
        "{}:{}:{}:tool-root-bindings-v2",
        crate::sdk_cache::ABI,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
    .into_bytes();
    compiler_env_seed_for_analysis(t.secret_env.as_slice(), &mut seed);
    Ok(blake3::hash(&seed).to_hex().to_string())
}
fn compiler_env_seed_for_analysis(secret_env: &[String], seed: &mut Vec<u8>) {
    // Cargo prepends target/deps to PATH for test binaries. PATH is not an
    // analysis tool input (the canonical JAVA_HOME/SDK roots are), so including
    // it would make a model created by `wex analyze` miss when K2 is started
    // from `cargo test`, despite identical project and tool bytes.
    for name in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "JDK_JAVAC_OPTIONS",
        "CLASSPATH",
        "JAVA_OPTS",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "SOURCE_DATE_EPOCH",
    ] {
        seed.extend_from_slice(name.as_bytes());
        seed.push(0);
        if !secret_env.iter().any(|x| x == name)
            && let Some(value) = std::env::var_os(name)
        {
            seed.extend_from_slice(value.as_encoded_bytes());
        }
        seed.push(0);
    }
}
fn compiler_env_seed(t: &Tools, seed: &mut Vec<u8>) {
    for name in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "JDK_JAVAC_OPTIONS",
        "CLASSPATH",
        "JAVA_OPTS",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "SOURCE_DATE_EPOCH",
        "PATH",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
    ] {
        seed.extend_from_slice(name.as_bytes());
        seed.push(0);
        if !t.secret_env.iter().any(|x| x == name)
            && let Some(value) = std::env::var_os(name)
        {
            seed.extend_from_slice(value.as_encoded_bytes());
        }
        seed.push(0);
    }
}
fn check_sdk_image(t: &Tools, core: &Path, key: &Option<String>) -> Result<()> {
    if let Some(key) = key {
        ensure!(
            sdk_image_key(t, core)? == *key,
            "SDK image inputs changed; result not published"
        );
    }
    Ok(())
}
pub(crate) fn resource_program(t: &Tools) -> PathBuf {
    t.bt.join("aapt2")
}
pub(crate) fn resource_tool_key(t: &Tools) -> Result<String> {
    use crate::compile_cache::Input;
    let mut inputs = vec![Input {
        label: "aapt2".into(),
        path: t.bt.join("aapt2").canonicalize()?,
        semantic_jar: false,
    }];
    if t.bt.join("aapt2.real").is_file() {
        inputs.push(Input {
            label: "aapt2 wrapper backing".into(),
            path: t.bt.join("aapt2.real").canonicalize()?,
            semantic_jar: false,
        });
    }
    if t.bt.join("lib64").exists() {
        inputs.push(Input {
            label: "aapt2 native libraries".into(),
            path: t.bt.join("lib64").canonicalize()?,
            semantic_jar: false,
        });
    }
    let mut seed = format!(
        "{}:default compile/PNG crunch/stable per-file input/batch128/ordered ZIP:{}:{}",
        crate::resource_cache::ABI,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
    .into_bytes();
    for name in [
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "SOURCE_DATE_EPOCH",
        "PATH",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
    ] {
        seed.extend_from_slice(name.as_bytes());
        seed.push(0);
        if !t.secret_env.iter().any(|s| s == name)
            && let Some(v) = std::env::var_os(name)
        {
            seed.extend_from_slice(v.as_encoded_bytes());
        }
        seed.push(0);
    }
    crate::compile_cache::fingerprint(&seed, &inputs)
}
fn check_resources(t: &Tools, checks: &[(PathBuf, String)], tool: &Option<String>) -> Result<()> {
    for (path, hash) in checks {
        ensure!(
            crate::resource_cache::body(path)? == *hash,
            "Resource inputs changed; result not published"
        );
    }
    if let Some(tool) = tool {
        ensure!(
            resource_tool_key(t)? == *tool,
            "AAPT2 inputs changed; result not published"
        );
    }
    Ok(())
}
fn copy_symbol_classes(from: &Path, to: &Path, package: &str) -> Result<()> {
    if package.is_empty() {
        return Ok(());
    }
    ensure!(namespace(package), "invalid symbol namespace");
    let relative = PathBuf::from(package.replace('.', "/"));
    let source = from.join(&relative);
    if !source.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(source)? {
        let p = entry?.path();
        let n = p.file_name().unwrap().to_string_lossy();
        if p.is_file()
            && (n == "R.class"
                || n.starts_with("R$") && n.ends_with(".class")
                || n == "BuildConfig.class")
        {
            let dir = to.join(&relative);
            std::fs::create_dir_all(&dir)?;
            std::fs::copy(&p, dir.join(p.file_name().unwrap()))?;
        }
    }
    Ok(())
}
pub(crate) async fn command(
    t: &Tools,
    root: &Path,
    name: &str,
    program: &Path,
    args: Vec<OsString>,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Step> {
    event(RunEvent::Phase(name.into()));
    let start = Instant::now();
    let mut c = tokio::process::Command::new(program);
    c.args(&args)
        .current_dir(root)
        .env("JAVA_HOME", &t.jdk)
        .env("WEX_DEBUG_STOREPASS", "android")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for name in &t.secret_env {
        c.env_remove(name);
    }
    c.env_remove("WEX_PRIVATE_STOREPASS")
        .env_remove("WEX_PRIVATE_KEYPASS");
    let mut child = c.spawn().with_context(|| format!("starting {name}"))?;
    async fn pump<R: tokio::io::AsyncRead + Unpin>(
        r: R,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<String> {
        let mut r = BufReader::new(r);
        let mut all = vec![];
        loop {
            let mut line = vec![];
            let n = (&mut r).take(8192).read_until(b'\n', &mut line).await?;
            if n == 0 {
                break;
            }
            ensure!(
                all.len() + n <= 4 * 1024 * 1024,
                "tool output exceeds 4 MiB"
            );
            event(RunEvent::BuildLine(
                String::from_utf8_lossy(&line).trim_end().into(),
            ));
            all.extend(line);
        }
        Ok(String::from_utf8_lossy(&all).into())
    }
    let (stdout, stderr, status) = tokio::time::timeout(Duration::from_secs(300), async {
        tokio::join!(
            pump(child.stdout.take().unwrap(), event),
            pump(child.stderr.take().unwrap(), event),
            child.wait()
        )
    })
    .await
    .context("native build tool timed out")?;
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status?;
    ensure!(
        status.success(),
        "{name} failed ({status}): {stdout}{stderr}"
    );
    Ok(Step {
        name: name.into(),
        program: program.into(),
        args: args.iter().map(|s| s.to_string_lossy().into()).collect(),
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
        elapsed_ms: start.elapsed().as_millis(),
    })
}
fn os(s: impl AsRef<std::ffi::OsStr>) -> OsString {
    s.as_ref().to_owned()
}
fn join(paths: &[PathBuf]) -> Result<OsString> {
    Ok(std::env::join_paths(paths)?)
}
fn within(root: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|p| matches!(p, std::path::Component::Normal(_))),
        "path must be project-relative"
    );
    let p = root.join(path).canonicalize()?;
    ensure!(p.starts_with(root), "project path escapes root");
    Ok(p)
}
pub(crate) fn files(dir: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for entry in walkdir::WalkDir::new(dir).max_depth(30) {
        let entry = entry?;
        ensure!(
            !entry.file_type().is_symlink(),
            "symlink in native input tree"
        );
        if entry.file_type().is_file()
            && (extensions.is_empty()
                || entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| extensions.contains(&s)))
        {
            out.push(entry.path().into());
        }
        ensure!(out.len() <= 20000, "native input file limit exceeded");
    }
    out.sort();
    Ok(out)
}
fn extract_aar(path: &Path, out: &Path) -> Result<()> {
    let mut z = ZipArchive::new(std::fs::File::open(path)?)?;
    ensure!(z.len() <= 20000, "AAR entry limit exceeded");
    let mut total = 0;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let rel = f.enclosed_name().context("invalid AAR path")?;
        ensure!(
            f.unix_mode().is_none_or(|m| m & 0o170000 != 0o120000),
            "AAR symlink rejected"
        );
        total += f.size();
        ensure!(total <= 512 * 1024 * 1024, "AAR expansion exceeds 512 MiB");
        let p = out.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(p)?;
        } else {
            std::fs::create_dir_all(p.parent().unwrap())?;
            let n = std::io::copy(&mut f, &mut std::fs::File::create(p)?)?;
            ensure!(n <= 256 * 1024 * 1024, "AAR file too large");
        }
    }
    Ok(())
}
fn order(config: &Config, name: &str, out: &mut Vec<String>) -> Result<()> {
    if out.iter().any(|n| n == name) {
        return Ok(());
    }
    let m = config
        .modules
        .iter()
        .find(|m| m.name == name)
        .context("native module missing")?;
    for p in &m.projects {
        order(config, p, out)?;
    }
    out.push(name.into());
    Ok(())
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().context("output parent")?)?;
    f.write_all(bytes)?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub(crate) fn input_fingerprint(root: &Path, c: &Config) -> Result<String> {
    let mut paths = vec![
        root.join("wex.json"),
        root.join("wex.lock.json"),
        within(root, &c.build.manifest)?,
    ];
    let projects = crate::module_scope::project_order(c, &c.build.application_module)?;
    for m in c.modules.iter().filter(|m| projects.contains(&m.name)) {
        for p in &m.sources {
            paths.extend(files(&within(root, p)?, &["kt", "java"])?);
        }
        for p in m.resources.iter().chain(&m.assets) {
            paths.extend(files(&within(root, p)?, &[])?);
        }
        if let Some(p) = &m.manifest {
            paths.push(within(root, p)?);
        }
    }
    paths.sort();
    paths.dedup();
    let mut h = blake3::Hasher::new();
    let mut total = 0;
    for p in paths {
        let bytes = std::fs::read(&p)?;
        total += bytes.len();
        ensure!(total <= 512 * 1024 * 1024, "native inputs exceed 512 MiB");
        h.update(p.strip_prefix(root)?.to_string_lossy().as_bytes());
        h.update(&[0]);
        h.update(&bytes);
        h.update(&[0]);
    }
    Ok(h.finalize().to_hex().to_string())
}
/// POM resolver is locked for builds; dependency changes require explicit resolve.
/// Signed APK path used by Build/Run; preview never publishes into current.json.
pub async fn build(
    root: &Path,
    module: &str,
    variant: &str,
    offline: bool,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<BuildReport> {
    match build_inner(root, module, variant, offline, Product::Apk, None, event).await? {
        BuildOutcome::Apk(report) => Ok(*report),
        BuildOutcome::Preview(_) | BuildOutcome::Analysis(_) => unreachable!(),
    }
}
/// Own real Kotlin/Java/AAPT compilation; no DEX/sign/install work or APK pointer.
pub struct PreviewCompilation {
    _work: tempfile::TempDir,
    pub classpath: Vec<PathBuf>,
    pub resources: PathBuf,
    pub resource_blake3: String,
    pub compose: Option<ComposeBuild>,
    pub steps: Vec<Step>,
    pub sdk_cache: Vec<crate::compile_cache::Record>,
    pub resource_cache: Vec<crate::resource_cache::Record>,
    pub elapsed_ms: u128,
}
enum BuildOutcome {
    Apk(Box<BuildReport>),
    Preview(Box<PreviewCompilation>),
    Analysis(Box<AnalysisCompilation>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Product {
    Apk,
    Preview,
    Analysis,
}
pub(crate) struct AnalysisCompilation {
    pub config_blake3: String,
    pub lock_blake3: String,
    pub work: tempfile::TempDir,
    pub graph: ProjectGraph,
    pub sdk_cache: Vec<crate::compile_cache::Record>,
    pub resource_cache: Vec<crate::resource_cache::Record>,
    pub contexts: Vec<AnalysisContext>,
    pub compose: Option<ComposeBuild>,
    pub steps: Vec<Step>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisContext {
    pub root_module: String,
    /// Only these modules use generated roots from this context. Shared project
    /// dependencies already assigned by an earlier context are not duplicated.
    pub modules: Vec<String>,
    pub closure: Vec<String>,
    pub directory: PathBuf,
}
fn relocate_generated(module: &mut Module, old: &Path, new: &Path) {
    for path in module
        .classpath
        .iter_mut()
        .chain(&mut module.generated_source_roots)
        .chain(&mut module.generated_resource_roots)
        .chain(&mut module.classes_roots)
    {
        if let Ok(relative) = path.strip_prefix(old) {
            *path = new.join(relative);
        }
    }
}
pub(crate) fn analysis_sdk_inputs(config: &Config) -> Result<Vec<PathBuf>> {
    let t = tools(config, false)?;
    let mut inputs = vec![
        t.sdk.join(format!(
            "platforms/android-{}/android.jar",
            config.android.compile_sdk
        )),
        t.sdk.join(format!(
            "platforms/android-{}/core-for-system-modules.jar",
            config.android.compile_sdk
        )),
        t.bt.join("aapt2"),
        t.jdk.join("release"),
        t.jdk.join("bin/javac"),
        t.jdk.join("bin/jar"),
        t.jdk.join("bin/jmod"),
        t.jdk.join("bin/jlink"),
        t.jdk.join("lib/jrt-fs.jar"),
    ];
    if t.bt.join("lib64").is_dir() {
        inputs.extend(files(&t.bt.join("lib64").canonicalize()?, &[])?);
    }
    if t.bt.join("aapt2.real").is_file() {
        inputs.push(t.bt.join("aapt2.real"));
    }
    inputs.extend(sdk_jdk_inputs(&t.jdk)?);
    inputs.sort();
    inputs.dedup();
    Ok(inputs)
}
pub(crate) async fn compile_analysis(
    root: &Path,
    variant: &str,
    offline: bool,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<AnalysisCompilation> {
    let config_bytes = std::fs::read(root.join("wex.json"))?;
    let cfg = Config::from_bytes(&config_bytes)?;
    let expected_config = blake3::hash(&config_bytes).to_hex().to_string();
    let expected_lock = blake3::hash(&std::fs::read(root.join("wex.lock.json"))?)
        .to_hex()
        .to_string();
    let roots = crate::module_scope::analysis_roots(&cfg)?;
    let mut combined = None::<AnalysisCompilation>;
    let mut covered = BTreeSet::new();
    for name in roots {
        event(RunEvent::Phase(format!("Analysis context · {name}")));
        let BuildOutcome::Analysis(child) = build_inner(
            root,
            &name,
            variant,
            offline,
            Product::Analysis,
            None,
            event,
        )
        .await?
        else {
            unreachable!()
        };
        let mut child = *child;
        ensure!(
            child.config_blake3 == expected_config && child.lock_blake3 == expected_lock,
            "analysis context config/lock snapshot changed"
        );
        let closure = child
            .graph
            .modules
            .iter()
            .map(|m| m.name.trim_start_matches(':').to_owned())
            .collect::<Vec<_>>();
        let assigned = closure
            .iter()
            .filter(|m| !covered.contains(*m))
            .cloned()
            .collect::<Vec<_>>();
        ensure!(!assigned.is_empty(), "empty analysis context coverage");
        if let Some(main) = &mut combined {
            let old = child.work.path().to_owned();
            let directory = main.work.path().join("contexts").join(&name);
            std::fs::create_dir_all(directory.parent().unwrap())?;
            std::fs::rename(&old, &directory)?;
            for mut module in child.graph.modules {
                if covered.contains(module.name.trim_start_matches(':')) {
                    continue;
                }
                relocate_generated(&mut module, &old, &directory);
                main.graph.modules.push(module);
            }
            main.contexts.push(AnalysisContext {
                root_module: name,
                modules: assigned.clone(),
                closure,
                directory,
            });
            main.steps.extend(child.steps);
            main.sdk_cache.extend(child.sdk_cache);
            main.resource_cache.extend(child.resource_cache);
            match (&mut main.compose, child.compose) {
                (Some(info), Some(other)) => {
                    ensure!(
                        info.plugin_jar == other.plugin_jar
                            && info.plugin_blake3 == other.plugin_blake3
                            && info.compiler_version == other.compiler_version
                            && info.source_information == other.source_information
                            && info.strong_skipping == other.strong_skipping
                            && info.reports == other.reports,
                        "Compose toolchain changed between analysis contexts"
                    );
                    for module in other.modules {
                        if !info.modules.contains(&module) {
                            info.modules.push(module);
                        }
                    }
                }
                (None, None) => (),
                _ => anyhow::bail!("Compose configuration changed between analysis contexts"),
            }
        } else {
            child.contexts.push(AnalysisContext {
                root_module: name,
                modules: assigned.clone(),
                closure,
                directory: child.work.path().to_owned(),
            });
            combined = Some(child);
        }
        covered.extend(assigned);
    }
    ensure!(
        covered.len() == cfg.modules.len(),
        "incomplete workspace analysis coverage"
    );
    combined.context("no analysis contexts")
}
pub async fn compile_preview(
    root: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<PreviewCompilation> {
    compile_preview_cached(root, root, event).await
}
pub(crate) async fn compile_preview_cached(
    root: &Path,
    sdk_root: &Path,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<PreviewCompilation> {
    let cfg = Config::load(root)?;
    match build_inner(
        root,
        &cfg.build.application_module,
        "debug",
        true,
        Product::Preview,
        Some(sdk_root),
        event,
    )
    .await?
    {
        BuildOutcome::Preview(compilation) => Ok(*compilation),
        BuildOutcome::Apk(_) | BuildOutcome::Analysis(_) => unreachable!(),
    }
}
pub(crate) fn ensure_configured_key_not_packaged(root: &Path, config: &Config) -> Result<()> {
    if let Some(signing) = &config.build.release_signing {
        ensure_key_not_packaged(root, config, &root.join(&signing.keystore))?;
    }
    Ok(())
}
fn ensure_key_not_packaged(root: &Path, config: &Config, key: &Path) -> Result<()> {
    let key = key.canonicalize().unwrap_or_else(|_| key.to_owned());
    for p in config
        .modules
        .iter()
        .flat_map(|m| m.assets.iter().chain(&m.resources))
    {
        let dir = root.join(p);
        ensure!(
            !key.starts_with(&dir),
            "release keystore would be packaged as assets/resources; move key outside those roots"
        );
        if dir.exists() {
            ensure!(
                !key.starts_with(dir.canonicalize()?),
                "release keystore would be packaged as assets/resources; move key outside those roots"
            );
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn build_inner(
    root: &Path,
    module: &str,
    variant: &str,
    offline: bool,
    product: Product,
    sdk_root: Option<&Path>,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<BuildOutcome> {
    let start = Instant::now();
    let preview = product == Product::Preview;
    let root = root.canonicalize()?;
    let cache_root = sdk_root.unwrap_or(&root).canonicalize()?;
    let config_bytes = std::fs::read(root.join("wex.json"))?;
    let config_blake3 = blake3::hash(&config_bytes).to_hex().to_string();
    let config = Config::from_bytes(&config_bytes)?;
    ensure!(
        matches!(variant, "debug" | "release"),
        "native variant must be debug or release"
    );
    let debuggable = variant == "debug";
    // Metadata-only check also protects Debug/analysis/Preview input snapshots.
    // Missing external release keys/credentials remain irrelevant to these modes.
    ensure_configured_key_not_packaged(&root, &config)?;

    let release = if !debuggable && product == Product::Apk {
        Some(crate::signing::Prepared::load(
            &root,
            config
                .build
                .release_signing
                .as_ref()
                .context("release requires build.release_signing; no debug-key fallback")?,
        )?)
    } else {
        None
    };
    let mut t = tools(&config, product == Product::Apk)?;
    let signing_preflight = if let Some(key) = &release {
        Some(key.preflight(&t.jdk, event).await?)
    } else {
        None
    };
    let module = module.trim_start_matches(':');
    ensure!(
        product == Product::Analysis || module == config.build.application_module,
        "select configured application module"
    );
    ensure!(
        namespace(&config.android.application_id),
        "invalid application ID"
    );
    let app_ns = if config.build.namespace.is_empty() {
        config.android.application_id.clone()
    } else {
        config.build.namespace.clone()
    };
    ensure!(namespace(&app_ns), "invalid app namespace");
    let selected = config
        .modules
        .iter()
        .find(|m| m.name == module)
        .context("analysis module missing")?;
    let ns = if module == config.build.application_module {
        app_ns.clone()
    } else if selected.namespace.is_empty() {
        // Private resource-link namespace only. Pure Kotlin modules keep their
        // real empty namespace and never receive this context's generated R.
        format!("dev.wex.analysis.root_{}", module.replace('-', "_"))
    } else {
        selected.namespace.clone()
    };
    ensure!(namespace(&ns), "invalid analysis context namespace");
    let parent = if product == Product::Analysis {
        root.join(".wex/analysis").join(variant).join("generations")
    } else {
        root.join(".wex/native").join(variant)
    };
    std::fs::create_dir_all(&parent)?;
    // OS create_new lock: shared by CLI and GUI, released even on cancellation.
    let _guard = BuildGuard::acquire(&parent)?;
    event(RunEvent::Phase("Checking pinned Kotlin toolchain".into()));
    t.compiler = prepare_tools(&root, offline).await?;
    let mut compose = if let Some(settings) = &config.build.compose {
        let file = compose_plugin_filename(&config.build.kotlin_version);
        let path = t
            .compiler
            .iter()
            .find(|p| p.file_name().is_some_and(|name| name == file.as_str()))
            .context("matching Compose compiler plugin missing from locked toolchain")?
            .clone();
        Some(ComposeBuild {
            compiler_version: config.build.kotlin_version.clone(),
            plugin_blake3: blake3::hash(&std::fs::read(&path)?).to_hex().to_string(),
            plugin_jar: path,
            source_information: settings.source_information,
            strong_skipping: settings.strong_skipping,
            reports: settings.reports,
            modules: vec![],
        })
    } else {
        None
    };
    let r = root.clone();
    let cache = native::default_cache();
    let lock = tokio::task::spawn_blocking(move || native::resolve_for_build(&r, &cache)).await??;
    ensure!(
        lock.config_blake3 == config_blake3
            && blake3::hash(&std::fs::read(root.join("wex.json"))?)
                .to_hex()
                .as_str()
                == config_blake3,
        "config changed while preparing build"
    );
    let input_blake3 = if product == Product::Analysis {
        String::new()
    } else {
        input_fingerprint(&root, &config)?
    };
    let lock_bytes = std::fs::read(root.join("wex.lock.json"))?;
    ensure!(
        serde_json::from_slice::<serde_json::Value>(&lock_bytes)? == serde_json::to_value(&lock)?,
        "lock changed while capturing build inputs"
    );
    let lock_blake3 = blake3::hash(&lock_bytes).to_hex().to_string();
    let scopes = crate::module_scope::plan(&config, &lock, module)?;
    let temp = tempfile::Builder::new()
        .prefix("build-")
        .tempdir_in(&parent)?;
    let work = temp.path().to_path_buf();
    let mut steps = signing_preflight.into_iter().collect::<Vec<_>>();
    let mut jars = vec![];
    let mut artifact_jars = BTreeMap::<usize, Vec<PathBuf>>::new();
    let mut artifact_packages = BTreeMap::<usize, String>::new();
    let mut manifests = vec![];
    let mut resources = vec![];
    let mut extra_packages = BTreeSet::new();
    let mut aar_contracts = BTreeMap::<String, crate::resource_symbols::Table>::new();
    let mut packaged = BTreeMap::new();
    event(RunEvent::Phase(
        "Preparing locked JAR/AAR dependencies".into(),
    ));
    for (i, a) in lock.artifacts.iter().enumerate().rev() {
        if !scopes.runtime.artifacts.contains(&i) {
            continue;
        }
        let before = jars.len();
        if a.packaging == "jar" {
            jars.push(native::default_cache().join(&a.file));
        } else if a.packaging == "aar" {
            let dest = work.join("aar").join(i.to_string());
            extract_aar(&native::default_cache().join(&a.file), &dest)?;
            if dest.join("classes.jar").is_file() {
                jars.push(dest.join("classes.jar"));
            }
            if dest.join("libs").is_dir() {
                jars.extend(files(&dest.join("libs"), &["jar"])?);
            }
            let text = std::fs::read_to_string(dest.join("AndroidManifest.xml"))?;
            let package = manifest::library_package(&text)?;
            ensure!(namespace(&package), "invalid AAR package");
            artifact_packages.insert(i, package.clone());
            extra_packages.insert(package.clone());
            if dest.join("R.txt").is_file() {
                aar_contracts
                    .entry(package.clone())
                    .or_default()
                    .merge_contract(crate::resource_symbols::Table::parse(
                        &std::fs::read_to_string(dest.join("R.txt"))?,
                    )?)?;
            }
            manifests.push(text);
            if dest.join("res").is_dir() && !files(&dest.join("res"), &[])?.is_empty() {
                resources.push((
                    package,
                    dest.join("res"),
                    format!("{}!/res", native::default_cache().join(&a.file).display()),
                ));
            }
            for (folder, prefix) in if product == Product::Analysis {
                vec![]
            } else {
                vec![("assets", "assets"), ("jni", "lib")]
            } {
                if dest.join(folder).is_dir() {
                    for f in files(&dest.join(folder), &[])? {
                        let name = format!(
                            "{prefix}/{}",
                            f.strip_prefix(dest.join(folder))?.to_string_lossy()
                        );
                        add_payload(&mut packaged, name, std::fs::read(f)?)?;
                    }
                }
            }
        }
        artifact_jars.insert(i, jars[before..].to_vec());
    }
    let mut modules = vec![];
    order(&config, module, &mut modules)?;
    let mut project_packages = BTreeSet::new();
    for name in &modules {
        let m = config.modules.iter().find(|m| &m.name == name).unwrap();
        let package = if name == &config.build.application_module {
            &app_ns
        } else {
            &m.namespace
        };
        if !package.is_empty() {
            ensure!(namespace(package), "invalid module namespace");
            ensure!(
                project_packages.insert(package.clone()),
                "project modules share an R namespace: {package}"
            );
            ensure!(
                !artifact_packages.values().any(|p| p == package),
                "project/AAR R namespace collision: {package}"
            );
            extra_packages.insert(package.clone());
        }
        if name != module
            && let Some(p) = &m.manifest
        {
            manifests.push(std::fs::read_to_string(within(&root, p)?)?);
        }
        for p in &m.resources {
            ensure!(
                !package.is_empty(),
                "resource-owning module requires a namespace: {name}"
            );
            resources.push((
                package.clone(),
                within(&root, p)?,
                cache_root.join(p).to_string_lossy().into_owned(),
            ));
        }
        for p in m.assets.iter().filter(|_| product != Product::Analysis) {
            let dir = within(&root, p)?;
            for f in files(&dir, &[])? {
                add_payload(
                    &mut packaged,
                    format!("assets/{}", f.strip_prefix(&dir)?.to_string_lossy()),
                    std::fs::read(f)?,
                )?;
            }
        }
    }
    let app_manifest = if module == config.build.application_module {
        std::fs::read_to_string(within(&root, &config.build.manifest)?)?
    } else if let Some(path) = &selected.manifest {
        std::fs::read_to_string(within(&root, path)?)?
    } else {
        format!("<manifest package=\"{ns}\"/>")
    };
    let merge_manifest =
        if product == Product::Analysis && module != config.build.application_module {
            manifest::merged_library_context
        } else {
            manifest::merged
        };
    let text = merge_manifest(
        &app_manifest,
        &manifests,
        &manifest::Settings {
            namespace: &ns,
            id: &config.android.application_id,
            // A detached library analysis context is not an application runtime
            // compatibility check. Its SDK ceiling can resolve its declared AARs
            // without imposing the unrelated app's minSdk on that library.
            min_sdk: if product == Product::Analysis && module != config.build.application_module {
                config.android.compile_sdk
            } else {
                config.android.min_sdk
            },
            target_sdk: if product == Product::Analysis && module != config.build.application_module
            {
                config.android.compile_sdk
            } else {
                config.android.target_sdk
            },
            debug: debuggable,
            version_code: config.build.version_code,
            version_name: &config.build.version_name,
        },
    )?;
    let merged = work.join("AndroidManifest.xml");
    std::fs::write(&merged, text)?;
    let generated = work.join("generated");
    std::fs::create_dir_all(&generated)?;
    let android = t.sdk.join(format!(
        "platforms/android-{}/android.jar",
        config.android.compile_sdk
    ));
    let mut link = vec![
        os("link"),
        os("-o"),
        os(work.join("resources.apk")),
        os("-I"),
        os(&android),
        os("--manifest"),
        os(&merged),
        os("--java"),
        os(&generated),
        os("--custom-package"),
        os(&ns),
        os("--auto-add-overlay"),
        os("--output-text-symbols"),
        os(work.join("R.txt")),
    ];
    let mut resource_records = vec![];
    let mut resource_sources = crate::resource_cache::Sources::default();
    let mut resource_checks = vec![];
    let mut resource_tool = None;
    let mut owned_zips = BTreeMap::<String, Vec<PathBuf>>::new();
    for (i, (package, dir, origin)) in resources.iter().enumerate() {
        let zip = work.join(format!("res-{i}.zip"));
        let compiled = crate::resource_cache::compile(
            &t,
            &root,
            &cache_root,
            package,
            origin,
            dir,
            &zip,
            event,
        )
        .await?;
        resource_records.push(compiled.record);
        steps.extend(compiled.steps);
        resource_sources.extend(compiled.sources);
        resource_checks.extend(compiled.checks);
        if !compiled.tool_key.is_empty() {
            resource_tool = Some(compiled.tool_key);
        }
        owned_zips
            .entry(package.clone())
            .or_default()
            .push(zip.clone());
        link.extend([os("-R"), os(zip)]);
    }
    steps.push(
        crate::resource_cache::mapped(
            &t,
            &root,
            "AAPT2 link + generated R",
            &t.bt.join("aapt2"),
            link,
            &resource_sources,
            event,
        )
        .await?,
    );
    // Compile --dir --output-text-symbols overwrites the file for each input; it
    // is NOT a complete local inventory. A real owner-only static link captures
    // values, file resources, @+id exports and styleable groups in one SDK pass.
    // -I final resources resolves aliases/external attrs without importing fields.
    let final_symbols =
        crate::resource_symbols::Table::parse(&std::fs::read_to_string(work.join("R.txt"))?)?;
    let ownership = work.join("resource-ownership");
    std::fs::create_dir_all(&ownership)?;
    let owner_manifest = ownership.join("AndroidManifest.xml");
    // Resource references resolve in the final table's package (applicationId),
    // not the independent Java R namespace. They commonly differ in real apps.
    std::fs::write(
        &owner_manifest,
        format!("<manifest package=\"{}\"/>", config.android.application_id),
    )?;
    for package in &extra_packages {
        let mut owned = crate::resource_symbols::Table::default();
        if let Some(zips) = owned_zips.get(package) {
            let symbols = ownership.join(format!("{package}.txt"));
            let mut args = vec![
                os("link"),
                os("--static-lib"),
                os("--auto-add-overlay"),
                os("-I"),
                os(&android),
                os("-I"),
                os(work.join("resources.apk")),
                os("--manifest"),
                os(&owner_manifest),
                os("--custom-package"),
                os(package),
                os("-o"),
                os(ownership.join(format!("{package}.apk"))),
                os("--output-text-symbols"),
                os(&symbols),
            ];
            for zip in zips {
                args.extend([os("-R"), os(zip)]);
            }
            steps.push(
                crate::resource_cache::mapped(
                    &t,
                    &root,
                    &format!("AAPT2 owned resource symbols · {package}"),
                    &t.bt.join("aapt2"),
                    args,
                    &resource_sources,
                    event,
                )
                .await?,
            );
            owned = crate::resource_symbols::Table::parse(&std::fs::read_to_string(symbols)?)?;
        }
        let binary = aar_contracts.get(package);
        ensure!(
            !artifact_packages.values().any(|p| p == package)
                || !owned.has_styleables()
                || binary.is_some(),
            "AAR styleables require R.txt binary index ABI: {package}"
        );
        let linked = owned.link(&final_symbols, binary)?;
        let target = generated.join(package.replace('.', "/"));
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join("R.java"), linked.java(package)?)?;
    }
    for name in &modules {
        let m = config.modules.iter().find(|m| &m.name == name).unwrap();
        let package = if name == &config.build.application_module {
            app_ns.clone()
        } else {
            m.namespace.clone()
        };
        if package.is_empty() {
            continue;
        }
        let dir = generated.join(package.replace('.', "/"));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("BuildConfig.java"),
            format!(
                "package {package};\n\npublic final class BuildConfig {{\n    public static final boolean DEBUG = {debuggable};\n    public static final String APPLICATION_ID = {:?};\n    public static final int VERSION_CODE = {};\n    public static final String VERSION_NAME = {:?};\n}}\n",
                config.android.application_id, config.build.version_code, config.build.version_name
            ),
        )?;
    }
    let gen_classes = work.join("generated-classes");
    std::fs::create_dir_all(&gen_classes)?;
    // Construct an Android java.base image directly from SDK stubs (no AGP transform).
    let core = t.sdk.join(format!(
        "platforms/android-{}/core-for-system-modules.jar",
        config.android.compile_sdk
    ));
    ensure!(core.is_file(), "Android core module stubs missing");
    let system_image = work.join("android-java-image");
    let sdk_root = cache_root.clone();
    let mut sdk_record = crate::compile_cache::Record {
        module: "android-java-image".into(),
        key: String::new(),
        state: "disabled".into(),
        detail: "SDK cache unavailable".into(),
        files: 0,
    };
    let sdk_key = match sdk_image_key(&t, &core) {
        Ok(key) => {
            sdk_record.key = key.clone();
            Some(key)
        }
        Err(e) => {
            sdk_record.detail = format!("SDK fingerprint unavailable: {e:#}");
            None
        }
    };
    let mut sdk_guard = None;
    let sdk_cache =
        sdk_key.as_ref().and_then(|key| {
            match crate::compile_cache::Cache::sdk_image(&sdk_root, key)
                .and_then(|cache| Ok((cache, crate::sdk_cache::Guard::acquire(&sdk_root, key)?)))
            {
                Ok((cache, Some(guard))) => {
                    sdk_guard = Some(guard);
                    sdk_record.state = "miss".into();
                    sdk_record.detail = "No verified SDK image for these inputs".into();
                    Some(cache)
                }
                Ok((_, None)) => {
                    sdk_record.detail = "SDK cache busy; generating independent image".into();
                    None
                }
                Err(e) => {
                    sdk_record.detail = format!("SDK cache unavailable: {e:#}");
                    None
                }
            }
        });
    let mut sdk_reused = false;
    if let Some(cache) = &sdk_cache {
        std::fs::create_dir(&system_image)?;
        match cache.restore(&system_image) {
            Ok(Some(files)) => {
                sdk_reused = true;
                sdk_record.state = "hit".into();
                sdk_record.files = files;
                sdk_record.detail = "Verified Android Java image copied into this work tree".into();
            }
            Ok(None) => (),
            Err(e) => {
                sdk_record.detail = format!("Invalid SDK image; regenerating: {e:#}");
            }
        }
        if !sdk_reused {
            std::fs::remove_dir_all(&system_image)?;
        }
    }
    event(RunEvent::Phase(format!(
        "SDK_IMAGE_{} · {}",
        sdk_record.state.to_ascii_uppercase(),
        sdk_record.detail.chars().take(512).collect::<String>()
    )));
    if !sdk_reused {
        let mut archive = ZipArchive::new(std::fs::File::open(&core)?)?;
        let mut packages = BTreeSet::new();
        for i in 0..archive.len() {
            let f = archive.by_index(i)?;
            let name = f.name();
            if name.ends_with(".class")
                && let Some((package, _)) = name.rsplit_once('/')
            {
                packages.insert(package.replace('/', "."));
            }
        }
        let descriptor = work.join("module-info.java");
        let descriptor_classes = work.join("java-base-descriptor");
        std::fs::create_dir_all(&descriptor_classes)?;
        std::fs::write(
            &descriptor,
            format!(
                "module java.base {{ {} }}",
                packages
                    .iter()
                    .map(|p| format!("exports {p};"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        )?;
        steps.push(
            command(
                &t,
                &root,
                "Android java.base descriptor",
                &t.jdk.join("bin/javac"),
                vec![
                    os("--system"),
                    os("none"),
                    os("--patch-module"),
                    os(format!("java.base={}", core.display())),
                    os("-d"),
                    os(&descriptor_classes),
                    os(&descriptor),
                ],
                event,
            )
            .await?,
        );
        let module_jar = work.join("java-base.jar");
        std::fs::copy(&core, &module_jar)?;
        steps.push(
            command(
                &t,
                &root,
                "Android java.base module JAR",
                &t.jdk.join("bin/jar"),
                vec![
                    os("--update"),
                    os("--file"),
                    os(&module_jar),
                    os("-C"),
                    os(&descriptor_classes),
                    os("module-info.class"),
                ],
                event,
            )
            .await?,
        );
        let jmods = work.join("jmods");
        std::fs::create_dir_all(&jmods)?;
        steps.push(
            command(
                &t,
                &root,
                "Android java.base JMOD",
                &t.jdk.join("bin/jmod"),
                vec![
                    os("create"),
                    os("--target-platform"),
                    os(format!(
                        "{}-{}",
                        std::env::consts::OS,
                        if std::env::consts::ARCH == "x86_64" {
                            "amd64"
                        } else {
                            std::env::consts::ARCH
                        }
                    )),
                    os("--module-version"),
                    os(std::fs::read_to_string(t.jdk.join("release"))?
                        .lines()
                        .find_map(|l| l.strip_prefix("JAVA_VERSION="))
                        .context("JDK release version missing")?
                        .trim_matches('"')),
                    os("--class-path"),
                    os(&module_jar),
                    os(jmods.join("java.base.jmod")),
                ],
                event,
            )
            .await?,
        );
        steps.push(
            command(
                &t,
                &root,
                "Android Java system image",
                &t.jdk.join("bin/jlink"),
                vec![
                    os("--module-path"),
                    os(&jmods),
                    os("--add-modules"),
                    os("java.base"),
                    os("--output"),
                    os(&system_image),
                    os("--disable-plugin"),
                    os("system-modules"),
                ],
                event,
            )
            .await?,
        );
        std::fs::copy(
            t.jdk.join("lib/jrt-fs.jar"),
            system_image.join("lib/jrt-fs.jar"),
        )?;

        check_sdk_image(&t, &core, &sdk_key)?;
        if let Some(cache) = &sdk_cache {
            match cache.publish(&system_image) {
                Ok(files) => sdk_record.files = files,
                Err(e) => sdk_record.detail = format!("Generated; SDK cache store skipped: {e:#}"),
            }
        }
    }
    check_sdk_image(&t, &core, &sdk_key)?;
    drop(sdk_guard);
    sdk_record.detail = sdk_record.detail.chars().take(1024).collect();
    let sdk_records = vec![sdk_record];
    let mut cp = vec![android.clone(), core.clone()];
    cp.extend(jars.clone());
    let javac_args = |cp: &[PathBuf], out: &Path, source: &[PathBuf]| -> Result<Vec<OsString>> {
        let platform_args = if config.build.jvm_target == 8 {
            vec![
                os("-bootclasspath"),
                join(&[android.clone(), core.clone()])?,
            ]
        } else {
            vec![os("--system"), os(&system_image)]
        };
        let mut a = vec![
            os("-g"),
            os("-proc:none"),
            os("-encoding"),
            os("UTF-8"),
            os("-source"),
            os(config.build.jvm_target.to_string()),
            os("-target"),
            os(config.build.jvm_target.to_string()),
            os("-classpath"),
            join(cp)?,
            os("-d"),
            os(out),
        ];
        a.extend(platform_args);
        a.extend(source.iter().map(os));
        Ok(a)
    };
    steps.push(
        command(
            &t,
            &root,
            "Javac generated R/BuildConfig",
            &t.jdk.join("bin/javac"),
            javac_args(&cp, &gen_classes, &files(&generated, &["java"])?)?,
            event,
        )
        .await?,
    );
    let mut scope_jars = BTreeMap::<String, PathBuf>::new();
    let mut module_generated_classes = BTreeMap::<String, PathBuf>::new();
    for name in &modules {
        let own = work.join("module-generated-classes").join(name);
        std::fs::create_dir_all(&own)?;
        let m = config.modules.iter().find(|m| &m.name == name).unwrap();
        let own_ns = if name == &config.build.application_module {
            &app_ns
        } else {
            &m.namespace
        };
        copy_symbol_classes(&gen_classes, &own, own_ns)?;
        module_generated_classes.insert(name.clone(), own);
        let mut packages = BTreeSet::new();
        for project in &scopes.modules[name].projects {
            let m = config.modules.iter().find(|m| &m.name == project).unwrap();
            let p = if project == &config.build.application_module {
                &app_ns
            } else {
                &m.namespace
            };
            if !p.is_empty() {
                packages.insert(p.clone());
            }
        }
        for i in &scopes.modules[name].artifacts {
            if let Some(p) = artifact_packages.get(i) {
                packages.insert(p.clone());
            }
        }
        let scoped = work.join("symbol-inputs").join(name);
        std::fs::create_dir_all(&scoped)?;
        for package in packages {
            copy_symbol_classes(&gen_classes, &scoped, &package)?;
        }
        let jar = work.join("symbols").join(format!("{name}.jar"));
        std::fs::create_dir_all(jar.parent().unwrap())?;
        steps.push(
            command(
                &t,
                &root,
                &format!("Package scoped R/BuildConfig · {name}"),
                &t.jdk.join("bin/jar"),
                vec![
                    os("--create"),
                    os("--file"),
                    os(&jar),
                    os("-C"),
                    os(&scoped),
                    os("."),
                ],
                event,
            )
            .await?,
        );
        scope_jars.insert(name.clone(), jar);
    }
    let external = |name: &str| -> Vec<PathBuf> {
        lock.artifacts
            .iter()
            .enumerate()
            .rev()
            .filter(|(i, _)| scopes.modules[name].artifacts.contains(i))
            .flat_map(|(i, _)| artifact_jars.get(&i).into_iter().flatten().cloned())
            .collect()
    };
    if product == Product::Analysis {
        check_sdk_image(&t, &core, &sdk_key)?;
        check_resources(&t, &resource_checks, &resource_tool)?;
        let mut graph_modules = vec![];
        for name in &modules {
            let m = config.modules.iter().find(|m| &m.name == name).unwrap();
            let package = if name == &config.build.application_module {
                app_ns.clone()
            } else {
                m.namespace.clone()
            };
            let module_gen = work.join("module-generated").join(name);
            std::fs::create_dir_all(&module_gen)?;
            if !package.is_empty() {
                let target = module_gen.join(package.replace('.', "/"));
                std::fs::create_dir_all(&target)?;
                for f in ["R.java", "BuildConfig.java"] {
                    let source = generated.join(package.replace('.', "/")).join(f);
                    if source.is_file() {
                        std::fs::copy(source, target.join(f))?;
                    }
                }
            }
            graph_modules.push(Module {
                name: format!(":{name}"),
                directory: root.clone(),
                source_roots: m
                    .sources
                    .iter()
                    .map(|p| within(&root, p))
                    .collect::<Result<Vec<_>>>()?,
                classpath: external(name)
                    .into_iter()
                    .chain(std::iter::once(scope_jars[name].clone()))
                    .collect(),
                variants: vec!["debug".into(), "release".into()],
                namespace: package,
                project_type: if name == &config.build.application_module {
                    "APPLICATION"
                } else {
                    "LIBRARY"
                }
                .into(),
                resource_roots: m.resources.iter().map(|p| root.join(p)).collect(),
                generated_source_roots: vec![module_gen],
                boot_classpath: vec![android.clone(), core.clone()],
                classes_roots: vec![module_generated_classes[name].clone()],
                module_dependencies: m.projects.iter().map(|p| format!(":{p}")).collect(),
                selected_variant: variant.into(),
                assemble_task: format!("native:{name}:{variant}"),
                application_id: if name == &config.build.application_module {
                    config.android.application_id.clone()
                } else {
                    String::new()
                },
                ..Default::default()
            });
        }
        if let Some(info) = &mut compose {
            info.modules = modules.clone();
        }
        event(RunEvent::Phase(
            "ANALYSIS_GENERATE_OK · R/BuildConfig · no user compilation/DEX/sign/APK".into(),
        ));
        return Ok(BuildOutcome::Analysis(Box::new(AnalysisCompilation {
            config_blake3,
            lock_blake3,
            resource_cache: resource_records,
            sdk_cache: sdk_records,
            contexts: vec![],
            work: temp,
            graph: ProjectGraph {
                schema: 2,
                root: root.clone(),
                fingerprint: String::new(),
                modules: graph_modules,
                selected_variant: variant.into(),
                model_api: "Wex native analysis v1".into(),
                model_version: "1".into(),
                dependency_stamps: vec![],
            },
            compose,
            steps,
        })));
    }
    let mut outputs = BTreeMap::<String, PathBuf>::new();
    let mut class_dirs = vec![gen_classes.clone()];
    let mut kotlin_files = 0;
    let mut java_files = 0;
    let mut compilation_cache = vec![];
    let mut compilation_checks = vec![];
    let mut graph_modules = vec![];
    for name in &modules {
        let m = config.modules.iter().find(|m| &m.name == name).unwrap();
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "invalid native module name"
        );
        let out = work.join("classes").join(name);
        std::fs::create_dir_all(&out)?;
        let mut cp = external(name);
        cp.extend([android.clone(), core.clone(), scope_jars[name].clone()]);
        for dependency in scopes.modules[name].projects.iter().filter(|p| *p != name) {
            cp.push(
                outputs
                    .get(dependency)
                    .context("project dependency output missing")?
                    .clone(),
            );
        }
        let mut source = vec![];
        let mut roots = vec![];
        for p in &m.sources {
            let dir = within(&root, p)?;
            roots.push(dir.clone());
            source.extend(files(&dir, &["kt", "java"])?);
        }
        source.sort();
        source.dedup();
        let kotlin: Vec<_> = source
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "kt"))
            .cloned()
            .collect();
        let java: Vec<_> = source
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "java"))
            .cloned()
            .collect();
        kotlin_files += kotlin.len();
        java_files += java.len();
        let cache_enabled =
            product == Product::Apk && !config.build.compose.as_ref().is_some_and(|c| c.reports);
        let mut cache_record = crate::compile_cache::Record {
            module: name.clone(),
            key: String::new(),
            state: "disabled".into(),
            detail: "Private preview snapshot or Compose reports".into(),
            files: 0,
        };
        let key = if cache_enabled {
            match compile_key(
                &root,
                &t,
                &config,
                variant,
                name,
                &source,
                &cp,
                &system_image,
            ) {
                Ok(key) => {
                    cache_record.key = key.clone();
                    cache_record.state = "miss".into();
                    cache_record.detail = "No verified class output for these inputs".into();
                    Some(key)
                }
                Err(e) => {
                    cache_record.detail = format!("Cache fingerprint unavailable: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let cache = key.as_ref().and_then(|key| {
            match crate::compile_cache::Cache::open(&root, variant, name, key) {
                Ok(cache) => Some(cache),
                Err(e) => {
                    cache_record.state = "disabled".into();
                    cache_record.detail = format!("Cache unavailable: {e:#}");
                    None
                }
            }
        });
        let mut reused = false;
        if let Some(cache) = &cache {
            match cache.restore(&out) {
                Ok(Some(files)) => {
                    reused = true;
                    cache_record.state = "hit".into();
                    cache_record.detail = "Verified class output copied into this build".into();
                    cache_record.files = files;
                }
                Ok(None) => (),
                Err(e) => {
                    cache_record.detail = format!("Invalid class cache; recompiling: {e:#}");
                    std::fs::remove_dir_all(&out)?;
                    std::fs::create_dir_all(&out)?;
                }
            }
        }
        event(RunEvent::Phase(format!(
            "MODULE_CLASSES_{} · {name} · {}",
            cache_record.state.to_ascii_uppercase(),
            cache_record.detail.chars().take(512).collect::<String>()
        )));
        if let (Some(info), Some(_)) = (&mut compose, &config.build.compose)
            && !kotlin.is_empty()
        {
            info.modules.push(name.clone());
        }
        if !reused && !kotlin.is_empty() {
            let mut a = vec![
                os("-cp"),
                join(&t.compiler)?,
                os("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler"),
                os("-no-stdlib"),
                os("-no-reflect"),
                os("-no-jdk"),
                os("-jvm-target"),
                os(if config.build.jvm_target == 8 {
                    "1.8".into()
                } else {
                    config.build.jvm_target.to_string()
                }),
                os("-module-name"),
                os(name),
                os("-classpath"),
                join(&cp)?,
                os("-d"),
                os(&out),
            ];
            if let (Some(info), Some(settings)) = (&mut compose, &config.build.compose) {
                a.extend(compose_args(&info.plugin_jar, settings, name, &work));
            }
            a.extend(source.iter().map(os));
            steps.push(
                command(
                    &t,
                    &root,
                    &format!("Kotlin K2 · {name}"),
                    &t.jdk.join("bin/java"),
                    a,
                    event,
                )
                .await?,
            );
        }
        if !reused && !java.is_empty() {
            let mut java_cp = cp.clone();
            java_cp.push(out.clone());
            steps.push(
                command(
                    &t,
                    &root,
                    &format!("Javac · {name}"),
                    &t.jdk.join("bin/javac"),
                    javac_args(&java_cp, &out, &java)?,
                    event,
                )
                .await?,
            );
        }
        if let Some(key) = key {
            ensure!(
                compile_key(
                    &root,
                    &t,
                    &config,
                    variant,
                    name,
                    &source,
                    &cp,
                    &system_image
                )? == key,
                "Module compiler inputs changed during compilation/cache restore"
            );
            if !reused && let Some(cache) = &cache {
                match cache.publish(&out) {
                    Ok(files) => cache_record.files = files,
                    Err(e) => {
                        cache_record.detail = format!("Compiled; cache publication skipped: {e:#}");
                        event(RunEvent::Phase(
                            cache_record.detail.chars().take(512).collect(),
                        ));
                    }
                }
            }
            compilation_checks.push((name.clone(), source.clone(), cp.clone(), key));
        }
        cache_record.detail = cache_record.detail.chars().take(1024).collect();
        compilation_cache.push(cache_record);
        let module_gen = work.join("module-generated").join(name);
        std::fs::create_dir_all(&module_gen)?;
        let package = if name == &config.build.application_module {
            app_ns.clone()
        } else {
            m.namespace.clone()
        };
        if !package.is_empty() {
            let target = module_gen.join(package.replace('.', "/"));
            std::fs::create_dir_all(&target)?;
            for f in ["R.java", "BuildConfig.java"] {
                let p = generated.join(package.replace('.', "/")).join(f);
                if p.is_file() {
                    std::fs::copy(p, target.join(f))?;
                }
            }
        }
        graph_modules.push(Module {
            name: format!(":{name}"),
            directory: root.clone(),
            source_roots: roots,
            classpath: external(name)
                .into_iter()
                .chain(std::iter::once(scope_jars[name].clone()))
                .collect(),
            variants: vec!["debug".into(), "release".into()],
            namespace: if name == &config.build.application_module {
                app_ns.clone()
            } else {
                m.namespace.clone()
            },
            project_type: if name == &config.build.application_module {
                "APPLICATION"
            } else {
                "LIBRARY"
            }
            .into(),
            resource_roots: m.resources.iter().map(|p| root.join(p)).collect(),
            generated_source_roots: vec![module_gen],
            boot_classpath: vec![android.clone(), core.clone()],
            classes_roots: vec![out.clone(), module_generated_classes[name].clone()],
            module_dependencies: m.projects.iter().map(|s| format!(":{s}")).collect(),
            selected_variant: variant.into(),
            assemble_task: format!("native:{name}:{variant}"),
            application_id: if name == &config.build.application_module {
                config.android.application_id.clone()
            } else {
                String::new()
            },
            ..Default::default()
        });
        class_dirs.push(out.clone());
        outputs.insert(name.clone(), out);
    }
    let mut program_jars = jars.clone();
    for (i, dir) in class_dirs.iter().enumerate() {
        let jar = work.join(format!("module-{i}.jar"));
        steps.push(
            command(
                &t,
                &root,
                "Package class files",
                &t.jdk.join("bin/jar"),
                vec![
                    os("--create"),
                    os("--file"),
                    os(&jar),
                    os("-C"),
                    os(dir),
                    os("."),
                ],
                event,
            )
            .await?,
        );
        program_jars.push(jar);
    }
    if preview {
        check_sdk_image(&t, &core, &sdk_key)?;
        check_resources(&t, &resource_checks, &resource_tool)?;
        ensure!(
            input_fingerprint(&root, &config)? == input_blake3,
            "preview compilation input changed"
        );
        native::resolve(&root, &native::default_cache(), true, true)?;
        if let Some(info) = &compose {
            ensure!(
                blake3::hash(&std::fs::read(&info.plugin_jar)?)
                    .to_hex()
                    .as_str()
                    == info.plugin_blake3,
                "Compose plugin changed during preview compilation"
            );
        }
        let resources = work.join("resources.apk");
        let resource_blake3 = blake3::hash(&std::fs::read(&resources)?)
            .to_hex()
            .to_string();
        class_dirs.push(work.join("module-0.jar"));
        event(RunEvent::Phase(
            "PREVIEW_COMPILE_OK · JVM classes/resources · no DEX/sign/install".into(),
        ));
        return Ok(BuildOutcome::Preview(Box::new(PreviewCompilation {
            _work: temp,
            resource_cache: resource_records,
            sdk_cache: sdk_records,
            classpath: class_dirs,
            resources,
            resource_blake3,
            compose,
            steps,
            elapsed_ms: start.elapsed().as_millis(),
        })));
    }
    let dex = work.join("dex");
    std::fs::create_dir_all(&dex)?;
    let mut d8 = vec![
        os(if debuggable { "--debug" } else { "--release" }),
        os("--min-api"),
        os(config.android.min_sdk.to_string()),
        os("--lib"),
        os(&android),
        os("--lib"),
        os(&core),
        os("--output"),
        os(&dex),
    ];
    d8.extend(program_jars.iter().map(os));
    let mut dex_record = crate::compile_cache::Record {
        module: "dex".into(),
        key: String::new(),
        state: "disabled".into(),
        detail: "DEX fingerprint unavailable".into(),
        files: 0,
    };
    let dex_key = match dex_key(
        &root,
        &t,
        variant,
        config.android.min_sdk,
        &program_jars,
        &android,
        &core,
    ) {
        Ok(key) => {
            dex_record.key = key.clone();
            dex_record.state = "miss".into();
            dex_record.detail = "No verified DEX for ordered bytecode inputs".into();
            Some(key)
        }
        Err(e) => {
            dex_record.detail = format!("DEX fingerprint unavailable: {e:#}");
            None
        }
    };
    let mut dex_guard = None;
    let dex_cache = dex_key.as_ref().and_then(|key| {
        match crate::compile_cache::Cache::dex(&root, key)
            .and_then(|cache| Ok((cache, crate::sdk_cache::Guard::dex(&root, key)?)))
        {
            Ok((cache, Some(guard))) => {
                dex_guard = Some(guard);
                Some(cache)
            }
            Ok((_, None)) => {
                dex_record.state = "disabled".into();
                dex_record.detail = "DEX cache busy; independent D8 output".into();
                None
            }
            Err(e) => {
                dex_record.state = "disabled".into();
                dex_record.detail = format!("DEX cache unavailable: {e:#}");
                None
            }
        }
    });
    let mut dex_reused = false;
    if let Some(cache) = &dex_cache {
        match cache.restore(&dex).and_then(|n| {
            if n.is_some() {
                crate::dex_cache::validate(&dex)?;
            }
            Ok(n)
        }) {
            Ok(Some(n)) => {
                dex_reused = true;
                dex_record.state = "hit".into();
                dex_record.files = n;
                dex_record.detail = "Verified DEX copied into this build".into();
            }
            Ok(None) => {}
            Err(e) => {
                dex_record.detail = format!("Invalid DEX cache; regenerating: {e:#}");
                std::fs::remove_dir_all(&dex)?;
                std::fs::create_dir(&dex)?;
            }
        }
    }
    event(RunEvent::BuildLine(format!(
        "DEX {} · {}",
        dex_record.state.to_ascii_uppercase(),
        dex_record.detail.chars().take(512).collect::<String>()
    )));
    if !dex_reused {
        steps.push(
            command(
                &t,
                &root,
                "D8 · JVM bytecode → DEX",
                &t.bt.join("d8"),
                d8,
                event,
            )
            .await?,
        );
    }
    let dex_files = crate::dex_cache::validate(&dex)?;
    if let Some(key) = &dex_key {
        ensure!(
            self::dex_key(
                &root,
                &t,
                variant,
                config.android.min_sdk,
                &program_jars,
                &android,
                &core
            )? == *key,
            "D8 inputs changed; DEX not published"
        );
        if !dex_reused && let Some(cache) = &dex_cache {
            match cache.publish(&dex) {
                Ok(n) => dex_record.files = n,
                Err(e) => dex_record.detail = format!("Generated; DEX cache store skipped: {e:#}"),
            }
        }
    }
    dex_record.detail = dex_record.detail.chars().take(1024).collect();
    drop(dex_guard);
    for f in dex_files {
        add_payload(
            &mut packaged,
            f.file_name().unwrap().to_string_lossy().into(),
            std::fs::read(f)?,
        )?;
    }
    for jar in &program_jars {
        jar_resources(jar, &mut packaged)?;
    }
    let unsigned = work.join("unsigned.apk");
    apk_package(&work.join("resources.apk"), &unsigned, &packaged)?;
    let aligned = work.join("aligned.apk");
    steps.push(
        command(
            &t,
            &root,
            "Zipalign · 16 KiB native pages",
            &t.bt.join("zipalign"),
            vec![
                os("-f"),
                os("-P"),
                os("16"),
                os("4"),
                os(&unsigned),
                os(&aligned),
            ],
            event,
        )
        .await?,
    );
    let apk = work.join(format!("{module}-{variant}.apk"));
    let signing = if let Some(key) = &release {
        steps.push(
            key.sign(&t.jdk, &t.bt.join("apksigner"), &aligned, &apk, event)
                .await?,
        );
        let (step, info) = key
            .verify(&t.jdk, &t.bt.join("apksigner"), &apk, event)
            .await?;
        steps.push(step);
        Some(info)
    } else {
        let keystore = std::env::var_os("WEX_DEBUG_KEYSTORE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".android/debug.keystore"));
        if !keystore.exists() {
            std::fs::create_dir_all(keystore.parent().context("keystore parent")?)?;
            steps.push(
                command(
                    &t,
                    &root,
                    "Create persistent debug signing key",
                    &t.jdk.join("bin/keytool"),
                    vec![
                        os("-genkeypair"),
                        os("-keystore"),
                        os(&keystore),
                        os("-storepass:env"),
                        os("WEX_DEBUG_STOREPASS"),
                        os("-keypass:env"),
                        os("WEX_DEBUG_STOREPASS"),
                        os("-alias"),
                        os("androiddebugkey"),
                        os("-keyalg"),
                        os("RSA"),
                        os("-keysize"),
                        os("2048"),
                        os("-validity"),
                        os("10000"),
                        os("-dname"),
                        os("CN=Android Debug,O=Android,C=US"),
                    ],
                    event,
                )
                .await?,
            );
        }
        steps.push(
            command(
                &t,
                &root,
                "APK sign · debug key",
                &t.bt.join("apksigner"),
                vec![
                    os("sign"),
                    os("--ks"),
                    os(&keystore),
                    os("--ks-key-alias"),
                    os("androiddebugkey"),
                    os("--ks-pass"),
                    os("env:WEX_DEBUG_STOREPASS"),
                    os("--key-pass"),
                    os("env:WEX_DEBUG_STOREPASS"),
                    os("--v4-signing-enabled"),
                    os("false"),
                    os("--out"),
                    os(&apk),
                    os(&aligned),
                ],
                event,
            )
            .await?,
        );
        steps.push(
            command(
                &t,
                &root,
                "Verify APK signature",
                &t.bt.join("apksigner"),
                vec![os("verify"), os("--verbose"), os("--print-certs"), os(&apk)],
                event,
            )
            .await?,
        );
        let cert = crate::signing::certificate(
            &steps.last().context("signature verification step")?.stdout,
        )?;
        Some(crate::signing::Info {
            kind: "debug".into(),
            keystore: keystore.clone(),
            key_alias: "androiddebugkey".into(),
            keystore_blake3: blake3::hash(&std::fs::read(&keystore)?)
                .to_hex()
                .to_string(),
            certificate_sha256: cert,
        })
    };
    steps.push(
        command(
            &t,
            &root,
            "Verify ZIP alignment",
            &t.bt.join("zipalign"),
            vec![os("-c"), os("-P"), os("16"), os("4"), os(&apk)],
            event,
        )
        .await?,
    );
    ensure!(
        input_blake3 == input_fingerprint(&root, &config)?,
        "native inputs changed while building; artifact not published"
    );
    let r = root.clone();
    let cache = native::default_cache();
    tokio::task::spawn_blocking(move || native::resolve(&r, &cache, true, true)).await??;
    if let Some(info) = &compose {
        ensure!(
            blake3::hash(&std::fs::read(&info.plugin_jar)?)
                .to_hex()
                .as_str()
                == info.plugin_blake3,
            "Compose compiler plugin changed during build"
        );
    }
    for (name, source, cp, key) in &compilation_checks {
        ensure!(
            compile_key(&root, &t, &config, variant, name, source, cp, &system_image)? == *key,
            "Module compiler inputs changed before APK publication"
        );
    }
    check_sdk_image(&t, &core, &sdk_key)?;
    check_resources(&t, &resource_checks, &resource_tool)?;
    if let Some(key) = &dex_key {
        ensure!(
            self::dex_key(
                &root,
                &t,
                variant,
                config.android.min_sdk,
                &program_jars,
                &android,
                &core
            )? == *key,
            "D8 inputs changed before APK publication"
        );
    }
    let data = std::fs::read(&apk)?;
    let config_blake3 = blake3::hash(&std::fs::read(root.join("wex.json"))?)
        .to_hex()
        .to_string();
    let graph_file = work.join("project-graph.json");
    let graph = ProjectGraph {
        schema: 2,
        root: root.clone(),
        fingerprint: config_blake3.clone(),
        modules: graph_modules,
        selected_variant: variant.into(),
        model_api: "Wex native config v1".into(),
        model_version: "1".into(),
        dependency_stamps: vec![],
    };
    std::fs::write(&graph_file, serde_json::to_vec_pretty(&graph)?)?;
    if let Some(key) = &release {
        key.unchanged()?;
    }
    let report = BuildReport {
        dex_cache: vec![dex_record],
        compilation_cache,
        sdk_cache: sdk_records,
        resource_cache: resource_records,
        compose,
        signing,
        debuggable,
        backend: "wex-native-v1".into(),
        module: format!(":{module}"),
        variant: variant.into(),
        apk: ApkArtifact {
            path: apk,
            application_id: config.android.application_id,
            variant: variant.into(),
        },
        blake3: blake3::hash(&data).to_hex().to_string(),
        bytes: data.len() as u64,
        config_blake3,
        input_blake3,
        kotlin_files,
        java_files,
        dependency_artifacts: scopes.runtime.artifacts.len(),
        build_dir: work.clone(),
        graph_file,
        steps,
        elapsed_ms: start.elapsed().as_millis(),
    };
    std::fs::write(
        work.join("build-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let _kept = temp.keep();
    write_atomic(
        &parent.join("current.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    event(RunEvent::Phase(format!(
        "NATIVE_BUILD_OK · {kotlin_files} Kotlin / {java_files} Java · signed APK"
    )));
    Ok(BuildOutcome::Apk(Box::new(report)))
}
struct BuildGuard {
    path: PathBuf,
}
impl BuildGuard {
    fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join("building.lock");
        let mut f=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).context("another native build is active; inspect building.lock if a previous process crashed")?;
        write!(f, "{}", std::process::id())?;
        Ok(Self { path })
    }
}
impl Drop for BuildGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn add_payload(files: &mut BTreeMap<String, Vec<u8>>, name: String, data: Vec<u8>) -> Result<()> {
    ensure!(
        Path::new(&name)
            .components()
            .all(|p| matches!(p, std::path::Component::Normal(_))),
        "invalid APK payload path"
    );
    if let Some(old) = files.get_mut(&name) {
        if name.starts_with("META-INF/services/") {
            old.extend(b"\n");
            old.extend(data);
        } else {
            ensure!(old == &data, "conflicting APK payload {name}");
        }
    } else {
        files.insert(name, data);
    }
    Ok(())
}
fn jar_resources(jar: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let mut z = ZipArchive::new(std::fs::File::open(jar)?)?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let name = f.name().to_owned();
        if f.is_dir()
            || name.ends_with(".class")
            || (name.starts_with("META-INF/") && !name.starts_with("META-INF/services/"))
        {
            continue;
        }
        ensure!(f.size() <= 16 * 1024 * 1024, "JAR resource too large");
        let mut bytes = vec![];
        f.read_to_end(&mut bytes)?;
        add_payload(out, name, bytes)?;
    }
    Ok(())
}
fn apk_package(resources: &Path, out: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let mut z = ZipArchive::new(std::fs::File::open(resources)?)?;
    let mut w = ZipWriter::new(std::fs::File::create(out)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut names = BTreeSet::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let name = f.name().to_owned();
        if f.is_dir() {
            continue;
        }
        ensure!(names.insert(name.clone()), "duplicate resource ZIP name");
        w.start_file(&name, options)?;
        std::io::copy(&mut f, &mut w)?;
    }
    for (name, bytes) in files {
        ensure!(names.insert(name.clone()), "duplicate APK name {name}");
        w.start_file(name, options)?;
        w.write_all(bytes)?;
    }
    w.finish()?;
    Ok(())
}
pub fn load_graph(root: &Path, variant: &str) -> Result<ProjectGraph> {
    ensure!(
        matches!(variant, "debug" | "release"),
        "native graph variant unavailable"
    );
    let report: BuildReport = serde_json::from_slice(&std::fs::read(
        root.join(".wex/native").join(variant).join("current.json"),
    )?)
    .context("Build/Run project first to generate native K2 graph")?;
    ensure!(
        report.config_blake3
            == blake3::hash(&std::fs::read(root.join("wex.json"))?)
                .to_hex()
                .as_str(),
        "native config changed; Build/Run to refresh K2 graph"
    );
    ensure!(
        report.variant == variant && report.debuggable == (variant == "debug"),
        "native graph variant/debuggable mismatch"
    );
    let graph: ProjectGraph = serde_json::from_slice(&std::fs::read(report.graph_file)?)?;
    ensure!(
        graph.selected_variant == variant,
        "native graph selection mismatch"
    );
    ensure!(
        graph
            .modules
            .iter()
            .flat_map(|m| &m.classpath)
            .all(|p| p.is_file()),
        "native classpath missing; rebuild"
    );
    Ok(graph)
}

#[cfg(test)]
mod tests {
    #[test]
    fn application_input_fingerprint_ignores_detached_bodies_and_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut cfg: crate::native::Config =
            serde_json::from_str(include_str!("../tests/fixtures/native-android/wex.json"))
                .unwrap();
        cfg.modules = vec![
            crate::native::Module {
                name: "app".into(),
                sources: vec!["app-src".into()],
                dependencies: vec![],
                ..Default::default()
            },
            crate::native::Module {
                name: "detached".into(),
                sources: vec!["detached-src".into()],
                resources: vec!["detached-res".into()],
                dependencies: vec![],
                ..Default::default()
            },
        ];
        cfg.build.manifest = "AndroidManifest.xml".into();
        for dir in ["app-src", "detached-src", "detached-res"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (path, text) in [
            ("AndroidManifest.xml", "manifest"),
            ("wex.lock.json", "lock"),
            ("app-src/App.kt", "val app=1"),
            ("detached-src/Detached.kt", "val detached=1"),
            ("detached-res/broken.xml", "<broken>"),
        ] {
            std::fs::write(root.join(path), text).unwrap();
        }
        std::fs::write(root.join("wex.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
        let original = super::input_fingerprint(root, &cfg).unwrap();
        std::fs::write(
            root.join("detached-src/Detached.kt"),
            "deliberate Kotlin error",
        )
        .unwrap();
        std::fs::write(
            root.join("detached-res/broken.xml"),
            "malformed unused resource",
        )
        .unwrap();
        assert_eq!(super::input_fingerprint(root, &cfg).unwrap(), original);
        std::fs::write(root.join("app-src/App.kt"), "val app=2").unwrap();
        assert_ne!(super::input_fingerprint(root, &cfg).unwrap(), original);
    }

    #[test]
    fn detached_context_rebases_only_generated_paths_not_user_sources() {
        let old = std::path::Path::new("/old/generation");
        let new = std::path::Path::new("/new/generation/contexts/detached");
        let mut module = crate::project::Module {
            source_roots: vec!["/workspace/src".into()],
            classpath: vec!["/cache/library.jar".into(), old.join("symbols/lib.jar")],
            generated_source_roots: vec![old.join("module-generated/lib")],
            generated_resource_roots: vec![old.join("res")],
            classes_roots: vec![old.join("module-generated-classes/lib")],
            ..Default::default()
        };
        super::relocate_generated(&mut module, old, new);
        assert_eq!(
            module.source_roots,
            [std::path::PathBuf::from("/workspace/src")]
        );
        assert_eq!(
            module.classpath,
            [
                std::path::PathBuf::from("/cache/library.jar"),
                new.join("symbols/lib.jar")
            ]
        );
        assert_eq!(
            module.classes_roots,
            [new.join("module-generated-classes/lib")]
        );
        assert_eq!(
            module.generated_source_roots,
            [new.join("module-generated/lib")]
        );
        assert_eq!(module.generated_resource_roots, [new.join("res")]);
    }

    use super::*;
    #[tokio::test]
    async fn release_without_signing_fails_before_tools_or_publication() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wex.json"),
            include_str!("../tests/fixtures/native-android/wex.json"),
        )
        .unwrap();
        let events = std::sync::Mutex::new(Vec::new());
        let error = build(root.path(), ":app", "release", true, &|e| {
            events.lock().unwrap().push(e)
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("release_signing"));
        assert!(events.lock().unwrap().is_empty());
        assert!(!root.path().join(".wex/native").exists());
    }
    #[test]
    fn scoped_symbol_copy_does_not_recurse_into_child_namespaces_or_user_classes() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        let to = tmp.path().join("to");
        std::fs::create_dir_all(from.join("foo/base/child")).unwrap();
        for name in ["R.class", "R$attr.class", "BuildConfig.class", "Main.class"] {
            std::fs::write(from.join("foo/base").join(name), b"test").unwrap();
        }
        std::fs::write(from.join("foo/base/child/R.class"), b"child").unwrap();
        copy_symbol_classes(&from, &to, "foo.base").unwrap();
        assert!(to.join("foo/base/R$attr.class").is_file());
        assert!(!to.join("foo/base/child").exists() && !to.join("foo/base/Main.class").exists());
    }
    #[test]
    fn release_key_inside_resources_or_assets_is_rejected_before_signing() {
        let root = tempfile::tempdir().unwrap();
        let cfg: Config =
            serde_json::from_str(include_str!("../tests/fixtures/native-android/wex.json"))
                .unwrap();
        let path = root.path().join("app/src/main/assets/release.jks");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "unit key fixture").unwrap();
        assert!(
            ensure_key_not_packaged(root.path(), &cfg, &path.canonicalize().unwrap())
                .unwrap_err()
                .to_string()
                .contains("would be packaged")
        );
        assert!(
            ensure_key_not_packaged(root.path(), &cfg, &root.path().join("outside.jks")).is_ok()
        );
    }
    #[cfg(unix)]
    #[test]
    fn d8_relative_path_search_uses_project_root_and_skips_nonexecutable_files() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        for name in ["blocked", "runtime"] {
            let p = project.path().join(name).join("bin/java");
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "test launcher").unwrap();
            std::fs::set_permissions(
                &p,
                std::fs::Permissions::from_mode(if name == "blocked" { 0o644 } else { 0o755 }),
            )
            .unwrap();
        }
        let path = std::env::join_paths(["blocked/bin", "runtime/bin"]).unwrap();
        assert_eq!(
            d8_java(project.path(), &path).unwrap(),
            project
                .path()
                .join("runtime/bin/java")
                .canonicalize()
                .unwrap()
        );
        let first = project.path().join("blocked/bin/java");
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            d8_java(project.path(), &path).unwrap(),
            first.canonicalize().unwrap()
        );
        assert!(d8_java(Path::new("relative-root"), &path).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn d8_empty_path_entry_uses_project_java_and_resolves_its_runtime_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let project = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let java = runtime.path().join("bin/java");
        std::fs::create_dir(java.parent().unwrap()).unwrap();
        std::fs::write(&java, "test launcher").unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&java, project.path().join("java")).unwrap();
        let path = std::env::join_paths([Path::new(""), Path::new("missing")]).unwrap();
        assert_eq!(
            d8_java(project.path(), &path).unwrap(),
            java.canonicalize().unwrap()
        );
    }
    #[test]
    fn missing_packaged_key_is_rejected_without_credentials_or_existing_roots() {
        let root = tempfile::tempdir().unwrap();
        let cfg: Config =
            serde_json::from_str(include_str!("../tests/fixtures/native-android/wex.json"))
                .unwrap();
        assert!(
            ensure_key_not_packaged(
                root.path(),
                &cfg,
                &root.path().join("app/src/main/res/raw/private.jks")
            )
            .is_err()
        );
        assert!(
            ensure_key_not_packaged(root.path(), &cfg, &root.path().join("outside/missing.jks"))
                .is_ok()
        );
    }
    #[test]
    fn compose_options_are_typed_matched_and_isolated_per_module() {
        let settings = native::ComposeSettings {
            source_information: false,
            reports: true,
            strong_skipping: false,
        };
        let a = compose_args(
            Path::new("/cache/plugin.jar"),
            &settings,
            "shared",
            Path::new("/build"),
        );
        let a = a
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(a[0], "-Xplugin=/cache/plugin.jar");
        assert!(a.contains(
            &"plugin:androidx.compose.compiler.plugins.kotlin:sourceInformation=false".into()
        ));
        assert!(a.contains(
            &"plugin:androidx.compose.compiler.plugins.kotlin:featureFlag=-StrongSkipping".into()
        ));
        assert!(a.contains(&"plugin:androidx.compose.compiler.plugins.kotlin:reportsDestination=/build/compose/shared/reports".into()));
        assert_eq!(
            compose_plugin_filename("2.4.0"),
            "kotlin-compose-compiler-plugin-embeddable-2.4.0.jar"
        );
    }
    #[test]
    fn compose_defaults_do_not_change_non_compose_toolchain_configuration() {
        let plain = serde_json::to_value(native::BuildSettings::default()).unwrap();
        assert!(plain.get("compose").is_none());
        let c: native::ComposeSettings = serde_json::from_str("{}").unwrap();
        assert!(c.source_information && c.strong_skipping && !c.reports);
        assert!(
            serde_json::from_str::<native::ComposeSettings>(r#"{"compiler_version":"1.0"}"#)
                .is_err()
        );
    }
    #[test]
    fn archive_traversal_and_symlinks_rejected() {
        for name in ["../escape", "/absolute"] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("bad.aar");
            let mut z = ZipWriter::new(std::fs::File::create(&p).unwrap());
            z.start_file(name, SimpleFileOptions::default()).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
            assert!(extract_aar(&p, &d.path().join("out")).is_err());
        }
    }
    #[test]
    fn duplicate_payload_requires_identical_bytes_and_services_merge() {
        let mut f = BTreeMap::new();
        add_payload(&mut f, "assets/a".into(), vec![1]).unwrap();
        assert!(add_payload(&mut f, "assets/a".into(), vec![2]).is_err());
        add_payload(&mut f, "META-INF/services/x".into(), b"a".to_vec()).unwrap();
        add_payload(&mut f, "META-INF/services/x".into(), b"b".to_vec()).unwrap();
        assert_eq!(f["META-INF/services/x"], b"a\nb");
    }
    #[test]
    fn concurrent_build_lock_releases_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let first = BuildGuard::acquire(d.path()).unwrap();
        assert!(BuildGuard::acquire(d.path()).is_err());
        drop(first);
        assert!(BuildGuard::acquire(d.path()).is_ok());
    }
    #[test]
    fn publication_is_atomic_and_apk_preserves_resource_payload() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().join("resources.apk");
        let mut z = ZipWriter::new(std::fs::File::create(&r).unwrap());
        z.start_file("AndroidManifest.xml", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"AXML").unwrap();
        z.finish().unwrap();
        let out = d.path().join("unsigned.apk");
        apk_package(
            &r,
            &out,
            &BTreeMap::from([("classes.dex".into(), b"dex\n039\0".to_vec())]),
        )
        .unwrap();
        let mut zip = ZipArchive::new(std::fs::File::open(out).unwrap()).unwrap();
        assert_eq!(zip.len(), 2);
        assert_eq!(zip.by_name("AndroidManifest.xml").unwrap().size(), 4);
        let current = d.path().join("current.json");
        write_atomic(&current, b"old").unwrap();
        write_atomic(&current, b"new").unwrap();
        assert_eq!(std::fs::read(current).unwrap(), b"new");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_tool_returns_failure_and_preserves_previous_artifact_pointer() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("fail");
        std::fs::write(&script, "#!/bin/sh\necho actual-failure >&2\nexit 73\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = Tools {
            sdk: PathBuf::new(),
            bt: PathBuf::new(),
            jdk: PathBuf::new(),
            compiler: vec![],
            secret_env: vec![],
        };
        let pointer = d.path().join("current.json");
        std::fs::write(&pointer, b"previous-artifact").unwrap();
        // Read the freshly written fixture via its interpreter; do not execute a
        // new script inode while other parallel tests can be forking children.
        let e = command(
            &t,
            d.path(),
            "Kotlin",
            Path::new("/bin/sh"),
            vec![os(&script)],
            &|_| {},
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string().contains("73") && e.to_string().contains("actual-failure"),
            "full tool error: {e:#}"
        );
        assert_eq!(std::fs::read(pointer).unwrap(), b"previous-artifact");
    }
}
