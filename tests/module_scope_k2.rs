//! Opt-in real K2 probe driven by scripts/check-module-scope.sh, never a fake LSP.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use wexandroidstudio::{analysis, editor::Document, kotlin};
#[tokio::test]
#[ignore = "Requires explicit WEX_SCOPE_PROJECT, real Android SDK and pinned K2"]
async fn genuine_k2_dependency_visibility() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_SCOPE_PROJECT")?).canonicalize()?;
    let out = PathBuf::from(std::env::var("WEX_SCOPE_OUTPUT")?);
    let visible = std::env::var("WEX_SCOPE_VISIBLE")? == "1";
    let positive = std::env::var("WEX_SCOPE_POSITIVE").unwrap_or("TransitiveApi".into());
    let forbidden = std::env::var("WEX_SCOPE_FORBIDDEN").unwrap_or("AppOnly".into());
    let model = analysis::prepare(&root, "debug", true, false, &|_| {}).await?;
    let source = root.join("shared/src/main/kotlin/dev/wex/shared/ScopeProbe.kt");
    let original = std::fs::read_to_string(&source)?;
    let mut session =
        kotlin::KotlinSession::start(&root, &model.graph, &kotlin::default_server()).await?;
    ensure!(
        !session.capabilities["capabilities"]["diagnosticProvider"].is_null(),
        "real pull diagnostics required"
    );
    session.open(&source, &original, 1).await?;
    let partial = original.replace(&format!("{positive}.label()"), &format!("{positive}.lab"));
    let at = partial.find(&format!("{positive}.lab")).unwrap() + format!("{positive}.lab").len();
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
        if labels.iter().any(|s| s.starts_with("label")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        labels.iter().any(|s| s.starts_with("label")),
        "declared transitive dependency never indexed: {labels:?}"
    );
    session.change(&source, &original, 3).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut diagnostics = Value::Null;
    let bad_lines = original
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(&forbidden))
        .map(|(i, _)| i as u64)
        .collect::<Vec<_>>();
    while tokio::time::Instant::now() < deadline {
        diagnostics = session.pull_diagnostics(&source).await?;
        let errors = diagnostics["items"]
            .as_array()
            .map(|v| v.iter().filter(|x| x["severity"] == 1).collect::<Vec<_>>())
            .unwrap_or_default();
        let app_error = errors.iter().any(|x| {
            x["message"]
                .as_str()
                .is_some_and(|m| m.contains(&forbidden))
        });
        let extra = errors.iter().any(|x| {
            !x["range"]["start"]["line"]
                .as_u64()
                .is_some_and(|n| bad_lines.contains(&n))
        });
        let foreign = errors
            .iter()
            .any(|x| x["message"].as_str().is_some_and(|m| m.contains(&positive)));
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
            .any(|x| x["message"].as_str().is_some_and(|m| m.contains(&positive))),
        "allowed transitive symbol unresolved: {diagnostics}"
    );
    if visible {
        ensure!(
            errors.is_empty(),
            "explicitly declared dependency unresolved: {diagnostics}"
        );
    } else {
        ensure!(
            errors.iter().all(|x| x["range"]["start"]["line"]
                .as_u64()
                .is_some_and(|n| bad_lines.contains(&n))),
            "unexpected/stale diagnostic outside forbidden symbol: {diagnostics}"
        );
        ensure!(
            errors.iter().any(|x| x["message"]
                .as_str()
                .is_some_and(|m| m.contains(&forbidden))),
            "parent-only dependency leaked: {diagnostics}"
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
            &json!({"result":"K2_MODULE_SCOPE_OK","visible":visible,"completion_labels":labels,"diagnostics":diagnostics,"disk_source_unchanged":true,"graph":model.graph}),
        )?,
    )?;
    println!(
        "K2_MODULE_SCOPE_OK: explicit={visible}; transitive member completion; parent-only symbol visibility verified by real diagnostics"
    );
    Ok(())
}
