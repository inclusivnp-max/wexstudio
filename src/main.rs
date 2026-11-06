use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, time::Instant};
use tokio::io::AsyncReadExt;
use wexandroidstudio::{
    adb::AdbClient,
    cache::DiskCache,
    editor::Document,
    logcat::{Decoder, LogFilter},
    project,
};
#[derive(Parser)]
#[command(name = "wex", version, about = "Native Android workbench")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:5037", global = true)]
    adb: String,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Doctor {
        #[arg(long)]
        require_support: bool,
    },
    PrepareJavaFormatter {
        #[arg(long)]
        offline: bool,
    },
    PrepareLsp {
        #[arg(long)]
        offline: bool,
    },
    Recent {
        #[arg(long)]
        history: Option<PathBuf>,
        #[arg(long, conflicts_with = "remove")]
        add: Option<PathBuf>,
        #[arg(long)]
        remove: Option<PathBuf>,
    },
    Browse {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        hidden: bool,
    },
    New {
        parent: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        directory: String,
        #[arg(long)]
        package: String,
        #[arg(long, default_value="compose", value_parser=["compose","kotlin","java"])]
        template: String,
        #[arg(long, default_value_t = 26)]
        min_sdk: u32,
    },
    Resolve {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        locked: bool,
    },
    Sync {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        bridge: Option<PathBuf>,
    },
    K2Check {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        server: Option<PathBuf>,
    },
    Analyze {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        force: bool,
    },
    PreparePreview {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        offline: bool,
    },
    PreviewList {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    Preview {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        entry: Option<String>,
        #[arg(long)]
        output: PathBuf,
    },
    DeviceInfo {
        #[arg(long)]
        serial: String,
    },
    Install {
        #[arg(long)]
        serial: String,
        apk: PathBuf,
    },
    Run {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        serial: String,
        #[arg(long, default_value = ":app")]
        module: String,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        offline: bool,
        #[arg(long, default_value_t = 5)]
        log_seconds: u64,
        #[arg(long)]
        require_marker: Option<String>,
    },
    DebugCheck {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        serial: String,
        #[arg(long)]
        output: PathBuf,
    },
    LogcatReplay {
        input: PathBuf,
    },
    Devices,
    Logcat {
        #[arg(long)]
        serial: String,
        #[arg(long)]
        regex: Option<String>,
        #[arg(long)]
        pid: Option<u32>,
        #[arg(long, default_value_t = 2)]
        min_priority: u8,
    },
    Push {
        #[arg(long)]
        serial: String,
        local: PathBuf,
        remote: String,
    },
    ForwardJdwp {
        #[arg(long)]
        serial: String,
        #[arg(long)]
        port: u16,
        #[arg(long)]
        pid: u32,
    },
    Inspect {
        file: PathBuf,
    },
    Project {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    PrepareTools {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        offline: bool,
    },
    Build {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = ":app")]
        module: String,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        offline: bool,
    },
    GradleBuild {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = ":app:assembleDebug")]
        task: String,
    },
    Bench {
        #[arg(long, default_value_t = 10000)]
        lines: usize,
        #[arg(long, default_value_t = 100)]
        edits: usize,
    },
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let adb = AdbClient::new(cli.adb);
    match cli.command {
        Action::Doctor { require_support } => {
            let report = wexandroidstudio::runtime_paths::doctor();
            println!("{}", serde_json::to_string_pretty(&report)?);
            ensure!(
                !require_support || report["support_verified"] == true,
                "Packaged support verification failed"
            );
        }
        Action::PrepareJavaFormatter { offline } => {
            let (_tx, mut rx) = tokio::sync::watch::channel(false);
            let jar =
                wexandroidstudio::java_format::prepare(&std::env::current_dir()?, offline, &mut rx)
                    .await?;
            println!(
                "{}",
                serde_json::json!({"result":"JAVA_FORMATTER_TOOLS_OK","jar":jar,"backend":"google-java-format 1.37.0/AOSP","app_classpath":false})
            );
        }
        Action::PrepareLsp { offline } => {
            let p = wexandroidstudio::runtime_paths::paths();
            wexandroidstudio::runtime_paths::verify_support()?;
            let mut cmd = tokio::process::Command::new("python3");
            cmd.arg(p.support.join("scripts/install-kotlin-lsp.py"))
                .env("WEX_TOOLS_HOME", &p.tools);
            if offline {
                cmd.arg("--offline");
            }
            let output = cmd.output().await?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Action::Recent {
            history,
            add,
            remove,
        } => {
            let store = match history {
                Some(p) => wexandroidstudio::project_history::Store::new(p)?,
                None => Default::default(),
            };
            let records = if let Some(p) = add {
                store.record(&p)?
            } else if let Some(p) = remove {
                store.remove(&p)?
            } else {
                store.load()?
            };
            println!("{}", serde_json::to_string_pretty(&records)?);
        }
        Action::Browse { root, hidden } => println!(
            "{}",
            serde_json::to_string_pretty(&wexandroidstudio::project_history::folders(
                &root, hidden
            )?)?
        ),
        Action::New {
            parent,
            name,
            directory,
            package,
            template,
            min_sdk,
        } => {
            use wexandroidstudio::scaffold::{self, Options, Template};
            let template = match template.as_str() {
                "java" => Template::Java,
                "kotlin" => Template::Kotlin,
                _ => Template::Compose,
            };
            let result = scaffold::create(
                &parent,
                &Options {
                    name,
                    directory,
                    application_id: package,
                    min_sdk,
                    template,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Action::DebugCheck {
            root,
            serial,
            output,
        } => {
            let report = wexandroidstudio::debugger::check(&root, &adb, &serial, &output).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Action::Resolve {
            root,
            cache,
            offline,
            locked,
        } => {
            let cache = cache.unwrap_or_else(wexandroidstudio::native::default_cache);
            let lock = tokio::task::spawn_blocking(move || {
                wexandroidstudio::native::resolve(&root, &cache, offline, locked)
            })
            .await??;
            println!("{}", serde_json::to_string_pretty(&lock)?);
        }
        Action::Sync {
            root,
            variant,
            force,
            offline,
            bridge,
        } => {
            if root.join("wex.json").is_file() {
                let report =
                    wexandroidstudio::analysis::prepare(&root, &variant, offline, force, &|e| {
                        eprintln!("ANALYSIS: {e:?}")
                    })
                    .await?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            let mut options = wexandroidstudio::gradle::SyncOptions {
                variant,
                force,
                offline,
                ..Default::default()
            };
            if let Some(bridge) = bridge {
                options.bridge = bridge;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &wexandroidstudio::gradle::sync_project(&root, &options).await?
                )?
            );
        }
        Action::K2Check {
            root,
            file,
            server,
            variant,
        } => {
            let root = root.canonicalize()?;
            let file = match file {
                Some(path) => {
                    if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    }
                }
                None => {
                    let sources = wexandroidstudio::project::source_files(&root)
                        .into_iter()
                        .filter(|p| p.file_name().is_some_and(|n| n == "MainActivity.kt"))
                        .collect::<Vec<_>>();
                    ensure!(
                        sources.len() == 1,
                        "k2-check requires one MainActivity.kt or an explicit --file"
                    );
                    sources[0].clone()
                }
            }
            .canonicalize()?;
            ensure!(
                file.starts_with(&root),
                "K2 source must belong to the selected project"
            );
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &wexandroidstudio::kotlin::check_android(
                        &root,
                        &file,
                        &server.unwrap_or_else(wexandroidstudio::kotlin::default_server),
                        &variant,
                    )
                    .await?
                )?
            );
        }
        Action::Analyze {
            root,
            variant,
            offline,
            force,
        } => {
            let report =
                wexandroidstudio::analysis::prepare(&root, &variant, offline, force, &|e| {
                    eprintln!("ANALYSIS: {e:?}")
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Action::PreparePreview { root, offline } => {
            wexandroidstudio::preview::prepare(&root.canonicalize()?, offline).await?;
            println!("PREVIEW_TOOLS_OK");
        }
        Action::PreviewList { root } => {
            let report = wexandroidstudio::preview::list(&root.canonicalize()?, vec![], &|e| {
                eprintln!("PREVIEW: {e:?}")
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Action::Preview {
            root,
            entry,
            output,
        } => {
            let root = root.canonicalize()?;
            let output = if output.is_absolute() {
                output
            } else {
                std::env::current_dir()?.join(output)
            };
            let report = wexandroidstudio::preview::render_selected(
                &root,
                wexandroidstudio::preview::Selection {
                    id: entry,
                    dark: None,
                },
                vec![],
                &output,
                &|e| eprintln!("PREVIEW: {e:?}"),
                &|_| {},
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Action::DeviceInfo { serial } => println!(
            "{}",
            serde_json::to_string_pretty(
                &wexandroidstudio::run::device_info(&adb, &serial).await?
            )?
        ),
        Action::Install { serial, apk } => println!(
            "{}",
            serde_json::to_string_pretty(&adb.install_apk(&serial, &apk).await?)?
        ),
        Action::Run {
            root,
            serial,
            module,
            variant,
            offline,
            log_seconds,
            require_marker,
        } => {
            ensure!(
                (1..=300).contains(&log_seconds),
                "log observation must be 1..300 seconds"
            );
            let options = wexandroidstudio::run::RunOptions {
                serial,
                module,
                variant,
                offline,
            };
            let callback = |event| match event {
                wexandroidstudio::run::RunEvent::Phase(s) => eprintln!("RUN: {s}"),
                wexandroidstudio::run::RunEvent::BuildLine(s) => eprintln!("{s}"),
            };
            let mut deployment =
                wexandroidstudio::run::deploy_project(&root, &adb, &options, &callback).await?;
            let logs = wexandroidstudio::run::observe_logs(
                &mut deployment,
                std::time::Duration::from_secs(log_seconds),
                require_marker.as_deref(),
            )
            .await?;
            let observed = require_marker.is_none() || logs.marker_found;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "result": if observed { "RUN_OK" } else { "LOGCAT_MARKER_MISSING" },
                    "deployment": deployment.report, "logcat": logs,
                }))?
            );
            ensure!(
                observed,
                "fresh run marker not observed in PID-bound Logcat"
            );
        }
        Action::Devices => println!("{}", serde_json::to_string_pretty(&adb.devices().await?)?),
        Action::LogcatReplay { input } => println!(
            "{}",
            serde_json::to_string_pretty(&wexandroidstudio::logcat::replay_file(&input)?)?
        ),
        Action::Logcat {
            serial,
            regex,
            pid,
            min_priority,
        } => {
            let filter = LogFilter {
                minimum_priority: min_priority,
                pid,
                expression: regex.map(|r| regex::Regex::new(&r)).transpose()?,
            };
            let mut stream = adb.logcat(&serial).await?;
            let mut decoder = Decoder::default();
            let mut bytes = [0; 16384];
            loop {
                tokio::select! {_ = tokio::signal::ctrl_c()=>break, n=stream.read(&mut bytes)=>{let n=n?;if n==0{decoder.finish()?;break;}for entry in decoder.feed(&bytes[..n])?{if filter.matches(&entry){println!("{}",serde_json::to_string(&entry)?);}}}}
            }
        }
        Action::Push {
            serial,
            local,
            remote,
        } => println!("pushed {} bytes", adb.push(&serial, &local, &remote).await?),
        Action::ForwardJdwp { serial, port, pid } => {
            adb.forward_jdwp(&serial, port, pid).await?;
            println!("127.0.0.1:{port} -> {serial} jdwp:{pid}");
        }
        Action::Inspect { file } => {
            let doc = Document::open(file)?;
            println!(
                "{}",
                serde_json::json!({"lines":doc.rope().len_lines(),"bytes":doc.rope().len_bytes(),"syntax_errors":doc.tree().context("Kotlin syntax tree is pending")?.root_node().has_error(),"tree":doc.tree().context("Kotlin syntax tree is pending")?.root_node().to_sexp()})
            );
        }
        Action::Project { root } => {
            let root = root.canonicalize()?;
            let cache = DiskCache::open(&root.join(".wex/index"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"root":root,"fingerprint":project::fingerprint(&root)?,"cached_gradle_graph":project::cached_graph(&cache,&root)?,"files":project::source_files(&root)})
                )?
            );
        }
        Action::PrepareTools { root, offline } => {
            let files = wexandroidstudio::build::prepare_tools(&root, offline).await?;
            println!(
                "{}",
                serde_json::json!({"result":"KOTLIN_TOOLS_OK","classpath":files})
            );
        }
        Action::Build {
            root,
            module,
            variant,
            offline,
        } => {
            let report =
                wexandroidstudio::build::build(&root, &module, &variant, offline, &|e| match e {
                    wexandroidstudio::run::RunEvent::Phase(s) => eprintln!("BUILD: {s}"),
                    wexandroidstudio::run::RunEvent::BuildLine(s) => eprintln!("{s}"),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Action::GradleBuild { root, task } => {
            ensure!(
                !task.is_empty()
                    && !task.starts_with('-')
                    && task
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ":_-".contains(c)),
                "invalid Gradle task"
            );
            let root = root.canonicalize()?;
            let wrapper = root.join("gradlew");
            ensure!(wrapper.is_file(), "project has no Gradle wrapper");
            wexandroidstudio::gradle::cached_distribution(&root)?;
            let status = tokio::process::Command::new(&wrapper)
                .arg(task)
                .arg("--console=plain")
                .current_dir(&root)
                .kill_on_drop(true)
                .status()
                .await
                .context("starting Gradle wrapper")?;
            ensure!(status.success(), "Gradle exited with {status}");
        }
        Action::Bench { lines, edits } => {
            ensure!(
                (1..=1000000).contains(&lines) && edits <= 10000,
                "benchmark size out of range"
            );
            let input = "val value = 1\n".repeat(lines);
            let start = Instant::now();
            let mut d = Document::new(&input)?;
            let parse_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            for i in 0..edits {
                d.edit(12..13, if i % 2 == 0 { "2" } else { "1" })?;
            }
            println!(
                "{}",
                serde_json::json!({"lines":lines,"edits":edits,"parse_ms":parse_ms,"edit_total_ms":start.elapsed().as_secs_f64()*1000.,"syntax_errors":d.tree().context("Kotlin syntax tree is pending")?.root_node().has_error()})
            );
        }
    }
    Ok(())
}
