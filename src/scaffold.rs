//! Native project templates. Generate privately, validate, publish without replacing any path.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Template {
    #[default]
    Compose,
    Kotlin,
    Java,
}
impl Template {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compose => "Compose · Expressive",
            Self::Kotlin => "Kotlin · Views",
            Self::Java => "Java · Views",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub name: String,
    pub directory: String,
    pub application_id: String,
    pub min_sdk: u32,
    pub template: Template,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            name: "My Application".into(),
            directory: "MyApplication".into(),
            application_id: "com.example.myapplication".into(),
            min_sdk: 26,
            template: Template::Compose,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Created {
    pub root: PathBuf,
    pub entry: PathBuf,
    pub template: Template,
    pub files: Vec<PathBuf>,
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 80
                && !self.name.chars().any(char::is_control)
                && !self
                    .name
                    .chars()
                    .any(|c| matches!(c, '\u{fffe}' | '\u{ffff}')),
            "Name must contain 1–80 visible characters"
        );
        ensure!(
            !self.directory.is_empty()
                && self.directory.len() <= 80
                && self
                    .directory
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_- ".contains(&b))
                && !self.directory.starts_with(' ')
                && !self.directory.ends_with(' '),
            "Directory must be one name using letters, digits, spaces, - or _"
        );
        const RESERVED: &[&str] = &[
            "as",
            "assert",
            "boolean",
            "break",
            "byte",
            "case",
            "catch",
            "char",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extends",
            "false",
            "final",
            "finally",
            "float",
            "for",
            "fun",
            "goto",
            "if",
            "implements",
            "import",
            "in",
            "instanceof",
            "int",
            "interface",
            "is",
            "long",
            "native",
            "new",
            "null",
            "object",
            "package",
            "private",
            "protected",
            "public",
            "return",
            "short",
            "static",
            "strictfp",
            "super",
            "switch",
            "synchronized",
            "this",
            "throw",
            "throws",
            "transient",
            "true",
            "try",
            "typealias",
            "typeof",
            "val",
            "var",
            "void",
            "volatile",
            "when",
            "while",
            "_",
        ];
        let parts: Vec<_> = self.application_id.split('.').collect();
        ensure!(
            self.application_id.len() <= 180
                && parts.len() >= 2
                && parts.iter().all(
                    |p| p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        && !RESERVED.contains(p)
                ),
            "Package requires at least two non-keyword identifiers starting with letters"
        );
        ensure!(
            (23..=36).contains(&self.min_sdk),
            "Minimum SDK must be between 23 and 36"
        );
        Ok(())
    }
    pub fn destination(&self, parent: &Path) -> Result<PathBuf> {
        self.validate()?;
        let parent = parent
            .canonicalize()
            .context("Choose an existing parent directory")?;
        ensure!(parent.is_dir(), "Parent must be a directory");
        let dest = parent.join(&self.directory);
        ensure!(
            std::fs::symlink_metadata(&dest)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "Destination already exists or is inaccessible; choose another directory"
        );
        Ok(dest)
    }
    pub fn files(&self) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
        self.validate()?;
        use serde_json::json;
        let compose = self.template == Template::Compose;
        let java = self.template == Template::Java;
        let sources = if java {
            "app/src/main/java"
        } else {
            "app/src/main/kotlin"
        };
        let deps = if compose {
            json!([
                "org.jetbrains.kotlin:kotlin-stdlib:2.4.0",
                "androidx.activity:activity-compose:1.11.0",
                "androidx.compose.material3:material3-android:1.4.0-alpha18",
                "androidx.compose.ui:ui-android:1.9.3"
            ])
        } else if java {
            json!([])
        } else {
            json!(["org.jetbrains.kotlin:kotlin-stdlib:2.4.0"])
        };
        let mut config = json!({"schema":1,"name":self.name,"repositories":["https://dl.google.com/dl/android/maven2","https://repo.maven.apache.org/maven2"],"android":{"compile_sdk":36,"target_sdk":36,"min_sdk":self.min_sdk,"application_id":self.application_id},"modules":[{"name":"app","sources":[sources],"resources":["app/src/main/res"],"dependencies":deps}],"build":{"application_module":"app","namespace":self.application_id,"manifest":"app/src/main/AndroidManifest.xml","build_tools":"36.0.0","kotlin_version":"2.4.0","jvm_target":17,"version_code":1,"version_name":"1.0"}});
        if compose {
            config["build"]["compose"] =
                json!({"source_information":true,"reports":false,"strong_skipping":true});
            config["android_variants"] = json!(true);
            config["constraints"] =
                json!(["androidx.compose.material3:material3-android:1.4.0-alpha18"]);
            config["platforms"] = json!(["androidx.compose:compose-bom:2025.10.01"]);
        }
        let mut files = BTreeMap::new();
        let mut add = |p: PathBuf, s: String| {
            files.insert(p, s.into_bytes());
        };
        add(
            "wex.json".into(),
            format!("{}\n", serde_json::to_string_pretty(&config)?),
        );
        let template = match self.template {
            Template::Compose => include_str!("templates/compose.kt"),
            Template::Kotlin => include_str!("templates/activity.kt"),
            Template::Java => include_str!("templates/activity.java"),
        };
        add(
            PathBuf::from(sources)
                .join(self.application_id.replace('.', "/"))
                .join(if java {
                    "MainActivity.java"
                } else {
                    "MainActivity.kt"
                }),
            template.replace("__PACKAGE__", &self.application_id),
        );
        add(
            "app/src/main/res/values/strings.xml".into(),
            format!(
                "<resources>\n    <string name=\"app_name\" formatted=\"false\">{}</string>\n</resources>\n",
                xml_string(&self.name)
            ),
        );
        add("app/src/main/AndroidManifest.xml".into(),"<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n    <application android:label=\"@string/app_name\" android:theme=\"@android:style/Theme.Material.Light.NoActionBar\">\n        <activity android:name=\".MainActivity\" android:exported=\"true\">\n            <intent-filter>\n                <action android:name=\"android.intent.action.MAIN\"/>\n                <category android:name=\"android.intent.category.LAUNCHER\"/>\n            </intent-filter>\n        </activity>\n    </application>\n</manifest>\n".into());
        add(
            ".gitignore".into(),
            "/.wex/\n/build/\n/local.properties\n*.jks\n*.keystore\n".into(),
        );
        add("README.md".into(),"# Native Android project\n\nOpen this folder in Wex Studio. Choose a device explicitly, then Run.\nBuild configuration: `wex.json`; dependencies: Wex resolver + `wex.lock.json`.\nRequires Android SDK 36/build-tools 36.0.0 and JDK 21. Kotlin toolchain is pinned to 2.4.0.\nNo Gradle or Maven runtime. Debug signing is automatic; release signing must be configured explicitly.\n".into());
        if compose {
            add(
                "wex.preview.json".into(),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(
                        &json!({"schema":1,"entries":[{"name":"App","composable":format!("{}.MainActivityKt.WexPreview",self.application_id),"width_dp":360,"height_dp":640,"dark":false,"font_scale":1.0}]})
                    )?
                ),
            );
        }
        Ok(files)
    }
}
fn xml_string(s: &str) -> String {
    // Android string syntax needs its own quoting in addition to XML escaping.
    let mut out = String::from("\"");
    for c in s.chars() {
        out.push_str(&match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "\\&quot;".into(),
            '\'' => "\\&apos;".into(),
            '\\' => "\\\\".into(),
            _ => c.to_string(),
        });
    }
    out.push('"');
    out
}
pub fn create(parent: &Path, options: &Options) -> Result<Created> {
    let destination = options.destination(parent)?;
    let files = options.files()?;
    let parent = destination.parent().unwrap();
    let temp = tempfile::Builder::new()
        .prefix(".wex-create-")
        .tempdir_in(parent)?;
    for (p, b) in &files {
        let target = temp.path().join(p);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(target, b)?;
    }
    crate::native::Config::load(temp.path()).context("Generated configuration validation")?;
    publish(temp.path(), &destination)?;
    let entry = files
        .keys()
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n == "MainActivity.kt" || n == "MainActivity.java")
        })
        .unwrap();
    Ok(Created {
        root: destination.clone(),
        entry: destination.join(entry),
        template: options.template,
        files: files.keys().cloned().collect(),
    })
}
#[cfg(target_os = "linux")]
fn publish(from: &Path, to: &Path) -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Linux renameat2 gives an atomic, race-free no-replace directory publication.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .context("Project destination changed or publication failed");
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn publish(_from: &Path, _to: &Path) -> Result<()> {
    anyhow::bail!("Atomic project publication is not implemented for this OS")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_templates_are_valid_and_no_gradle() {
        for template in [Template::Compose, Template::Kotlin, Template::Java] {
            let temp = tempfile::tempdir().unwrap();
            let options = Options {
                template,
                ..Default::default()
            };
            let created = create(temp.path(), &options).unwrap();
            let c = crate::native::Config::load(&created.root).unwrap();
            assert_eq!(c.build.compose.is_some(), template == Template::Compose);
            assert!(created.entry.is_file());
            assert_eq!(
                created.files.len(),
                if template == Template::Compose { 7 } else { 6 }
            );
            assert!(!created.root.join(".wex").exists());
            assert!(temp.path().read_dir().unwrap().all(|p| {
                !p.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".wex-create-")
            }));
            assert!(
                !created
                    .files
                    .iter()
                    .any(|p| p.to_string_lossy().contains("gradle"))
            );
        }
    }
    #[test]
    fn invalid_fields_create_nothing() {
        let temp = tempfile::tempdir().unwrap();
        for field in ["directory", "application_id", "name", "min_sdk"] {
            for bad in ["", "../escape", "class.example", "x.1bad", "com.when"] {
                let mut o = Options::default();
                match field {
                    "directory" => o.directory = bad.into(),
                    "application_id" => o.application_id = bad.into(),
                    "name" => o.name = "\0".into(),
                    _ => o.min_sdk = 22,
                };
                if o.validate().is_ok() {
                    continue;
                }
                assert!(create(temp.path(), &o).is_err());
                assert_eq!(temp.path().read_dir().unwrap().count(), 0);
            }
        }
    }
    #[test]
    fn existing_directory_and_files_are_never_replaced() {
        let t = tempfile::tempdir().unwrap();
        let o = Options::default();
        let dest = t.path().join(&o.directory);
        std::fs::create_dir(&dest).unwrap();
        assert!(create(t.path(), &o).is_err());
        std::fs::write(dest.join("keep"), "unchanged").unwrap();
        assert!(create(t.path(), &o).is_err());
        assert_eq!(
            std::fs::read_to_string(dest.join("keep")).unwrap(),
            "unchanged"
        );
        let stage = tempfile::tempdir_in(t.path()).unwrap();
        std::fs::write(stage.path().join("new"), "new").unwrap();
        assert!(publish(stage.path(), &dest).is_err());
        assert!(!dest.join("new").exists());
    }
    #[cfg(unix)]
    #[test]
    fn destination_symlink_is_rejected_even_when_broken() {
        let t = tempfile::tempdir().unwrap();
        let o = Options::default();
        std::os::unix::fs::symlink(t.path().join("missing"), t.path().join(&o.directory)).unwrap();
        assert!(create(t.path(), &o).is_err());
        assert!(t.path().join(&o.directory).is_symlink());
    }
    #[test]
    fn escaping_and_package_validation() {
        let o = Options {
            name: "Name & <tag> \"quote\" ' x \\ %1$s @ ?".into(),
            ..Default::default()
        };
        let files = o.files().unwrap();
        let xml =
            std::str::from_utf8(&files[Path::new("app/src/main/res/values/strings.xml")]).unwrap();
        assert!(roxmltree::Document::parse(xml).is_ok());
        assert!(xml.contains("&amp;") && xml.contains("\\&quot;") && xml.contains("\\\\"));
        for bad in [
            "com..app",
            "1com.app",
            "com.class",
            "com.when",
            "com.é",
            "com._",
            "com.example;evil",
        ] {
            assert!(
                Options {
                    application_id: bad.into(),
                    ..Default::default()
                }
                .validate()
                .is_err(),
                "{bad}"
            );
        }
    }
    #[test]
    fn publication_race_rejects_even_an_empty_directory_or_file() {
        let t = tempfile::tempdir().unwrap();
        for directory in [true, false] {
            let staging = tempfile::tempdir_in(t.path()).unwrap();
            std::fs::write(staging.path().join("owned"), "new").unwrap();
            let dest = t.path().join(if directory { "directory" } else { "file" });
            if directory {
                std::fs::create_dir(&dest).unwrap();
            } else {
                std::fs::write(&dest, "old").unwrap();
            }
            assert!(publish(staging.path(), &dest).is_err());
            assert!(staging.path().join("owned").exists());
            if directory {
                assert_eq!(dest.read_dir().unwrap().count(), 0);
            } else {
                assert_eq!(std::fs::read_to_string(dest).unwrap(), "old");
            }
        }
    }
    #[test]
    fn concurrent_creators_publish_exactly_one_complete_project() {
        let t = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let parent = t.path().to_owned();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    create(&parent, &Options::default())
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        let root = t.path().join(Options::default().directory);
        for (p, b) in Options::default().files().unwrap() {
            assert_eq!(std::fs::read(root.join(p)).unwrap(), b);
        }
        assert_eq!(t.path().read_dir().unwrap().count(), 1);
    }
}
