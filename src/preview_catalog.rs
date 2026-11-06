//! Catalog from real compiler-emitted annotation metadata; no source regex/import guessing.
use crate::preview::{Config, Entry};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub entry: Entry,
    pub origin: String,
    pub group: String,
    pub source_file: String,
    pub line: u32,
    pub descriptor: String,
    pub annotation_path: Vec<String>,
    pub parameters: Value,
    pub problem: Option<String>,
}
impl Item {
    pub fn label(&self) -> String {
        let group = if self.group.is_empty() {
            String::new()
        } else {
            format!("{} · ", self.group)
        };
        format!("{group}{} · {}", self.entry.name, self.entry.composable)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub items: Vec<Item>,
    pub issues: Vec<Issue>,
    pub project_classes: u32,
    pub classes_read: u32,
    pub backend: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub member: String,
    pub message: String,
}
#[derive(Deserialize)]
pub(crate) struct Compiled {
    result: String,
    project_classes_executed: bool,
    project_classes: u32,
    classes_read: u32,
    entries: Vec<Annotation>,
    issues: Vec<Issue>,
}
#[derive(Deserialize)]
struct Annotation {
    identity: String,
    member: String,
    descriptor: String,
    source_file: String,
    line: u32,
    annotation_path: Vec<String>,
    parameters: Value,
    problem: String,
}
impl Catalog {
    pub fn select(&self, selector: Option<&str>) -> Result<&Item> {
        if let Some(selector) = selector.filter(|s| !s.is_empty()) {
            let found = self
                .items
                .iter()
                .filter(|i| {
                    i.id == selector || i.entry.name == selector || i.entry.composable == selector
                })
                .collect::<Vec<_>>();
            ensure!(
                found.len() <= 1,
                "Ambiguous preview selection; use its catalog ID"
            );
            let item = found
                .first()
                .context("Preview selection missing; choose a current catalog entry")?;
            ensure!(
                item.problem.is_none(),
                "Preview unavailable: {}",
                item.problem.as_deref().unwrap_or_default()
            );
            return Ok(item);
        }
        self.items.iter().find(|i|i.problem.is_none()).context("No renderable @Preview functions; add @Preview to a zero-argument @Composable or explicit wex.preview.json entry")
    }
    pub(crate) fn build(explicit: Option<Config>, raw: Compiled, compile_sdk: u32) -> Result<Self> {
        ensure!(
            raw.result == "COMPILED_PREVIEW_DISCOVERY_OK" && !raw.project_classes_executed,
            "invalid discovery backend receipt"
        );
        ensure!(
            raw.entries.len() <= 512
                && raw.issues.len() <= 1024
                && raw.project_classes <= 20000
                && raw.classes_read <= 20000,
            "preview discovery limits exceeded"
        );
        let mut catalog = Self {
            items: Vec::new(),
            issues: raw.issues,
            project_classes: raw.project_classes,
            classes_read: raw.classes_read,
            backend: "compiler-bytecode-asm-v1".into(),
        };
        if let Some(config) = explicit {
            for entry in config.entries {
                catalog.items.push(Item {
                    id: format!("configured:{}", entry.name),
                    entry,
                    origin: "wex.preview.json".into(),
                    group: String::new(),
                    source_file: String::new(),
                    line: 0,
                    descriptor: String::new(),
                    annotation_path: vec![],
                    parameters: Value::Null,
                    problem: None,
                });
            }
        }
        for a in raw.entries {
            ensure!(
                a.identity.len() <= 4096
                    && a.member.len() <= 1024
                    && a.source_file.len() <= 1024
                    && a.annotation_path.len() <= 16,
                "invalid discovery entry metadata"
            );
            let parameters = &a.parameters;
            let string = |key: &str| parameters[key].as_str().unwrap_or("");
            let integer = |key: &str, default: i64| parameters[key].as_i64().unwrap_or(default);
            let name = string("name");
            let mode = integer("uiMode", 0);
            let mut entry = Entry {
                name: if name.is_empty() {
                    a.member.rsplit('.').next().unwrap_or("Preview").to_owned()
                } else {
                    name.to_owned()
                },
                composable: a.member.clone(),
                ..Default::default()
            };
            let mut problems = vec![];
            if !a.problem.is_empty() {
                problems.push(a.problem);
            }
            if let Some(fields) = parameters.as_object() {
                for (key, value) in fields {
                    let valid = match key.as_str() {
                        "name" | "group" | "locale" | "device" => value.is_string(),
                        "widthDp" | "heightDp" | "apiLevel" | "uiMode" | "wallpaper"
                        | "backgroundColor" => value.as_i64().is_some(),
                        "fontScale" => value.as_f64().is_some(),
                        "showBackground" | "showSystemUi" => value.is_boolean(),
                        _ => {
                            problems.push(format!("Unsupported Preview attribute: {key}"));
                            true
                        }
                    };
                    if !valid {
                        problems.push(format!("Invalid compiler Preview attribute type: {key}"));
                    }
                }
            } else {
                problems.push("Missing compiler Preview parameter object".into());
            }

            for (key, value, target) in [
                ("widthDp", integer("widthDp", -1), &mut entry.width_dp),
                ("heightDp", integer("heightDp", -1), &mut entry.height_dp),
            ] {
                if value != -1 {
                    if let Ok(value) = u32::try_from(value) {
                        *target = value;
                    } else {
                        problems.push(format!("{key} outside supported bounds"));
                    }
                }
            }
            entry.dark = mode & 0x30 == 0x20;
            entry.font_scale = parameters["fontScale"].as_f64().unwrap_or(1.) as f32;
            if !string("locale").is_empty() {
                entry.locale = string("locale").replace("-r", "-");
            }
            entry.show_background = parameters["showBackground"].as_bool().unwrap_or(false);
            let color = integer("backgroundColor", 0);
            if (0..=i64::from(u32::MAX)).contains(&color) {
                entry.background_color = color as u32;
            } else {
                problems.push("backgroundColor must be32-bit ARGB".into());
            }
            // Unsupported attributes remain visible/disabled, never silently rendered as defaults.
            if !string("device").is_empty() {
                problems.push(format!(
                    "Device specification requires hardware-profile support: {}",
                    string("device")
                ));
            }
            if parameters["showSystemUi"] == true {
                problems.push("System UI decoration requires preview decor support".into());
            }
            if integer("wallpaper", -1) != -1 {
                problems.push("Wallpaper/dynamic color preview requires wallpaper support".into());
            }
            if mode & !0x31 != 0 || !matches!(mode & 0x30, 0 | 0x10 | 0x20) {
                problems.push("Unsupported uiMode beyond day/night".into());
            }
            let api = integer("apiLevel", -1);
            if api != -1 && api != i64::from(compile_sdk) {
                problems.push(format!(
                    "Preview API {api} differs from installed compile platform {compile_sdk}"
                ));
            }
            if let Err(error) = entry.validate() {
                problems.push(error.to_string());
            }
            let problem = if problems.is_empty() {
                None
            } else {
                Some(problems.join("; "))
            };
            catalog.items.push(Item {
                id: format!(
                    "annotation:{}",
                    blake3::hash(a.identity.as_bytes()).to_hex()
                ),
                entry,
                origin: "@Preview".into(),
                group: string("group").to_owned(),
                source_file: a.source_file,
                line: a.line,
                descriptor: a.descriptor,
                annotation_path: a.annotation_path,
                parameters: a.parameters,
                problem,
            });
        }
        let mut ids = BTreeSet::new();
        ensure!(
            catalog.items.iter().all(|i| ids.insert(&i.id)),
            "duplicate preview catalog identity"
        );
        Ok(catalog)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn compiled(params: Value) -> Compiled {
        serde_json::from_value(json!({"result":"COMPILED_PREVIEW_DISCOVERY_OK","project_classes_executed":false,"project_classes":1,"classes_read":2,"entries":[{"identity":"example.FooKt.Preview@0","member":"example.FooKt.Preview","descriptor":"(Landroidx/compose/runtime/Composer;I)V","source_file":"Foo.kt","line":7,"annotation_path":["Preview"],"parameters":params,"problem":""}],"issues":[]})).unwrap()
    }
    #[test]
    fn invalid_or_new_annotation_attributes_are_visible_not_silently_defaulted() {
        for parameters in [
            serde_json::json!({"widthDp":1.5}),
            serde_json::json!({"fontScale":"big"}),
            serde_json::json!({"newPreviewFeature":true}),
            serde_json::Value::Null,
        ] {
            let c = Catalog::build(None, compiled(parameters), 36).unwrap();
            assert!(c.items[0].problem.is_some());
            assert!(c.select(None).is_err());
        }
    }
    #[test]
    fn maps_compiler_constants_and_common_scene_parameters() {
        let c=Catalog::build(None,compiled(json!({"name":"Night","group":"Phone","widthDp":240,"heightDp":400,"fontScale":1.25,"uiMode":32,"locale":"fr-rFR","showBackground":true,"backgroundColor":4278255360u64})),36).unwrap();
        let i = c.select(None).unwrap();
        assert_eq!(i.entry.name, "Night");
        assert_eq!(i.entry.width_dp, 240);
        assert!(i.entry.dark && i.entry.show_background);
        assert_eq!(i.entry.locale, "fr-FR");
        assert_eq!(i.entry.background_color, 0xff00ff00);
        assert_eq!(i.entry.font_scale, 1.25);
        assert_eq!(i.group, "Phone");
        assert!(i.label().contains("example.FooKt.Preview"));
    }
    #[test]
    fn ids_are_stable_when_annotation_values_change() {
        let a = Catalog::build(None, compiled(json!({"name":"Old"})), 36).unwrap();
        let b = Catalog::build(None, compiled(json!({"name":"New","widthDp":600})), 36).unwrap();
        assert_eq!(a.items[0].id, b.items[0].id);
    }
    #[test]
    fn unsupported_scenes_remain_visible_and_selection_reports_reason() {
        for p in [
            json!({"device":"id:pixel_4"}),
            json!({"apiLevel":12}),
            json!({"wallpaper":2}),
            json!({"uiMode":4}),
            json!({"showSystemUi":true}),
            json!({"widthDp":0}),
            json!({"fontScale":4}),
            json!({"backgroundColor":4294967296u64}),
        ] {
            let c = Catalog::build(None, compiled(p), 36).unwrap();
            assert_eq!(c.items.len(), 1);
            assert!(c.items[0].problem.is_some());
            assert!(
                c.select(Some(&c.items[0].id))
                    .unwrap_err()
                    .to_string()
                    .contains("Preview unavailable")
            );
        }
    }
    #[test]
    fn explicit_config_stays_first_and_unknown_or_duplicate_name_never_falls_back() {
        let e = Entry {
            name: "Preview".into(),
            composable: "example.ManualKt.Preview".into(),
            ..Default::default()
        };
        let c = Catalog::build(
            Some(Config {
                schema: 1,
                entries: vec![e],
            }),
            compiled(json!({})),
            36,
        )
        .unwrap();
        assert_eq!(c.select(None).unwrap().origin, "wex.preview.json");
        assert!(
            c.select(Some("Preview"))
                .unwrap_err()
                .to_string()
                .contains("Ambiguous")
        );
        assert!(c.select(Some("missing")).is_err());
        assert_eq!(
            c.select(Some("configured:Preview"))
                .unwrap()
                .entry
                .composable,
            "example.ManualKt.Preview"
        );
    }
    #[test]
    fn invalid_backend_identity_or_duplicate_compiled_identity_is_rejected() {
        let mut a = compiled(json!({}));
        a.project_classes_executed = true;
        assert!(Catalog::build(None, a, 36).is_err());
        let mut a = compiled(json!({}));
        let other = compiled(json!({})).entries.pop().unwrap();
        a.entries.push(other);
        assert!(Catalog::build(None, a, 36).is_err());
    }
}
