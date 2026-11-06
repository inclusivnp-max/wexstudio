//! Relocatable immutable support assets and separate writable external tool data.
use serde::Serialize;
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Serialize)]
pub struct RuntimePaths {
    pub support: PathBuf,
    pub tools: PathBuf,
    pub mode: String,
}
struct Settings {
    cwd: PathBuf,
    dev: PathBuf,
    packaged: Option<PathBuf>,
    support: Option<PathBuf>,
    tools: Option<PathBuf>,
    data: Option<PathBuf>,
    home: Option<PathBuf>,
}
fn absolute_at(cwd: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        cwd.join(p)
    }
}
fn resolve(s: Settings) -> RuntimePaths {
    let configured = s.support.is_some();
    let portable = s.packaged.is_some();
    let support = absolute_at(&s.cwd, &s.support.or(s.packaged).unwrap_or(s.dev));
    let tools = if let Some(p) = s.tools {
        absolute_at(&s.cwd, &p)
    } else if portable || configured {
        let data = s
            .data
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| s.home.unwrap_or(s.cwd.clone()).join(".local/share"));
        absolute_at(&s.cwd, &data.join("wex/tools"))
    } else {
        support.join(".wex")
    };
    RuntimePaths {
        support,
        tools,
        mode: if configured {
            "configured"
        } else if portable {
            "portable"
        } else {
            "development"
        }
        .into(),
    }
}
pub fn paths() -> &'static RuntimePaths {
    static PATHS: std::sync::OnceLock<RuntimePaths> = std::sync::OnceLock::new();
    PATHS.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let packaged = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(|p| p.join("share/wex")))
            .filter(|p| p.join("support.json").is_file() || !cfg!(debug_assertions));
        resolve(Settings {
            cwd,
            dev: env!("CARGO_MANIFEST_DIR").into(),
            packaged,
            support: std::env::var_os("WEX_SUPPORT_HOME").map(PathBuf::from),
            tools: std::env::var_os("WEX_TOOLS_HOME").map(PathBuf::from),
            data: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME").map(PathBuf::from),
        })
    })
}
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}
/// Hash-check the packaged immutable support inventory. Development checkout has no manifest.
pub fn verify_support() -> anyhow::Result<bool> {
    use anyhow::{Context, ensure};
    use sha2::Digest;
    let root = &paths().support;
    let manifest = root.join("support.json");
    if !manifest.exists() {
        ensure!(
            paths().mode == "development",
            "Packaged support manifest missing"
        );
        return Ok(false);
    }
    let bytes = std::fs::read(&manifest)?;
    ensure!(bytes.len() <= 1024 * 1024, "Support manifest too large");
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(value["schema"] == 1, "Support manifest schema mismatch");
    let files = value["files"]
        .as_object()
        .context("Support inventory missing")?;
    ensure!(
        !files.is_empty() && files.len() <= 128,
        "Support inventory count invalid"
    );
    let mut expected = std::collections::BTreeSet::from([PathBuf::from("support.json")]);
    for (name, sha) in files {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Invalid support path"
        );
        let p = root.join(path);
        let m = std::fs::symlink_metadata(&p)?;
        ensure!(
            m.is_file() && !m.file_type().is_symlink() && m.len() <= 4 * 1024 * 1024,
            "Invalid support file"
        );
        let actual = sha2::Sha256::digest(std::fs::read(&p)?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        ensure!(
            Some(actual.as_str()) == sha.as_str(),
            "Support hash mismatch: {name}"
        );
        expected.insert(path.to_owned());
    }
    let mut actual = std::collections::BTreeSet::new();
    for e in walkdir::WalkDir::new(root).max_depth(12) {
        let e = e?;
        ensure!(!e.file_type().is_symlink(), "Support symlink not allowed");
        if e.file_type().is_file() {
            actual.insert(e.path().strip_prefix(root)?.to_owned());
        }
    }
    ensure!(actual == expected, "Support inventory mismatch");
    Ok(true)
}
pub fn doctor() -> serde_json::Value {
    let p = paths();
    let support = verify_support();
    let exe = std::env::current_exe().ok();
    let frontend = exe
        .as_ref()
        .and_then(|e| e.parent())
        .map(|p| p.join("wex-ide"));
    let frontend_bytes = frontend
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len());
    let sdk = absolute(
        &std::env::var_os("ANDROID_HOME")
            .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Android/Sdk")
            }),
    );
    let jdk = std::env::var_os("JAVA_HOME")
        .map(PathBuf::from)
        .map(|p| absolute(&p))
        .or_else(|| {
            std::fs::canonicalize("/usr/bin/java")
                .ok()
                .and_then(|p| p.parent()?.parent().map(Path::to_owned))
        });
    let lsp = crate::kotlin::default_server();
    let layoutlib = absolute(
        &std::env::var_os("WEX_LAYOUTLIB_STUDIO")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/opt/android-studio".into()),
    );
    serde_json::json!({"schema":1,"runtime":p,"executable":exe,"frontend":frontend,"frontend_bytes":frontend_bytes,"frontend_budget_bytes":20_000_000,"frontend_budget_ok":frontend_bytes.map(|n|n<=20_000_000),"support_verified":support.as_ref().is_ok_and(|v|*v),"support_error":support.err().map(|e|format!("{e:#}")),"kotlin_lsp":{"path":lsp,"launcher_present":lsp.is_file()},"android_sdk":{"path":sdk,"platform36_present":sdk.join("platforms/android-36/android.jar").is_file(),"build_tools36_present":(["aapt2","d8","apksigner","zipalign"].iter().all(|n|sdk.join("build-tools/36.0.0").join(n).is_file()))},"jdk":{"path":jdk,"javac_present":jdk.as_ref().is_some_and(|p|p.join("bin/javac").is_file())},"layoutlib":{"path":layoutlib,"present":layoutlib.join("plugins/design-tools/lib/layoutlib.jar").is_file()},"external_tools_bundled":false,"downloads_performed":false})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            cwd: "/caller".into(),
            dev: "/repo".into(),
            packaged: None,
            support: None,
            tools: None,
            data: None,
            home: Some("/home/test".into()),
        }
    }
    #[test]
    fn dev_paths_preserve_existing_repo_cache() {
        let p = resolve(settings());
        assert_eq!(p.support, PathBuf::from("/repo"));
        assert_eq!(p.tools, PathBuf::from("/repo/.wex"));
        assert_eq!(p.mode, "development");
    }
    #[test]
    fn portable_support_does_not_write_install_directory() {
        let mut s = settings();
        s.packaged = Some("/install/share/wex".into());
        s.data = Some("/data".into());
        let p = resolve(s);
        assert_eq!(p.support, PathBuf::from("/install/share/wex"));
        assert_eq!(p.tools, PathBuf::from("/data/wex/tools"));
        assert_eq!(p.mode, "portable");
    }
    #[test]
    fn relative_overrides_bind_caller_once() {
        let mut s = settings();
        s.support = Some("assets".into());
        s.tools = Some("runtime-data".into());
        s.packaged = Some("/install/share/wex".into());
        let p = resolve(s);
        assert_eq!(p.support, PathBuf::from("/caller/assets"));
        assert_eq!(p.tools, PathBuf::from("/caller/runtime-data"));
        assert_eq!(p.mode, "configured");
    }
    #[test]
    fn configured_support_without_data_override_uses_user_data() {
        let mut s = settings();
        s.support = Some("/read-only/support".into());
        s.data = Some("relative-xdg-is-invalid".into());
        let p = resolve(s);
        assert_eq!(p.tools, PathBuf::from("/home/test/.local/share/wex/tools"));
    }
}
