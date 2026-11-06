//! Persist only stopped, genuinely imported K2 profiles. Unfinished slots are
//! quarantined by losing their clean pointer, NEVER reused/deleted after a crash.
use crate::{analysis, project::ProjectGraph, sdk_cache::Guard};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

const ABI: &str = "k2-profile-v1";
const MAX_ENTRIES: usize = 30_000;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub status: String,
    pub key: Option<String>,
    pub workspace: PathBuf,
    pub system: PathBuf,
    pub reused_files: usize,
    pub detail: Option<String>,
}
#[derive(Clone)]
pub(crate) struct Inputs {
    seed: Vec<u8>,
    files: Vec<PathBuf>,
    workspace_hash: String,
}
fn field(h: &mut blake3::Hasher, value: &[u8]) {
    h.update(&(value.len() as u64).to_le_bytes());
    h.update(value);
}
impl Inputs {
    pub fn capture(
        graph: &ProjectGraph,
        server: &Path,
        jdk: &Path,
        workspace: &[u8],
        removed: &[String],
    ) -> Result<Self> {
        ensure!(
            graph.model_api == analysis::MODEL_API,
            "K2 cache requires a verified native graph"
        );
        let distribution = server
            .parent()
            .and_then(Path::parent)
            .context("K2 distribution root")?
            .canonicalize()?;
        ensure!(
            distribution.join("product-info.json").is_file()
                && distribution.join("build.txt").is_file(),
            "custom K2 distribution has no bounded identity"
        );
        let mut files = BTreeSet::new();
        for entry in walkdir::WalkDir::new(&distribution).max_depth(64) {
            let entry = entry?;
            ensure!(
                entry.depth() < 64 || !entry.file_type().is_dir(),
                "K2 distribution depth limit"
            );
            if entry.file_type().is_dir() {
                continue;
            }
            let path = entry.path().canonicalize()?;
            ensure!(
                path.starts_with(&distribution) && path.is_file(),
                "K2 distribution link escapes or is not a file"
            );
            files.insert(entry.path().to_path_buf());
            ensure!(files.len() <= MAX_ENTRIES, "K2 tool inventory limit");
        }
        files.extend(
            graph
                .modules
                .iter()
                .flat_map(|m| m.classpath.iter().chain(&m.boot_classpath))
                .cloned(),
        );
        for name in ["release", "bin/java", "lib/modules", "lib/jrt-fs.jar"] {
            files.insert(jdk.join(name));
        }
        if jdk.join("jmods").is_dir() {
            for entry in std::fs::read_dir(jdk.join("jmods"))? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|e| e == "jmod") {
                    files.insert(entry.path());
                }
            }
        }
        for root in graph
            .modules
            .iter()
            .flat_map(|m| m.generated_source_roots.iter().chain(&m.classes_roots))
        {
            for entry in walkdir::WalkDir::new(root).min_depth(1).max_depth(64) {
                let entry = entry?;
                ensure!(!entry.file_type().is_symlink(), "K2 generated input link");
                ensure!(
                    entry.depth() < 64 || !entry.file_type().is_dir(),
                    "K2 generated input depth limit"
                );
                if entry.file_type().is_file() {
                    files.insert(entry.into_path());
                }
                ensure!(files.len() <= MAX_ENTRIES, "K2 input file count limit");
            }
        }
        let exported: serde_json::Value = serde_json::from_slice(workspace)?;
        if let Some(settings) = exported["kotlinSettings"].as_array() {
            for setting in settings {
                let arguments = setting["compilerArguments"]
                    .as_str()
                    .and_then(|v| v.strip_prefix('J'))
                    .context("K2 plugin compiler arguments format")?;
                let arguments: serde_json::Value = serde_json::from_str(arguments)?;
                for plugin in arguments["pluginClasspaths"]
                    .as_array()
                    .context("K2 plugin classpaths")?
                {
                    files.insert(PathBuf::from(
                        plugin.as_str().context("K2 plugin path type")?,
                    ));
                }
            }
        }
        let mut seed = ABI.as_bytes().to_vec();
        seed.extend(serde_json::to_vec(graph)?);
        seed.extend(workspace);
        for name in ["JDK_HOME", "REMOTE_DEV_JDK"] {
            ensure!(
                removed.iter().any(|v| v == name) || std::env::var_os(name).is_none(),
                "custom K2 runtime requires an independent profile"
            );
        }
        for name in ["LD_PRELOAD", "LD_LIBRARY_PATH"] {
            ensure!(
                removed.iter().any(|v| v == name)
                    || std::env::var_os(name).is_none_or(|v| v.is_empty()),
                "custom K2 native runtime requires an independent profile"
            );
        }
        for name in [
            "JAVA_HOME",
            "IJ_JAVA_OPTIONS",
            "JAVA_TOOL_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
        ] {
            seed.extend(name.as_bytes());
            seed.push(0);
            if !removed.iter().any(|v| v == name)
                && let Some(value) = std::env::var_os(name)
            {
                if name.ends_with("OPTIONS") {
                    let value = value.to_string_lossy();
                    ensure!(
                        value.split_whitespace().all(|v| v.starts_with("-Xmx")
                            || v.starts_with("-Xms")
                            || v.starts_with("-XX:ActiveProcessorCount=")),
                        "custom K2 JVM options require an independent profile"
                    );
                }
                seed.extend(value.as_encoded_bytes());
            }
            seed.push(0);
        }
        Ok(Self {
            seed,
            files: files.into_iter().collect(),
            workspace_hash: blake3::hash(workspace).to_hex().to_string(),
        })
    }
    pub fn key(&self) -> Result<String> {
        let mut h = blake3::Hasher::new();
        field(&mut h, &self.seed);
        let mut bytes = 0u64;
        for path in &self.files {
            let meta = std::fs::metadata(path)?;
            ensure!(meta.is_file(), "K2 input is not a file");
            bytes = bytes
                .checked_add(meta.len())
                .context("K2 input size overflow")?;
            ensure!(bytes <= 4 * MAX_BYTES, "K2 input budget");
            field(&mut h, path.as_os_str().as_encoded_bytes());
            field(&mut h, analysis::hash(path)?.as_bytes());
        }
        Ok(h.finalize().to_hex().to_string())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    directory: bool,
    bytes: u64,
    hash: String,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    schema: u32,
    key: String,
    slot: String,
    entries: Vec<Entry>,
}
fn inventory(slot: &Path) -> Result<Vec<Entry>> {
    let mut result = vec![];
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(slot).min_depth(1).max_depth(64) {
        let entry = entry?;
        let p = entry.path();
        let meta = std::fs::symlink_metadata(p)?;
        ensure!(
            !meta.file_type().is_symlink() && (meta.is_dir() || meta.is_file()),
            "K2 profile contains a link or special file"
        );
        ensure!(
            entry.depth() < 64 || !meta.is_dir(),
            "K2 profile depth limit"
        );
        total = total
            .checked_add(meta.len())
            .context("K2 profile size overflow")?;
        ensure!(
            total <= MAX_BYTES && result.len() < MAX_ENTRIES,
            "K2 profile inventory limit"
        );
        result.push(Entry {
            path: p.strip_prefix(slot)?.to_owned(),
            directory: meta.is_dir(),
            bytes: if meta.is_dir() { 0 } else { meta.len() },
            hash: if meta.is_dir() {
                String::new()
            } else {
                analysis::hash(p)?
            },
        });
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}
fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("K2 receipt parent")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn clean(parent: &Path, key: &str) -> Result<(PathBuf, usize, Vec<u8>)> {
    let pointer = parent.join("current.json");
    let meta = std::fs::symlink_metadata(&pointer)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 4 * 1024 * 1024,
        "invalid K2 clean receipt"
    );
    let bytes = std::fs::read(&pointer)?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    ensure!(
        receipt.schema == 1
            && receipt.key == key
            && receipt.slot.starts_with("slot-")
            && Path::new(&receipt.slot).components().count() == 1,
        "foreign K2 clean receipt"
    );
    let slot = parent.join(&receipt.slot);
    ensure!(
        slot.is_dir() && !slot.is_symlink() && slot.canonicalize()?.parent() == Some(parent),
        "K2 slot escaped entry"
    );
    let actual = inventory(&slot)?;
    ensure!(
        actual == receipt.entries,
        "K2 profile inventory/hash mismatch"
    );
    ensure!(
        slot.join("workspace/workspace.json").is_file() && slot.join("system/workspaces").is_dir(),
        "K2 profile has no imported index"
    );
    let count = actual.iter().filter(|v| !v.directory).count();
    Ok((slot, count, bytes))
}

fn admit(base: &Path, locks: &Path, current: &str) -> Result<()> {
    let admission = blake3::hash(b"k2-profile-admission-v1")
        .to_hex()
        .to_string();
    let _admission = Guard::k2(locks, &admission)?.context("K2 cache admission busy")?;
    let mut slots = 0usize;
    let mut entries = 0usize;
    let mut candidates = vec![];
    for keydir in std::fs::read_dir(base)? {
        let keydir = keydir?;
        entries += 1;
        ensure!(entries <= MAX_ENTRIES, "K2 cache catalog limit");
        if keydir.file_name() == "locks" {
            continue;
        }
        let name = keydir.file_name().to_string_lossy().into_owned();
        ensure!(
            keydir.file_type()?.is_dir()
                && name.len() == 64
                && name.bytes().all(|v| v.is_ascii_hexdigit()),
            "invalid K2 cache key directory"
        );
        for entry in std::fs::read_dir(keydir.path())? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= MAX_ENTRIES, "K2 cache catalog limit");
            if entry.file_name().to_string_lossy().starts_with("slot-") {
                slots += 1;
            }
        }
        if name != current
            && let Ok(meta) = std::fs::symlink_metadata(keydir.path().join("current.json"))
            && meta.is_file()
            && !meta.file_type().is_symlink()
        {
            candidates.push((meta.modified()?, name, keydir.path()));
        }
    }
    if slots < 8 {
        return Ok(());
    }
    candidates.sort();
    for (_, key, parent) in candidates {
        let Some(_key_guard) = Guard::k2(locks, &key)? else {
            continue;
        };
        let Ok((slot, _, _)) = clean(&parent, &key) else {
            continue;
        };
        // This fully verified, stopped profile has an available locator and its
        // own exclusive key lease. Never remove unpointed/dirty or live slots.
        std::fs::remove_file(parent.join("current.json"))?;
        std::fs::remove_dir_all(slot)?;
        slots -= 1;
        if slots < 8 {
            return Ok(());
        }
    }
    anyhow::bail!(
        "K2 retained-slot capacity has no evictable stopped profile; use an independent profile"
    )
}
pub(crate) struct Slot {
    _guard: Guard,
    parent: PathBuf,
    path: PathBuf,
    inputs: Inputs,
    key: String,
    launched: bool,
    restorable: Option<Vec<u8>>,
}
impl Slot {
    pub fn acquire(base: &Path, inputs: Inputs) -> Result<Option<(Self, Record)>> {
        let key = inputs.key()?;
        std::fs::create_dir_all(base)?;
        let base = base.canonicalize()?;
        let locks = base.join("locks");
        std::fs::create_dir_all(&locks)?;
        ensure!(!locks.is_symlink(), "K2 cache lock directory is a link");
        let Some(guard) = Guard::k2(&locks, &key)? else {
            return Ok(None);
        };
        let parent = base.join(&key);
        std::fs::create_dir_all(&parent)?;
        ensure!(
            !parent.is_symlink() && parent.canonicalize()?.parent() == Some(base.as_path()),
            "invalid K2 cache entry directory"
        );
        let pointer = parent.join("current.json");
        let cached = clean(&parent, &key).and_then(|entry| {
            // Consume before ANY child starts. Dirty/aborted writers never leave
            // an available locator, even if their Rust owner subsequently drops.
            std::fs::remove_file(&pointer)?;
            Ok(entry)
        });
        let (path, reused_files, status, restorable, fresh) = match cached {
            Ok((path, count, bytes)) => (path, count, "hit", Some(bytes), None),
            Err(_) => {
                // Admission is brief and process-owned. Never evict a dirty slot:
                // an orphaned server might still be writing it after client Drop.
                admit(&base, &locks, &key)?;
                let slot = tempfile::Builder::new()
                    .prefix("slot-")
                    .tempdir_in(&parent)?;
                let path = slot.path().to_owned();
                (path, 0, "miss", None, Some(slot))
            }
        };
        let workspace = path.join("workspace");
        let system = path.join("system");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(&system)?;
        let record = Record {
            status: status.into(),
            key: Some(key.clone()),
            workspace,
            system,
            reused_files,
            detail: None,
        };
        let value = Self {
            _guard: guard,
            parent,
            path,
            inputs,
            key,
            launched: false,
            restorable,
        };
        if let Some(fresh) = fresh {
            let _path = fresh.keep();
        }
        Ok(Some((value, record)))
    }
    pub fn mark_launched(&mut self) {
        self.launched = true;
    }
    pub fn launch_failed(&mut self) {
        // Only the typed spawn error path calls this: no child was returned.
        self.launched = false;
    }
    /// Caller must have observed actual successful child exit AND a genuine
    /// import/diagnostic checkpoint. A flag alone must never call this method.
    pub fn seal(self, clean_and_ready: bool) -> Result<bool> {
        if !clean_and_ready {
            return Ok(false);
        }
        ensure!(
            self.inputs.key()? == self.key,
            "K2 inputs changed while server ran"
        );
        ensure!(
            analysis::hash(&self.path.join("workspace/workspace.json"))?
                == self.inputs.workspace_hash,
            "K2 workspace changed while server ran"
        );
        let entries = inventory(&self.path)?;
        ensure!(
            self.path.join("system/workspaces").is_dir(),
            "K2 never created an imported index"
        );
        ensure!(
            entries
                .iter()
                .any(|e| !e.directory && e.bytes > 0 && e.path.starts_with("system/workspaces")),
            "K2 produced no persistent index bytes"
        );
        let receipt = Receipt {
            schema: 1,
            key: self.key.clone(),
            slot: self
                .path
                .file_name()
                .context("K2 slot name")?
                .to_string_lossy()
                .into_owned(),
            entries,
        };
        let bytes = serde_json::to_vec(&receipt)?;
        ensure!(bytes.len() <= 4 * 1024 * 1024, "K2 receipt size limit");
        publish(&self.parent.join("current.json"), &bytes)?;
        Ok(true)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        if self.launched {
            return;
        }
        if let Some(bytes) = self.restorable.take() {
            // A cancelled blocking preparation never started a child. Restore the
            // consumed clean locator, but never overwrite a newer external pointer.
            let _ = (|| -> Result<()> {
                let mut file = tempfile::NamedTempFile::new_in(&self.parent)?;
                file.write_all(&bytes)?;
                file.persist_noclobber(self.parent.join("current.json"))
                    .map_err(|e| e.error)?;
                Ok(())
            })();
        } else {
            // Only this never-launched fresh UUID belongs to us. Large dirty
            // profiles whose child started are deliberately never deleted here.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn inputs(root: &Path) -> Inputs {
        let file = root.join("tool");
        if !file.exists() {
            std::fs::write(&file, b"actual unit bytes").unwrap();
        }
        Inputs {
            seed: b"unit context".to_vec(),
            files: vec![file],
            workspace_hash: blake3::hash(b"{}").to_hex().to_string(),
        }
    }
    fn acquire(root: &Path) -> (Slot, Record) {
        let (mut slot, record) = Slot::acquire(&root.join("cache"), inputs(root))
            .unwrap()
            .unwrap();
        slot.mark_launched();
        (slot, record)
    }
    #[test]
    fn abandoned_pre_spawn_cold_preparation_does_not_consume_slot_capacity() {
        let root = tempfile::tempdir().unwrap();
        for _ in 0..12 {
            let (slot, record) = Slot::acquire(&root.path().join("cache"), inputs(root.path()))
                .unwrap()
                .unwrap();
            let path = record.workspace.parent().unwrap().to_owned();
            drop(slot);
            assert!(!path.exists());
        }
    }
    #[test]
    fn abandoned_pre_spawn_hit_restores_the_same_clean_locator() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        s.seal(true).unwrap();
        for _ in 0..12 {
            let (mut slot, record) = Slot::acquire(&root.path().join("cache"), inputs(root.path()))
                .unwrap()
                .unwrap();
            assert_eq!(record.status, "hit");
            assert_eq!(record.workspace, r.workspace);
            slot.mark_launched();
            slot.launch_failed();
            drop(slot);
        }
    }
    fn payload(record: &Record) {
        std::fs::write(record.workspace.join("workspace.json"), b"{}").unwrap();
        let dir = record
            .system
            .join("workspaces/unit/index/kotlin-server/rocks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("data.sst"),
            b"unit opaque profile bytes, NOT a semantic server",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn cancelled_blocking_preparation_restores_its_unstarted_hit() {
        let root = tempfile::tempdir().unwrap();
        let (slot, record) = acquire(root.path());
        payload(&record);
        slot.seal(true).unwrap();
        let path = root.path().to_owned();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let handle = tokio::task::spawn_blocking(move || {
            let (slot, record) = Slot::acquire(&path.join("cache"), inputs(&path))
                .unwrap()
                .unwrap();
            entered.send(record).unwrap();
            wait.recv().unwrap();
            slot
        });
        let hit = ready.await.unwrap();
        assert_eq!(hit.status, "hit");
        drop(handle);
        release.send(()).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let key = record.key.as_deref().unwrap();
            let parent = root.path().join("cache").join(key);
            if parent.join("current.json").exists()
                && let Some(_guard) = Guard::k2(&root.path().join("cache/locks"), key).unwrap()
            {
                let (path, _, _) = clean(&parent, key).unwrap();
                assert_eq!(path.join("workspace"), record.workspace);
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    #[test]
    fn only_sealed_profile_reopens_identical_workspace_and_inventory() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        assert_eq!(r.status, "miss");
        payload(&r);
        assert!(s.seal(true).unwrap());
        let (s2, r2) = acquire(root.path());
        assert_eq!(r2.status, "hit");
        assert_eq!(r2.workspace, r.workspace);
        assert!(r2.reused_files >= 2);
        assert!(s2.seal(false).is_ok());
    }
    #[test]
    fn busy_key_never_opens_or_mutates_the_active_workspace() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        assert!(
            Slot::acquire(&root.path().join("cache"), inputs(root.path()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read(r.workspace.join("workspace.json")).unwrap(),
            b"{}"
        );
        drop(s);
        let (_, r2) = acquire(root.path());
        assert_ne!(r.workspace, r2.workspace);
        assert_eq!(r2.status, "miss");
    }
    #[test]
    fn cancelled_consumed_slot_is_quarantined_instead_of_reused_or_deleted() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        s.seal(true).unwrap();
        let (s2, r2) = acquire(root.path());
        assert_eq!(r2.status, "hit");
        drop(s2);
        let (_, r3) = acquire(root.path());
        assert_eq!(r3.status, "miss");
        assert_ne!(r3.workspace, r.workspace);
        assert!(r.workspace.join("workspace.json").exists());
    }
    #[test]
    fn changed_missing_or_injected_profile_bytes_are_not_trusted() {
        for mode in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let (s, r) = acquire(root.path());
            payload(&r);
            s.seal(true).unwrap();
            let file = r
                .system
                .join("workspaces/unit/index/kotlin-server/rocks/data.sst");
            match mode {
                0 => std::fs::write(&file, b"tampered").unwrap(),
                1 => std::fs::remove_file(&file).unwrap(),
                _ => std::fs::write(r.system.join("extra"), b"injected").unwrap(),
            }
            let (_, r2) = acquire(root.path());
            assert_eq!(r2.status, "miss");
            assert_ne!(r2.workspace, r.workspace);
            assert!(r.workspace.exists());
        }
    }
    #[test]
    fn tool_change_prevents_seal_and_selects_a_different_context_key() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        std::fs::write(root.path().join("tool"), b"changed bytes").unwrap();
        assert!(s.seal(true).is_err());
        let (_, r2) = acquire(root.path());
        assert_ne!(r2.key, r.key);
        assert_ne!(r2.workspace, r.workspace);
    }
    #[test]
    fn unimported_profile_and_changed_workspace_cannot_be_sealed() {
        for mode in 0..2 {
            let root = tempfile::tempdir().unwrap();
            let (s, r) = acquire(root.path());
            if mode == 1 {
                payload(&r);
                std::fs::write(r.workspace.join("workspace.json"), b"changed model").unwrap();
            }
            assert!(s.seal(true).is_err());
            let (_, r2) = acquire(root.path());
            assert_eq!(r2.status, "miss");
            assert_ne!(r2.workspace, r.workspace);
        }
    }
    #[test]
    fn profile_symlinks_cannot_be_imported_or_sealed() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        std::os::unix::fs::symlink(root.path().join("tool"), r.system.join("link")).unwrap();
        assert!(s.seal(true).is_err());
    }
    #[test]
    fn foreign_or_malformed_clean_receipt_is_a_new_slot_not_a_traversal() {
        for value in [
            "{malformed",
            r#"{"schema":1,"key":"foreign","slot":"../escape","entries":[]}"#,
        ] {
            let root = tempfile::tempdir().unwrap();
            let (s, r) = acquire(root.path());
            payload(&r);
            let parent = s.parent.clone();
            s.seal(true).unwrap();
            std::fs::write(parent.join("current.json"), value).unwrap();
            let (_, r2) = acquire(root.path());
            assert_eq!(r2.status, "miss");
            assert_ne!(r2.workspace, r.workspace);
        }
    }
    #[test]
    fn retained_slot_admission_is_bounded_without_deleting_dirty_slots() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = vec![];
        for _ in 0..8 {
            let (s, r) = acquire(root.path());
            paths.push(r.workspace.clone());
            drop(s);
        }
        assert!(Slot::acquire(&root.path().join("cache"), inputs(root.path())).is_err());
        for p in paths {
            assert!(p.exists());
        }
    }

    #[test]
    fn admission_evicts_only_clean_stopped_profiles_and_keeps_live_slots() {
        let root = tempfile::tempdir().unwrap();
        let mut old = vec![];
        for n in 0..8 {
            let mut input = inputs(root.path());
            input.seed = format!("context-{n}").into_bytes();
            let (mut slot, record) = Slot::acquire(&root.path().join("cache"), input)
                .unwrap()
                .unwrap();
            slot.mark_launched();
            payload(&record);
            slot.seal(true).unwrap();
            old.push(record);
        }
        let mut input = inputs(root.path());
        input.seed = b"context-0".to_vec();
        let (mut active, live) = Slot::acquire(&root.path().join("cache"), input)
            .unwrap()
            .unwrap();
        active.mark_launched();
        let mut input = inputs(root.path());
        input.seed = b"ninth-context".to_vec();
        let (mut slot, record) = Slot::acquire(&root.path().join("cache"), input)
            .unwrap()
            .unwrap();
        slot.mark_launched();
        payload(&record);
        slot.seal(true).unwrap();
        assert!(live.workspace.exists());
        assert!(record.workspace.exists());
        assert_eq!(old.iter().filter(|r| !r.workspace.exists()).count(), 1);
        assert_eq!(old.iter().filter(|r| r.workspace.exists()).count(), 7);
    }
    #[test]
    fn other_process_os_lease_blocks_the_same_key_and_crash_releases_it() {
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        let lock = root
            .path()
            .join("cache/locks")
            .join(format!("{}.lock", r.key.unwrap()));
        let code = "import fcntl,sys; f=open(sys.argv[1],'r+');\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(7)\nsys.exit(0)";
        assert_eq!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", code])
                .arg(&lock)
                .status()
                .unwrap()
                .code(),
            Some(7)
        );
        drop(s);
        assert_eq!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", code])
                .arg(&lock)
                .status()
                .unwrap()
                .code(),
            Some(0)
        );
        use std::os::unix::process::ExitStatusExt;
        let crash = "import fcntl,os,sys; f=open(sys.argv[1],'r+');fcntl.flock(f,fcntl.LOCK_EX);os.kill(os.getpid(),9)";
        assert_eq!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", crash])
                .arg(&lock)
                .status()
                .unwrap()
                .signal(),
            Some(9)
        );
        let (_, r2) = acquire(root.path());
        assert_eq!(r2.status, "miss");
    }
    #[test]
    fn a_still_running_writer_cannot_have_its_consumed_slot_reopened() {
        use std::io::BufRead;
        struct Owned(std::process::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (s, r) = acquire(root.path());
        payload(&r);
        s.seal(true).unwrap();
        let (s, r) = acquire(root.path());
        let path = r.system.join("still-writing");
        let code = "import pathlib,sys,time; p=pathlib.Path(sys.argv[1]);p.write_text('live');print('READY',flush=True);\nwhile True: p.write_text('live');time.sleep(.02)";
        let mut child = Owned(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", code])
                .arg(&path)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        std::io::BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "READY\n");
        drop(s);
        let (_, r2) = acquire(root.path());
        assert_ne!(r2.workspace, r.workspace);
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(path.exists());
    }
}
