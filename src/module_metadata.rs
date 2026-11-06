//! Android/JVM runtime selection from published module metadata; no Gradle runtime.
use crate::native::Coordinate;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug)]
pub struct Dependency {
    pub coordinate: Coordinate,
    pub exclusions: BTreeSet<String>,
}
#[derive(Debug)]
pub struct Binary {
    pub kind: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}
#[derive(Debug)]
pub struct Selection {
    pub name: String,
    pub alias: Option<Coordinate>,
    pub dependencies: Vec<Dependency>,
    pub binary: Option<Binary>,
    pub requirements: BTreeMap<String, String>,
}
fn coordinate(v: &Value, versions: &BTreeMap<String, String>) -> Result<Coordinate> {
    let group = v["group"]
        .as_str()
        .context("metadata dependency group missing")?;
    let module = v["module"]
        .as_str()
        .context("metadata dependency module missing")?;
    let ga = format!("{group}:{module}");
    let version = v["version"]["strictly"]
        .as_str()
        .or_else(|| v["version"]["requires"].as_str())
        .or_else(|| v["version"].as_str())
        .or_else(|| {
            versions
                .get(&ga)
                .or_else(|| versions.get(&format!("{group}:*")))
                .map(String::as_str)
        })
        .context("metadata dependency version missing")?;
    let version = if version.starts_with('[') && version.ends_with(']') && !version.contains(',') {
        &version[1..version.len() - 1]
    } else {
        version
    };
    Coordinate::parse(&format!("{ga}:{version}"))
}
fn canonical_reference(source: &Coordinate, reference: &str, target: &Coordinate) -> Result<bool> {
    let path = |c: &Coordinate| {
        format!(
            "{}/{}/{}/{}-{}.module",
            c.group.replace('.', "/"),
            c.artifact,
            c.version,
            c.artifact,
            c.version
        )
    };
    let base = url::Url::parse(&format!("https://metadata.invalid/{}", path(source)))?;
    Ok(base.join(reference)?.as_str() == format!("https://metadata.invalid/{}", path(target)))
}
pub fn select(
    data: &[u8],
    component: &Coordinate,
    jvm_target: u32,
    project_versions: &BTreeMap<String, String>,
) -> Result<Selection> {
    ensure!(
        data.len() <= 4 * 1024 * 1024,
        "module metadata exceeds 4 MiB"
    );
    let json: Value = serde_json::from_slice(data)?;
    ensure!(
        json["formatVersion"] == "1.1",
        "unsupported module metadata version"
    );
    if let Some(identity) = json.get("component") {
        let actual = coordinate(identity, &BTreeMap::new())?;
        if let Some(reference) = identity["url"].as_str() {
            ensure!(
                canonical_reference(component, reference, &actual)?,
                "metadata component reference mismatch"
            );
        } else {
            ensure!(actual == *component, "metadata component mismatch");
        }
    }
    let variants = json["variants"]
        .as_array()
        .context("module variants missing")?;
    ensure!(variants.len() <= 128, "too many module variants");
    let mut candidates = Vec::new();
    for v in variants {
        let a = &v["attributes"];
        if a["org.gradle.category"]
            .as_str()
            .is_some_and(|s| s != "library")
        {
            continue;
        }
        let usage = match a["org.gradle.usage"].as_str() {
            Some("java-runtime") => 0,
            Some("java-api") => 1,
            None if v.get("files").is_none()
                && v.get("available-at").is_none()
                && v.get("dependencies").is_some()
                && a.as_object().is_some_and(|a| {
                    a.len() == 1
                        && a.get("org.gradle.category").and_then(Value::as_str) == Some("library")
                }) =>
            {
                2
            }
            _ => continue,
        };
        if a["org.gradle.jvm.version"]
            .as_u64()
            .is_some_and(|n| n > jvm_target as u64)
        {
            continue;
        }
        let env = a["org.gradle.jvm.environment"].as_str().unwrap_or("");
        let platform = a["org.jetbrains.kotlin.platform.type"]
            .as_str()
            .unwrap_or("");
        if !["", "jvm", "androidJvm"].contains(&platform) || env == "non-jvm" {
            continue;
        }
        let android = if env == "android" || platform == "androidJvm" {
            0
        } else {
            1
        };
        let build = match a["com.android.build.api.attributes.BuildTypeAttr"].as_str() {
            Some("release") | None => 0,
            _ => 1,
        };
        candidates.push(((android, usage, build), v));
    }
    candidates.sort_by_key(|(score, _)| *score);
    let (score, variant) = candidates
        .first()
        .context("no compatible Android/JVM runtime variant")?;
    ensure!(
        candidates.iter().filter(|(s, _)| s == score).count() == 1,
        "ambiguous Android/JVM runtime variants"
    );
    let name = variant["name"]
        .as_str()
        .context("variant name missing")?
        .to_owned();
    if let Some(alias) = variant.get("available-at") {
        let coordinate = coordinate(alias, &BTreeMap::new())?;
        ensure!(
            canonical_reference(
                component,
                alias["url"]
                    .as_str()
                    .context("alias metadata URL missing")?,
                &coordinate
            )?,
            "noncanonical module variant alias URL"
        );
        let alias = coordinate;
        ensure!(&alias != component, "module variant aliases itself");
        ensure!(
            variant.get("files").is_none() && variant.get("dependencies").is_none(),
            "alias variant also defines files/dependencies"
        );
        return Ok(Selection {
            name,
            alias: Some(alias),
            dependencies: vec![],
            binary: None,
            requirements: BTreeMap::new(),
        });
    }
    let mut versions = BTreeMap::new();
    let mut requirements = BTreeMap::new();
    if let Some(constraints) = variant.get("dependencyConstraints") {
        for v in constraints
            .as_array()
            .context("invalid variant constraints")?
        {
            let c = coordinate(v, &BTreeMap::new())?;
            requirements.insert(format!("{}:{}", c.group, c.artifact), c.version.clone());
            versions.insert(format!("{}:{}", c.group, c.artifact), c.version);
        }
    }
    versions.extend(project_versions.clone());
    for (pattern, version) in project_versions {
        if let Some(group) = pattern.strip_suffix('*') {
            for (ga, current) in &mut versions {
                if ga.starts_with(group) && !project_versions.contains_key(ga) {
                    current.clone_from(version);
                }
            }
        }
    }
    let mut dependencies = Vec::new();
    if let Some(deps) = variant.get("dependencies") {
        let deps = deps.as_array().context("invalid variant dependencies")?;
        ensure!(deps.len() <= 2048, "too many variant dependencies");
        for d in deps {
            ensure!(
                d.get("requestedCapabilities").is_none()
                    && d.get("thirdPartyCompatibility").is_none(),
                "custom dependency capabilities/classifiers require explicit support"
            );
            let mut exclusions = BTreeSet::new();
            if let Some(xs) = d.get("excludes") {
                for x in xs.as_array().context("invalid metadata exclusions")? {
                    exclusions.insert(format!(
                        "{}:{}",
                        x["group"].as_str().context("exclusion group missing")?,
                        x["module"].as_str().context("exclusion module missing")?
                    ));
                }
            }
            let group = d["group"].as_str().context("metadata group missing")?;
            let module = d["module"].as_str().context("metadata module missing")?;
            let mut dependency = d.clone();
            if let Some(pin) = project_versions
                .get(&format!("{group}:{module}"))
                .or_else(|| project_versions.get(&format!("{group}:*")))
            {
                dependency["version"] = serde_json::json!(pin);
            }
            dependencies.push(Dependency {
                coordinate: coordinate(&dependency, &versions)?,
                exclusions,
            });
        }
    }
    let files = variant.get("files").and_then(Value::as_array);
    if files.is_none_or(Vec::is_empty) {
        return Ok(Selection {
            name,
            alias: None,
            dependencies,
            binary: None,
            requirements,
        });
    }
    let files = files.unwrap();
    ensure!(
        files.len() == 1,
        "runtime variant requires exactly one binary"
    );
    let file = &files[0];
    let url = file["url"].as_str().context("binary URL missing")?;
    let kind = if url.ends_with(".aar") { "aar" } else { "jar" };
    ensure!(
        url == format!("{}-{}.{}", component.artifact, component.version, kind),
        "noncanonical/classified runtime binary URL"
    );
    let sha256 = file["sha256"].as_str().map(str::to_owned);
    if let Some(hash) = &sha256 {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid module binary checksum"
        );
    }
    Ok(Selection {
        name,
        alias: None,
        dependencies,
        requirements,
        binary: Some(Binary {
            kind: kind.into(),
            size: file["size"].as_u64(),
            sha256,
        }),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn module(variants: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"formatVersion":"1.1","component":{"group":"dev.test","module":"root","version":"1"},"variants":variants})).unwrap()
    }
    fn variant(env: &str, name: &str) -> Value {
        json!({"name":name,"attributes":{"org.gradle.category":"library","org.gradle.usage":"java-runtime","org.gradle.jvm.environment":env,"org.jetbrains.kotlin.platform.type":"jvm"},"available-at":{"group":"dev.test","module":name,"version":"1","url":format!("../../{name}/1/{name}-1.module")}})
    }
    #[test]
    fn dependency_only_bridge_has_no_pom_fallback_binary() {
        let c = Coordinate::parse("dev.test:root:1").unwrap();
        let v = json!({"name":"bridge","attributes":{"org.gradle.category":"library"},"dependencies":[{"group":"dev.test","module":"jvm","version":{"requires":"1"}}]});
        let s = select(&module(json!([v])), &c, 17, &BTreeMap::new()).unwrap();
        assert!(s.binary.is_none() && s.alias.is_none());
        assert_eq!(s.dependencies[0].coordinate.artifact, "jvm");
    }
    #[test]
    fn android_runtime_wins_over_jvm_and_documentation() {
        let mut documentation = variant("android", "sources");
        documentation["attributes"]["org.gradle.category"] = json!("documentation");
        let s = select(
            &module(json!([
                variant("standard-jvm", "jvm"),
                documentation,
                variant("android", "android")
            ])),
            &Coordinate::parse("dev.test:root:1").unwrap(),
            17,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(s.alias.unwrap().artifact, "android");
    }
    #[test]
    fn jvm_runtime_is_selected_when_no_android_target_is_published() {
        let s = select(
            &module(json!([variant("standard-jvm", "jvm")])),
            &Coordinate::parse("dev.test:root:1").unwrap(),
            17,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(s.name, "jvm");
    }
    #[test]
    fn ambiguity_wrong_component_and_unsupported_targets_fail() {
        let c = Coordinate::parse("dev.test:root:1").unwrap();
        assert!(
            select(
                &module(json!([variant("android", "a"), variant("android", "b")])),
                &c,
                17,
                &BTreeMap::new()
            )
            .is_err()
        );
        assert!(
            select(
                &module(json!([variant("android", "a")])),
                &Coordinate::parse("dev.test:other:1").unwrap(),
                17,
                &BTreeMap::new()
            )
            .is_err()
        );
        assert!(select(&module(json!([])), &c, 17, &BTreeMap::new()).is_err());
    }
    #[test]
    fn constraints_supply_omitted_dependency_versions_and_files_are_validated() {
        let v = json!({"name":"runtime","attributes":{"org.gradle.usage":"java-runtime"},"dependencyConstraints":[{"group":"dev.test","module":"dep","version":{"requires":"2"}}],"dependencies":[{"group":"dev.test","module":"dep"}],"files":[{"url":"root-1.jar","size":4,"sha256":"a".repeat(64)}]});
        let c = Coordinate::parse("dev.test:root:1").unwrap();
        let s = select(&module(json!([v.clone()])), &c, 17, &BTreeMap::new()).unwrap();
        assert_eq!(s.dependencies[0].coordinate.version, "2");
        assert_eq!(s.binary.unwrap().size, Some(4));
        let s = select(
            &module(json!([v])),
            &c,
            17,
            &BTreeMap::from([("dev.test:dep".into(), "3".into())]),
        )
        .unwrap();
        assert_eq!(s.dependencies[0].coordinate.version, "3");
    }
}
