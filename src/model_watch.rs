//! Content-based native model input watching. No SDK/process or GPU-thread work.
use crate::{build, native};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
#[derive(Clone, Debug)]
pub struct Revision {
    pub id: u64,
    pub fingerprint: String,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    pub revision: u64,
    pub attempt: u64,
}
#[derive(Default)]
pub(crate) struct Gate {
    pub revision: u64,
    pub inflight: Option<Ticket>,
    pub failed: bool,
    pending: bool,
    due: u64,
    attempt: u64,
    pub force: bool,
}
impl Gate {
    pub fn observe(&mut self, revision: u64, now: u64) -> bool {
        if self.revision == revision {
            return false;
        }
        self.revision = revision;
        self.pending = true;
        self.failed = false;
        self.inflight = None;
        self.due = now + 650;
        self.force = false;
        true
    }
    pub fn restart(&mut self, force: bool, now: u64) {
        self.pending = true;
        self.failed = false;
        self.inflight = None;
        self.due = now;
        self.force = force;
    }
    pub fn retry(&mut self, force: bool, now: u64) {
        if force || self.failed {
            self.restart(force, now);
        }
    }
    pub fn next_ticket(&self) -> Ticket {
        Ticket {
            revision: self.revision,
            attempt: self.attempt + 1,
        }
    }
    pub fn settled(&self) -> bool {
        !self.pending && !self.failed && self.inflight.is_none()
    }
    pub fn begin(&mut self, now: u64) -> Option<Ticket> {
        if !self.pending || self.inflight.is_some() || now < self.due {
            return None;
        }
        self.attempt += 1;
        let ticket = Ticket {
            revision: self.revision,
            attempt: self.attempt,
        };
        self.inflight = Some(ticket);
        Some(ticket)
    }
    pub fn finish(&mut self, ticket: Ticket, success: bool) -> bool {
        if self.inflight != Some(ticket) || ticket.revision != self.revision {
            return false;
        }
        self.inflight = None;
        self.pending = false;
        self.failed = !success;
        self.force = false;
        true
    }
}
pub struct Watcher {
    pub changes: watch::Receiver<Option<Revision>>,
    task: tokio::task::JoinHandle<()>,
    stop: Arc<AtomicBool>,
}
impl Watcher {
    pub fn start(root: PathBuf) -> Self {
        let (sender, changes) = watch::channel(None);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = tokio::spawn(async move {
            let mut scanner = Scanner {
                external: true,
                ..Default::default()
            };
            let mut last = String::new();
            let mut revision = 0;
            loop {
                let path = root.clone();
                let cancelled = flag.clone();
                let next = tokio::task::spawn_blocking(move || {
                    let result = scanner.scan(&path, &cancelled);
                    (scanner, result)
                })
                .await;
                let Ok((state, result)) = next else {
                    break;
                };
                scanner = state;
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                if result.0 != last {
                    revision += 1;
                    last = result.0.clone();
                    sender.send_replace(Some(Revision {
                        id: revision,
                        fingerprint: result.0,
                        error: result.1,
                    }));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        Self {
            changes,
            task,
            stop,
        }
    }
}
impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}
#[derive(Default)]
struct Scanner {
    external: bool,
    config: Option<native::Config>,
    memo: BTreeMap<PathBuf, (String, String)>,
}
impl Scanner {
    fn scan(&mut self, root: &Path, stop: &AtomicBool) -> (String, Option<String>) {
        let mut hasher = blake3::Hasher::new();
        let mut errors = vec![];
        let mut paths = BTreeSet::new();
        let mut trees = vec![];
        paths.insert(root.join("wex.json"));
        paths.insert(root.join("wex.lock.json"));
        match native::Config::load(root) {
            Ok(c) => self.config = Some(c),
            Err(e) => errors.push(format!("configuration: {e:#}")),
        }
        if let Some(c) = &self.config {
            paths.insert(root.join(&c.build.manifest));
            for m in &c.modules {
                for p in &m.resources {
                    trees.push(root.join(p));
                }
                if let Some(p) = &m.manifest {
                    paths.insert(root.join(p));
                }
            }
            if self.external {
                match build::analysis_sdk_context(c) {
                    Ok(context) => {
                        hasher.update(context.as_bytes());
                    }
                    Err(e) => errors.push(format!("SDK context: {e:#}")),
                }
                match build::analysis_sdk_inputs(c) {
                    Ok(inputs) => paths.extend(inputs),
                    Err(e) => errors.push(format!("SDK: {e:#}")),
                }
                match build::toolchain_directory(c) {
                    Ok(dir) => {
                        paths.insert(dir.join("wex.json"));
                        paths.insert(dir.join("wex.lock.json"));
                        if let Err(e) = lock_paths(&dir.join("wex.lock.json"), &mut paths) {
                            errors.push(format!("compiler lock: {e:#}"));
                        }
                    }
                    Err(e) => errors.push(format!("compiler: {e:#}")),
                }
            }
        }
        if self.external
            && let Err(e) = lock_paths(&root.join("wex.lock.json"), &mut paths)
        {
            errors.push(format!("dependency lock: {e:#}"));
        }
        for tree in trees {
            hasher.update(tree.as_os_str().as_encoded_bytes());
            if tree.starts_with(root.join(".wex")) {
                errors.push("engine-owned resource roots are not watchable".into());
                continue;
            }
            if !tree.exists() {
                hasher.update(b"missing-tree");
                continue;
            }
            for entry in walkdir::WalkDir::new(&tree).max_depth(32) {
                if stop.load(Ordering::Relaxed) {
                    return ("stopped".into(), None);
                }
                match entry {
                    Ok(e) => {
                        if e.file_type().is_symlink() {
                            errors.push(format!("resource symlink: {}", e.path().display()));
                        } else if e.file_type().is_file() {
                            paths.insert(e.into_path());
                        }
                    }
                    Err(e) => errors.push(e.to_string()),
                }
                if paths.len() > 30000 {
                    errors.push("model watch file limit".into());
                    break;
                }
            }
        }
        let mut bytes = 0u64;
        for path in &paths {
            if stop.load(Ordering::Relaxed) {
                return ("stopped".into(), None);
            }
            hasher.update(path.as_os_str().as_encoded_bytes());
            hasher.update(&[0]);
            match self.content(path, &mut bytes, stop) {
                Ok(hash) => {
                    hasher.update(hash.as_bytes());
                }
                Err(e) => {
                    let error = format!("{}: {e:#}", path.display());
                    hasher.update(error.as_bytes());
                    errors.push(error);
                }
            }
            hasher.update(&[0]);
        }
        self.memo.retain(|path, _| paths.contains(path));
        errors.truncate(24);
        errors.sort();
        errors.dedup();
        let error =
            (!errors.is_empty()).then(|| errors.join("; ").chars().take(4096).collect::<String>());
        if let Some(e) = &error {
            hasher.update(e.as_bytes());
        }
        (hasher.finalize().to_hex().to_string(), error)
    }
    fn content(&mut self, path: &Path, bytes: &mut u64, stop: &AtomicBool) -> Result<String> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("missing".into()),
            Err(e) => return Err(e.into()),
        };
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "invalid model input file"
        );
        *bytes += meta.len();
        ensure!(*bytes <= 1024 * 1024 * 1024, "model watch byte limit");
        let stamp = stamp(&meta);
        if let Some((old, hash)) = self.memo.get(path)
            && old == &stamp
        {
            return Ok(hash.clone());
        }
        let mut f = std::fs::File::open(path)?;
        let mut h = blake3::Hasher::new();
        let mut buf = [0; 65536];
        loop {
            ensure!(!stop.load(Ordering::Relaxed), "watch cancelled");
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        ensure!(
            stamp == self::stamp(&f.metadata()?),
            "model input changed during scan"
        );
        let hash = h.finalize().to_hex().to_string();
        self.memo.insert(path.to_owned(), (stamp, hash.clone()));
        Ok(hash)
    }
}
fn stamp(meta: &std::fs::Metadata) -> String {
    let value = format!("{}:{:?}", meta.len(), meta.modified());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{value}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        value
    }
}
fn lock_paths(path: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 32 * 1024 * 1024, "watch lock size limit");
    let lock: native::Lock = serde_json::from_slice(&bytes).context("invalid lock JSON")?;
    ensure!(lock.files.len() <= 30000, "watch lock file count");
    for file in lock.files {
        let p = PathBuf::from(file.path);
        ensure!(
            !p.is_absolute()
                && p.components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "watch dependency path escapes cache"
        );
        paths.insert(native::default_cache().join(p));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let config = serde_json::json!({"schema":1,"name":"watch unit","repositories":["https://repo.maven.apache.org/maven2"],"android":{"compile_sdk":36,"min_sdk":26,"target_sdk":36,"application_id":"dev.watch"},"build":{"manifest":"AndroidManifest.xml"},"modules":[{"name":"app","sources":["src"],"resources":["res"],"assets":["assets"],"dependencies":[]}]});
        for dir in ["src", "res/values", "assets", ".wex/analysis"] {
            std::fs::create_dir_all(d.path().join(dir)).unwrap();
        }
        for (path, text) in [
            ("AndroidManifest.xml", "<manifest/>"),
            ("src/Main.kt", "val value=1"),
            ("res/values/strings.xml", "<resources/>"),
            ("assets/item", "asset"),
        ] {
            std::fs::write(d.path().join(path), text).unwrap();
        }
        std::fs::write(
            d.path().join("wex.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        d
    }
    #[test]
    fn source_bodies_assets_and_owned_generations_do_not_trigger_model_refresh() {
        let d = project();
        let mut scanner = Scanner::default();
        let stop = AtomicBool::new(false);
        let first = scanner.scan(d.path(), &stop);
        assert!(first.1.is_none());
        for (path, text) in [
            ("src/Main.kt", "deliberate syntax error"),
            ("assets/item", "changed"),
            (".wex/analysis/noise", "output"),
        ] {
            std::fs::write(d.path().join(path), text).unwrap();
        }
        assert_eq!(first, scanner.scan(d.path(), &stop));
    }
    #[test]
    fn resource_edit_creation_rename_and_delete_change_input_signature() {
        let d = project();
        let mut scanner = Scanner::default();
        let stop = AtomicBool::new(false);
        let mut last = scanner.scan(d.path(), &stop).0;
        for (path, text) in [
            ("res/values/strings.xml", "changed"),
            ("res/values/added.xml", "added"),
        ] {
            std::fs::write(d.path().join(path), text).unwrap();
            let next = scanner.scan(d.path(), &stop).0;
            assert_ne!(last, next);
            last = next;
        }
        std::fs::rename(
            d.path().join("res/values/added.xml"),
            d.path().join("res/values/renamed.xml"),
        )
        .unwrap();
        let next = scanner.scan(d.path(), &stop).0;
        assert_ne!(last, next);
        std::fs::remove_file(d.path().join("res/values/renamed.xml")).unwrap();
        assert_ne!(next, scanner.scan(d.path(), &stop).0);
    }
    #[test]
    fn invalid_or_deleted_config_keeps_previous_resource_roots_for_recovery() {
        let d = project();
        let mut scanner = Scanner::default();
        let stop = AtomicBool::new(false);
        let original = std::fs::read(d.path().join("wex.json")).unwrap();
        scanner.scan(d.path(), &stop);
        std::fs::write(d.path().join("wex.json"), "{").unwrap();
        let invalid = scanner.scan(d.path(), &stop);
        assert!(invalid.1.is_some());
        std::fs::write(
            d.path().join("res/values/strings.xml"),
            "changed while config invalid",
        )
        .unwrap();
        assert_ne!(invalid.0, scanner.scan(d.path(), &stop).0);
        std::fs::remove_file(d.path().join("wex.json")).unwrap();
        assert!(scanner.scan(d.path(), &stop).1.is_some());
        std::fs::write(d.path().join("wex.json"), original).unwrap();
        assert!(scanner.scan(d.path(), &stop).1.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn resource_symlink_is_reported_without_following_its_target() {
        let d = project();
        std::os::unix::fs::symlink(d.path().join("src"), d.path().join("res/linked")).unwrap();
        let mut scanner = Scanner::default();
        let result = scanner.scan(d.path(), &AtomicBool::new(false));
        assert!(result.1.unwrap().contains("symlink"));
    }
    #[test]
    fn variant_restart_rewarms_without_forcing_sdk_generation() {
        let mut gate = Gate::default();
        gate.observe(1, 0);
        let first = gate.begin(700).unwrap();
        gate.finish(first, true);
        gate.restart(false, 800);
        assert!(!gate.force && !gate.settled());
        let second = gate.begin(800).unwrap();
        assert_ne!(first.attempt, second.attempt);
        gate.finish(second, true);
        assert!(gate.settled());
    }

    use super::*;
    #[test]
    fn burst_debounce_failure_retry_and_stale_attempts() {
        let mut gate = Gate::default();
        assert!(gate.observe(1, 0));
        assert!(gate.begin(649).is_none());
        let first = gate.begin(650).unwrap();
        assert!(gate.observe(2, 660));
        assert!(!gate.finish(first, true));
        let next = gate.begin(1310).unwrap();
        assert!(gate.finish(next, false));
        assert!(gate.begin(9000).is_none());
        gate.retry(false, 9000);
        let retry = gate.begin(9000).unwrap();
        assert_ne!(retry.attempt, next.attempt);
        assert!(!gate.finish(next, true));
        assert!(gate.finish(retry, true));
        assert!(gate.begin(9999).is_none());
    }
    #[test]
    fn forced_reload_without_input_change_has_a_fresh_attempt() {
        let mut gate = Gate::default();
        gate.observe(1, 0);
        let first = gate.begin(700).unwrap();
        gate.finish(first, true);
        gate.retry(true, 800);
        assert!(gate.force);
        let next = gate.begin(800).unwrap();
        assert_eq!(next.revision, first.revision);
        assert_ne!(next.attempt, first.attempt);
    }
    #[test]
    fn content_hash_ignores_metadata_only_touch_and_observes_atomic_replace() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("input");
        std::fs::write(&p, b"a").unwrap();
        let mut s = Scanner::default();
        let stop = AtomicBool::new(false);
        let a = s.content(&p, &mut 0, &stop).unwrap();
        std::fs::write(&p, b"a").unwrap();
        assert_eq!(a, s.content(&p, &mut 0, &stop).unwrap());
        let q = d.path().join("new");
        std::fs::write(&q, b"b").unwrap();
        std::fs::rename(q, &p).unwrap();
        assert_ne!(a, s.content(&p, &mut 0, &stop).unwrap());
        std::fs::remove_file(&p).unwrap();
        assert_eq!(s.content(&p, &mut 0, &stop).unwrap(), "missing");
    }
}
