//! Actual pinned Java library; separate from the retained raw K2 Java-null audit.
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use wexandroidstudio::{
    editor::Document,
    formatting::{Backend, Capture},
    java_format,
    kotlin::{Position, Range},
};
fn capture(root: &Path, path: &Path, doc: &Document) -> Result<Capture> {
    Capture::read(root, path, doc.rope().to_string(), doc.version)
}
fn document(path: &Path, text: &str) -> Result<Document> {
    let mut d = Document::open(path)?;
    d.edit_batch_parsed(&[(0..d.rope().len_chars(), text.into())])?;
    Ok(d)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires explicit isolated Java formatter tools, JDK21+ and fixture; no Android device"]
async fn genuine_google_java_format_review() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_JAVA_FORMAT_PROJECT")?).canonicalize()?;
    let out = PathBuf::from(std::env::var("WEX_JAVA_FORMAT_OUTPUT")?);
    let path = root.join("app/src/main/java/dev/wex/android/JavaGreeting.java");
    let disk = std::fs::read(&path)?;
    let (tx, rx) = tokio::sync::watch::channel(false);
    let dirty = "package dev.wex.android;\nimport java.util.Map;\nimport java.util.List;\npublic final class JavaGreeting{public static String decorate( String value ){return \"Java:\"+value;}static String literal(){return \"a;{} 😀\";}}\n";
    let doc = document(&path, dirty)?;
    let r = java_format::review(capture(&root, &path, &doc)?, None, rx.clone()).await?;
    ensure!(
        r.backend == Backend::GoogleJava && !r.unchanged && r.after.contains("\n    public static")
    );
    ensure!(
        r.after.contains("import java.util.Map;")
            && r.after.find("java.util.Map") < r.after.find("java.util.List"),
        "formatter changed import policy"
    );
    ensure!(r.after.contains("a;{} 😀"), "literal changed");
    let first = r.after.clone();
    // Track actual library formatting edits, including inserted newline/indentation.
    // Metadata does not identify deleted/replaced code by a guessed nearby line.
    let token_start = dirty[..dirty.find("decorate(").context("tracked method")?]
        .chars()
        .count();
    let mut anchor = doc
        .tracking_snapshot()
        .anchor(token_start..token_start + "decorate".len())?;
    let mut applied = r.apply(&doc)?;
    let wexandroidstudio::text_tracking::Projection::Mapped(span) =
        applied.tracking_snapshot().project(&mut anchor)?
    else {
        anyhow::bail!("actual Java formatting lost surviving method token");
    };
    ensure!(
        applied.rope().slice(span.clone()) == "decorate"
            && applied.rope().char_to_line(span.start) > 2
    );
    ensure!(applied.dirty());
    ensure!(applied.undo()? && *applied.rope() == dirty);
    let wexandroidstudio::text_tracking::Projection::Mapped(span) =
        applied.tracking_snapshot().project(&mut anchor)?
    else {
        anyhow::bail!("Undo lost anchor")
    };
    ensure!(span.start == token_start);
    ensure!(applied.redo()? && *applied.rope() == first);
    let noop = java_format::review(capture(&root, &path, &applied)?, None, rx.clone()).await?;
    ensure!(noop.unchanged && noop.apply(&applied).is_err());
    let selected = "package dev.wex.android;\nclass JavaGreeting {\nstatic int left( int a ){return a+1;}\nstatic int chosen( int a ){return a+1;}\nstatic int right( int a ){return a+1;}\n}\n";
    let start = selected
        .find("a+1;}\nstatic int right")
        .context("selection marker")?;
    let d = document(&path, selected)?;
    let a = d.lsp_position(selected[..start].chars().count())?;
    let b = d.lsp_position(selected[..start + 3].chars().count())?;
    let range = Range {
        start: Position {
            line: a.0,
            character: a.1,
        },
        end: Position {
            line: b.0,
            character: b.1,
        },
    };
    let selection =
        java_format::review(capture(&root, &path, &d)?, Some(range), rx.clone()).await?;
    ensure!(
        !selection.unchanged && selection.outside_selection_changed,
        "real Java range context expansion missing"
    );
    ensure!(
        selection
            .after
            .contains("static int left( int a ){return a+1;}")
            && selection
                .after
                .contains("static int right( int a ){return a+1;}"),
        "unselected methods changed"
    );
    let bad = document(&path, "class JavaGreeting { ??? }\n")?;
    ensure!(
        java_format::review(capture(&root, &path, &bad)?, None, rx.clone())
            .await
            .is_err()
    );
    let type_bad = document(
        &path,
        "class JavaGreeting{static int broken(){return unknownSymbol;}}\n",
    )?;
    ensure!(
        !java_format::review(capture(&root, &path, &type_bad)?, None, rx.clone())
            .await?
            .unchanged,
        "syntax formatter required fake semantic success"
    );
    let crlf = document(
        &path,
        "class JavaGreeting{static String emoji(){return \"😀\";}}\r\n",
    )?;
    let windows = java_format::review(capture(&root, &path, &crlf)?, None, rx.clone()).await?;
    ensure!(!windows.unchanged && windows.after.contains("\r\n"));
    let semantic = "import java.util.Map;\npublic class Semantic{record R(int value){}public static void main(String[] args){String s=\"a;{} 😀\";System.out.println(new R(42).value()+\"|\"+s+\"|\"+true);}}\n";
    let d = document(&path, semantic)?;
    let formatted = java_format::review(capture(&root, &path, &d)?, None, rx.clone()).await?;
    let jdk = PathBuf::from(std::env::var("WEX_FORMAT_JAVA_HOME")?).canonicalize()?;
    let mut semantic_results = vec![];
    for (label, text) in [("before", semantic), ("after", formatted.after.as_str())] {
        let dir = out.parent().unwrap().join(label);
        std::fs::create_dir(&dir)?;
        std::fs::write(dir.join("Semantic.java"), text)?;
        let compiled = tokio::process::Command::new(jdk.join("bin/javac"))
            .args(["--release", "17", "-encoding", "UTF-8", "-d"])
            .arg(&dir)
            .arg(dir.join("Semantic.java"))
            .env_remove("JAVA_TOOL_OPTIONS")
            .env_remove("_JAVA_OPTIONS")
            .env_remove("JDK_JAVA_OPTIONS")
            .output()
            .await?;
        ensure!(
            compiled.status.success(),
            "actual javac: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let executed = tokio::process::Command::new(jdk.join("bin/java"))
            .arg("-cp")
            .arg(&dir)
            .arg("Semantic")
            .env_remove("JAVA_TOOL_OPTIONS")
            .env_remove("_JAVA_OPTIONS")
            .env_remove("JDK_JAVA_OPTIONS")
            .output()
            .await?;
        ensure!(
            executed.status.success()
                && String::from_utf8_lossy(&executed.stdout) == "42|a;{} 😀|true\n",
            "actual Java semantics changed"
        );
        semantic_results.push(json!({"case":label,"stdout":String::from_utf8_lossy(&executed.stdout),"exit":executed.status.code()}));
    }
    // A separate process changes disk while the actual formatter JVM is alive.
    let trace_path = PathBuf::from(std::env::var("WEX_JAVA_FORMAT_TRACE")?);
    let at = std::fs::metadata(&trace_path)?.len() as usize;
    let pending = tokio::spawn(java_format::review(
        Capture::read(&root, &path, dirty.into(), doc.version)?,
        None,
        rx.clone(),
    ));
    let _owned = wait_for_java(&trace_path, at).await?;
    let writer=tokio::process::Command::new("python3").args(["-c","from pathlib import Path;import sys;Path(sys.argv[1]).write_bytes(b'// external writer\\n')"]).arg(&path).output().await?;
    let raced = pending.await?;
    std::fs::write(&path, &disk)?;
    ensure!(
        writer.status.success() && raced.is_err() && *doc.rope() == dirty,
        "external disk conflict was bypassed"
    );
    // Synchronize cancellation on an actual owned Java child, never a synthetic parser delay.
    let trace = PathBuf::from(std::env::var("WEX_JAVA_FORMAT_TRACE")?);
    let offset = std::fs::metadata(&trace)?.len() as usize;
    let large = "class JavaGreeting {\n".to_owned()
        + &(0..10000)
            .map(|n| format!("int field{n};\n"))
            .collect::<String>()
        + "}\n";
    let c = Capture::read(&root, &path, large, 99)?;
    let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(java_format::review(c, None, cancel_rx));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let pid = loop {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "real Java child did not start"
        );
        let data = std::fs::read_to_string(&trace)?;
        let found = data
            .get(offset..)
            .unwrap_or("")
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|x| {
                x["stage"] == "spawn"
                    && x["program"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("/bin/java"))
            });
        if let Some(q) = found {
            let pid = q["pid"].as_u64().context("child pid")?;
            ensure!(
                Path::new("/proc").join(pid.to_string()).exists(),
                "Java request completed before cancellation evidence"
            );
            break pid;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    cancel.send_replace(true);
    ensure!(
        task.await?.is_err() && !Path::new("/proc").join(pid.to_string()).exists(),
        "cancel did not reap actual Java formatter"
    );
    ensure!(std::fs::read(&path)? == disk);
    drop(tx);
    std::fs::write(
        out,
        serde_json::to_vec_pretty(
            &json!({"result":"ACTUAL_JAVA_FORMATTER_OK","backend":r.backend.label(),"before":dirty,"after":first,"single_undo_redo":true,"no_op":true,"imports_preserved":true,"unicode_literal_preserved":true,"selection":{"before":selected,"after":selection.after,"expanded_context":selection.outside_selection_changed},"malformed_source_rejected":true,"type_errors_still_format":true,"CRLF":windows.crlf,"actual_javac_and_java":semantic_results,"external_disk_conflict_rejected":true,"actual_cancelled_java_pid":pid,"disk_unchanged":true,"no_APK_or_device":true}),
        )?,
    )?;
    println!(
        "ACTUAL_JAVA_FORMATTER_OK: pinned Google library/AOSP; file+selection; Unicode/CRLF/imports; no-op/one Undo; syntax error; real Javac/Java unchanged semantics; actual owned Java cancellation/reap; disk unchanged; no Gradle/APK/device"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Explicit isolated missing/damaged/tampered tool fixture; never network or user files"]
async fn genuine_java_formatter_failure_keeps_source() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_JAVA_FORMAT_PROJECT")?).canonicalize()?;
    let path = root.join("app/src/main/java/dev/wex/android/JavaGreeting.java");
    let original = std::fs::read(&path)?;
    let mut doc = document(&path, "class JavaGreeting{int value=1;}\n")?;
    let before = doc.rope().to_string();
    let version = doc.version;
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let result = java_format::review(capture(&root, &path, &doc)?, None, rx).await;
    let error = format!(
        "{:#}",
        result
            .err()
            .context("tampered formatter unexpectedly succeeded")?
    );
    let expected = std::env::var("WEX_JAVA_FORMAT_EXPECT_ERROR")?;
    ensure!(error.contains(&expected), "unexpected error: {error}");
    ensure!(*doc.rope() == before && doc.version == version && std::fs::read(&path)? == original);
    ensure!(doc.undo()? && *doc.rope() == String::from_utf8(original)?);
    println!("JAVA_FORMATTER_FAILURE_OK: {expected}; buffer/version/Undo/disk retained");
    Ok(())
}

async fn wait_for_java(trace: &Path, offset: usize) -> Result<u64> {
    let until = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(
            tokio::time::Instant::now() < until,
            "real Java process did not start"
        );
        let text = std::fs::read_to_string(trace)?;
        if let Some(q) = text
            .get(offset..)
            .unwrap_or("")
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|x| {
                x["stage"] == "spawn"
                    && x["program"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("/bin/java"))
            })
        {
            let pid = q["pid"].as_u64().context("Java pid")?;
            ensure!(
                Path::new("/proc").join(pid.to_string()).exists(),
                "formatter completed before writer synchronization"
            );
            return Ok(pid);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
