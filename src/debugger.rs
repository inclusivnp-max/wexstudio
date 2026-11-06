//! Native build/deploy, direct ADB JDWP tunnel, unmodified Java Debug Server DAP.
use crate::{
    adb::AdbClient,
    dap::Client,
    run::{self, Deployment, RunEvent, RunOptions},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, task::JoinHandle};
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Breakpoint {
    pub id: Option<u64>,
    pub line: u32,
    pub verified: bool,
    pub message: String,
}
pub type Breakpoints = BTreeMap<PathBuf, BTreeSet<u32>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopTarget {
    pub serial: String,
    pub package: String,
    pub pid: u32,
    pub run_id: String,
}

pub struct Connection {
    pub client: Arc<Client>,
    pub events: mpsc::Receiver<Value>,
    tunnel: JoinHandle<()>,
    _sources: tempfile::NamedTempFile,
    pub serial: String,
    pub package: String,
}
impl Connection {
    pub fn stop_target(&self, pid: u32, run_id: impl Into<String>) -> StopTarget {
        StopTarget {
            serial: self.serial.clone(),
            package: self.package.clone(),
            pid,
            run_id: run_id.into(),
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.tunnel.abort();
    }
}
pub struct Started {
    pub deployment: Deployment,
    pub connection: Connection,
    pub breakpoints: BTreeMap<PathBuf, Vec<Breakpoint>>,
    pub source_hashes: BTreeMap<PathBuf, String>,
}
pub async fn set_breakpoints(
    client: &Client,
    path: &Path,
    lines: &BTreeSet<u32>,
) -> Result<Vec<Breakpoint>> {
    ensure!(
        lines.len() <= 2000 && lines.iter().all(|l| *l > 0),
        "invalid breakpoint lines"
    );
    let b=client.request("setBreakpoints",json!({"source":{"path":path,"name":path.file_name().unwrap_or_default().to_string_lossy()},"breakpoints":lines.iter().map(|l|json!({"line":l})).collect::<Vec<_>>(),"sourceModified":false})).await?;
    parse_breakpoints(&b, lines)
}
fn parse_breakpoints(body: &Value, requested: &BTreeSet<u32>) -> Result<Vec<Breakpoint>> {
    let values = body["breakpoints"]
        .as_array()
        .context("missing DAP breakpoints")?;
    ensure!(
        values.len() == requested.len(),
        "adapter returned incomplete breakpoint replacement"
    );
    values
        .iter()
        .zip(requested)
        .map(|(value, requested_line)| {
            let verified = value["verified"]
                .as_bool()
                .context("invalid breakpoint verification")?;
            let line = match value.get("line") {
                Some(n) => u32::try_from(n.as_u64().context("invalid breakpoint line")?)?,
                None if !verified => *requested_line,
                None => anyhow::bail!("verified breakpoint omitted source line"),
            };
            ensure!(line > 0, "invalid breakpoint line zero");
            let id = value
                .get("id")
                .map(|v| v.as_u64().context("invalid breakpoint ID"))
                .transpose()?;
            let message = value
                .get("message")
                .map(|v| v.as_str().context("invalid breakpoint message"))
                .transpose()?
                .unwrap_or("")
                .to_owned();
            Ok(Breakpoint {
                id,
                line,
                verified,
                message,
            })
        })
        .collect()
}

pub async fn launch(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    breakpoints: &Breakpoints,
    event: &(dyn Fn(RunEvent) + Send + Sync),
) -> Result<Started> {
    launch_mode(root, adb, options, breakpoints, event, None).await
}
pub async fn launch_checked(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    breakpoints: &Breakpoints,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    sources: &BTreeMap<PathBuf, String>,
) -> Result<Started> {
    run::verify_source_guard(root, sources)?;
    launch_mode(root, adb, options, breakpoints, event, Some(sources)).await
}
async fn launch_mode(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    breakpoints: &Breakpoints,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    sources: Option<&BTreeMap<PathBuf, String>>,
) -> Result<Started> {
    run::require_device(adb, &options.serial).await?;
    let mutation_started = std::sync::atomic::AtomicBool::new(false);
    let result = launch_inner(
        root,
        adb,
        options,
        breakpoints,
        event,
        sources,
        &mutation_started,
    )
    .await;
    if result.is_err()
        && (sources.is_none() || mutation_started.load(std::sync::atomic::Ordering::Acquire))
        && let Ok(c) = crate::native::Config::load(root)
    {
        let _ = adb
            .shell(
                &options.serial,
                &[
                    "am",
                    "force-stop",
                    "--user",
                    "current",
                    &c.android.application_id,
                ],
            )
            .await;
        let _ =
            run::clear_target_debug_state(adb, &options.serial, &c.android.application_id, false)
                .await;
    }
    result
}
async fn launch_inner(
    root: &Path,
    adb: &AdbClient,
    options: &RunOptions,
    breakpoints: &Breakpoints,
    event: &(dyn Fn(RunEvent) + Send + Sync),
    sources: Option<&BTreeMap<PathBuf, String>>,
    mutation_started: &std::sync::atomic::AtomicBool,
) -> Result<Started> {
    let repo = &crate::runtime_paths::paths().support;
    let tools = &crate::runtime_paths::paths().tools;
    crate::runtime_paths::verify_support()?;
    event(RunEvent::Phase(
        "Preparing pinned Java Debug Server (no Gradle)".into(),
    ));
    let output = Command::new("python3")
        .arg(repo.join("scripts/prepare-debugger.py"))
        .env("WEX_TOOLS_HOME", tools)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "debugger tools: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source_hashes = source_hashes(root)?;
    let mut deployment = if let Some(sources) = sources {
        run::deploy_debug_checked(root, adb, options, event, sources, mutation_started).await?
    } else {
        run::deploy_debug(root, adb, options, event).await?
    };
    let claimed = deployment
        .jdwp
        .take()
        .context("Debug deployment did not retain its JDWP claim")?;
    ensure!(
        source_hashes == self::source_hashes(root)?,
        "Sources changed while launching Debug; restart with saved sources"
    );
    let serial = options.serial.clone();
    let pid = deployment.report.pid;
    event(RunEvent::Phase(format!(
        "Attaching debugger · {serial} · PID {pid}"
    )));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let folder = root.join(".wex/debugger");
    std::fs::create_dir_all(&folder)?;
    let mut sources = tempfile::NamedTempFile::new_in(folder)?;
    use std::io::Write;
    sources.write_all(&serde_json::to_vec(&crate::project::source_files(root))?)?;
    sources.flush()?;
    let java = std::env::var_os("WEX_DEBUG_JAVA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/jvm/java-21-openjdk"))
        .canonicalize()
        .context("Debugger JDK directory missing")?;
    let cp = format!(
        "{}:{}",
        tools.join("debugger/classes").display(),
        tools.join("debugger/lib/*").display()
    );
    let mut command = Command::new(java.join("bin/java"));
    command
        .args(["--add-modules", "jdk.jdi", "-cp", &cp, "WexDebugServer"])
        .arg(sources.path())
        .arg(&deployment.report.native_build.build_dir)
        .current_dir(root);
    let transport = adb.clone();
    let tunnel_serial = serial.clone();
    let (tunnel_tx, mut tunnel_rx) = tokio::sync::oneshot::channel();
    let tunnel = tokio::spawn(async move {
        let result = async {
            let (local, _) = listener.accept().await?;
            relay_jdwp(&transport, &tunnel_serial, pid, local, Some(claimed)).await
        }
        .await
        .map_err(|e: anyhow::Error| format!("{e:#}"));
        if let Err(error) = &result {
            eprintln!("JDWP tunnel: {error}");
        }
        let _ = tunnel_tx.send(result);
    });
    // Every failure after spawning is guarded; no abandoned socket/adapter.
    let (client, events) = match Client::spawn(&mut command) {
        Ok(pair) => pair,
        Err(e) => {
            tunnel.abort();
            return Err(e);
        }
    };
    let connection = Connection {
        client,
        events,
        tunnel,
        _sources: sources,
        serial: serial.clone(),
        package: deployment.report.apk.application_id.clone(),
    };
    connection.client.request("initialize",json!({"clientID":"wex","adapterID":"java","linesStartAt1":true,"columnsStartAt1":true,"pathFormat":"path","supportsVariablePaging":true,"supportsRunInTerminalRequest":false})).await?;
    let attach = connection.client.request("attach",json!({"hostName":"127.0.0.1","port":port,"timeout":12000,"sourcePaths":[root],"stepFilters":{"skipClasses":["java.*","android.*","kotlin.*","androidx.*","com.android.*"],"skipSynthetics":true,"skipStaticInitializers":true,"skipConstructors":true}}));
    tokio::select! {
        biased;
        tunnel_result = &mut tunnel_rx => {
            let cause = match tunnel_result {
                Ok(Err(error)) => error,
                Ok(Ok(())) => "JDWP stream closed during attach".into(),
                Err(error) => error.to_string(),
            };
            anyhow::bail!("Debug attach: {cause}")
        }
        result = attach => { result?; }
    }

    let mut verified = BTreeMap::new();
    for (path, lines) in breakpoints {
        verified.insert(
            path.clone(),
            set_breakpoints(&connection.client, path, lines).await?,
        );
    }
    connection
        .client
        .request("configurationDone", json!({}))
        .await?;
    Ok(Started {
        deployment,
        connection,
        breakpoints: verified,
        source_hashes,
    })
}

pub async fn stopped(events: &mut mpsc::Receiver<Value>) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(25), async {
        while let Some(event) = events.recv().await {
            match event["event"].as_str() {
                Some("stopped") => return Ok(event["body"].clone()),
                Some("terminated") => anyhow::bail!("debug session ended: {event}"),
                _ => {}
            }
        }
        anyhow::bail!("debug event stream closed")
    })
    .await
    .context("breakpoint/step stop timed out")?
}
pub async fn inspect(client: &Client, thread: u64) -> Result<Value> {
    let stack = client
        .request("stackTrace", json!({"threadId":thread}))
        .await?;
    let frame = stack["stackFrames"][0]["id"]
        .as_u64()
        .context("no suspended frame")?;
    let scopes = client.request("scopes", json!({"frameId":frame})).await?;
    let mut variables = Vec::new();
    if let Some(all) = scopes["scopes"].as_array() {
        for scope in all.iter().filter(|s| s["expensive"] != true) {
            let r = scope["variablesReference"]
                .as_u64()
                .context("invalid scope reference")?;
            variables.push(json!({"scope":scope,"body":client.request("variables",json!({"variablesReference":r,"start":0,"count":100})).await?}));
        }
    }
    Ok(json!({"stack":stack,"scopes":scopes,"variables":variables}))
}
pub async fn check(root: &Path, adb: &AdbClient, serial: &str, out: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let source = crate::project::source_files(&root)
        .into_iter()
        .find(|p| p.ends_with("MainActivity.kt"))
        .context("debug check requires native MainActivity fixture")?;
    let original = std::fs::read_to_string(&source)?;
    let line = original
        .lines()
        .position(|l| l.contains("val answer = debugNested(doubled)"))
        .context("debugProbe fixture missing")? as u32
        + 1;
    let mut bp = Breakpoints::new();
    bp.insert(source.clone(), BTreeSet::from([line]));
    let c = crate::native::Config::load(&root)?;
    let options = RunOptions {
        serial: serial.into(),
        module: format!(":{}", c.build.application_module),
        variant: "debug".into(),
        offline: true,
    };
    let mut started = launch(&root, adb, &options, &bp, &|e| {
        eprintln!("DEBUG: {e:?}");
    })
    .await?;
    let result=async {
        let client=started.connection.client.clone();
        let first=stopped(&mut started.connection.events).await?;
        ensure!(first["reason"].as_str().is_some_and(|s|s.contains("breakpoint")),"expected breakpoint stop: {first}");
        let thread=first["threadId"].as_u64().context("missing stopped thread")?;
        let threads=client.request("threads",json!({})).await?;
        let before=inspect(&client,thread).await?;
        fn variable(snapshot:&Value,name:&str,value:&str)->bool{
            snapshot["variables"].as_array().into_iter().flatten().flat_map(|s|s["body"]["variables"].as_array().into_iter().flatten()).any(|v|v["name"]==name&&v["value"]==value)
        }
        ensure!(before["stack"]["stackFrames"][0]["name"].as_str().is_some_and(|s|s.contains("debugProbe")),"wrong breakpoint frame: {before}");
        ensure!(variable(&before,"seed","7")&&variable(&before,"doubled","14"),"locals missing/wrong: {before}");
        let array=before["variables"].as_array().into_iter().flatten().flat_map(|s|s["body"]["variables"].as_array().into_iter().flatten()).find(|v|v["name"]=="samples").context("samples array missing")?;
        let array_reference=array["variablesReference"].as_u64().context("array reference missing")?;
        let mut array_pages=Vec::new();
        for (start,expected) in [(0,100),(100,100),(200,50)] {
            let page=client.request("variables",json!({"variablesReference":array_reference,"filter":"indexed","start":start,"count":expected})).await?;
            ensure!(page["variables"].as_array().is_some_and(|v|v.len()==expected),"array page wrong: {page}");
            array_pages.push(page);
        }
        ensure!(array_pages[2]["variables"][49]["value"]=="747","last array value wrong");
        client.request("stepIn",json!({"threadId":thread})).await?;
        let into_stop=stopped(&mut started.connection.events).await?;let into=inspect(&client,thread).await?;
        ensure!(into["stack"]["stackFrames"][0]["name"].as_str().is_some_and(|s|s.contains("debugNested")),"stepInto wrong frame: {into}");
        client.request("next",json!({"threadId":thread})).await?;
        let next_stop=stopped(&mut started.connection.events).await?;let next=inspect(&client,thread).await?;
        ensure!(variable(&next,"adjusted","17"),"stepOver locals wrong: {next}");
        client.request("stepOut",json!({"threadId":thread})).await?;
        let out_stop=stopped(&mut started.connection.events).await?;let after=inspect(&client,thread).await?;
        ensure!(variable(&after,"->debugNested()","17"),"stepOut return value wrong: {after}");
        client.request("next",json!({"threadId":thread})).await?;
        let assigned_stop=stopped(&mut started.connection.events).await?;
        let assigned=inspect(&client,thread).await?;
        ensure!(variable(&assigned,"answer","17"),"returned local value wrong: {assigned}");
        // Dynamic removal uses the same DAP path as toggling a live gutter breakpoint.
        let removed=set_breakpoints(&client,&source,&BTreeSet::new()).await?;ensure!(removed.is_empty(),"breakpoint removal failed");
        client.request("continue",json!({"threadId":thread})).await?;
        let logs=run::observe_logs(&mut started.deployment,Duration::from_secs(4),Some("WEX_DEBUG_OK")).await?;
        ensure!(logs.marker_found&&logs.entries.iter().any(|e|e.message.contains("WEX_DEBUG_OK result=17")),"debug app did not continue to Logcat marker");
        client.request("pause",json!({"threadId":thread})).await?;
        let pause_stop=stopped(&mut started.connection.events).await?;let paused=inspect(&client,thread).await?;
        client.request("continue",json!({"threadId":thread})).await?;
        client.request("disconnect",json!({"terminateDebuggee":false})).await?;
        ensure!(std::fs::read_to_string(&source)?==original,"debug check changed disk source");
        Ok::<_,anyhow::Error>(json!({"result":"ANDROID_DEBUG_OK","adapter":"Microsoft Java Debug Server 0.53.1","transport":"direct ADB JDWP + loopback JDI tunnel + DAP","run":started.deployment.report,"breakpoint_line":line,"source":source,"threads":threads,"breakpoint":first,"before":before,"array_pages":array_pages,"stepInto":{"stop":into_stop,"snapshot":into},"stepOver":{"stop":next_stop,"snapshot":next},"stepOut":{"stop":out_stop,"snapshot":after,"assigned_stop":assigned_stop,"assigned":assigned},"pause":{"stop":pause_stop,"snapshot":paused},"logcat":logs,"disk_source_unchanged":true}))
    }.await;
    // End only this explicit-serial application, including failure paths.
    let target = StopTarget {
        serial: serial.to_owned(),
        package: c.android.application_id.clone(),
        pid: started.deployment.report.pid,
        run_id: started.deployment.report.run_id.clone(),
    };
    let stop = stop_app(adb, &target).await?;
    let mut report = result?;
    report["stop"] = stop;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    Ok(report)
}

pub async fn stop_app(adb: &AdbClient, target: &StopTarget) -> Result<Value> {
    ensure!(!target.serial.is_empty(), "debug stop target has no serial");
    ensure!(
        !target.package.is_empty(),
        "debug stop target has no package"
    );
    ensure!(target.pid > 0, "debug stop target has no PID");
    let before = adb
        .shell(&target.serial, &["pidof", &target.package])
        .await?
        .check("debug stop target")?;
    let pids = before
        .stdout
        .split_whitespace()
        .filter_map(|p| p.parse::<u32>().ok())
        .collect::<Vec<_>>();
    ensure!(
        pids.contains(&target.pid),
        "debug stop target changed or already disappeared (serial={}, package={}, pid={}, current={:?}, run_id={})",
        target.serial,
        target.package,
        target.pid,
        pids,
        target.run_id
    );
    let stop = adb
        .shell(
            &target.serial,
            &["am", "force-stop", "--user", "current", &target.package],
        )
        .await?
        .check("debug Stop")?;
    for _ in 0..20 {
        let pid = adb
            .shell(&target.serial, &["pidof", &target.package])
            .await?;
        let remaining = pid
            .stdout
            .split_whitespace()
            .filter_map(|p| p.parse::<u32>().ok())
            .collect::<Vec<_>>();
        if !remaining.contains(&target.pid) {
            // A replacement process under the same package is not proof that the
            // original debuggee stopped; leave the result explicit for Retry.
            ensure!(
                remaining.is_empty(),
                "debug stop found a replacement process (serial={}, package={}, original_pid={}, remaining={:?}, run_id={})",
                target.serial,
                target.package,
                target.pid,
                remaining,
                target.run_id
            );
            let reset =
                run::clear_target_debug_state(adb, &target.serial, &target.package, false).await?;
            return Ok(json!({
                "target": target,
                "shell": stop,
                "pid_gone": true,
                "transient_debug_state_reset": reset
            }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "debug application did not stop (serial={}, package={}, pid={}, run_id={})",
        target.serial,
        target.package,
        target.pid,
        target.run_id
    )
}

fn source_hashes(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    crate::project::source_files(root)
        .into_iter()
        .map(|p| {
            let hash = blake3::hash(&std::fs::read(&p)?).to_hex().to_string();
            Ok((p, hash))
        })
        .collect()
}

/// Relay a completed, retained ART handshake to JDI exactly once. The lazy
/// path is retained for transport tests; Android launches pass a preclaimed stream.
async fn relay_jdwp(
    adb: &AdbClient,
    serial: &str,
    pid: u32,
    mut local: tokio::net::TcpStream,
    claimed: Option<tokio::net::TcpStream>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const HANDSHAKE: &[u8; 14] = b"JDWP-Handshake";
    let mut hello = [0u8; 14];
    tokio::time::timeout(Duration::from_secs(10), local.read_exact(&mut hello))
        .await
        .context("JDI handshake timeout")??;
    ensure!(&hello == HANDSHAKE, "invalid JDI handshake");
    let mut remote = match claimed {
        Some(stream) => stream,
        None => connect_art_jdwp(adb, serial, pid, Duration::from_secs(10)).await?,
    };
    local.write_all(HANDSHAKE).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}

/// ActivityManager emits its exact-package fork before ART advertises JDWP to
/// DDM monitors. Drain to a unique marker BEFORE launch; accept only system_server
/// UID/PID messages, never package text supplied by an arbitrary application.
pub(crate) struct LaunchWatcher {
    stream: tokio::net::TcpStream,
    decoder: crate::logcat::Decoder,
    system_pid: u32,
    user: u32,
    jdwp_updates: tokio::net::TcpStream,
    transport: tokio::net::TcpStream,
}
impl LaunchWatcher {
    pub(crate) async fn prepare(adb: &AdbClient, serial: &str, since: u64) -> Result<Self> {
        use tokio::io::AsyncReadExt;
        tokio::time::timeout(Duration::from_secs(3), async {
            let server = adb
                .shell(serial, &["pidof", "system_server"])
                .await?
                .check("system_server PID")?;
            let system_pid = server
                .stdout
                .trim()
                .parse::<u32>()
                .context("ambiguous system_server PID")?;
            ensure!(system_pid > 0, "invalid system_server PID");
            let user = adb
                .shell(serial, &["am", "get-current-user"])
                .await?
                .check("current Android user")?
                .stdout
                .trim()
                .parse::<u32>()
                .context("invalid current Android user")?;
            let stream = adb.logcat_since(serial, since).await?;
            let jdwp_updates = adb.service(serial, "track-jdwp").await?;
            let transport = adb.transport(serial).await?;
            let mut watcher = Self {
                stream,
                decoder: crate::logcat::Decoder::default(),
                system_pid,
                user,
                jdwp_updates,
                transport,
            };
            let marker = format!(
                "wex-claim-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            adb.shell(serial, &["log", "-p", "i", "-t", "WexDebugClaim", &marker])
                .await?
                .check("JDWP startup marker")?;
            let mut bytes = [0; 8192];
            loop {
                let n = watcher.stream.read(&mut bytes).await?;
                ensure!(n > 0, "startup Logcat closed");
                if watcher
                    .decoder
                    .feed(&bytes[..n])?
                    .iter()
                    .any(|e| e.uid == Some(2000) && e.tag == "WexDebugClaim" && e.message == marker)
                {
                    return Ok(watcher);
                }
            }
        })
        .await
        .context("JDWP startup watcher timeout")?
    }
    pub(crate) async fn claim(mut self, package: &str) -> Result<(u32, tokio::net::TcpStream)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::time::timeout(Duration::from_secs(4), async {
            let mut bytes = [0; 8192];
            loop {
                let n = self.stream.read(&mut bytes).await?;
                ensure!(n > 0, "startup Logcat closed");
                for entry in self.decoder.feed(&bytes[..n])? {
                    if let Some(pid) = forked_target(&entry, self.system_pid, self.user, package) {
                        // Wait for this PID on an already-open tracker, then send one
                        // service request on an already-selected transport. No polling
                        // gap or fresh host negotiation; a competing monitor may still win.
                        loop {
                            let frame = crate::adb::read_frame(&mut self.jdwp_updates).await?;
                            let text =
                                std::str::from_utf8(&frame).context("invalid track-jdwp UTF-8")?;
                            let pids = text
                                .split_whitespace()
                                .map(str::parse::<u32>)
                                .collect::<std::result::Result<Vec<_>, _>>()?;
                            if pids.contains(&pid) {
                                break;
                            }
                        }
                        crate::adb::request(&mut self.transport, &format!("jdwp:{pid}")).await?;
                        self.transport.write_all(b"JDWP-Handshake").await?;
                        let mut hello = [0; 14];
                        self.transport
                            .read_exact(&mut hello)
                            .await
                            .context("ART closed prearmed JDWP handshake")?;
                        ensure!(&hello == b"JDWP-Handshake", "invalid ART handshake");
                        return Ok((pid, self.transport));
                    }
                }
            }
        })
        .await
        .context("no exact target ActivityManager fork/JDWP claim")?
    }
}
fn forked_target(
    entry: &crate::logcat::LogEntry,
    system_pid: u32,
    current_user: u32,
    package: &str,
) -> Option<u32> {
    if entry.pid != system_pid
        || entry.uid != Some(1000)
        || entry.tag != "ActivityManager"
        || entry.truncated
    {
        return None;
    }
    let rest = entry.message.strip_prefix("Start proc ")?;
    let (pid, rest) = rest.split_once(':')?;
    let rest = rest.strip_prefix(package)?.strip_prefix('/')?;
    let (user, reason) = rest.split_once(" for ")?;
    let app_id = user.strip_prefix(&format!("u{current_user}a"))?;
    if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) || reason.is_empty() {
        return None;
    }
    pid.parse::<u32>().ok().filter(|p| *p > 0)
}
/// Claim only the requested package's single PID. No scan/open of unrelated
/// processes, forwarded ports or other devices; handshake sends no VM commands.
pub(crate) async fn claim_on_launch(
    adb: &AdbClient,
    serial: &str,
    package: &str,
    budget: Duration,
) -> Result<(u32, tokio::net::TcpStream)> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut last = "target process not ready".to_owned();
    while tokio::time::Instant::now() < deadline {
        let attempt = async {
            let reply = adb.shell(serial, &["pidof", package]).await?;
            ensure!(
                matches!(reply.exit_code, 0 | 1),
                "target pidof failed: {}{}",
                reply.stdout,
                reply.stderr
            );
            let pids = reply.stdout.split_whitespace().collect::<Vec<_>>();
            ensure!(
                pids.len() <= 1,
                "ambiguous target PID (multiple Android users)"
            );
            let pid = pids
                .first()
                .and_then(|p| p.parse::<u32>().ok())
                .filter(|p| *p > 0)
                .context("target PID not ready")?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let stream = connect_art_jdwp_backoff(
                adb,
                serial,
                pid,
                remaining.min(Duration::from_secs(1)),
                Duration::from_millis(5),
            )
            .await?;
            Ok::<_, anyhow::Error>((pid, stream))
        };
        match tokio::time::timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            attempt,
        )
        .await
        {
            Ok(Ok(claimed)) => return Ok(claimed),
            Ok(Err(error)) => {
                last = format!("{error:#}");
                if last.contains("ambiguous target PID") {
                    anyhow::bail!(last);
                }
            }
            Err(_) => break,
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(5)),
        )
        .await;
    }
    anyhow::bail!("JDWP claim {serial} / {package}: {last}")
}
/// Preserve the final ART/ADB error rather than reporting only an adapter attach timeout.
async fn connect_art_jdwp(
    adb: &AdbClient,
    serial: &str,
    pid: u32,
    budget: Duration,
) -> Result<tokio::net::TcpStream> {
    connect_art_jdwp_backoff(adb, serial, pid, budget, Duration::from_millis(100)).await
}
async fn connect_art_jdwp_backoff(
    adb: &AdbClient,
    serial: &str,
    pid: u32,
    budget: Duration,
    retry_delay: Duration,
) -> Result<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    ensure!(pid > 0, "invalid JDWP PID");
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempts = 0;
    let mut last_error = "ART not ready".to_string();
    while tokio::time::Instant::now() < deadline {
        attempts += 1;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let attempt = async {
            let mut stream = adb.service(serial, &format!("jdwp:{pid}")).await?;
            stream.write_all(b"JDWP-Handshake").await?;
            let mut reply = [0u8; 14];
            stream
                .read_exact(&mut reply)
                .await
                .context("ART closed JDWP handshake")?;
            ensure!(&reply == b"JDWP-Handshake", "invalid ART handshake");
            Ok::<_, anyhow::Error>(stream)
        };
        match tokio::time::timeout(remaining.min(Duration::from_secs(2)), attempt).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = format!("{error:#}"),
            Err(_) => last_error = "ART JDWP handshake attempt timed out".into(),
        }
        tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + retry_delay)).await;
    }
    anyhow::bail!("ART JDWP {serial} PID {pid}: {last_error} ({attempts} attempts)")
}

#[cfg(test)]
mod tunnel_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn request(s: &mut tokio::net::TcpStream) -> String {
        let mut n = [0; 4];
        s.read_exact(&mut n).await.unwrap();
        let n = usize::from_str_radix(std::str::from_utf8(&n).unwrap(), 16).unwrap();
        let mut b = vec![0; n];
        s.read_exact(&mut b).await.unwrap();
        String::from_utf8(b).unwrap()
    }
    #[test]
    fn breakpoint_acknowledgements_are_complete_and_bounded() {
        let requested = BTreeSet::from([7, 12]);
        for body in [
            json!({}),
            json!({"breakpoints":[]}),
            json!({"breakpoints":[{"verified":true,"line":7},{"verified":true,"line":4294967296u64}]}),
            json!({"breakpoints":[{"verified":true,"line":7},{"verified":true}]}),
            json!({"breakpoints":[{"verified":true,"line":7},{"verified":true,"line":0}]}),
        ] {
            assert!(parse_breakpoints(&body, &requested).is_err());
        }
        assert!(
            parse_breakpoints(&json!({"breakpoints":[]}), &BTreeSet::new())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn unresolved_breakpoints_retain_requested_line_and_real_verification_status() {
        let values=parse_breakpoints(&json!({"breakpoints":[{"verified":false,"message":"Class not loaded"},{"id":2,"verified":true,"line":13}]}),&BTreeSet::from([7,12])).unwrap();
        assert_eq!(values[0].line, 7);
        assert!(!values[0].verified);
        assert_eq!(values[0].message, "Class not loaded");
        assert_eq!(values[1].line, 13);
        assert!(values[1].verified);
    }
    #[tokio::test]
    async fn stop_app_rejects_replacement_pid_without_force_stop_or_receipt() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response_command(&socket, "shell,v2,raw:'pidof' 'dev.wex.test'", "88\n", 0).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), socket.accept())
                    .await
                    .is_err()
            );
        });
        let target = StopTarget {
            serial: "EXACT".into(),
            package: "dev.wex.test".into(),
            pid: 77,
            run_id: "run-original".into(),
        };
        let error = stop_app(&adb, &target).await.unwrap_err().to_string();
        assert!(
            error.contains("target changed") && error.contains("run-original"),
            "{error}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stop_app_requires_original_pid_before_force_stop() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response_command(&socket, "shell,v2,raw:'pidof' 'dev.wex.test'", "", 0).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), socket.accept())
                    .await
                    .is_err()
            );
        });
        let target = StopTarget {
            serial: "EXACT".into(),
            package: "dev.wex.test".into(),
            pid: 77,
            run_id: "run-gone".into(),
        };
        let error = stop_app(&adb, &target).await.unwrap_err().to_string();
        assert!(
            error.contains("target changed") && error.contains("run-gone"),
            "{error}"
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn jdwp_retries_premature_remote_close_then_relays_real_handshake_and_payload() {
        let adb = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = AdbClient::new(adb.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            for attempt in 0..3 {
                let (mut s, _) = adb.accept().await.unwrap();
                assert_eq!(request(&mut s).await, "host:transport:SERIAL");
                s.write_all(b"OKAY").await.unwrap();
                assert_eq!(request(&mut s).await, "jdwp:77");
                s.write_all(b"OKAY").await.unwrap();
                let mut hello = [0; 14];
                s.read_exact(&mut hello).await.unwrap();
                assert_eq!(&hello, b"JDWP-Handshake");
                if attempt < 2 {
                    continue;
                }
                s.write_all(b"JDWP-Handshake").await.unwrap();
                let mut b = [0; 4];
                s.read_exact(&mut b).await.unwrap();
                assert_eq!(&b, b"ping");
                s.write_all(b"pong").await.unwrap();
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut jdi = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (local, _) = listener.accept().await.unwrap();
        let relay =
            tokio::spawn(async move { relay_jdwp(&client, "SERIAL", 77, local, None).await });
        jdi.write_all(b"JDWP-Handshakeping").await.unwrap();
        let mut b = [0; 18];
        jdi.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"JDWP-Handshakepong");
        drop(jdi);
        server.await.unwrap();
        relay.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn absent_exact_serial_never_prepares_builds_or_stops_a_different_transport() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut stream, _) = socket.accept().await.unwrap();
            assert_eq!(request(&mut stream).await, "host:devices-l");
            let list = b"adb-EXACT-wifi device model:Pixel\nemulator-5580 device\n";
            stream
                .write_all(format!("OKAY{:04x}", list.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(list).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), socket.accept())
                    .await
                    .is_err()
            );
        });
        let root = tempfile::tempdir().unwrap();
        let options = RunOptions {
            serial: "EXACT".into(),
            module: ":app".into(),
            variant: "debug".into(),
            offline: true,
        };
        let events = std::sync::Mutex::new(Vec::new());
        let result = launch(root.path(), &adb, &options, &Breakpoints::new(), &|e| {
            events.lock().unwrap().push(e)
        })
        .await;
        let error = match result {
            Ok(_) => panic!("missing device started"),
            Err(e) => format!("{e:#}"),
        };
        assert!(error.contains("EXACT") && error.contains("state: absent"));
        assert!(events.lock().unwrap().is_empty());
        assert!(!root.path().join(".wex").exists());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn malformed_art_handshake_reports_serial_pid_and_real_cause() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut stream, _) = socket.accept().await.unwrap();
            assert_eq!(request(&mut stream).await, "host:transport:EXACT");
            stream.write_all(b"OKAY").await.unwrap();
            assert_eq!(request(&mut stream).await, "jdwp:77");
            stream.write_all(b"OKAY").await.unwrap();
            let mut bytes = [0; 14];
            stream.read_exact(&mut bytes).await.unwrap();
            stream.write_all(b"NOT-A-JDWP-REQ!").await.unwrap();
        });
        let error = connect_art_jdwp(&adb, "EXACT", 77, Duration::from_millis(80))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("EXACT PID 77")
                && error.contains("invalid ART handshake")
                && error.contains("1 attempts"),
            "{error}"
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn invalid_local_handshake_never_contacts_the_device() {
        let adb = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = AdbClient::new(adb.local_addr().unwrap().to_string());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut jdi = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (local, _) = listener.accept().await.unwrap();
        jdi.write_all(b"NOT-A-JDWP-REQ!").await.unwrap();
        assert!(
            relay_jdwp(&client, "SERIAL", 77, local, None)
                .await
                .unwrap_err()
                .to_string()
                .contains("invalid JDI")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), adb.accept())
                .await
                .is_err()
        );
    }
    async fn pair() -> (tokio::net::TcpStream, tokio::net::TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }
    #[tokio::test]
    async fn preclaimed_stream_relays_queued_vm_event_without_second_handshake_or_adb_connection() {
        let adb = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = AdbClient::new(adb.local_addr().unwrap().to_string());
        let (remote, mut art) = pair().await;
        // ART may queue VMStart after its completed handshake before JDI exists.
        art.write_all(b"VMStart").await.unwrap();
        let (mut jdi, local) = pair().await;
        let relay =
            tokio::spawn(
                async move { relay_jdwp(&client, "EXACT", 77, local, Some(remote)).await },
            );
        jdi.write_all(b"JDWP-Handshakeping").await.unwrap();
        let mut bytes = [0; 21];
        jdi.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"JDWP-HandshakeVMStart");
        let mut ping = [0; 4];
        art.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"ping"); // A second handshake would corrupt this packet.
        art.write_all(b"pong").await.unwrap();
        jdi.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"pong");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), adb.accept())
                .await
                .is_err()
        );
        drop(jdi);
        drop(art);
        relay.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn malformed_jdi_releases_preclaimed_target_without_sending_vm_commands() {
        let client = AdbClient::new("127.0.0.1:1");
        let (remote, mut art) = pair().await;
        let (mut jdi, local) = pair().await;
        jdi.write_all(b"NOT-A-JDWP-REQ!").await.unwrap();
        assert!(
            relay_jdwp(&client, "EXACT", 77, local, Some(remote))
                .await
                .is_err()
        );
        let mut b = [0];
        assert_eq!(art.read(&mut b).await.unwrap(), 0);
    }
    async fn shell_response(listener: &TcpListener, stdout: &str, exit: u8) {
        shell_response_command(
            listener,
            "shell,v2,raw:'pidof' 'dev.wex.test'",
            stdout,
            exit,
        )
        .await;
    }
    async fn shell_response_command(listener: &TcpListener, command: &str, stdout: &str, exit: u8) {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(request(&mut stream).await, "host:transport:EXACT");
        stream.write_all(b"OKAY").await.unwrap();
        assert_eq!(request(&mut stream).await, command);
        stream.write_all(b"OKAY").await.unwrap();
        let mut close = [0; 5];
        stream.read_exact(&mut close).await.unwrap();
        assert_eq!(close, [4, 0, 0, 0, 0]);
        stream.write_all(&[1]).await.unwrap();
        stream
            .write_all(&(stdout.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(stdout.as_bytes()).await.unwrap();
        stream.write_all(&[3, 1, 0, 0, 0, exit]).await.unwrap();
    }
    #[tokio::test]
    async fn launch_claim_waits_for_exact_package_then_retries_only_its_pid() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response(&socket, "", 1).await;
            shell_response(&socket, "77\n", 0).await;
            for attempt in 0..2 {
                let (mut stream, _) = socket.accept().await.unwrap();
                assert_eq!(request(&mut stream).await, "host:transport:EXACT");
                stream.write_all(b"OKAY").await.unwrap();
                assert_eq!(request(&mut stream).await, "jdwp:77");
                stream.write_all(b"OKAY").await.unwrap();
                let mut hello = [0; 14];
                stream.read_exact(&mut hello).await.unwrap();
                assert_eq!(&hello, b"JDWP-Handshake");
                if attempt == 0 {
                    continue;
                }
                stream.write_all(b"JDWP-Handshake").await.unwrap();
                let mut b = [0];
                assert_eq!(stream.read(&mut b).await.unwrap(), 0);
            }
        });
        let (pid, stream) = claim_on_launch(&adb, "EXACT", "dev.wex.test", Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(pid, 77);
        drop(stream);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn ambiguous_launch_pid_is_terminal_without_jdwp_or_other_target_attempts() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response(&socket, "77 78\n", 0).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(30), socket.accept())
                    .await
                    .is_err()
            );
        });
        let error = claim_on_launch(&adb, "EXACT", "dev.wex.test", Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous target PID"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn stop_gate_waits_for_previous_target_pid_to_disappear() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response(&socket, "77\n", 0).await;
            shell_response(&socket, "", 1).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(30), socket.accept())
                    .await
                    .is_err()
            );
        });
        run::wait_target_stopped(&adb, "EXACT", "dev.wex.test")
            .await
            .unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn stop_gate_does_not_treat_failed_pidof_as_process_gone() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response(&socket, "", 2).await;
        });
        assert!(
            run::wait_target_stopped(&adb, "EXACT", "dev.wex.test")
                .await
                .unwrap_err()
                .to_string()
                .contains("pidof failed")
        );
        server.await.unwrap();
    }
    fn fork_entry() -> crate::logcat::LogEntry {
        crate::logcat::LogEntry{seconds:1,nanos:0,pid:778,tid:816,buffer:1,uid:Some(1000),priority:4,tag:"ActivityManager".into(),message:"Start proc 5736:dev.wex.test/u0a218 for next-top-activity {dev.wex.test/.MainActivity}".into(),truncated:false}
    }
    #[test]
    fn fork_claim_requires_system_server_identity_exact_package_and_android_user() {
        let e = fork_entry();
        assert_eq!(forked_target(&e, 778, 0, "dev.wex.test"), Some(5736));
        assert_eq!(forked_target(&e, 778, 1, "dev.wex.test"), None);
        assert_eq!(forked_target(&e, 778, 0, "dev.wex"), None);
        assert_eq!(forked_target(&e, 779, 0, "dev.wex.test"), None);
        for uid in [None, Some(2000), Some(10218)] {
            let mut e = fork_entry();
            e.uid = uid;
            assert_eq!(forked_target(&e, 778, 0, "dev.wex.test"), None);
        }
        let mut e = fork_entry();
        e.truncated = true;
        assert_eq!(forked_target(&e, 778, 0, "dev.wex.test"), None);
        let mut e = fork_entry();
        e.tag = "Fake".into();
        assert_eq!(forked_target(&e, 778, 0, "dev.wex.test"), None);
    }
    #[test]
    fn malformed_or_secondary_process_fork_never_opens_jdwp() {
        for message in [
            "Start proc 0:dev.wex.test/u0a218 for launch",
            "Start proc -1:dev.wex.test/u0a218 for launch",
            "Start proc 77:dev.wex.test:remote/u0a218 for launch",
            "Start proc 77:dev.wex.test/u0a for launch",
            "Start proc 77:dev.wex.test/u0a218 for ",
            "Start proc 77:dev.wex.test/u0a218 extra for launch",
            "Start proc 77:dev.wex.test2/u0a218 for launch",
        ] {
            let mut e = fork_entry();
            e.message = message.into();
            assert_eq!(forked_target(&e, 778, 0, "dev.wex.test"), None, "{message}");
        }
    }
    #[tokio::test]
    async fn fork_watcher_claims_only_selected_process_before_adapter_exists() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let (stream, mut log) = pair().await;
        let (jdwp_updates, mut tracker) = pair().await;
        let transport = adb.transport("EXACT");
        let transport_server = tokio::spawn(async move {
            let (mut stream, _) = socket.accept().await.unwrap();
            assert_eq!(request(&mut stream).await, "host:transport:EXACT");
            stream.write_all(b"OKAY").await.unwrap();
            stream
        });
        let transport = transport.await.unwrap();
        let mut target = transport_server.await.unwrap();
        let watcher = LaunchWatcher {
            stream,
            decoder: crate::logcat::Decoder::default(),
            system_pid: 778,
            user: 0,
            jdwp_updates,
            transport,
        };
        let server = tokio::spawn(async move {
            // Unrelated target/fake UID are ignored before the real system fork.
            for (uid, msg) in [
                (10218, "Start proc 88:dev.wex.test/u0a218 for launch"),
                (1000, "Start proc 89:other.app/u0a219 for launch"),
                (1000, "Start proc 77:dev.wex.test/u0a218 for launch"),
            ] {
                let payload = format!("\x04ActivityManager\0{msg}\0").into_bytes();
                let mut frame = Vec::new();
                frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                frame.extend_from_slice(&28u16.to_le_bytes());
                for value in [778u32, 816, 1, 0, 1, uid] {
                    frame.extend_from_slice(&value.to_le_bytes());
                }
                frame.extend(payload);
                log.write_all(&frame).await.unwrap();
            }
            tracker.write_all(b"000338\n000638\n77\n").await.unwrap();
            let stream = &mut target;
            assert_eq!(request(stream).await, "jdwp:77");
            stream.write_all(b"OKAY").await.unwrap();
            let mut hello = [0; 14];
            stream.read_exact(&mut hello).await.unwrap();
            assert_eq!(&hello, b"JDWP-Handshake");
            stream.write_all(b"JDWP-Handshake").await.unwrap();
            let mut b = [0];
            assert_eq!(stream.read(&mut b).await.unwrap(), 0);
        });
        let (pid, stream) = watcher.claim("dev.wex.test").await.unwrap();
        assert_eq!(pid, 77);
        drop(stream);
        server.await.unwrap();
    }
    fn binary_log(pid: u32, uid: u32, tag: &str, message: &str) -> Vec<u8> {
        let payload = format!("\x04{tag}\0{message}\0").into_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&28u16.to_le_bytes());
        for v in [pid, pid, 1, 0, 1, uid] {
            frame.extend_from_slice(&v.to_le_bytes());
        }
        frame.extend(payload);
        frame
    }
    async fn negotiate(listener: &TcpListener) -> (tokio::net::TcpStream, String) {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        assert_eq!(request(&mut stream).await, "host:transport:EXACT");
        stream.write_all(b"OKAY").await.unwrap();
        let service = request(&mut stream).await;
        stream.write_all(b"OKAY").await.unwrap();
        (stream, service)
    }
    async fn reply_shell(mut stream: tokio::net::TcpStream, stdout: &str) {
        let mut close = [0; 5];
        stream.read_exact(&mut close).await.unwrap();
        assert_eq!(close, [4, 0, 0, 0, 0]);
        let mut bytes = vec![1];
        bytes.extend_from_slice(&(stdout.len() as u32).to_le_bytes());
        bytes.extend_from_slice(stdout.as_bytes());
        bytes.extend_from_slice(&[3, 1, 0, 0, 0, 0]);
        stream.write_all(&bytes).await.unwrap();
    }
    #[tokio::test]
    async fn prepared_watcher_drains_old_forks_and_prearms_only_exact_transport_before_launch() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (stream, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "shell,v2,raw:'pidof' 'system_server'");
            reply_shell(stream, "778\n").await;
            let (stream, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "shell,v2,raw:'am' 'get-current-user'");
            reply_shell(stream, "0\n").await;
            let (mut log, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "exec:logcat -B -b main -b system -b crash -T 1.0");
            let (mut tracker, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "track-jdwp");
            let (mut ready, _) = socket.accept().await.unwrap();
            ready.set_nodelay(true).unwrap();
            assert_eq!(request(&mut ready).await, "host:transport:EXACT");
            ready.write_all(b"OKAY").await.unwrap();
            let (stream, cmd) = negotiate(&socket).await;
            assert!(
                cmd.starts_with("shell,v2,raw:'log' '-p' 'i' '-t' 'WexDebugClaim' 'wex-claim-")
            );
            let marker = cmd.rsplit('\'').nth(1).unwrap().to_owned();
            reply_shell(stream, "").await;
            log.write_all(&binary_log(
                778,
                1000,
                "ActivityManager",
                "Start proc 88:dev.wex.test/u0a218 for old-launch",
            ))
            .await
            .unwrap();
            log.write_all(&binary_log(900, 2000, "WexDebugClaim", &marker))
                .await
                .unwrap();
            // Wait for the caller to launch after prepare returned, not a wall-clock delay.
            let (stream, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "shell,v2,raw:'fixture-launch'");
            reply_shell(stream, "").await;
            log.write_all(&binary_log(
                778,
                1000,
                "ActivityManager",
                "Start proc 77:dev.wex.test/u0a218 for next-top-activity",
            ))
            .await
            .unwrap();
            tracker
                .write_all(b"000638\n88\n000977\n38\n88\n")
                .await
                .unwrap();
            assert_eq!(request(&mut ready).await, "jdwp:77");
            ready.write_all(b"OKAY").await.unwrap();
            let mut hello = [0; 14];
            ready.read_exact(&mut hello).await.unwrap();
            assert_eq!(&hello, b"JDWP-Handshake");
            ready.write_all(&hello).await.unwrap();
            let mut b = [0];
            assert_eq!(ready.read(&mut b).await.unwrap(), 0);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), socket.accept())
                    .await
                    .is_err()
            );
        });
        let watcher = LaunchWatcher::prepare(&adb, "EXACT", 1).await.unwrap();
        adb.shell("EXACT", &["fixture-launch"]).await.unwrap();
        let (pid, stream) = watcher.claim("dev.wex.test").await.unwrap();
        assert_eq!(pid, 77);
        drop(stream);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn transient_target_cleanup_restores_absent_persistent_settings_and_does_not_touch_other_apps()
     {
        for own in [true, false] {
            let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
            let server = tokio::spawn(async move {
                let (stream, cmd) = negotiate(&socket).await;
                assert_eq!(cmd, "shell,v2,raw:'dumpsys' 'activity' 'processes'");
                reply_shell(stream,if own {"mDebugApp=dev.wex.test/orig=dev.wex.test mDebugTransient=true mOrigWaitForDebugger=true\n"}else{"mDebugApp=other.app/orig=null mDebugTransient=true mOrigWaitForDebugger=false\n"}).await;
                if own {
                    for (expected, reply) in [
                        (
                            "shell,v2,raw:'settings' 'list' 'global'",
                            "unrelated=value\n",
                        ),
                        ("shell,v2,raw:'am' 'clear-debug-app'", ""),
                        (
                            "shell,v2,raw:'settings' 'delete' 'global' 'debug_app'",
                            "Deleted 1 rows\n",
                        ),
                        (
                            "shell,v2,raw:'settings' 'delete' 'global' 'wait_for_debugger'",
                            "Deleted 1 rows\n",
                        ),
                        (
                            "shell,v2,raw:'settings' 'list' 'global'",
                            "unrelated=value\n",
                        ),
                    ] {
                        let (stream, cmd) = negotiate(&socket).await;
                        assert_eq!(cmd, expected);
                        reply_shell(stream, reply).await;
                    }
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), socket.accept())
                        .await
                        .is_err()
                );
            });
            let result = run::clear_target_debug_state(&adb, "EXACT", "dev.wex.test", true).await;
            if own {
                assert!(result.unwrap());
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Another debug-app selection")
                );
            }
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn persistent_developer_debug_app_is_not_cleared() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (stream, cmd) = negotiate(&socket).await;
            assert!(cmd.contains("dumpsys"));
            reply_shell(stream,"mDebugApp=dev.wex.test/orig=dev.wex.test mDebugTransient=true mOrigWaitForDebugger=true\n").await;
            let (stream, cmd) = negotiate(&socket).await;
            assert_eq!(cmd, "shell,v2,raw:'settings' 'list' 'global'");
            reply_shell(stream, "debug_app=dev.wex.test\nwait_for_debugger=1\n").await;
            assert!(
                tokio::time::timeout(Duration::from_millis(30), socket.accept())
                    .await
                    .is_err()
            );
        });
        assert!(
            !run::clear_target_debug_state(&adb, "EXACT", "dev.wex.test", true)
                .await
                .unwrap()
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn early_fork_claim_waits_for_package_name_without_reopening_jdwp() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            shell_response(&socket, "", 1).await;
            shell_response(&socket, "77\n", 0).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(30), socket.accept())
                    .await
                    .is_err()
            );
        });
        run::verify_claimed_pid(&adb, "EXACT", "dev.wex.test", 77, Duration::from_secs(1))
            .await
            .unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn claimed_pid_recheck_rejects_changed_or_multiple_users_without_guessing() {
        for output in ["78\n", "77 78\n"] {
            let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
            let server = tokio::spawn(async move {
                shell_response(&socket, output, 0).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), socket.accept())
                        .await
                        .is_err()
                );
            });
            assert!(
                run::verify_claimed_pid(&adb, "EXACT", "dev.wex.test", 77, Duration::from_secs(1))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("PID changed")
            );
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn claimed_pid_recheck_has_a_whole_operation_deadline() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let adb = AdbClient::new(socket.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (_stream, _) = socket.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(80)).await;
        });
        assert!(
            run::verify_claimed_pid(&adb, "EXACT", "dev.wex.test", 77, Duration::from_millis(20))
                .await
                .unwrap_err()
                .to_string()
                .contains("exact package name")
        );
        server.await.unwrap();
    }
}
