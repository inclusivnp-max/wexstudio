//! Native recent-project metadata and bounded directory listings. No build or resolve.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub const MAX_RECENT: usize = 24;
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recent {
    pub root: PathBuf,
    pub name: String,
    pub opened_at: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    schema: u32,
    projects: Vec<Recent>,
}
#[derive(Clone, Debug)]
pub struct Store {
    pub path: PathBuf,
}
impl Default for Store {
    fn default() -> Self {
        let path = if let Some(p) = std::env::var_os("WEX_PROJECT_HISTORY") {
            PathBuf::from(p)
        } else {
            let parent = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
                });
            parent.join("wex/recent-projects.json")
        };
        Self {
            path: std::path::absolute(&path).unwrap_or(path),
        }
    }
}
impl Store {
    pub fn new(path: PathBuf) -> Result<Self> {
        Ok(Self {
            path: std::path::absolute(path)?,
        })
    }
    pub fn load(&self) -> Result<Vec<Recent>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e).context("Read recent projects"),
        };
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() <= MAX_HISTORY_BYTES,
            "Recent projects file exceeds limit or is not regular"
        );
        let mut bytes = Vec::new();
        file.take(MAX_HISTORY_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_HISTORY_BYTES,
            "Recent projects file grew past limit"
        );
        let h: History = serde_json::from_slice(&bytes)
            .context("Recent projects file is malformed; original bytes retained")?;
        ensure!(
            h.schema == 1 && h.projects.len() <= MAX_RECENT,
            "Unsupported recent projects schema/count"
        );
        let mut seen = BTreeSet::new();
        for p in &h.projects {
            ensure!(
                p.root.is_absolute()
                    && p.root.as_os_str().len() <= 4096
                    && seen.insert(p.root.clone())
                    && !p.name.trim().is_empty()
                    && p.name.chars().count() <= 160
                    && !p.name.chars().any(char::is_control),
                "Invalid recent project record"
            );
        }
        Ok(h.projects)
    }
    fn update(&self, action: impl FnOnce(&mut Vec<Recent>)) -> Result<Vec<Recent>> {
        let parent = self
            .path
            .parent()
            .context("Recent projects path has no parent")?;
        std::fs::create_dir_all(parent)?;
        let lock = self.path.with_file_name(format!(
            "{}.lock",
            self.path
                .file_name()
                .context("Recent filename missing")?
                .to_string_lossy()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(lock)?;
        ensure!(
            lock.metadata()?.is_file(),
            "Recent lock must be a regular file"
        );
        let _guard = lock_file(lock)?;
        let mut records = self.load()?;
        let original = records.clone();
        action(&mut records);
        records.truncate(MAX_RECENT);
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            &mut temp,
            &History {
                schema: 1,
                projects: records.clone(),
            },
        )?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        ensure!(
            self.load()? == original,
            "Recent projects changed externally; retry"
        );
        temp.persist(&self.path)
            .map_err(|e| e.error)
            .context("Publish recent projects")?;
        File::open(parent)?
            .sync_all()
            .context("Sync recent projects directory")?;
        Ok(records)
    }
    pub fn record(&self, root: &Path) -> Result<Vec<Recent>> {
        let root = root
            .canonicalize()
            .context("Recent project directory missing")?;
        let c = crate::native::Config::load(&root)
            .context("Recent project must have valid wex.json")?;
        let name = c
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect::<String>();
        ensure!(!name.trim().is_empty(), "Recent project name empty");
        let entry = Recent {
            root,
            name,
            opened_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        };
        self.update(move |items| {
            items.retain(|p| p.root != entry.root);
            items.insert(0, entry);
        })
    }
    pub fn remove(&self, root: &Path) -> Result<Vec<Recent>> {
        ensure!(
            root.is_absolute(),
            "Remove requires the stored absolute root"
        );
        // Do not canonicalize: missing/moved directories must remain removable.
        self.update(|items| items.retain(|p| p.root != root))
    }
}
#[cfg(unix)]
struct Lock(File);
#[cfg(unix)]
impl Drop for Lock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
fn lock_file(file: File) -> Result<Lock> {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Lock(file));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::WouldBlock {
            return Err(e).context("Recent projects lock");
        }
        ensure!(Instant::now() < deadline, "Recent projects busy; retry");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(not(unix))]
fn lock_file(_file: File) -> Result<File> {
    anyhow::bail!("Concurrent recent-project storage requires a supported file-lock backend")
}

#[derive(Clone, Debug, Serialize)]
pub struct Folder {
    pub path: PathBuf,
    pub name: String,
    pub symlink: bool,
    pub wex_config: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Listing {
    pub root: PathBuf,
    pub parent: Option<PathBuf>,
    pub folders: Vec<Folder>,
    pub truncated: bool,
    pub scanned: usize,
    pub skipped_errors: usize,
}
pub fn folders(root: &Path, hidden: bool) -> Result<Listing> {
    let root = root.canonicalize().context("Directory unavailable")?;
    ensure!(root.is_dir(), "Choose a directory");
    let mut result = Listing {
        parent: root.parent().map(Path::to_owned),
        root: root.clone(),
        folders: vec![],
        truncated: false,
        scanned: 0,
        skipped_errors: 0,
    };
    for entry in std::fs::read_dir(root)? {
        if result.scanned >= 20_000 || result.folders.len() >= 4096 {
            result.truncated = true;
            break;
        }
        result.scanned += 1;
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                result.skipped_errors += 1;
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(t) => t,
            Err(_) => {
                result.skipped_errors += 1;
                continue;
            }
        };
        if !(kind.is_dir() || kind.is_symlink() && entry.path().is_dir()) {
            continue;
        }
        result.folders.push(Folder {
            wex_config: entry.path().join("wex.json").is_file(),
            path: entry.path(),
            name,
            symlink: kind.is_symlink(),
        });
    }
    result.folders.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.name.cmp(&b.name))
    });
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn project(parent: &Path, n: &str) -> PathBuf {
        crate::scaffold::create(
            parent,
            &crate::scaffold::Options {
                directory: n.into(),
                name: n.into(),
                ..Default::default()
            },
        )
        .unwrap()
        .root
    }
    #[test]
    fn missing_history_read_is_empty_and_writes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let s = Store::new(t.path().join("missing/history.json")).unwrap();
        assert!(s.load().unwrap().is_empty());
        assert!(!t.path().join("missing").exists());
    }
    #[test]
    fn dedup_order_and_removal_never_touch_projects() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "A");
        let b = project(t.path(), "B");
        let s = Store::new(t.path().join("state/history.json")).unwrap();
        s.record(&a).unwrap();
        s.record(&b).unwrap();
        let x = s.record(&a).unwrap();
        assert_eq!(x.iter().map(|p| &p.root).collect::<Vec<_>>(), vec![&a, &b]);
        s.remove(&a).unwrap();
        assert!(a.join("wex.json").is_file());
        std::fs::remove_dir_all(&b).unwrap();
        assert!(s.remove(&b).unwrap().is_empty());
    }
    #[test]
    fn malformed_history_is_not_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "A");
        let p = t.path().join("history.json");
        let s = Store::new(p.clone()).unwrap();
        for text in [
            "not json",
            r#"{"schema":99,"projects":[]}"#,
            r#"{"schema":1,"projects":[{"root":"relative","name":"x","opened_at":1}]}"#,
        ] {
            std::fs::write(&p, text).unwrap();
            assert!(s.load().is_err());
            assert!(s.record(&a).is_err());
            assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        }
    }
    #[test]
    fn concurrent_updates_merge_under_real_file_lock() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "A");
        let b = project(t.path(), "B");
        let s = Store::new(t.path().join("history.json")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [a, b]
            .into_iter()
            .map(|p| {
                let s = s.clone();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    s.record(&p).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.load().unwrap().len(), 2);
    }
    #[test]
    fn history_is_bounded_and_invalid_record_does_not_add() {
        let t = tempfile::tempdir().unwrap();
        let s = Store::new(t.path().join("history.json")).unwrap();
        for n in 0..27 {
            s.record(&project(t.path(), &format!("App{n}"))).unwrap();
        }
        assert_eq!(s.load().unwrap().len(), MAX_RECENT);
        assert_eq!(s.load().unwrap()[0].name, "App26");
        assert!(s.record(t.path()).is_err());
        assert_eq!(s.load().unwrap().len(), MAX_RECENT);
    }
    #[cfg(unix)]
    #[test]
    fn symlink_history_is_not_followed_or_replaced() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "A");
        let p = t.path().join("history.json");
        let other = t.path().join("other");
        std::fs::write(&other, "keep").unwrap();
        std::os::unix::fs::symlink(&other, &p).unwrap();
        let s = Store::new(p.clone()).unwrap();
        assert!(s.load().is_err());
        assert!(s.record(&a).is_err());
        assert!(p.is_symlink());
        assert_eq!(std::fs::read_to_string(other).unwrap(), "keep");
    }
    #[test]
    fn directory_listing_is_sorted_hidden_aware_and_native_marked() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "Z project");
        std::fs::create_dir(t.path().join("alpha")).unwrap();
        std::fs::create_dir(t.path().join(".hidden")).unwrap();
        std::fs::write(t.path().join("file"), "not a folder").unwrap();
        let l = folders(t.path(), false).unwrap();
        assert_eq!(
            l.folders
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "Z project"]
        );
        assert!(l.folders[1].wex_config);
        assert!(!l.truncated);
        assert_eq!(folders(t.path(), true).unwrap().folders.len(), 3);
        assert!(folders(&a.join("missing"), false).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn directory_symlinks_navigate_canonically() {
        let t = tempfile::tempdir().unwrap();
        let p = project(t.path(), "Project");
        std::os::unix::fs::symlink(&p, t.path().join("Alias")).unwrap();
        std::os::unix::fs::symlink(t.path().join("missing"), t.path().join("Broken")).unwrap();
        let l = folders(t.path(), false).unwrap();
        assert!(l.folders.iter().any(|f| f.name == "Alias" && f.symlink));
        assert!(!l.folders.iter().any(|f| f.name == "Broken"));
        assert_eq!(folders(&t.path().join("Alias"), false).unwrap().root, p);
        let s = Store::new(t.path().join("history.json")).unwrap();
        s.record(&t.path().join("Alias")).unwrap();
        s.record(&p).unwrap();
        assert_eq!(s.load().unwrap().len(), 1);
    }
    #[test]
    fn external_history_edit_is_fenced_before_publication() {
        let t = tempfile::tempdir().unwrap();
        let p = project(t.path(), "A");
        let s = Store::new(t.path().join("history.json")).unwrap();
        s.record(&p).unwrap();
        assert!(
            s.update(|_| std::fs::write(&s.path, "external malformed edit").unwrap())
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&s.path).unwrap(),
            "external malformed edit"
        );
    }
}
