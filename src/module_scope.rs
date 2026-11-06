//! Declared project/Maven reachability. One workspace lock selects versions; scopes
//! select visibility and runtime packaging, not a second version-resolution policy.
use crate::native::{Config, Coordinate, Lock};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize)]
pub struct Scope {
    /// Dependency-first order, including the module itself.
    pub projects: Vec<String>,
    pub artifacts: BTreeSet<usize>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub modules: BTreeMap<String, Scope>,
    pub runtime: Scope,
}
fn ga(c: &Coordinate) -> String {
    format!("{}:{}", c.group, c.artifact)
}
fn projects(
    config: &Config,
    name: &str,
    active: &mut BTreeSet<String>,
    done: &mut Vec<String>,
) -> Result<()> {
    if done.iter().any(|n| n == name) {
        return Ok(());
    }
    ensure!(
        active.insert(name.to_owned()),
        "project dependency cycle at {name}"
    );
    ensure!(active.len() <= 256, "project scope depth limit");
    let m = config
        .modules
        .iter()
        .find(|m| m.name == name)
        .context("scope project missing")?;
    for child in &m.projects {
        projects(config, child, active, done)?;
    }
    active.remove(name);
    done.push(name.into());
    Ok(())
}
pub(crate) fn project_order(config: &Config, root: &str) -> Result<Vec<String>> {
    let mut order = vec![];
    projects(config, root, &mut BTreeSet::new(), &mut order)?;
    Ok(order)
}
/// Cover every declared module with independent dependency-closed analysis roots.
/// The configured application remains the first context, never a dependency of a
/// library. Prefer uncovered parents over compiling each leaf a second time.
pub(crate) fn analysis_roots(config: &Config) -> Result<Vec<String>> {
    ensure!(config.modules.len() <= 1024, "analysis module count limit");
    let mut closures = BTreeMap::new();
    for module in &config.modules {
        ensure!(
            !module.name.is_empty()
                && module.name.len() <= 128
                && module
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "invalid analysis module name"
        );
        let closure = project_order(config, &module.name)?;
        ensure!(
            module.name == config.build.application_module
                || !closure.contains(&config.build.application_module),
            "library cannot depend on the application: {}",
            module.name
        );
        ensure!(
            closures.insert(module.name.clone(), closure).is_none(),
            "duplicate analysis module"
        );
    }
    let application = &config.build.application_module;
    let mut covered = closures
        .get(application)
        .context("analysis application missing")?
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut roots = vec![application.clone()];
    while covered.len() < closures.len() {
        let mut best = None;
        let mut size = 0;
        for module in &config.modules {
            if covered.contains(&module.name) {
                continue;
            }
            let n = closures[&module.name]
                .iter()
                .filter(|m| !covered.contains(*m))
                .count();
            if n > size {
                best = Some(module.name.clone());
                size = n;
            }
        }
        let root = best.context("analysis root coverage stalled")?;
        covered.extend(closures[&root].iter().cloned());
        roots.push(root);
    }
    Ok(roots)
}
fn artifacts(
    lock: &Lock,
    index: &BTreeMap<String, usize>,
    roots: impl IntoIterator<Item = String>,
) -> Result<BTreeSet<usize>> {
    let mut todo = roots.into_iter().collect::<Vec<_>>();
    let mut selected = BTreeSet::new();
    while let Some(pin) = todo.pop() {
        let key = ga(&Coordinate::parse(&pin)?);
        let i = *index
            .get(&key)
            .with_context(|| format!("scope dependency missing from selected lock: {pin}"))?;
        if selected.insert(i) {
            todo.extend(lock.artifacts[i].dependencies.iter().cloned());
        }
    }
    Ok(selected)
}
pub fn plan(config: &Config, lock: &Lock, application: &str) -> Result<Plan> {
    ensure!(
        lock.schema == 1 && lock.resolver_abi == crate::native::RESOLVER_ABI,
        "scope-aware lock ABI required"
    );
    let mut index = BTreeMap::new();
    for (i, a) in lock.artifacts.iter().enumerate() {
        ensure!(
            index.insert(ga(&a.coordinate), i).is_none(),
            "duplicate selected Maven GA in lock"
        );
    }
    // Kotlin's selected language runtime is shared by native modules. No other
    // app-only dependency is implicitly a language/platform dependency.
    let platform = index.get("org.jetbrains.kotlin:kotlin-stdlib").map(|i| {
        let c = &lock.artifacts[*i].coordinate;
        format!("{}:{}:{}", c.group, c.artifact, c.version)
    });
    let mut modules = BTreeMap::new();
    for module in &config.modules {
        ensure!(
            !modules.contains_key(&module.name),
            "duplicate project module"
        );
        let mut ordered = vec![];
        projects(config, &module.name, &mut BTreeSet::new(), &mut ordered)?;
        let mut selected = artifacts(lock, &index, platform.iter().cloned())?;
        for name in &ordered {
            let declared = lock
                .module_scopes
                .get(name)
                .context("lock lacks module/exclusion scopes; resolve again")?;
            let m = config.modules.iter().find(|m| &m.name == name).unwrap();
            for pin in &m.dependencies {
                ensure!(
                    declared.contains(&ga(&Coordinate::parse(pin)?)),
                    "scope omits declared root dependency"
                );
            }
            for key in declared {
                selected.insert(
                    *index
                        .get(key)
                        .context("scope artifact missing from selected lock")?,
                );
            }
        }
        let scope = Scope {
            artifacts: selected,
            projects: ordered,
        };
        modules.insert(module.name.clone(), scope);
    }
    let runtime = modules
        .get(application.trim_start_matches(':'))
        .context("application scope missing")?
        .clone();
    Ok(Plan { modules, runtime })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{Artifact, Module};
    #[test]
    fn analysis_roots_cover_detached_parent_closures_without_leaf_duplication() {
        let (mut c, _) = fixture();
        c.modules.push(Module {
            name: "detached".into(),
            projects: vec!["unused".into(), "leaf".into()],
            ..Default::default()
        });
        c.modules.push(Module {
            name: "pure".into(),
            projects: vec!["unused".into()],
            ..Default::default()
        });
        assert_eq!(analysis_roots(&c).unwrap(), ["app", "detached", "pure"]);
        assert_eq!(
            project_order(&c, "detached").unwrap(),
            ["unused", "leaf", "detached"]
        );
    }
    #[test]
    fn analysis_roots_reject_application_dependency_and_bad_module_identity() {
        let (mut c, _) = fixture();
        c.modules[4].projects = vec!["app".into()];
        assert!(analysis_roots(&c).is_err());
        c.modules[4].projects.clear();
        c.modules[4].name = "../escape".into();
        assert!(analysis_roots(&c).is_err());
    }
    #[test]
    fn analysis_roots_reject_missing_application_cycles_and_duplicate_modules() {
        let (mut c, _) = fixture();
        c.build.application_module = "missing".into();
        assert!(analysis_roots(&c).is_err());
        c.build.application_module = "app".into();
        c.modules[3].projects = vec!["left".into()];
        assert!(analysis_roots(&c).is_err());
        c.modules[3].projects.clear();
        c.modules.push(c.modules[3].clone());
        assert!(analysis_roots(&c).is_err());
    }
    fn fixture() -> (Config, Lock) {
        let mut c: Config =
            serde_json::from_str(include_str!("../tests/fixtures/native-android/wex.json"))
                .unwrap();
        c.modules = vec![
            Module {
                name: "app".into(),
                projects: vec!["left".into(), "right".into()],
                dependencies: vec!["scope:app:1".into()],
                ..Default::default()
            },
            Module {
                name: "left".into(),
                projects: vec!["leaf".into()],
                dependencies: vec!["scope:left:1".into()],
                ..Default::default()
            },
            Module {
                name: "right".into(),
                dependencies: vec!["scope:right:1".into()],
                ..Default::default()
            },
            Module {
                name: "leaf".into(),
                dependencies: vec!["scope:leaf:1".into()],
                ..Default::default()
            },
            Module {
                name: "unused".into(),
                dependencies: vec!["scope:unused:1".into()],
                ..Default::default()
            },
        ];
        let a = |pin: &str, deps: Vec<&str>| Artifact {
            coordinate: Coordinate::parse(pin).unwrap(),
            packaging: "jar".into(),
            file: format!("{pin}.jar"),
            dependencies: deps.into_iter().map(str::to_owned).collect(),
            variant: None,
        };
        let l = Lock {
            schema: 1,
            resolver_abi: crate::native::RESOLVER_ABI,
            config_blake3: "test".into(),
            module_scopes: BTreeMap::from([
                ("app".into(), BTreeSet::from(["scope:app".into()])),
                (
                    "left".into(),
                    BTreeSet::from(["scope:left".into(), "scope:common".into()]),
                ),
                ("right".into(), BTreeSet::from(["scope:right".into()])),
                ("leaf".into(), BTreeSet::from(["scope:leaf".into()])),
                ("unused".into(), BTreeSet::from(["scope:unused".into()])),
            ]),
            files: vec![],
            mediation: "test".into(),
            artifacts: vec![
                a("scope:app:2", vec![]),
                a("scope:left:1", vec!["scope:common:1"]),
                a("scope:right:1", vec![]),
                a("scope:leaf:1", vec![]),
                a("scope:common:1", vec![]),
                a("scope:unused:1", vec![]),
                a(
                    "org.jetbrains.kotlin:kotlin-stdlib:2.4.0",
                    vec!["scope:annotations:1"],
                ),
                a("scope:annotations:1", vec![]),
            ],
        };
        (c, l)
    }
    #[test]
    fn parent_siblings_and_unused_modules_do_not_leak() {
        let (c, l) = fixture();
        let p = plan(&c, &l, "app").unwrap();
        assert_eq!(p.modules["left"].projects, ["leaf", "left"]);
        assert_eq!(p.modules["left"].artifacts, BTreeSet::from([1, 3, 4, 6, 7]));
        assert_eq!(p.modules["right"].artifacts, BTreeSet::from([2, 6, 7]));
        assert_eq!(p.runtime.artifacts, BTreeSet::from([0, 1, 2, 3, 4, 6, 7]));
        assert!(!p.runtime.projects.contains(&"unused".into()));
    }
    #[test]
    fn selected_versions_and_transitive_cycles_use_lock_identity() {
        let (c, mut l) = fixture();
        l.artifacts[4].dependencies = vec!["scope:left:1".into()];
        let p = plan(&c, &l, ":app").unwrap();
        assert!(p.runtime.artifacts.contains(&0));
        assert_eq!(l.artifacts[0].coordinate.version, "2");
    }
    #[test]
    fn invalid_project_or_missing_selected_dependency_fails_closed() {
        let (mut c, l) = fixture();
        c.modules[3].projects.push("left".into());
        assert!(plan(&c, &l, "app").is_err());
        c.modules[3].projects = vec!["absent".into()];
        assert!(plan(&c, &l, "app").is_err());
        c.modules[3].projects.clear();
        c.modules[0].dependencies.push("scope:absent:1".into());
        assert!(plan(&c, &l, "app").is_err());
    }
    #[test]
    fn duplicate_selected_ga_cannot_make_ambiguous_scope() {
        let (c, mut l) = fixture();
        l.artifacts.push(l.artifacts[0].clone());
        assert!(plan(&c, &l, "app").is_err());
    }
}
