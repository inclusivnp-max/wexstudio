//! Build -> PackageManager streamed install -> launcher -> PID-bound binary Logcat.
use crate::{
    adb::{AdbClient, InstallResult, ShellOutput},
    logcat::{Decoder, LogEntry},
    project::Module,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    net::TcpStream,
};

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub serial: String,
    pub module: String,
    pub variant: String,
    pub offline: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub model: String,
    pub sdk: u32,
    pub emulator: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkArtifact {
    pub path: PathBuf,
    pub application_id: String,
    pub variant: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: String,
    pub serial: String,
    pub device: DeviceInfo,
    pub module: String,
    pub variant: String,
    pub build_task: String,
    pub build_backend: String,
    pub native_build: crate::build::BuildReport,
    pub apk: ApkArtifact,
    pub install: InstallResult,
    pub stop_before_install: ShellOutput,
    pub component: String,
    pub launch: ShellOutput,
    pub pid: u32,
    pub log_since_seconds: u64,
    pub elapsed_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_transport: Option<DebugTransport>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugTransport {
    pub kind: String,
    pub pid: u32,
    pub launch_attempt: u32,
    pub claim_source: String,
    pub claimed_before_launch_return: bool,
}
pub struct Deployment {
    pub report: RunReport,
    pub logs: TcpStream,
    pub jdwp: Option<TcpStream>,
}
#[derive(Debug, Clone)]
pub enum RunEvent {
    Phase(String),
    BuildLine(String),
}
#[derive(Debug, Serialize)]
pub struct LogObservation {
    pub received: u64,
    pub entries: Vec<LogEntry>,
    pub marker_found: bool,
}

/// Require this exact transport before tools, build or device mutation. Never alias USB to Wi-Fi.
pub async fn require_device(adb: &AdbClient, serial: &str) -> Result<crate::adb::Device> {
    ensure!(
        !serial.is_empty() && !serial.contains(['\n', '\r', '\0']),
        "invalid device serial"
    );
    let devices = adb.devices().await?;
    devices.iter().find(|d| d.serial == serial && d.usable()).cloned().with_context(|| {
        let state = devices.iter().find(|d| d.serial == serial).map(|d| d.state.as_str()).unwrap_or("absent");
        format!("Selected device {serial:?} is not connected/authorized (state: {state}); connect this exact serial")
    })
}

pub async fn device_info(adb: &AdbClient, serial: &str) -> Result<DeviceInfo> {
    async fn prop(adb: &AdbClient, serial: &str, key: &str) -> Result<String> {
        Ok(adb
            .shell(serial, &["getprop", key])
            .await?
            .check("device property")?
            .stdout
            .trim()
            .to_owned())
    }
    Ok(DeviceInfo {
        model: prop(adb, serial, "ro.product.model").await?,
        sdk: prop(adb, serial, "ro.build.version.sdk").await?.parse()?,
        emulator: prop(adb, serial, "ro.kernel.qemu").await? == "1",
    })
}
fn package_valid(package: &str) -> bool {
    package.split('.').count() >= 2
        && package.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                && part.as_bytes()[0].is_ascii_alphabetic()
        })
}
pub fn launcher_component(output: &str, package: &str) -> Result<String> {
    ensure!(package_valid(package), "invalid application ID");
    for line in output.lines().rev() {
        let line = line.trim();
        if let Some((p, class)) = line.split_once('/')
            && p == package
            && !class.is_empty()
            && class
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._$".contains(&c))
        {
            return Ok(line.to_owned());
        }
    }
    bail!("No launcher Activity resolved for {package}: {output}")
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    application_id: String,
    variant_name: String,
    elements: Vec<Element>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Element {
    output_file: String,
    #[serde(default)]
    filters: Vec<serde_json::Value>,
}
/// Follow the AGP-provided file locator, not a guessed build/outputs path.
pub fn built_apk(module: &Module) -> Result<ApkArtifact> {
    ensure!(
        !module.apk_listing_file.as_os_str().is_empty(),
        "Android model has no APK output listing"
    );
    let mut listing = module.apk_listing_file.clone();
    for _ in 0..4 {
        let bytes = std::fs::read(&listing)
            .with_context(|| format!("reading APK output listing {}", listing.display()))?;
        ensure!(bytes.len() <= 1024 * 1024, "APK listing exceeds 1 MiB");
        if bytes.starts_with(b"{") {
            let meta: Metadata = serde_json::from_slice(&bytes)?;
            ensure!(
                meta.application_id == module.application_id
                    && meta.variant_name == module.selected_variant,
                "APK metadata does not match selected application/variant"
            );
            ensure!(
                meta.elements.len() == 1 && meta.elements[0].filters.is_empty(),
                "Run requires a single/universal APK output"
            );
            let element = &meta.elements[0];
            let parent = listing
                .parent()
                .context("APK listing parent")?
                .canonicalize()?;
            let path = parent.join(&element.output_file).canonicalize()?;
            ensure!(
                path.starts_with(&parent)
                    && path.extension().is_some_and(|e| e == "apk")
                    && path.is_file(),
                "invalid APK output path"
            );
            return Ok(ApkArtifact {
                path,
                application_id: meta.application_id,
                variant: meta.variant_name,
            });
        }
        let text = std::str::from_utf8(&bytes)?;
        let next = text
            .lines()
            .find_map(|l| l.strip_prefix("listingFile="))
            .context("invalid APK file locator")?;
        listing = listing
            .parent()
            .context("locator parent")?
            .join(next)
            .canonicalize()?;
    }
    bail!("APK file locator chain exceeds four files")
}
pub async fn build_task(
    root: &Path,
    task: &str,
    offline: bool,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<()> {
    ensure!(
        !task.is_empty()
            && !task.starts_with('-')
            && task
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ":_-".contains(c)),
        "invalid Gradle task"
    );
    let wrapper = root.join("gradlew");
    ensure!(wrapper.is_file(), "project has no Gradle wrapper");
    crate::gradle::cached_distribution(root)?;
    let mut command = tokio::process::Command::new(wrapper);
    command
        .arg(task)
        .arg("--console=plain")
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if offline {
        command.arg("--offline");
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    async fn pump<R: tokio::io::AsyncRead + Unpin>(
        r: R,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<()> {
        let mut r = BufReader::new(r);
        loop {
            let mut bytes = Vec::new();
            let n = (&mut r).take(8192).read_until(b'\n', &mut bytes).await?;
            if n == 0 {
                break;
            }
            event(RunEvent::BuildLine(
                String::from_utf8_lossy(&bytes).trim_end().to_owned(),
            ));
        }
        Ok(())
    }
    let (a, b, status) = tokio::time::timeout(Duration::from_secs(600), async {
        tokio::join!(pump(stdout, event), pump(stderr, event), child.wait())
    })
    .await
    .context("Gradle build timed out")?;
    a?;
    b?;
    let status = status?;
    ensure!(status.success(), "Gradle build failed: {status}");
    Ok(())
}
#[derive(Debug, PartialEq)]
struct DebugAppState {
    app: String,
    original: String,
    transient: bool,
}
fn debug_app_state(text: &str) -> Result<Option<DebugAppState>> {
    let rows = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("mDebugApp="))
        .collect::<Vec<_>>();
    ensure!(rows.len() <= 1, "ambiguous Android debug-app state");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mut fields = row.split_whitespace();
    let (app, original) = fields
        .next()
        .context("missing debug-app state")?
        .split_once("/orig=")
        .context("malformed debug-app state")?;
    let transient = fields
        .find_map(|s| s.strip_prefix("mDebugTransient="))
        .context("missing transient debug-app state")?;
    ensure!(
        matches!(transient, "true" | "false"),
        "invalid transient debug-app state"
    );
    ensure!(
        [app, original]
            .iter()
            .all(|s| *s == "null" || package_valid(s)),
        "invalid debug-app package"
    );
    Ok(Some(DebugAppState {
        app: app.into(),
        original: original.into(),
        transient: transient == "true",
    }))
}
fn global_debug_settings(text: &str) -> Result<[Option<String>; 2]> {
    let mut values: [Option<String>; 2] = [None, None];
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=')
            && let Some(i) = ["debug_app", "wait_for_debugger"]
                .iter()
                .position(|k| *k == key)
        {
            ensure!(values[i].is_none(), "duplicate global debug setting");
            values[i] = Some(value.to_owned());
        }
    }
    Ok(values)
}
/// Repeated `am start -D` before ART attaches can nest the same transient app as
/// its own original, leaking wait-for-debugger into subsequent normal Run. Reset
/// only this stopped target's transient state with no persistent app selected.
/// Preserve the presence and exact values of both developer-option settings.
pub(crate) async fn clear_target_debug_state(
    adb: &AdbClient,
    serial: &str,
    package: &str,
    require_free: bool,
) -> Result<bool> {
    let dump = adb
        .shell(serial, &["dumpsys", "activity", "processes"])
        .await?
        .check("Android debug-app state")?;
    let Some(state) = debug_app_state(&dump.stdout)? else {
        return Ok(false);
    };
    if state.app == "null" {
        return Ok(false);
    }
    if state.app != package || (state.original != "null" && state.original != package) {
        ensure!(
            !require_free,
            "Another debug-app selection is active; finish it before Debug"
        );
        return Ok(false);
    }
    if !state.transient {
        return Ok(false);
    }
    let globals = adb
        .shell(serial, &["settings", "list", "global"])
        .await?
        .check("persistent debug settings")?;
    let before = global_debug_settings(&globals.stdout)?;
    if before[0].is_some() {
        return Ok(false);
    } // Keep an explicit developer selection.
    adb.shell(serial, &["am", "clear-debug-app"])
        .await?
        .check("reset target transient debug state")?;
    for (key, value) in ["debug_app", "wait_for_debugger"].iter().zip(&before) {
        let command = match value {
            Some(v) => vec!["settings", "put", "global", key, v],
            None => vec!["settings", "delete", "global", key],
        };
        adb.shell(serial, &command)
            .await?
            .check("restore persistent debug setting")?;
    }
    let after = adb
        .shell(serial, &["settings", "list", "global"])
        .await?
        .check("verify persistent debug settings")?;
    ensure!(
        global_debug_settings(&after.stdout)? == before,
        "persistent debug settings changed during cleanup"
    );
    Ok(true)
}

/// The system fork/JDWP registration precedes Android's argv/process-name setup.
/// Retain the completed handshake while exact-package pidof becomes ready; an
/// empty first reply is not evidence that the correctly claimed fork disappeared.
pub(crate) async fn verify_claimed_pid(
    adb: &AdbClient,
    serial: &str,
    package: &str,
    pid: u32,
    budget: Duration,
) -> Result<()> {
    tokio::time::timeout(budget, async {
        loop {
            let reply = adb.shell(serial, &["pidof", package]).await?;
            ensure!(
                matches!(reply.exit_code, 0 | 1),
                "claimed target pidof failed"
            );
            let pids = reply.stdout.split_whitespace().collect::<Vec<_>>();
            if !pids.is_empty() {
                ensure!(
                    reply.exit_code == 0
                        && pids.len() == 1
                        && pids[0].parse::<u32>().ok() == Some(pid),
                    "target PID changed while claiming JDWP"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("claimed JDWP PID never acquired exact package name")?
}

/// A force-stop shell acknowledgment is not proof that the old PID disappeared.
/// Gate every cold/retry launch so a claim cannot bind to the previous generation.
pub(crate) async fn wait_target_stopped(
    adb: &AdbClient,
    serial: &str,
    package: &str,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let reply = adb.shell(serial, &["pidof", package]).await?;
            ensure!(
                matches!(reply.exit_code, 0 | 1),
                "target pidof failed: {}{}",
                reply.stdout,
                reply.stderr
            );
            if reply.stdout.trim().is_empty() {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("previous target PID did not stop (check Android users)")?
}

pub async fn deploy_project(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Deployment> {
    deploy_mode(root, adb, options, event, false, None, None).await
}
pub async fn deploy_debug(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Deployment> {
    deploy_mode(root, adb, options, event, true, None, None).await
}
#[derive(Debug)]
pub struct SourceGuardChanged(String);
impl std::fmt::Display for SourceGuardChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SourceGuardChanged {}
pub fn verify_source_guard(
    root: &Path,
    expected: &std::collections::BTreeMap<PathBuf, String>,
) -> Result<()> {
    let result = (|| -> Result<()> {
        for (path, hash) in expected {
            ensure!(
                path.starts_with(root) && path.canonicalize()? == *path,
                "Breakpoint source path changed"
            );
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            use std::io::Read;
            let file = options.open(path)?;
            ensure!(
                file.metadata()?.is_file(),
                "Breakpoint source is not regular"
            );
            let mut bytes = vec![];
            file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 8 * 1024 * 1024 && blake3::hash(&bytes).to_hex().as_str() == hash,
                "Breakpoint source changed · review and Debug again"
            );
        }
        Ok(())
    })();
    result.map_err(|e| SourceGuardChanged(e.to_string()).into())
}
pub async fn deploy_debug_checked(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    sources: &std::collections::BTreeMap<PathBuf, String>,
    mutation_started: &std::sync::atomic::AtomicBool,
) -> Result<Deployment> {
    deploy_mode(
        root,
        adb,
        options,
        event,
        true,
        Some(sources),
        Some(mutation_started),
    )
    .await
}
async fn deploy_mode(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    debug: bool,
    source_guard: Option<&std::collections::BTreeMap<PathBuf, String>>,
    mutation_started: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Deployment> {
    ensure!(
        !debug || options.variant == "debug",
        "JDWP requires debug variant; release is non-debuggable"
    );
    let start = std::time::Instant::now();
    let root = root.canonicalize()?;
    if let Some(guard) = source_guard {
        verify_source_guard(&root, guard)?;
    }
    require_device(adb, &options.serial).await?;
    let device = device_info(adb, &options.serial).await?;
    let native_build = crate::build::build(
        &root,
        &options.module,
        &options.variant,
        options.offline,
        event,
    )
    .await?;
    let apk = native_build.apk.clone();
    if let Some(guard) = source_guard {
        verify_source_guard(&root, guard)?;
    }
    ensure!(
        adb.devices()
            .await?
            .iter()
            .any(|d| d.serial == options.serial && d.usable()),
        "Selected device disconnected during build"
    );
    // Stop BEFORE replacement, otherwise Android may asynchronously relaunch the
    // old task's base intent after install and race our fresh launch/extras/PID.
    event(RunEvent::Phase(
        "Stopping previous application before replacement".into(),
    ));
    if let Some(started) = mutation_started {
        started.store(true, std::sync::atomic::Ordering::Release);
    }
    let stop_before_install = adb
        .shell(
            &options.serial,
            &["am", "force-stop", "--user", "current", &apk.application_id],
        )
        .await?
        .check("stop previous application")?;
    wait_target_stopped(adb, &options.serial, &apk.application_id).await?;
    clear_target_debug_state(adb, &options.serial, &apk.application_id, debug).await?;
    event(RunEvent::Phase("Installing APK over direct ADB".into()));
    let install = adb.install_apk(&options.serial, &apk.path).await?;
    let resolved = adb
        .shell(
            &options.serial,
            &[
                "cmd",
                "package",
                "resolve-activity",
                "--brief",
                "--user",
                "current",
                "-a",
                "android.intent.action.MAIN",
                "-c",
                "android.intent.category.LAUNCHER",
                &apk.application_id,
            ],
        )
        .await?
        .check("resolve launcher Activity")?;
    let component = launcher_component(&resolved.stdout, &apk.application_id)?;
    // A service OKAY only acknowledges transport, not logcat readiness. Replay from
    // the DEVICE clock before launch so fast onCreate/onResume cannot race startup.
    let log_since_seconds: u64 = adb
        .shell(&options.serial, &["date", "+%s"])
        .await?
        .check("device clock")?
        .stdout
        .trim()
        .parse()
        .context("invalid device clock")?;
    let logs = adb.logcat_since(&options.serial, log_since_seconds).await?;
    let run_id = format!(
        "wex-{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    event(RunEvent::Phase(format!("Launching {component}")));
    let mut launch_args = vec!["am", "start"];
    if debug {
        launch_args.push("-D");
    } else {
        launch_args.push("-W");
    }
    launch_args.extend([
        "-S",
        "--user",
        "current",
        "-n",
        &component,
        "--es",
        "wex.run_id",
        &run_id,
    ]);
    let (launch, pid, jdwp, debug_transport) = if debug {
        let mut acquired = None;
        let mut last_error = String::new();
        for attempt in 1..=3 {
            if attempt > 1 {
                event(RunEvent::Phase(format!(
                    "Retrying target Debug launch · {attempt}/3 · {}",
                    options.serial
                )));
                adb.shell(
                    &options.serial,
                    &["am", "force-stop", "--user", "current", &apk.application_id],
                )
                .await?
                .check("stop target before JDWP retry")?;
                wait_target_stopped(adb, &options.serial, &apk.application_id).await?;
                clear_target_debug_state(adb, &options.serial, &apk.application_id, true).await?;
            }
            let watcher = match crate::debugger::LaunchWatcher::prepare(
                adb,
                &options.serial,
                log_since_seconds,
            )
            .await
            {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    event(RunEvent::Phase(format!(
                        "Startup watcher unavailable; exact-package PID claim: {error:#}"
                    )));
                    None
                }
            };
            let claim_source = if watcher.is_some() {
                "system-server-fork"
            } else {
                "exact-package-pidof"
            };
            // Start exact-package PID/handshake collection concurrently with am start.
            // Waiting for shell completion and JVM/JDI startup lets DDM monitors take
            // ART's single debugger connection first.
            let claimed_before = std::sync::atomic::AtomicBool::new(false);
            let launch_done = std::sync::atomic::AtomicBool::new(false);
            let launch_future = async {
                let reply = adb
                    .shell(&options.serial, &launch_args)
                    .await?
                    .check("launch debug Activity")?;
                launch_done.store(true, std::sync::atomic::Ordering::Release);
                ensure!(
                    reply.stdout.contains("Starting: Intent") && !reply.stdout.contains("Error:"),
                    "Debug Activity launch failed: {}{}",
                    reply.stdout,
                    reply.stderr
                );
                Ok::<_, anyhow::Error>(reply)
            };
            let claim_future = async {
                let connection = match watcher {
                    Some(watcher) => watcher.claim(&apk.application_id).await?,
                    None => {
                        crate::debugger::claim_on_launch(
                            adb,
                            &options.serial,
                            &apk.application_id,
                            Duration::from_secs(4),
                        )
                        .await?
                    }
                };
                claimed_before.store(
                    !launch_done.load(std::sync::atomic::Ordering::Acquire),
                    std::sync::atomic::Ordering::Release,
                );
                Ok::<_, anyhow::Error>(connection)
            };
            match tokio::try_join!(launch_future, claim_future) {
                Ok((reply, (pid, stream))) => {
                    if let Err(error) = verify_claimed_pid(
                        adb,
                        &options.serial,
                        &apk.application_id,
                        pid,
                        Duration::from_secs(2),
                    )
                    .await
                    {
                        last_error = format!("{error:#}");
                        drop(stream);
                        continue;
                    }
                    acquired = Some((
                        reply,
                        pid,
                        Some(stream),
                        Some(DebugTransport {
                            kind: "direct-adb-preclaimed-jdwp".into(),
                            pid,
                            launch_attempt: attempt,
                            claim_source: claim_source.into(),
                            claimed_before_launch_return: claimed_before
                                .load(std::sync::atomic::Ordering::Acquire),
                        }),
                    ));
                    break;
                }
                Err(error) => last_error = format!("{error:#}"),
            }
        }
        acquired.with_context(||format!("Debug launch could not claim exact target JDWP; another DDM/debugger client may own it: {last_error}"))?
    } else {
        let launch = adb
            .shell(&options.serial, &launch_args)
            .await?
            .check("launch Activity")?;
        ensure!(
            !launch.stdout.contains("Error:")
                && launch.stdout.lines().any(|l| l.trim() == "Status: ok"),
            "Activity launch was not successful: {}{}",
            launch.stdout,
            launch.stderr
        );
        let mut pid = None;
        for _ in 0..20 {
            let p = adb
                .shell(&options.serial, &["pidof", &apk.application_id])
                .await?;
            if p.exit_code == 0 {
                let pids: Vec<_> = p.stdout.split_whitespace().collect();
                ensure!(
                    pids.len() <= 1,
                    "Ambiguous application PID (multiple Android users): {}",
                    p.stdout
                );
                pid = pids
                    .first()
                    .and_then(|p| p.parse::<u32>().ok())
                    .filter(|p| *p > 0);
            }
            if pid.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let pid = pid.context("Activity launched but application process is not running")?;
        (launch, pid, None, None)
    };
    event(RunEvent::Phase(format!(
        "Running {} · PID {pid} · Logcat",
        apk.application_id
    )));
    Ok(Deployment {
        report: RunReport {
            run_id,
            serial: options.serial.clone(),
            device,
            module: options.module.clone(),
            variant: options.variant.clone(),
            build_task: format!("native:{}:{}", options.module, options.variant),
            build_backend: "wex-native-v1".into(),
            native_build,
            apk,
            install,
            stop_before_install,
            component,
            launch,
            pid,
            log_since_seconds,
            elapsed_ms: start.elapsed().as_millis(),
            debug_transport,
        },
        logs,
        jdwp,
    })
}
pub async fn observe_logs(
    deployment: &mut Deployment,
    duration: Duration,
    marker: Option<&str>,
) -> Result<LogObservation> {
    let deadline = tokio::time::Instant::now() + duration;
    let mut decoder = Decoder::default();
    let mut buffer = [0; 16384];
    let mut entries = std::collections::VecDeque::new();
    let mut received = 0;
    let mut marker_found = false;
    loop {
        let n = tokio::select! {_ = tokio::time::sleep_until(deadline)=>break,n=deployment.logs.read(&mut buffer)=>n?};
        ensure!(n > 0, "device Logcat stream disconnected");
        for entry in decoder.feed(&buffer[..n])? {
            if entry.pid != deployment.report.pid {
                continue;
            }
            received += 1;
            if marker.is_some_and(|m| {
                entry.message.contains(m) && entry.message.contains(&deployment.report.run_id)
            }) {
                marker_found = true;
            }
            if entries.len() == 2000 {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }
    Ok(LogObservation {
        received,
        entries: entries.into(),
        marker_found,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_app_state_is_strict_and_does_not_guess_other_selections() {
        assert_eq!(debug_app_state("unrelated process listing").unwrap(), None);
        let s=debug_app_state("  mDebugApp=dev.wex.test/orig=dev.wex.test mDebugTransient=true mOrigWaitForDebugger=true\n").unwrap().unwrap();
        assert!(s.transient);
        assert_eq!(s.app, "dev.wex.test");
        assert_eq!(s.original, "dev.wex.test");
        for text in [
            "mDebugApp=bad-package/orig=null mDebugTransient=true",
            "mDebugApp=dev.wex.test/orig=null mDebugTransient=maybe",
            "mDebugApp=dev.wex.test/orig=null",
            "mDebugApp=dev.wex.test/orig=null mDebugTransient=true\nmDebugApp=other.app/orig=null mDebugTransient=true",
        ] {
            assert!(debug_app_state(text).is_err(), "{text}");
        }
    }
    #[test]
    fn persistent_debug_settings_preserve_absence_empty_and_explicit_wait_value() {
        assert_eq!(
            global_debug_settings("unrelated=value").unwrap(),
            [None, None]
        );
        assert_eq!(
            global_debug_settings("debug_app=\nwait_for_debugger=0\n").unwrap(),
            [Some("".into()), Some("0".into())]
        );
        assert_eq!(
            global_debug_settings("debug_app=other.app\nwait_for_debugger=1\n").unwrap(),
            [Some("other.app".into()), Some("1".into())]
        );
        assert!(global_debug_settings("debug_app=a.b\ndebug_app=c.d").is_err());
    }
    #[test]
    fn launcher_validation() {
        assert_eq!(
            launcher_component(
                "priority=0\ndev.wex.android.integration/dev.wex.android.MainActivity\n",
                "dev.wex.android.integration"
            )
            .unwrap(),
            "dev.wex.android.integration/dev.wex.android.MainActivity"
        );
        assert!(launcher_component("No activity found", "dev.wex.android.integration").is_err());
        assert!(launcher_component("p/Cls;reboot", "p").is_err());
    }
    #[test]
    fn metadata_redirect_and_package_validation() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        std::fs::write(dir.join("app.apk"), b"apk fixture").unwrap();
        std::fs::write(
            dir.join("redirect.txt"),
            "#- File Locator -\nlistingFile=output-metadata.json\n",
        )
        .unwrap();
        std::fs::write(dir.join("output-metadata.json"),r#"{"applicationId":"dev.wex.test","variantName":"debug","elements":[{"outputFile":"app.apk","filters":[]}]}"#).unwrap();
        let mut m = Module {
            apk_listing_file: dir.join("redirect.txt"),
            application_id: "dev.wex.test".into(),
            selected_variant: "debug".into(),
            ..Default::default()
        };
        assert_eq!(built_apk(&m).unwrap().path, dir.join("app.apk"));
        m.selected_variant = "release".into();
        assert!(built_apk(&m).is_err());
    }
    #[test]
    fn split_outputs_are_not_silently_selected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("metadata.json");
        std::fs::write(&p,r#"{"applicationId":"dev.wex.test","variantName":"debug","elements":[{"outputFile":"x.apk","filters":[{"filterType":"ABI"}]},{"outputFile":"y.apk"}]}"#).unwrap();
        let m = Module {
            apk_listing_file: p,
            application_id: "dev.wex.test".into(),
            selected_variant: "debug".into(),
            ..Default::default()
        };
        assert!(
            built_apk(&m)
                .unwrap_err()
                .to_string()
                .contains("single/universal")
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_build_stops_run_pipeline() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("gradlew");
        std::fs::write(
            &p,
            "#!/bin/sh\nprintf 'intentional build failure\\n'\nexit 73\n",
        )
        .unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = build_task(d.path(), ":app:assembleDebug", true, &|_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("73"));
        assert!(
            build_task(d.path(), "--init-script", true, &|_| {})
                .await
                .is_err()
        );
    }
}
