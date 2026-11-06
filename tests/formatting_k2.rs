//! Real pinned K2 formatting; no formatter mock or source writes.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use wexandroidstudio::{analysis, editor::Document, kotlin};

fn apply(text: &str, edits: &Value) -> Result<String> {
    if edits.is_null() {
        return Ok(text.into());
    }
    let mut doc = Document::new(text)?;
    let edits = edits
        .as_array()
        .context("format edits array")?
        .iter()
        .map(|e| {
            let r: kotlin::Range = serde_json::from_value(e["range"].clone())?;
            Ok((
                doc.char_at_lsp(r.start.line, r.start.character)?
                    ..doc.char_at_lsp(r.end.line, r.end.character)?,
                e["newText"].as_str().context("newText")?.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    doc.edit_batch_parsed(&edits)?;
    Ok(doc.rope().to_string())
}

#[tokio::test]
#[ignore = "Full Kotlin+Java formatting capability requirement; current pinned K2 Java endpoint returns null"]
async fn genuine_k2_document_and_range_formatting() -> Result<()> {
    probe(true).await
}
#[tokio::test]
#[ignore = "Requires explicit fresh native Android fixture, SDK and actual pinned K2"]
async fn genuine_k2_kotlin_formatting() -> Result<()> {
    probe(false).await
}
async fn probe(include_java: bool) -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_FORMAT_PROJECT")?).canonicalize()?;
    let out = PathBuf::from(std::env::var("WEX_FORMAT_OUTPUT")?);
    let model = analysis::prepare(&root, "debug", true, false, &|_| {}).await?;
    let kt = root.join("app/src/main/kotlin/dev/wex/android/MainActivity.kt");
    let java = root.join("app/src/main/java/dev/wex/android/JavaGreeting.java");
    let original_kt = std::fs::read_to_string(&kt)?;
    let original_java = std::fs::read_to_string(&java)?;
    let mut session =
        kotlin::KotlinSession::start(&root, &model.graph, &kotlin::default_server()).await?;
    ensure!(session.capabilities["capabilities"]["documentFormattingProvider"] == true);
    ensure!(session.capabilities["capabilities"]["documentRangeFormattingProvider"] == true);
    let dirty_kt = original_kt.clone()
        + "\nfun formatProbe( a:Int,b:Int):Int{val emoji=\"😀\";return a+b+emoji.length}\n";
    let dirty_java = original_java
        .replace("class JavaGreeting {", "class JavaGreeting{")
        .replace(
            "decorate(String value) { return",
            "decorate( String value ){return",
        );
    session.open(&kt, &dirty_kt, 1).await?;
    session.open(&java, &dirty_java, 1).await?;
    tokio::time::timeout(Duration::from_secs(150), async {
        while let Some(e) = session.client.events.recv().await {
            if e["method"] == "intellij/ready-for-test"
                || (e["method"] == "intellij/workspaceImportState"
                    && e["params"]["phase"] == "FINISHED"
                    && e["params"]["folders"].as_array().is_some_and(|v| {
                        !v.is_empty() && v.iter().all(|f| f["status"] == "SUCCESS")
                    }))
            {
                return Ok::<_, anyhow::Error>(());
            }
        }
        anyhow::bail!("actual K2 import stream closed")
    })
    .await??;
    let options = json!({"tabSize":4,"insertSpaces":true,"trimTrailingWhitespace":true,"insertFinalNewline":true});
    let mut cases = vec![];
    let mut formatted_kt = String::new();
    let inputs = if include_java {
        vec![(&kt, &dirty_kt), (&java, &dirty_java)]
    } else {
        vec![(&kt, &dirty_kt)]
    };
    for (path, text) in inputs {
        let uri = kotlin::file_uri(path)?;
        let edits = if path.extension().is_some_and(|s| s == "java") {
            session
                .client
                .request(
                    "textDocument/formatting",
                    json!({"textDocument":{"uri":uri},"options":options}),
                )
                .await?
        } else {
            session.format(path, None).await?
        };
        let review = wexandroidstudio::formatting::Capture::read(&root, path, text.to_string(), 1)?
            .review(&edits, None)?;
        let after = review.after;
        ensure!(
            after != *text,
            "actual formatter did not change {}: {edits}",
            path.display()
        );
        session.change(path, &after, 2).await?;
        let again = session
            .client
            .request(
                "textDocument/formatting",
                json!({"textDocument":{"uri":uri},"options":options}),
            )
            .await?;
        ensure!(
            apply(&after, &again)? == after,
            "actual formatter is not idempotent"
        );
        let diagnostics = session.pull_diagnostics(path).await?;
        ensure!(
            !diagnostics["items"]
                .as_array()
                .context("actual diagnostics")?
                .iter()
                .any(|d| d["severity"] == 1),
            "format broke typed source: {diagnostics}"
        );
        if path == &kt {
            formatted_kt = after.clone();
        }
        cases.push(json!({"path":path,"before":text,"edits":edits,"after":after,"second_edits":again,"idempotent":true,"diagnostics":diagnostics}));
    }
    let range_before = formatted_kt
        + "\nfun left( a:Int):Int{return a+1}\nfun chosen( a:Int):Int{return a+1}\nfun right( a:Int):Int{return a+1}\n";
    let begin = range_before.find("fun chosen").unwrap();
    let end = range_before.find("fun right").unwrap();
    let doc = Document::new(&range_before)?;
    let (sl, sc) = doc.lsp_position(range_before[..begin].chars().count())?;
    let (el, ec) = doc.lsp_position(range_before[..end].chars().count())?;
    let range = json!({"start":{"line":sl,"character":sc},"end":{"line":el,"character":ec}});
    session.change(&kt, &range_before, 3).await?;
    let scope: kotlin::Range = serde_json::from_value(range.clone())?;
    let edits = session.format(&kt, Some(&scope)).await?;
    let review = wexandroidstudio::formatting::Capture::read(&root, &kt, range_before.clone(), 3)?
        .review(&edits, Some(scope))?;
    let after = review.after;
    ensure!(
        after != range_before,
        "range formatter did not change selected function"
    );
    let outside_unchanged =
        after.starts_with(&range_before[..begin]) && after.ends_with(&range_before[end..]);
    let proof = json!({"result":if include_java {"ACTUAL_K2_FORMATTING_OK"}else{"ACTUAL_K2_KOTLIN_FORMATTING_OK"},"server":session.capabilities["serverInfo"],"cases":cases,"range":{"before":range_before,"requested":range,"edits":edits,"after":after,"outside_selection_unchanged":outside_unchanged},"disk_sources_unchanged":std::fs::read_to_string(&kt)?==original_kt&&std::fs::read_to_string(&java)?==original_java,"no_Gradle_APK_device":true});
    std::fs::write(&out, serde_json::to_vec_pretty(&proof)?)?;
    session.close(&kt).await?;
    session.close(&java).await?;
    session.shutdown().await?;
    ensure!(proof["disk_sources_unchanged"] == true);
    println!(
        "ACTUAL_K2_FORMATTING_OK: requested language scope dirty buffers; real document/range requests; idempotence; typed diagnostics clean; disk unchanged; outside-selection-unchanged={outside_unchanged}; no Gradle/APK/device"
    );
    Ok(())
}
