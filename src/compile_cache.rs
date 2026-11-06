//! Verified project-local module class outputs. This is not compiler incremental FIR.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;
pub(crate) const ABI: &str = "wex-module-classes-v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub module: String,
    pub key: String,
    pub state: String,
    pub detail: String,
    pub files: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    directory: bool,
    bytes: u64,
    blake3: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    key: String,
    entries: Vec<Entry>,
}
fn field(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}
fn raw(path: &Path, budget: &mut u64) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "cache input must be a regular file: {}",
        path.display()
    );
    *budget = budget
        .checked_add(meta.len())
        .context("cache byte count overflow")?;
    ensure!(*budget <= MAX_BYTES, "cache input exceeds1GiB");
    let mut file = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut b = [0u8; 65536];
    let mut read = 0;
    loop {
        let n = file.read(&mut b)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        ensure!(read <= meta.len(), "cache input grew while reading");
        h.update(&b[..n]);
    }
    ensure!(read == meta.len(), "cache input changed while reading");
    Ok(h.finalize().to_hex().to_string())
}
fn inventory(root: &Path) -> Result<Vec<Entry>> {
    let meta = std::fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "cache tree must be a directory"
    );
    let mut paths = vec![];
    for e in walkdir::WalkDir::new(root).min_depth(1).max_depth(128) {
        let e = e?;
        ensure!(!e.file_type().is_symlink(), "cache tree symlink");
        ensure!(
            e.file_type().is_dir() || e.file_type().is_file(),
            "unsupported cache tree entry"
        );
        ensure!(
            e.depth() < 128 || !e.file_type().is_dir(),
            "cache tree depth limit"
        );
        paths.push(e.into_path());
        ensure!(paths.len() <= MAX_ENTRIES, "cache entry count limit");
    }
    paths.sort();
    let mut budget = 0;
    let mut result = vec![];
    for p in paths {
        let meta = std::fs::symlink_metadata(&p)?;
        let directory = meta.is_dir();
        let blake3 = if directory {
            String::new()
        } else {
            raw(&p, &mut budget)?
        };
        result.push(Entry {
            path: p.strip_prefix(root)?.to_owned(),
            directory,
            bytes: if directory { 0 } else { meta.len() },
            blake3,
        });
    }
    Ok(result)
}
/// JAR timestamps/compression differ each fresh SDK link. Hash ordered entry names
/// and uncompressed bytes instead; duplicates/symlinks/zip bombs are not cacheable.
fn jar(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "invalid classpath JAR"
    );
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    ensure!(zip.len() <= MAX_ENTRIES, "cache JAR entry count limit");
    let mut h = blake3::Hasher::new();
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    let mut b = [0; 65536];
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        ensure!(
            names.insert(f.name().to_owned()),
            "duplicate cache JAR entry"
        );
        ensure!(f.enclosed_name().is_some(), "cache JAR traversal");
        ensure!(
            f.unix_mode().is_none_or(|m| m & 0o170000 != 0o120000),
            "cache JAR symlink"
        );
        field(&mut h, f.name().as_bytes());
        field(&mut h, &[f.is_dir() as u8]);
        total = total.checked_add(f.size()).context("cache JAR overflow")?;
        ensure!(total <= MAX_BYTES, "cache JAR expanded size limit");
        let size = f.size();
        let mut read = 0;
        let mut data = blake3::Hasher::new();
        loop {
            let n = f.read(&mut b)?;
            if n == 0 {
                break;
            }
            read += n as u64;
            ensure!(read <= size, "cache JAR entry grew");
            data.update(&b[..n]);
        }
        ensure!(read == size, "cache JAR entry size mismatch");
        field(&mut h, data.finalize().as_bytes());
    }
    Ok(h.finalize().to_hex().to_string())
}
#[derive(Clone)]
pub(crate) struct Input {
    pub label: String,
    pub path: PathBuf,
    pub semantic_jar: bool,
}
pub(crate) fn fingerprint(seed: &[u8], inputs: &[Input]) -> Result<String> {
    let mut h = blake3::Hasher::new();
    field(&mut h, ABI.as_bytes());
    field(&mut h, seed);
    let mut budget = 0;
    for i in inputs {
        field(&mut h, i.label.as_bytes());
        let meta = std::fs::symlink_metadata(&i.path)?;
        if meta.is_dir() {
            field(&mut h, b"tree");
            field(&mut h, &serde_json::to_vec(&inventory(&i.path)?)?);
        } else if i.semantic_jar {
            field(&mut h, b"jar");
            field(&mut h, jar(&i.path)?.as_bytes());
        } else {
            field(&mut h, b"file");
            field(&mut h, raw(&i.path, &mut budget)?.as_bytes());
        }
    }
    Ok(h.finalize().to_hex().to_string())
}
fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir(path)?;
    }
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "cache directory is not owned regular directory"
    );
    Ok(())
}
pub(crate) struct Cache {
    parent: PathBuf,
    path: PathBuf,
    key: String,
    payload: String,
}
impl Cache {
    pub fn open(root: &Path, variant: &str, module: &str, key: &str) -> Result<Self> {
        ensure!(
            matches!(variant, "debug" | "release"),
            "cache variant invalid"
        );
        ensure!(
            !module.is_empty()
                && module
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            "cache module invalid"
        );
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "cache key invalid"
        );
        let mut p = root.to_owned();
        for name in [".wex", "compile-cache", ABI, variant, module] {
            p = p.join(name);
            directory(&p)?;
        }
        Ok(Self {
            path: p.join(key),
            parent: p,
            key: key.into(),
            payload: "classes".into(),
        })
    }
    pub(crate) fn dex(root: &Path, key: &str) -> Result<Self> {
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "DEX cache key invalid"
        );
        let mut parent = root.to_owned();
        for name in [".wex", "dex-cache", crate::dex_cache::ABI] {
            parent = parent.join(name);
            directory(&parent)?;
        }
        Ok(Self {
            path: parent.join(key),
            parent,
            key: key.into(),
            payload: "dex".into(),
        })
    }
    pub(crate) fn sdk_image(root: &Path, key: &str) -> Result<Self> {
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "SDK cache key invalid"
        );
        let mut parent = root.to_owned();
        for name in [".wex", "sdk-cache", crate::sdk_cache::ABI] {
            parent = parent.join(name);
            directory(&parent)?;
        }
        Ok(Self {
            path: parent.join(key),
            parent,
            key: key.into(),
            payload: "image".into(),
        })
    }
    pub(crate) fn resource_file(root: &Path, key: &str) -> Result<Self> {
        Self::resource_namespace(root, key, "resource-cache", "flat")
    }
    pub(crate) fn resource_input(root: &Path, key: &str) -> Result<Self> {
        Self::resource_namespace(root, key, "resource-inputs", "source")
    }
    fn resource_namespace(root: &Path, key: &str, namespace: &str, payload: &str) -> Result<Self> {
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "resource key invalid"
        );
        let mut parent = root.to_owned();
        for p in [".wex", namespace, crate::resource_cache::ABI] {
            parent = parent.join(p);
            directory(&parent)?;
        }
        Ok(Self {
            path: parent.join(key),
            parent,
            key: key.into(),
            payload: payload.into(),
        })
    }
    pub(crate) fn payload_root(&self) -> PathBuf {
        self.path.join(&self.payload)
    }
    pub fn restore(&self, out: &Path) -> Result<Option<usize>> {
        if !self.path.try_exists()? {
            return Ok(None);
        }
        directory(&self.path)?;
        let receipt = self.path.join("receipt.json");
        ensure!(
            std::fs::symlink_metadata(&receipt)?.is_file(),
            "missing cache receipt"
        );
        ensure!(
            !std::fs::symlink_metadata(&receipt)?
                .file_type()
                .is_symlink(),
            "cache receipt symlink"
        );
        ensure!(
            std::fs::metadata(&receipt)?.len() <= 16 * 1024 * 1024,
            "cache receipt too large"
        );
        let r: Receipt = serde_json::from_slice(&std::fs::read(receipt)?)?;
        ensure!(
            r.schema == 1 && r.key == self.key,
            "cache receipt identity mismatch"
        );
        let actual = inventory(&self.path.join(&self.payload))?;
        ensure!(actual == r.entries, "cache output inventory/hash mismatch");
        ensure!(
            inventory(out)?.is_empty(),
            "cache destination must be empty"
        );
        copy(&self.path.join(&self.payload), out, &actual)?;
        ensure!(inventory(out)? == r.entries, "cache changed during restore");
        Ok(Some(actual.iter().filter(|e| !e.directory).count()))
    }
    pub fn publish(&self, out: &Path) -> Result<usize> {
        let entries = inventory(out)?;
        let tmp = tempfile::Builder::new()
            .prefix("entry-")
            .tempdir_in(&self.parent)?;
        let classes = tmp.path().join(&self.payload);
        std::fs::create_dir(&classes)?;
        copy(out, &classes, &entries)?;
        ensure!(
            inventory(&classes)? == entries,
            "class output changed during cache publication"
        );
        std::fs::write(
            tmp.path().join("receipt.json"),
            serde_json::to_vec_pretty(&Receipt {
                schema: 1,
                key: self.key.clone(),
                entries: entries.clone(),
            })?,
        )?;
        let old = tempfile::Builder::new()
            .prefix("replaced-")
            .tempdir_in(&self.parent)?;
        if self.path.try_exists()? {
            directory(&self.path)?;
            std::fs::rename(&self.path, old.path().join("old"))?;
        }
        std::fs::rename(tmp.path(), &self.path)?;
        Ok(entries.iter().filter(|e| !e.directory).count())
    }
}
fn copy(from: &Path, to: &Path, entries: &[Entry]) -> Result<()> {
    for e in entries {
        ensure!(
            !e.path.as_os_str().is_empty()
                && e.path
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_))),
            "cache relative path invalid"
        );
        let target = to.join(&e.path);
        if e.directory {
            std::fs::create_dir_all(target)?;
        } else {
            std::fs::create_dir_all(target.parent().context("cache file parent")?)?;
            std::fs::copy(from.join(&e.path), target)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn input(path: &Path) -> Input {
        Input {
            label: "cp0".into(),
            path: path.into(),
            semantic_jar: false,
        }
    }
    #[test]
    fn key_tracks_contents_names_order_and_seed_not_metadata() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A.kt");
        let b = d.path().join("B.kt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let inputs = vec![input(&a), input(&b)];
        let first = fingerprint(b"debug17", &inputs).unwrap();
        std::fs::write(&a, "a").unwrap();
        assert_eq!(fingerprint(b"debug17", &inputs).unwrap(), first);
        assert_ne!(fingerprint(b"release17", &inputs).unwrap(), first);
        let mut rev = inputs.clone();
        rev.reverse();
        assert_ne!(fingerprint(b"debug17", &rev).unwrap(), first);
        std::fs::write(&a, "changed").unwrap();
        assert_ne!(fingerprint(b"debug17", &inputs).unwrap(), first);
    }
    #[test]
    fn restored_outputs_are_copies_and_tamper_extra_missing_files_rejected() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("out");
        std::fs::create_dir_all(source.join("pkg")).unwrap();
        std::fs::write(source.join("pkg/A.class"), "compiled").unwrap();
        let key = "a".repeat(64);
        let cache = Cache::open(d.path(), "debug", "app", &key).unwrap();
        assert_eq!(cache.publish(&source).unwrap(), 1);
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        assert_eq!(cache.restore(&dest).unwrap(), Some(1));
        std::fs::write(dest.join("pkg/A.class"), "edit-copy").unwrap();
        assert_eq!(
            std::fs::read_to_string(cache.path.join("classes/pkg/A.class")).unwrap(),
            "compiled"
        );
        for damage in ["tamper", "extra", "missing"] {
            cache.publish(&source).unwrap();
            let target = cache.path.join("classes/pkg/A.class");
            match damage {
                "tamper" => std::fs::write(&target, "corrupt").unwrap(),
                "extra" => std::fs::write(cache.path.join("classes/Extra.class"), "extra").unwrap(),
                _ => std::fs::remove_file(&target).unwrap(),
            };
            let empty = tempfile::tempdir().unwrap();
            assert!(cache.restore(empty.path()).is_err());
            assert!(inventory(empty.path()).unwrap().is_empty());
        }
    }
    #[test]
    fn key_tree_tracks_removed_classes_and_kotlin_module_metadata() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("A.class"), "a").unwrap();
        let i = vec![input(d.path())];
        let before = fingerprint(b"", &i).unwrap();
        std::fs::write(d.path().join("app.kotlin_module"), "metadata").unwrap();
        assert_ne!(fingerprint(b"", &i).unwrap(), before);
        std::fs::remove_file(d.path().join("app.kotlin_module")).unwrap();
        assert_eq!(fingerprint(b"", &i).unwrap(), before);
        std::fs::remove_file(d.path().join("A.class")).unwrap();
        assert_ne!(fingerprint(b"", &i).unwrap(), before);
    }
    #[test]
    fn jars_ignore_zip_timestamps_but_preserve_payload_and_order() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("r.jar");
        let make = |year, text: &str, reverse| {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
            let options = SimpleFileOptions::default().last_modified_time(
                zip::DateTime::from_date_and_time(year, 1, 1, 0, 0, 0).unwrap(),
            );
            let mut entries = vec![("A.class", text), ("B.class", "b")];
            if reverse {
                entries.reverse();
            }
            for (n, s) in entries {
                z.start_file(n, options).unwrap();
                z.write_all(s.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        };
        make(2000, "a", false);
        let first = jar(&p).unwrap();
        make(2026, "a", false);
        assert_eq!(jar(&p).unwrap(), first);
        make(2026, "change", false);
        assert_ne!(jar(&p).unwrap(), first);
        make(2026, "a", true);
        assert_ne!(jar(&p).unwrap(), first);
    }
    #[test]
    fn receipt_identity_and_paths_are_validated_and_empty_module_is_cacheable() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let cache = Cache::open(d.path(), "debug", "app", &"b".repeat(64)).unwrap();
        cache.publish(&out).unwrap();
        let dest = tempfile::tempdir().unwrap();
        assert_eq!(cache.restore(dest.path()).unwrap(), Some(0));
        let receipt = cache.path.join("receipt.json");
        let mut j: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        j["key"] = "bad".into();
        std::fs::write(&receipt, serde_json::to_vec(&j).unwrap()).unwrap();
        assert!(cache.restore(dest.path()).is_err());
        for (variant, module, key) in [
            ("preview", "app", "a".repeat(64)),
            ("debug", "../escape", "a".repeat(64)),
            ("debug", "app", "../../bad".into()),
        ] {
            assert!(Cache::open(d.path(), variant, module, &key).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlink_tree_cache_entry_and_receipt_never_followed() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("real");
        std::fs::write(&file, "real").unwrap();
        let tree = d.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        symlink(&file, tree.join("link")).unwrap();
        assert!(fingerprint(b"", &[input(&tree)]).is_err());
        let key = "c".repeat(64);
        let cache = Cache::open(d.path(), "debug", "app", &key).unwrap();
        symlink(&tree, &cache.path).unwrap();
        let empty = tempfile::tempdir().unwrap();
        assert!(cache.restore(empty.path()).is_err());
        assert!(cache.publish(empty.path()).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "real");
    }
}
