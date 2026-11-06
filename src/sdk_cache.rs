//! Per-key OS lock: APK, analysis variants/contexts and Preview can share SDK output.
use anyhow::{Result, ensure};
use std::{fs::File, path::Path};
pub(crate) const ABI: &str = "android-java-image-v1";
pub(crate) struct Guard {
    _file: File,
}
impl Guard {
    pub(crate) fn k2(parent: &Path, key: &str) -> Result<Option<Self>> {
        Self::at(parent, key)
    }
    pub fn acquire(root: &Path, key: &str) -> Result<Option<Self>> {
        Self::at(&root.join(".wex/sdk-cache").join(ABI), key)
    }
    pub fn dex(root: &Path, key: &str) -> Result<Option<Self>> {
        Self::at(
            &root.join(".wex/dex-cache").join(crate::dex_cache::ABI),
            key,
        )
    }
    pub fn resource(root: &Path, key: &str) -> Result<Option<Self>> {
        Self::at(
            &root
                .join(".wex/resource-cache")
                .join(crate::resource_cache::ABI),
            key,
        )
    }
    pub fn editor_save(root: &Path, key: &str) -> Result<Option<Self>> {
        let dir = root.join(".wex/editor-saves/locks");
        std::fs::create_dir_all(&dir)?;
        Self::at(&dir, key)
    }
    #[cfg(unix)]
    fn at(parent: &Path, key: &str) -> Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        ensure!(
            key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
            "cache lock key invalid"
        );
        let path = parent.join(format!("{key}.lock"));
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "SDK lock is not a regular file"
            );
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .write(true)
            .open(path)?;
        // SAFETY: the fd belongs to the live owned File. flock takes no pointers;
        // closing the File releases the advisory lock, including after process crash.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e.into())
        }
    }
    #[cfg(not(unix))]
    fn at(_root: &Path, _key: &str) -> Result<Option<Self>> {
        Ok(None)
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::compile_cache::Cache;
    #[test]
    fn sdk_image_cache_namespace_and_class_cache_do_not_overlap() {
        let d = tempfile::tempdir().unwrap();
        let key = "a".repeat(64);
        let sdk = Cache::sdk_image(d.path(), &key).unwrap();
        let classes = Cache::open(d.path(), "debug", "app", &key).unwrap();
        let image = tempfile::tempdir().unwrap();
        std::fs::create_dir(image.path().join("lib")).unwrap();
        std::fs::write(image.path().join("lib/modules"), "sdk bytes").unwrap();
        sdk.publish(image.path()).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert_eq!(classes.restore(out.path()).unwrap(), None);
        assert_eq!(sdk.restore(out.path()).unwrap(), Some(1));
        assert_eq!(
            std::fs::read_to_string(out.path().join("lib/modules")).unwrap(),
            "sdk bytes"
        );
    }
    #[test]
    fn key_lock_excludes_other_process_and_releases_on_close() {
        let d = tempfile::tempdir().unwrap();
        let key = "b".repeat(64);
        Cache::sdk_image(d.path(), &key).unwrap();
        let path = d
            .path()
            .join(".wex/sdk-cache")
            .join(ABI)
            .join(format!("{key}.lock"));
        let guard = Guard::acquire(d.path(), &key).unwrap().unwrap();
        assert!(Guard::acquire(d.path(), &key).unwrap().is_none());
        let script = "import fcntl,sys; f=open(sys.argv[1],'r+');\ntry: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(7)\nsys.exit(0)";
        assert_eq!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", script])
                .arg(&path)
                .status()
                .unwrap()
                .code(),
            Some(7)
        );
        drop(guard);
        assert_eq!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", script])
                .arg(path)
                .status()
                .unwrap()
                .code(),
            Some(0)
        );
        assert!(Guard::acquire(d.path(), &key).unwrap().is_some());
    }
    #[test]
    fn process_exit_cannot_leave_a_stale_sdk_lock() {
        let d = tempfile::tempdir().unwrap();
        let key = "c".repeat(64);
        Cache::sdk_image(d.path(), &key).unwrap();
        let path = d
            .path()
            .join(".wex/sdk-cache")
            .join(ABI)
            .join(format!("{key}.lock"));
        std::fs::write(&path, "").unwrap();
        let script = "import os,fcntl,sys; f=open(sys.argv[1],'r+'); fcntl.flock(f,fcntl.LOCK_EX); os._exit(0)";
        assert!(
            std::process::Command::new("/usr/bin/python3")
                .args(["-c", script])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
        assert!(Guard::acquire(d.path(), &key).unwrap().is_some());
    }
}
