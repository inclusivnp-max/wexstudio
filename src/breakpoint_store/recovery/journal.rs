//! Linux journal ownership: fd lock, directory anchor, atomic no-replace publication.
use super::*;
use std::{
    fs::File,
    time::{Duration, Instant},
};
const LOCK: &str = ".journal.lock";
pub(super) struct Journal {
    root: PathBuf,
    dir: Arc<File>,
    _lock: File,
}
pub(super) struct Stage {
    file: Option<tempfile::NamedTempFile>,
    _dir: Arc<File>,
}
impl std::ops::Deref for Stage {
    type Target = tempfile::NamedTempFile;
    fn deref(&self) -> &Self::Target {
        self.file.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for Stage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.file.as_mut().unwrap()
    }
}
impl Journal {
    pub fn open(root: &Path, create: bool, wait: Duration) -> Result<Option<Self>> {
        if create {
            std::fs::create_dir_all(root)?;
        }
        if !checked_dir(root)? {
            return Ok(None);
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let dir = Arc::new(options.open(root)?);
        #[cfg(unix)]
        if create {
            use std::os::unix::fs::PermissionsExt;
            dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        let anchor = Self::anchor_for(&dir, root);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .mode(0o600);
        }
        let lock = options.open(anchor.join(LOCK))?;
        ensure!(
            lock.metadata()?.is_file() && lock.metadata()?.len() == 0,
            "Recovery lock is not an empty regular file"
        );
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let deadline = Instant::now() + wait;
            loop {
                if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                ensure!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ),
                    "Recovery lock: {error}"
                );
                ensure!(
                    Instant::now() < deadline,
                    "Recovery journal busy; another process owns the OS lock · Retry"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        #[cfg(not(unix))]
        anyhow::bail!("Recovery journal requires OS file locking");
        let journal = Self {
            root: root.to_owned(),
            dir,
            _lock: lock,
        };
        journal.check()?;
        Ok(Some(journal))
    }
    fn anchor_for(dir: &File, root: &Path) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let _ = root;
            PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dir;
            root.to_owned()
        }
    }
    fn anchor(&self) -> PathBuf {
        Self::anchor_for(&self.dir, &self.root)
    }
    pub fn check(&self) -> Result<()> {
        ensure!(checked_dir(&self.root)?, "Recovery directory disappeared");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let actual = std::fs::symlink_metadata(&self.root)?;
            let owned = self.dir.metadata()?;
            ensure!(
                (actual.dev(), actual.ino()) == (owned.dev(), owned.ino()),
                "Recovery directory identity changed"
            );
            let actual = std::fs::symlink_metadata(self.anchor().join(LOCK))?;
            let owned = self._lock.metadata()?;
            ensure!(
                !actual.file_type().is_symlink()
                    && (actual.dev(), actual.ino()) == (owned.dev(), owned.ino()),
                "Recovery lock identity changed; no publication"
            );
        }
        Ok(())
    }
    pub fn names(&self) -> Result<Vec<String>> {
        self.check()?;
        let mut names = vec![];
        for entry in std::fs::read_dir(self.anchor())? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Invalid recovery filename"))?;
            if name == LOCK {
                continue;
            }
            ensure!(
                names.len() < 16,
                "Recovery journal limit16 reached; review retained entries before saving more"
            );
            ensure!(
                valid_name(&name) || pending_name(&name),
                "Unexpected recovery entry; existing bytes retained"
            );
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
    pub fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        ensure!(valid_name(name), "Invalid recovery filename");
        self.check()?;
        super::read(&self.anchor().join(name), limit)
    }
    pub fn stage(&self) -> Result<Stage> {
        ensure!(
            self.names()?.len() < 16,
            "Recovery journal full; no existing entry removed"
        );
        Ok(Stage {
            file: Some(
                tempfile::Builder::new()
                    .prefix(".pending-")
                    .suffix(".tmp")
                    .tempfile_in(self.anchor())?,
            ),
            _dir: self.dir.clone(),
        })
    }

    pub fn publish(&self, mut stage: Stage) -> Result<PathBuf> {
        self.check()?;
        let file = stage.file.take().unwrap();
        ensure!(
            file.path().parent() == Some(self.anchor().as_path()),
            "Foreign recovery stage"
        );
        let name = file
            .path()
            .file_name()
            .and_then(|p| p.to_str())
            .context("Invalid recovery stage")?;
        ensure!(pending_name(name), "Invalid recovery stage name");
        let salt = name
            .strip_prefix(".pending-")
            .unwrap()
            .strip_suffix(".tmp")
            .unwrap();
        let name = format!("draft-{salt}.json");
        file.as_file().sync_all()?;
        // PersistError owns its temporary path. Dispose it while the directory
        // anchor is still alive; never let a /proc/fd path escape after fd reuse.
        match file.persist_noclobber(self.anchor().join(&name)) {
            Ok(file) => drop(file),
            Err(error) => {
                drop(error.file);
                return Err(error.error.into());
            }
        }
        self.dir.sync_all()?;
        self.check()?;
        Ok(self.root.join(name))
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        ensure!(valid_name(name), "Invalid recovery filename");
        self.check()?;
        std::fs::remove_file(self.anchor().join(name))?;
        self.dir.sync_all()?;
        Ok(())
    }
}
pub(super) fn pending_name(name: &str) -> bool {
    name.len() <= 96
        && name.starts_with(".pending-")
        && name.ends_with(".tmp")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Seek, SeekFrom},
        process::{Child, ChildStdout, Command, Stdio},
    };
    const HELPER: &str = "breakpoint_store::recovery::journal::tests::process_helper";
    fn local(root: &Path, id: u64) -> Preferences {
        let mut p = Preferences::default();
        p.files.insert(
            root.join("Main.kt"),
            vec![StoredMarker {
                id,
                line: 1,
                source_hash: Some(blake3::hash(b"target();\n").to_hex().to_string()),
                span: Some([0, 9]),
                reason: None,
            }],
        );
        p
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Main.kt"), b"target();\n").unwrap();
        let dir = t.path().join("journal");
        (t, root, dir)
    }
    fn save_one(root: &Path, dir: &Path, id: u64) -> PathBuf {
        super::super::save(
            root,
            dir,
            &Preferences::default(),
            &local(root, id),
            "journal protocol fixture",
        )
        .unwrap()
    }
    struct Peer {
        child: Child,
        reader: Option<BufReader<ChildStdout>>,
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            if self.child.try_wait().unwrap().is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
    impl Peer {
        fn new(root: &Path, dir: &Path, id: u64, action: &str) -> Self {
            let request = serde_json::json!({"root":root,"dir":dir,"id":id,"action":action});
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER, "--ignored", "--nocapture"])
                .env("WEX_JOURNAL_CHILD", request.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::sync_channel(1);
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        let _ = tx.send(Err("child ended before real checkpoint"));
                        break;
                    }
                    if line.trim_end().ends_with("JOURNAL_READY") {
                        let _ = tx.send(Ok(reader));
                        break;
                    }
                }
            });
            match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(Ok(reader)) => Self {
                    child,
                    reader: Some(reader),
                },
                error => {
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "journal checkpoint: {error:?}; {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        fn release(&mut self) {
            self.child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        }
        fn done(mut self) -> String {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success(), "journal helper exit {status}");
                    break;
                }
                assert!(Instant::now() < deadline, "journal helper did not finish");
                thread::sleep(Duration::from_millis(2));
            }
            let mut output = String::new();
            self.reader
                .take()
                .unwrap()
                .take(65536)
                .read_to_string(&mut output)
                .unwrap();
            assert!(output.contains("JOURNAL_RESULT"), "{output}");
            output
        }
        fn crash(&mut self) {
            self.child.kill().unwrap();
            let status = self.child.wait().unwrap();
            assert!(!status.success());
        }
    }
    #[test]
    #[ignore = "owned subprocess fixture, explicitly executed by the journal parent tests"]
    fn process_helper() {
        let request: serde_json::Value = serde_json::from_str(
            &std::env::var("WEX_JOURNAL_CHILD").expect("journal helper requires a request"),
        )
        .unwrap();
        let root = Path::new(request["root"].as_str().unwrap());
        let dir = Path::new(request["dir"].as_str().unwrap());
        let id = request["id"].as_u64().unwrap();
        let action = request["action"].as_str().unwrap();
        if action == "stage" {
            let journal = Journal::open(dir, true, Duration::from_secs(2))
                .unwrap()
                .unwrap();
            let mut stage = journal.stage().unwrap();
            stage.write_all(b"{").unwrap();
            stage.as_file().sync_all().unwrap();
            println!("JOURNAL_READY");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            stage.as_file_mut().set_len(0).unwrap();
            stage.seek(SeekFrom::Start(0)).unwrap();
            let draft = Draft {
                schema: 1,
                root: root.to_owned(),
                base: Preferences::default(),
                local: local(root, id),
                reason: "gated actual journal publication".into(),
            };
            stage
                .write_all(&serde_json::to_vec(&draft).unwrap())
                .unwrap();
            let path = journal.publish(stage).unwrap();
            println!("JOURNAL_RESULT ok {}", path.display());
        } else {
            println!("JOURNAL_READY");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            match super::super::save(
                root,
                dir,
                &Preferences::default(),
                &local(root, id),
                "concurrent actual writer",
            ) {
                Ok(path) => println!("JOURNAL_RESULT ok {}", path.display()),
                Err(error) => println!("JOURNAL_RESULT error {error:#}"),
            }
        }
    }
    #[test]
    fn partial_write_is_not_published_and_readers_observe_busy_then_complete_json() {
        let (_t, root, dir) = fixture();
        save_one(&root, &dir, 1);
        let before = super::super::list_at(&root, &dir).unwrap();
        let mut writer = Peer::new(&root, &dir, 2, "stage");
        assert!(
            super::super::list_at(&root, &dir)
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter(|e| valid_name(e.as_ref().unwrap().file_name().to_str().unwrap()))
                .count(),
            1
        );
        writer.release();
        assert!(writer.done().contains("JOURNAL_RESULT ok"));
        let after = super::super::list_at(&root, &dir).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|r| r.issue.is_none()));
        assert!(after.iter().any(|r| r.name == before[0].name));
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .all(|e| !pending_name(e.unwrap().file_name().to_str().unwrap()))
        );
    }
    #[test]
    fn killed_writer_releases_fd_lock_but_keeps_staging_visible_without_poisoning_good_drafts() {
        let (_t, root, dir) = fixture();
        let good = save_one(&root, &dir, 1);
        let bytes = std::fs::read(&good).unwrap();
        let mut writer = Peer::new(&root, &dir, 2, "stage");
        writer.crash();
        assert!(
            dir.join(LOCK).exists(),
            "lock filename is retained, never used as liveness"
        );
        let list = super::super::list_at(&root, &dir).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().filter(|r| r.issue.is_none()).count(), 1);
        let pending = list.iter().find(|r| r.issue.is_some()).unwrap();
        assert!(pending_name(&pending.name));
        assert_eq!(std::fs::read(dir.join(&pending.name)).unwrap(), b"{");
        assert_eq!(std::fs::read(&good).unwrap(), bytes);
        save_one(&root, &dir, 3);
        assert_eq!(super::super::list_at(&root, &dir).unwrap().len(), 3);
    }
    #[test]
    fn concurrent_last_slot_admission_never_publishes_seventeenth_entry() {
        let (_t, root, dir) = fixture();
        let old = (1..=15)
            .map(|id| {
                let p = save_one(&root, &dir, id);
                let b = std::fs::read(&p).unwrap();
                (p, b)
            })
            .collect::<Vec<_>>();
        let mut a = Peer::new(&root, &dir, 70, "write");
        let mut b = Peer::new(&root, &dir, 71, "write");
        assert_ne!(a.child.id(), b.child.id());
        a.release();
        b.release();
        let outputs = [a.done(), b.done()];
        assert_eq!(
            outputs
                .iter()
                .filter(|s| s.contains("JOURNAL_RESULT ok"))
                .count(),
            1
        );
        assert_eq!(
            outputs
                .iter()
                .filter(|s| s.contains("journal full"))
                .count(),
            1
        );
        assert_eq!(super::super::list_at(&root, &dir).unwrap().len(), 16);
        assert!(old.iter().all(|(p, b)| std::fs::read(p).unwrap() == *b));
    }
    #[test]
    fn concurrent_identical_payloads_deduplicate_under_the_same_os_lock() {
        let (_t, root, dir) = fixture();
        let mut a = Peer::new(&root, &dir, 9, "write");
        let mut b = Peer::new(&root, &dir, 9, "write");
        a.release();
        b.release();
        assert!(a.done().contains("JOURNAL_RESULT ok"));
        assert!(b.done().contains("JOURNAL_RESULT ok"));
        assert_eq!(super::super::list_at(&root, &dir).unwrap().len(), 1);
    }
    #[test]
    fn publication_is_no_replace_and_failed_temp_cleanup_keeps_its_directory_fd_alive() {
        use std::os::fd::AsRawFd;
        let (_t, _root, dir) = fixture();
        let journal = Journal::open(&dir, true, Duration::ZERO).unwrap().unwrap();
        let mut stage = journal.stage().unwrap();
        stage.write_all(b"new").unwrap();
        let name = stage.path().file_name().unwrap().to_str().unwrap();
        let salt = name
            .strip_prefix(".pending-")
            .unwrap()
            .strip_suffix(".tmp")
            .unwrap();
        let target = dir.join(format!("draft-{salt}.json"));
        std::fs::write(&target, b"existing").unwrap();
        assert!(journal.publish(stage).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"existing");
        for file in [&*journal.dir, &journal._lock] {
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        }
        let stage = journal.stage().unwrap();
        let pending = dir.join(stage.path().file_name().unwrap());
        let fd = stage._dir.as_raw_fd();
        drop(journal);
        assert!(PathBuf::from(format!("/proc/self/fd/{fd}")).exists());
        drop(stage);
        assert!(!pending.exists());
    }
    #[test]
    fn refresh_and_retry_rescan_after_real_writer_without_losing_last_good_or_source() {
        let (_t, root, dir) = fixture();
        save_one(&root, &dir, 1);
        let doc = crate::editor::Document::open(root.join("Main.kt")).unwrap();
        let id = doc.text_id().clone();
        let bytes = doc.rope().to_string();
        let mut state = State::new(root.clone(), load(&root));
        state.recovery_root = dir.clone();
        state.action(Action::Refresh).unwrap();
        assert_eq!(state.recoveries.len(), 1);
        let good = state.recoveries[0].name.clone();
        let mut writer = Peer::new(&root, &dir, 2, "stage");
        assert!(
            state
                .action(Action::Refresh)
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        assert_eq!(state.recoveries.len(), 1);
        assert_eq!(state.recoveries[0].name, good);
        writer.crash();
        state.action(Action::Retry).unwrap();
        assert_eq!(state.recoveries.len(), 2);
        assert_eq!(doc.text_id(), &id);
        assert_eq!(doc.rope().to_string(), bytes);
        assert!(state.preferences().files.is_empty());
    }
    #[test]
    fn lock_symlinks_and_directory_replacement_are_rejected_without_touching_foreign_bytes() {
        let (t, _root, dir) = fixture();
        std::fs::create_dir(&dir).unwrap();
        let foreign = t.path().join("foreign");
        std::fs::write(&foreign, b"keep").unwrap();
        std::os::unix::fs::symlink(&foreign, dir.join(LOCK)).unwrap();
        assert!(Journal::open(&dir, false, Duration::ZERO).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        std::fs::remove_file(dir.join(LOCK)).unwrap();
        let journal = Journal::open(&dir, false, Duration::ZERO).unwrap().unwrap();
        let stage = journal.stage().unwrap();
        let moved = t.path().join("moved");
        std::fs::rename(&dir, &moved).unwrap();
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("foreign"), b"untouched").unwrap();
        assert!(journal.publish(stage).is_err());
        assert_eq!(std::fs::read(dir.join("foreign")).unwrap(), b"untouched");
        assert!(
            std::fs::read_dir(moved)
                .unwrap()
                .all(|e| !pending_name(e.unwrap().file_name().to_str().unwrap()))
        );
    }
}
