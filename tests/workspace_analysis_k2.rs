//! Opt-in real K2 probe driven by scripts/check-module-scope.sh, never a fake LSP.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use wexandroidstudio::{analysis, editor::Document, kotlin};
#[tokio::test]
#[ignore = "Requires explicit WEX_WORKSPACE_PROJECT, real Android SDK and pinned K2"]
async fn genuine_k2_workspace_visibility() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_WORKSPACE_PROJECT")?).canonicalize()?;
    let out = PathBuf::from(std::env::var("WEX_WORKSPACE_OUTPUT")?);
    let visible = std::env::var("WEX_WORKSPACE_VISIBLE")? == "1";
    let positive = "FeatureApi";
    let forbidden = "workspaceForbidden";
    let model = analysis::prepare(&root, "debug", true, false, &|_| {}).await?;
    let source = root.join("feature/src/main/kotlin/dev/wex/feature/Feature.kt");
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
        .filter(|(_, line)| line.contains(forbidden))
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
                .is_some_and(|m| m.contains("Unresolved reference"))
        });
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
        "allowed transitive symbol unresolved: {diagnostics}"
    );
    if visible {
        ensure!(
            errors.is_empty(),
            "qualified leaf R recovery failed: {diagnostics}"
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
                .is_some_and(|m| m.contains("Unresolved reference"))),
            "application R leaked into detached module: {diagnostics}"
        );
    }
    let offset = original.find("FeatureJava.decorate").unwrap() + 2;
    let doc = Document::new(&original)?;
    let (line, col) = doc.lsp_position(original[..offset].chars().count())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let has_java_definition = |value: &Value| {
        value.as_array().is_some_and(|items| {
            items.iter().any(|x| {
                x["uri"]
                    .as_str()
                    .or_else(|| x["targetUri"].as_str())
                    .is_some_and(|uri| {
                        uri.ends_with("/feature/src/main/java/dev/wex/feature/FeatureJava.java")
                    })
            })
        })
    };
    let mut java_definition = Value::Null;
    while tokio::time::Instant::now() < deadline {
        java_definition = session.definition(&source, line, col).await?;
        if has_java_definition(&java_definition) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        has_java_definition(&java_definition),
        "detached Java source definition absent: {java_definition:?}"
    );
    let pure = root.join("pure/src/Pure.kt");
    let pure_source = std::fs::read_to_string(&pure)?;
    session.open(&pure, &pure_source, 1).await?;
    let partial = pure_source.replace("R.string.leaf_only", "R.string.leaf_");
    let offset = partial.find("R.string.leaf_").unwrap() + "R.string.leaf_".len();
    let doc = Document::new(&partial)?;
    let (line, col) = doc.lsp_position(partial[..offset].chars().count())?;
    session.change(&pure, &partial, 2).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut pure_completion = vec![];
    while tokio::time::Instant::now() < deadline {
        pure_completion = session
            .completion(&pure, line, col)
            .await?
            .into_iter()
            .map(|c| c.label)
            .collect();
        if pure_completion.iter().any(|s| s == "leaf_only") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        pure_completion.iter().any(|s| s == "leaf_only"),
        "pure module leaf R never indexed: {pure_completion:?}"
    );
    session.change(&pure, &pure_source, 3).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut pure_diagnostics = Value::Null;
    while tokio::time::Instant::now() < deadline {
        pure_diagnostics = session.pull_diagnostics(&pure).await?;
        if pure_diagnostics["items"]
            .as_array()
            .is_some_and(|x| !x.iter().any(|d| d["severity"] == 1))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        pure_diagnostics["items"]
            .as_array()
            .is_some_and(|x| !x.iter().any(|d| d["severity"] == 1)),
        "pure module cannot resolve declared leaf R: {pure_diagnostics}"
    );
    ensure!(
        std::fs::read_to_string(&pure)? == pure_source,
        "pure probe changed source"
    );
    session.close(&pure).await?;
    ensure!(
        std::fs::read_to_string(&source)? == original,
        "probe mutated disk source"
    );
    session.close(&source).await?;
    session.shutdown().await?;
    std::fs::write(
        out,
        serde_json::to_vec_pretty(
            &json!({"result":"K2_WORKSPACE_ANALYSIS_OK","visible":visible,"contexts":model.model.contexts,"completion_labels":labels,"diagnostics":diagnostics,"java_definition":java_definition,"pure_completion":pure_completion,"pure_diagnostics":pure_diagnostics,"disk_source_unchanged":true,"graph":model.graph}),
        )?,
    )?;
    println!(
        "K2_WORKSPACE_ANALYSIS_OK: explicit={visible}; detached member completion; application R visibility verified by real diagnostics"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Requires explicit WEX_WORKSPACE_COMPOSE_PROJECT, SDK and genuine K2"]
async fn genuine_detached_compose_metadata() -> Result<()> {
    let root = PathBuf::from(std::env::var("WEX_WORKSPACE_COMPOSE_PROJECT")?).canonicalize()?;
    let output = PathBuf::from(std::env::var("WEX_WORKSPACE_COMPOSE_OUTPUT")?);
    let model = analysis::prepare(&root, "release", true, false, &|_| {}).await?;
    let compose = model
        .model
        .compose
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Compose metadata missing"))?;
    ensure!(
        model.model.contexts.len() == 2 && compose.modules.iter().any(|m| m == "detached"),
        "detached Compose module omitted"
    );
    let source = root.join("detached/src/Detached.kt");
    let original = std::fs::read_to_string(&source)?;
    let mut session =
        kotlin::KotlinSession::start(&root, &model.graph, &kotlin::default_server()).await?;
    session.open(&source, &original, 1).await?;
    let partial = original.replace("ButtonDefaults.buttonColors()", "ButtonDefaults.buttonCol");
    let at = partial.find("ButtonDefaults.buttonCol").unwrap() + "ButtonDefaults.buttonCol".len();
    let doc = Document::new(&partial)?;
    let (line, col) = doc.lsp_position(partial[..at].chars().count())?;
    session.change(&source, &partial, 2).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(80);
    let mut labels = vec![];
    while tokio::time::Instant::now() < deadline {
        labels = session
            .completion(&source, line, col)
            .await?
            .into_iter()
            .map(|c| c.label)
            .collect();
        if labels.iter().any(|s| s.starts_with("buttonColors")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    ensure!(
        labels.iter().any(|s| s.starts_with("buttonColors")),
        "detached Compose dependency not indexed: {labels:?}"
    );
    let mut snapshots = vec![];
    for (version, text, bad) in [
        (3, original.clone(), false),
        (
            4,
            format!("{original}\nfun illegalDetachedCall() {{ DetachedScene() }}\n"),
            true,
        ),
        (5, original.clone(), false),
    ] {
        session.change(&source, &text, version).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        let mut diagnostics = Value::Null;
        while tokio::time::Instant::now() < deadline {
            diagnostics = session.pull_diagnostics(&source).await?;
            if diagnostics["items"].as_array().is_some_and(|items| {
                if bad {
                    items.iter().any(|d| d["code"] == "COMPOSABLE_INVOCATION")
                } else {
                    !items.iter().any(|d| d["severity"] == 1)
                }
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        ensure!(
            diagnostics["items"].as_array().is_some_and(|items| if bad {
                items.iter().any(|d| d["code"] == "COMPOSABLE_INVOCATION")
            } else {
                !items.iter().any(|d| d["severity"] == 1)
            }),
            "detached Compose plugin state incorrect: {diagnostics}"
        );
        snapshots.push(
            json!({"version":version,"expected_composable_error":bad,"diagnostics":diagnostics}),
        );
    }
    ensure!(
        std::fs::read_to_string(&source)? == original && !root.join(".wex/native").exists(),
        "Compose probe wrote source/APK"
    );
    session.close(&source).await?;
    session.shutdown().await?;
    std::fs::write(
        output,
        serde_json::to_vec_pretty(
            &json!({"result":"K2_DETACHED_COMPOSE_OK","completion_labels":labels,"snapshots":snapshots,"compose":compose,"contexts":model.model.contexts,"disk_source_unchanged":true,"apk_built":false}),
        )?,
    )?;
    println!(
        "K2_DETACHED_COMPOSE_OK: two actual SDK contexts; matching plugin metadata includes detached module; real Material3 completion/COMPOSABLE_INVOCATION/recovery; no APK or source mutation"
    );
    Ok(())
}
