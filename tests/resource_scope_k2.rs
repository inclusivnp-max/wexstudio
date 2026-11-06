//! Opt-in real K2 probe driven by scripts/check-resource-scope.sh, never a fake LSP.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use wexandroidstudio::{analysis, editor::Document, kotlin};
#[tokio::test]
#[ignore = "Requires explicit WEX_RESOURCE_PROJECT, real Android SDK and pinned K2"]
async fn genuine_k2_resource_visibility() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_RESOURCE_PROJECT")?).canonicalize()?;
    let out = PathBuf::from(std::env::var("WEX_RESOURCE_OUTPUT")?);
    let visible = std::env::var("WEX_RESOURCE_VISIBLE")? == "1";
    let module = std::env::var("WEX_RESOURCE_MODULE")?;
    ensure!(
        matches!(module.as_str(), "shared" | "app"),
        "invalid test module"
    );
    let positive = if module == "shared" {
        "shared_proof"
    } else {
        "app_name"
    };
    let forbidden = if module == "shared" {
        "app_scope_only"
    } else {
        "shared_proof"
    };
    let model = analysis::prepare(&root, "debug", true, false, &|_| {}).await?;
    let source = root.join(if module == "shared" {
        "shared/src/main/kotlin/dev/wex/shared/ResourceProbe.kt"
    } else {
        "app/src/main/kotlin/dev/wex/android/ResourceProbe.kt"
    });
    let original = std::fs::read_to_string(&source)?;
    let mut session =
        kotlin::KotlinSession::start(&root, &model.graph, &kotlin::default_server()).await?;
    ensure!(
        !session.capabilities["capabilities"]["diagnosticProvider"].is_null(),
        "real pull diagnostics required"
    );
    session.open(&source, &original, 1).await?;
    let partial = original.replace(&format!("R.string.{positive}"), "R.string.");
    let at = partial.find("R.string.").unwrap() + "R.string.".len();
    let doc = Document::new(&partial)?;
    let (line, col) = doc.lsp_position(partial[..at].chars().count())?;
    session.change(&source, &partial, 2).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut labels = vec![];
    while tokio::time::Instant::now() < deadline {
        labels = session
            .completion(&source, line, col)
            .await?
            .into_iter()
            .map(|x| x.label)
            .collect();
        if labels.iter().any(|s| s == positive) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        labels.iter().any(|s| s == positive),
        "owned resource never indexed: {labels:?}"
    );
    session.change(&source, &original, 3).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut diagnostics = Value::Null;
    let bad_lines = original
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(forbidden))
        .map(|(i, _)| i as u64)
        .collect::<Vec<_>>();
    while tokio::time::Instant::now() < deadline {
        diagnostics = session.pull_diagnostics(&source).await?;
        let errors = diagnostics["items"]
            .as_array()
            .map(|v| v.iter().filter(|x| x["severity"] == 1).collect::<Vec<_>>())
            .unwrap_or_default();
        let app_error = errors
            .iter()
            .any(|x| x["message"].as_str().is_some_and(|m| m.contains(forbidden)));
        let extra = errors.iter().any(|x| {
            !x["range"]["start"]["line"]
                .as_u64()
                .is_some_and(|n| bad_lines.contains(&n))
        });
        let foreign = errors
            .iter()
            .any(|x| x["message"].as_str().is_some_and(|m| m.contains(positive)));
        if !foreign
            && !extra
            && if visible {
                errors.is_empty()
            } else {
                app_error
            }
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let errors = diagnostics["items"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing diagnostics"))?
        .iter()
        .filter(|x| x["severity"] == 1)
        .collect::<Vec<_>>();
    ensure!(
        !errors
            .iter()
            .any(|x| x["message"].as_str().is_some_and(|m| m.contains(positive))),
        "owned resource unresolved: {diagnostics}"
    );
    if visible {
        ensure!(
            errors.is_empty(),
            "qualified resource recovery failed: {diagnostics}"
        );
    } else {
        ensure!(
            errors.iter().all(|x| x["range"]["start"]["line"]
                .as_u64()
                .is_some_and(|n| bad_lines.contains(&n))),
            "unexpected/stale diagnostic outside forbidden symbol: {diagnostics}"
        );
        ensure!(
            errors
                .iter()
                .any(|x| x["message"].as_str().is_some_and(|m| m.contains(forbidden))),
            "non-transitive resource alias leaked: {diagnostics}"
        );
    }
    ensure!(
        std::fs::read_to_string(&source)? == original,
        "probe mutated disk source"
    );
    session.close(&source).await?;
    session.shutdown().await?;
    std::fs::write(
        out,
        serde_json::to_vec_pretty(
            &json!({"result":"K2_RESOURCE_SCOPE_OK","visible":visible,"module":module,"completion_labels":labels,"diagnostics":diagnostics,"disk_source_unchanged":true,"graph":model.graph}),
        )?,
    )?;
    println!(
        "K2_RESOURCE_SCOPE_OK: explicit={visible}; owned R completion; non-transitive resource visibility verified by real diagnostics"
    );
    Ok(())
}
