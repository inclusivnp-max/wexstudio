//! Small strict manifest merger: namespaces, placeholders, component names and keyed nodes.
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
const ANDROID: &str = "http://schemas.android.com/apk/res/android";
const TOOLS: &str = "http://schemas.android.com/tools";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
}
fn parse(n: roxmltree::Node<'_, '_>, app: &str, package: &str) -> Result<Node> {
    let mut attrs = BTreeMap::new();
    for a in n.attributes() {
        let key = match a.namespace() {
            None => a.name().into(),
            Some(ANDROID) => format!("android:{}", a.name()),
            Some(TOOLS) => format!("tools:{}", a.name()),
            Some(ns) => anyhow::bail!("unsupported manifest attribute namespace {ns}"),
        };
        let mut value = a.value().replace("${applicationId}", app);
        ensure!(
            !value.contains("${"),
            "unresolved manifest placeholder: {value}"
        );
        if key == "android:name"
            && [
                "application",
                "activity",
                "activity-alias",
                "service",
                "receiver",
                "provider",
                "instrumentation",
            ]
            .contains(&n.tag_name().name())
            && (value.starts_with('.') || !value.contains('.'))
        {
            value = format!("{package}.{}", value.trim_start_matches('.'));
        }
        if key == "android:targetActivity" && (value.starts_with('.') || !value.contains('.')) {
            value = format!("{package}.{}", value.trim_start_matches('.'));
        }
        attrs.insert(key, value);
    }
    if let Some(v) = attrs.get("tools:node") {
        ensure!(
            ["merge", "remove", "replace"].contains(&v.as_str()),
            "unsupported tools:node={v}"
        );
    }
    for key in attrs.keys().filter(|k| k.starts_with("tools:")) {
        ensure!(
            ["tools:node", "tools:replace"].contains(&key.as_str()),
            "unsupported manifest directive {key}"
        );
    }
    let children = n
        .children()
        .filter(|n| n.is_element())
        .map(|n| parse(n, app, package))
        .collect::<Result<_>>()?;
    Ok(Node {
        name: n.tag_name().name().into(),
        attrs,
        children,
    })
}
fn key(n: &Node) -> String {
    if ["application", "uses-sdk", "supports-screens"].contains(&n.name.as_str()) {
        return n.name.clone();
    }
    if let Some(name) = n.attrs.get("android:name") {
        return format!("{}:{name}", n.name);
    }
    format!("{}:{:?}:{:?}", n.name, n.attrs, n.children)
}
fn merge(high: &mut Node, low: Node, app_priority: bool) -> Result<()> {
    if high
        .attrs
        .get("tools:node")
        .is_some_and(|v| v == "replace" || v == "remove")
    {
        return Ok(());
    }
    let replacements = high.attrs.get("tools:replace").cloned().unwrap_or_default();
    for (k, v) in low.attrs {
        if k.starts_with("tools:") || k == "package" {
            continue;
        }
        if let Some(h) = high.attrs.get(&k) {
            ensure!(
                h == &v || app_priority || replacements.split(',').any(|p| p.trim() == k),
                "conflicting manifest {} attribute {k}: {h} vs {v}",
                high.name
            );
        } else {
            high.attrs.insert(k, v);
        }
    }
    for n in low.children {
        if n.name == "uses-sdk" {
            continue;
        }
        if let Some(h) = high.children.iter_mut().find(|h| key(h) == key(&n)) {
            merge(h, n, app_priority)?;
        } else {
            high.children.push(n);
        }
    }
    Ok(())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn render(n: &Node, out: &mut String) {
    if n.attrs.get("tools:node").is_some_and(|v| v == "remove") {
        return;
    }
    out.push('<');
    out.push_str(&n.name);
    if n.name == "manifest" {
        out.push_str(&format!(" xmlns:android=\"{ANDROID}\""));
    }
    for (k, v) in &n.attrs {
        if !k.starts_with("tools:") {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(&escape(v));
            out.push('"');
        }
    }
    out.push('>');
    for c in &n.children {
        render(c, out);
    }
    out.push_str("</");
    out.push_str(&n.name);
    out.push('>');
}
pub fn library_package(text: &str) -> Result<String> {
    let d = roxmltree::Document::parse(text)?;
    Ok(d.root_element().attribute("package").unwrap_or("").into())
}
pub struct Settings<'a> {
    pub namespace: &'a str,
    pub id: &'a str,
    pub min_sdk: u32,
    pub target_sdk: u32,
    pub debug: bool,
    pub version_code: u32,
    pub version_name: &'a str,
}
pub fn merged(app_text: &str, libraries: &[String], settings: &Settings<'_>) -> Result<String> {
    merged_inner(app_text, libraries, settings, false)
}
/// An Android library may legitimately have no application element. This is a
/// private resource-link carrier only, never the manifest of a deployable app.
pub(crate) fn merged_library_context(
    text: &str,
    libraries: &[String],
    settings: &Settings<'_>,
) -> Result<String> {
    merged_inner(text, libraries, settings, true)
}
fn merged_inner(
    app_text: &str,
    libraries: &[String],
    settings: &Settings<'_>,
    library_context: bool,
) -> Result<String> {
    let Settings {
        namespace,
        id,
        min_sdk,
        target_sdk,
        debug,
        version_code,
        version_name,
    } = *settings;
    let doc = roxmltree::Document::parse(app_text)?;
    ensure!(
        doc.root_element().has_tag_name("manifest"),
        "expected Android manifest"
    );
    let mut app = parse(doc.root_element(), id, namespace)?;
    if library_context && !app.children.iter().any(|node| node.name == "application") {
        app.children.push(Node {
            name: "application".into(),
            attrs: BTreeMap::new(),
            children: vec![],
        });
    }
    for text in libraries {
        let d = roxmltree::Document::parse(text)?;
        let root = d.root_element();
        let package = root
            .attribute("package")
            .context("library manifest package missing")?;
        if let Some(sdk) = root
            .children()
            .find(|n| n.has_tag_name("uses-sdk"))
            .and_then(|n| n.attribute((ANDROID, "minSdkVersion")))
        {
            ensure!(
                sdk.parse::<u32>()? <= min_sdk,
                "library {package} requires minSdk {sdk}, configured {min_sdk}"
            );
        }
        merge(&mut app, parse(root, id, package)?, true)?;
    }
    app.attrs.insert("package".into(), id.into());
    app.attrs
        .insert("android:versionCode".into(), version_code.to_string());
    app.attrs
        .insert("android:versionName".into(), version_name.into());
    app.children.retain(|n| n.name != "uses-sdk");
    app.children.insert(
        0,
        Node {
            name: "uses-sdk".into(),
            attrs: BTreeMap::from([
                ("android:minSdkVersion".into(), min_sdk.to_string()),
                ("android:targetSdkVersion".into(), target_sdk.to_string()),
            ]),
            children: vec![],
        },
    );
    let application = app
        .children
        .iter_mut()
        .find(|n| n.name == "application")
        .context("application missing")?;
    application
        .attrs
        .insert("android:debuggable".into(), debug.to_string());
    let mut out = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n".to_owned();
    render(&app, &mut out);
    Ok(out)
}
#[cfg(test)]
mod tests {
    #[test]
    fn detached_library_may_omit_application_but_apk_stays_strict() {
        let settings = Settings {
            namespace: "dev.library",
            id: "dev.app",
            min_sdk: 36,
            target_sdk: 36,
            debug: false,
            version_code: 1,
            version_name: "1",
        };
        let xml = "<manifest package=\"dev.library\"/>";
        assert!(merged(xml, &[], &settings).is_err());
        let out = merged_library_context(xml, &[], &settings).unwrap();
        assert!(out.contains("<application") && out.contains("android:debuggable=\"false\""));
        roxmltree::Document::parse(&out).unwrap();
    }

    use super::*;
    #[test]
    fn preserves_namespace_expands_components_and_merges_startup_metadata() {
        let app = format!(
            "<manifest xmlns:android=\"{ANDROID}\"><application><activity android:name=\".MainActivity\"/></application></manifest>"
        );
        let low = format!(
            "<manifest xmlns:android=\"{ANDROID}\" package=\"androidx.startup\"><uses-sdk android:minSdkVersion=\"14\"/><application><provider android:name=\".Init\" android:authorities=\"${{applicationId}}.startup\"><meta-data android:name=\"Initializer\" android:value=\"yes\"/></provider></application></manifest>"
        );
        let out = merged(
            &app,
            &[low],
            &Settings {
                namespace: "dev.app.code",
                id: "dev.app.id",
                min_sdk: 26,
                target_sdk: 36,
                debug: true,
                version_code: 1,
                version_name: "1.0",
            },
        )
        .unwrap();
        assert!(out.contains("dev.app.code.MainActivity"));
        assert!(out.contains("androidx.startup.Init"));
        assert!(out.contains("dev.app.id.startup"));
        assert!(out.contains("Initializer"));
    }
    #[test]
    fn rejects_unresolved_placeholder_sdk_and_unsupported_merge_rules() {
        let app = format!(
            "<manifest xmlns:android=\"{ANDROID}\"><application android:label=\"${{missing}}\"/></manifest>"
        );
        assert!(
            merged(
                &app,
                &[],
                &Settings {
                    namespace: "a.b",
                    id: "a.c",
                    min_sdk: 26,
                    target_sdk: 36,
                    debug: true,
                    version_code: 1,
                    version_name: "1"
                }
            )
            .is_err()
        );
        let app = "<manifest><application/></manifest>";
        let lib = format!(
            "<manifest xmlns:android=\"{ANDROID}\" package=\"x.y\"><uses-sdk android:minSdkVersion=\"40\"/></manifest>"
        );
        assert!(
            merged(
                app,
                &[lib],
                &Settings {
                    namespace: "a.b",
                    id: "a.c",
                    min_sdk: 26,
                    target_sdk: 36,
                    debug: true,
                    version_code: 1,
                    version_name: "1"
                }
            )
            .is_err()
        );
    }
}
