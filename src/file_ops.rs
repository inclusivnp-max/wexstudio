//! Reviewed, root-scoped file operations. No replace, no symlink traversal, recoverable trash.
//! All filesystem work belongs on a worker, never on the GPU thread.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

const OWNED: &[&str] = &[".git", ".wex", ".gradle", "build", "target", "node_modules"];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    KotlinClass,
    JavaClass,
    KotlinFile,
    Layout,
    Text,
    Folder,
}
impl Template {
    pub const ALL: [Self; 6] = [
        Self::KotlinClass,
        Self::JavaClass,
        Self::KotlinFile,
        Self::Layout,
        Self::Text,
        Self::Folder,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::KotlinClass => "Kotlin class",
            Self::JavaClass => "Java class",
            Self::KotlinFile => "Kotlin file",
            Self::Layout => "XML layout",
            Self::Text => "Text file",
            Self::Folder => "Folder",
        }
    }
    pub fn render(self, name: &str, package: &str) -> Result<(String, Option<String>)> {
        ensure!(
            !name.is_empty()
                && name.trim() == name
                && !name.contains(['/', '\\', '\0'])
                && name != "."
                && name != "..",
            "Enter a single file or folder name"
        );
        if matches!(self, Self::KotlinClass | Self::JavaClass | Self::KotlinFile) {
            ensure!(identifier(name), "Invalid source identifier");
            ensure!(
                package.is_empty() || package.split('.').all(identifier),
                "Invalid package"
            );
        }
        let kt = if package.is_empty() {
            String::new()
        } else {
            format!("package {package}\n\n")
        };
        Ok(match self {
            Self::KotlinClass => (
                format!("{name}.kt"),
                Some(format!(
                    "{kt}class {name} {{\n    fun message(): String = \"Hello from {name}\"\n}}\n"
                )),
            ),
            Self::JavaClass => (
                format!("{name}.java"),
                Some(format!(
                    "{}public class {name} {{\n    public String message() {{\n        return \"Hello from {name}\";\n    }}\n}}\n",
                    if package.is_empty() {
                        String::new()
                    } else {
                        format!("package {package};\n\n")
                    }
                )),
            ),
            Self::KotlinFile => (format!("{name}.kt"), Some(kt)),
            Self::Layout => {
                ensure!(
                    name.starts_with(|c: char| c.is_ascii_lowercase())
                        && name
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "Layout name must use lowercase resource characters (without .xml)"
                );
                (format!("{name}.xml"), Some("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<FrameLayout xmlns:android=\"http://schemas.android.com/apk/res/android\"\n    android:layout_width=\"match_parent\"\n    android:layout_height=\"match_parent\" />\n".into()))
            }
            Self::Text => (name.into(), Some(String::new())),
            Self::Folder => (name.into(), None),
        })
    }
}
fn identifier(s: &str) -> bool {
    let reserved = "abstract as assert boolean break byte case catch char class const continue default do double else enum extends false final finally float for fun goto if implements import in instanceof int interface is long native new null object package private protected public return short static strictfp super switch synchronized this throw throws transient true try typealias typeof val var void volatile when while";
    let mut c = s.chars();
    c.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && c.all(|c| c == '_' || c.is_alphanumeric())
        && s != "_"
        && !reserved.split_whitespace().any(|v| v == s)
}
#[derive(Clone, Debug)]
pub enum Action {
    Create {
        path: PathBuf,
        contents: Option<String>,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    Trash {
        path: PathBuf,
    },
    Restore,
}
#[derive(Clone, Debug)]
pub struct Prepared {
    pub action: Action,
    stamp: Option<String>,
    receipt: Option<(String, Receipt)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    schema: u32,
    root: PathBuf,
    path: PathBuf,
    stamp: String,
}
#[derive(Debug)]
pub struct Outcome {
    pub from: Option<PathBuf>,
    pub to: Option<PathBuf>,
    pub trashed: bool,
}
fn valid(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty(),
        "Project root is not a file operation target"
    );
    for c in path.components() {
        match c {
            Component::Normal(n) => {
                ensure!(
                    !OWNED.iter().any(|v| n == *v),
                    "Managed output/history paths are excluded"
                );
                ensure!(!n.as_bytes().contains(&0), "NUL path");
            }
            _ => bail!("Use a project-relative path without . or .."),
        }
    }
    Ok(())
}
fn cstr(s: &std::ffi::OsStr) -> Result<CString> {
    Ok(CString::new(s.as_bytes())?)
}
fn open_at(dir: &File, name: &std::ffi::OsStr, flags: i32) -> Result<File> {
    let n = cstr(name)?;
    // SAFETY: a live owned directory fd, terminated path, fixed mode; ownership transfers once.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            n.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    ensure!(
        fd >= 0,
        "Open {}: {}",
        name.to_string_lossy(),
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn mkdir_at(dir: &File, name: &std::ffi::OsStr) -> Result<()> {
    let n = cstr(name)?;
    let r = unsafe { libc::mkdirat(dir.as_raw_fd(), n.as_ptr(), 0o700) };
    if r != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn noreplace(a: &File, an: &std::ffi::OsStr, b: &File, bn: &std::ffi::OsStr) -> Result<()> {
    let an = cstr(an)?;
    let bn = cstr(bn)?;
    let r = unsafe {
        libc::renameat2(
            a.as_raw_fd(),
            an.as_ptr(),
            b.as_raw_fd(),
            bn.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn fdpath(f: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", f.as_raw_fd()))
}
struct Root {
    path: PathBuf,
    fd: File,
    _lock: File,
}
impl Root {
    fn open(root: &Path) -> Result<Self> {
        let path = root.canonicalize()?;
        let fd = File::open(&path)?;
        ensure!(
            fdpath(&fd).canonicalize()? == path,
            "Root changed while opening"
        );
        ensure!(fd.metadata()?.is_dir(), "Project root must be a directory");
        match mkdir_at(&fd, ".wex".as_ref()) {
            Ok(()) => {}
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
        let data = open_at(&fd, ".wex".as_ref(), libc::O_RDONLY | libc::O_DIRECTORY)?;
        let lock = open_at(
            &data,
            "file-operations.lock".as_ref(),
            libc::O_RDWR | libc::O_CREAT,
        )?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another file operation is active; retry"
        );
        Ok(Self {
            path,
            fd,
            _lock: lock,
        })
    }
    fn dir(&self, path: &Path) -> Result<File> {
        let mut fd = self.fd.try_clone()?;
        for c in path.components() {
            ensure!(matches!(c, Component::Normal(_)), "Invalid parent");
            fd = open_at(&fd, c.as_os_str(), libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        ensure!(
            fdpath(&fd).canonicalize()?.starts_with(&self.path),
            "Parent moved outside project"
        );
        Ok(fd)
    }
    fn parent(&self, p: &Path) -> Result<File> {
        valid(p)?;
        self.dir(p.parent().context("Missing parent")?)
    }
    fn stamp(&self, p: &Path) -> Result<String> {
        let parent = self.parent(p)?;
        stamp(&open_at(
            &parent,
            p.file_name().unwrap(),
            libc::O_RDONLY | libc::O_NONBLOCK,
        )?)
    }
    fn history(&self) -> Result<File> {
        let d = self.dir(Path::new(".wex"))?;
        match mkdir_at(&d, "file-history".as_ref()) {
            Ok(()) => {}
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
        open_at(
            &d,
            "file-history".as_ref(),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }
    fn latest(&self) -> Result<(String, Receipt)> {
        let history = self.history()?;
        let mut names = std::fs::read_dir(fdpath(&history))?
            .take(10001)
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(names.len() <= 10000, "History entry limit reached");
        names.sort();
        for n in names.into_iter().rev() {
            let id = n.to_str().context("Invalid history entry")?;
            ensure!(
                id.bytes().all(|b| b.is_ascii_digit() || b == b'-'),
                "Invalid history ID"
            );
            let d = open_at(&history, &n, libc::O_RDONLY | libc::O_DIRECTORY)?;
            if !fdpath(&d).join("payload").try_exists()? {
                continue;
            }
            let mut f = open_at(&d, "receipt.json".as_ref(), libc::O_RDONLY)?;
            ensure!(f.metadata()?.len() <= 65536, "Oversized trash receipt");
            let mut b = vec![];
            f.read_to_end(&mut b)?;
            let r: Receipt = serde_json::from_slice(&b)?;
            ensure!(
                r.schema == 1 && r.root == self.path,
                "Trash receipt belongs to another project/version"
            );
            valid(&r.path)?;
            return Ok((id.into(), r));
        }
        bail!("Trash is empty")
    }
}
fn stamp(file: &File) -> Result<String> {
    fn visit(f: &File, h: &mut blake3::Hasher, n: &mut usize, bytes: &mut u64) -> Result<()> {
        *n += 1;
        ensure!(*n <= 10000, "Operation exceeds 10000 entries");
        let m = f.metadata()?;
        for v in [
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime() as u64,
            m.mtime_nsec() as u64,
        ] {
            h.update(&v.to_le_bytes());
        }
        if m.is_file() {
            *bytes += m.len();
            ensure!(*bytes <= 256 * 1024 * 1024, "Operation exceeds 256 MiB");
            h.update(b"file");
            let mut f = f.try_clone()?;
            let mut buf = [0; 65536];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
        } else if m.is_dir() {
            h.update(b"dir");
            let mut names = std::fs::read_dir(fdpath(f))?
                .take(10001)
                .map(|e| e.map(|e| e.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?;
            names.sort();
            ensure!(names.len() <= 10000, "Directory exceeds operation limit");
            for name in names {
                ensure!(
                    !OWNED.iter().any(|v| name == *v),
                    "Folder contains managed output/history"
                );
                let b = name.as_bytes();
                h.update(&(b.len() as u64).to_le_bytes());
                h.update(b);
                visit(
                    &open_at(f, &name, libc::O_RDONLY | libc::O_NONBLOCK)?,
                    h,
                    n,
                    bytes,
                )?;
            }
        } else {
            bail!("Only regular files and folders are supported");
        }
        Ok(())
    }
    let mut h = blake3::Hasher::new();
    visit(file, &mut h, &mut 0, &mut 0)?;
    Ok(h.finalize().to_hex().to_string())
}
pub fn prepare(root: &Path, action: Action) -> Result<Prepared> {
    let r = Root::open(root)?;
    let (stamp, receipt) = match &action {
        Action::Create { path, contents } => {
            r.parent(path)?;
            ensure!(
                contents
                    .as_ref()
                    .is_none_or(|s| s.len() <= 16 * 1024 * 1024),
                "New file exceeds limit"
            );
            (None, None)
        }
        Action::Move { from, to } => {
            r.parent(to)?;
            ensure!(!to.starts_with(from), "Destination is inside source");
            (Some(r.stamp(from)?), None)
        }
        Action::Trash { path } => (Some(r.stamp(path)?), None),
        Action::Restore => {
            let (id, receipt) = r.latest()?;
            r.parent(&receipt.path)?;
            let h = r.history()?;
            let d = open_at(&h, id.as_ref(), libc::O_RDONLY | libc::O_DIRECTORY)?;
            let s = stamp(&open_at(
                &d,
                "payload".as_ref(),
                libc::O_RDONLY | libc::O_NONBLOCK,
            )?)?;
            ensure!(
                s == receipt.stamp,
                "Trash payload changed; retained for review"
            );
            (Some(s), Some((id, receipt)))
        }
    };
    Ok(Prepared {
        action,
        stamp,
        receipt,
    })
}
impl Prepared {
    pub fn source(&self) -> Option<&Path> {
        match &self.action {
            Action::Move { from, .. } => Some(from),
            Action::Trash { path } => Some(path),
            _ => None,
        }
    }
    pub fn summary(&self) -> String {
        match &self.action {
            Action::Create { path, .. } => format!("Create {}", path.display()),
            Action::Move { from, to } => format!("{} → {}", from.display(), to.display()),
            Action::Trash { path } => format!("Move {} to project trash", path.display()),
            Action::Restore => format!(
                "Restore {}",
                self.receipt.as_ref().unwrap().1.path.display()
            ),
        }
    }
}
pub fn apply(root: &Path, p: Prepared) -> Result<Outcome> {
    let r = Root::open(root)?;
    match p.action {
        Action::Create { path, contents } => {
            let parent = r.parent(&path)?;
            let name = path.file_name().unwrap();
            if let Some(text) = contents {
                let mut t = tempfile::NamedTempFile::new_in(fdpath(&parent))?;
                t.write_all(text.as_bytes())?;
                t.as_file().sync_all()?;
                noreplace(&parent, t.path().file_name().unwrap(), &parent, name)?;
            } else {
                mkdir_at(&parent, name)?;
            }
            parent.sync_all()?;
            Ok(Outcome {
                from: None,
                to: Some(r.path.join(path)),
                trashed: false,
            })
        }
        Action::Move { from, to } => {
            ensure!(!to.starts_with(&from), "Destination is inside source");
            let a = r.parent(&from)?;
            let b = r.parent(&to)?;
            ensure!(
                r.stamp(&from)? == p.stamp.unwrap(),
                "Source changed after review; nothing moved"
            );
            noreplace(&a, from.file_name().unwrap(), &b, to.file_name().unwrap())?;
            a.sync_all()?;
            b.sync_all()?;
            Ok(Outcome {
                from: Some(r.path.join(from)),
                to: Some(r.path.join(to)),
                trashed: false,
            })
        }
        Action::Trash { path } => {
            let parent = r.parent(&path)?;
            ensure!(
                r.stamp(&path)? == p.stamp.clone().unwrap(),
                "Source changed after review; nothing trashed"
            );
            let history = r.history()?;
            let id = format!(
                "{:020}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos(),
                std::process::id()
            );
            mkdir_at(&history, id.as_ref())?;
            let d = open_at(&history, id.as_ref(), libc::O_RDONLY | libc::O_DIRECTORY)?;
            let receipt = Receipt {
                schema: 1,
                root: r.path.clone(),
                path: path.clone(),
                stamp: p.stamp.unwrap(),
            };
            let mut f = open_at(
                &d,
                "receipt.json".as_ref(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
            f.write_all(&serde_json::to_vec(&receipt)?)?;
            f.sync_all()?;
            d.sync_all()?;
            // Persist the receipt BEFORE moving data, so a crash cannot orphan a payload.
            noreplace(&parent, path.file_name().unwrap(), &d, "payload".as_ref())?;
            d.sync_all()?;
            parent.sync_all()?;
            Ok(Outcome {
                from: Some(r.path.join(path)),
                to: None,
                trashed: true,
            })
        }
        Action::Restore => {
            let (id, receipt) = p.receipt.context("Missing prepared receipt")?;
            ensure!(receipt.root == r.path, "Different project");
            let history = r.history()?;
            let d = open_at(&history, id.as_ref(), libc::O_RDONLY | libc::O_DIRECTORY)?;
            ensure!(
                stamp(&open_at(
                    &d,
                    "payload".as_ref(),
                    libc::O_RDONLY | libc::O_NONBLOCK
                )?)? == p.stamp.unwrap(),
                "Trash payload changed after review; retained"
            );
            let parent = r.parent(&receipt.path)?;
            noreplace(
                &d,
                "payload".as_ref(),
                &parent,
                receipt.path.file_name().unwrap(),
            )?;
            parent.sync_all()?;
            d.sync_all()?;
            Ok(Outcome {
                from: None,
                to: Some(r.path.join(receipt.path)),
                trashed: false,
            })
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn run(root: &Path, a: Action) -> Result<Outcome> {
        apply(root, prepare(root, a)?)
    }
    fn create(path: &str, text: &str) -> Action {
        Action::Create {
            path: path.into(),
            contents: Some(text.into()),
        }
    }
    #[test]
    fn templates_validate_and_escape_by_identifier() {
        for t in [
            Template::KotlinClass,
            Template::JavaClass,
            Template::KotlinFile,
        ] {
            assert!(t.render("Feature", "com.example").is_ok());
            for s in ["class", "../bad", "A\"oops", "_"] {
                assert!(t.render(s, "a.b").is_err());
            }
            assert!(t.render("A", "a.else").is_err());
        }
        assert!(
            Template::Layout
                .render("hello_view", "")
                .unwrap()
                .1
                .unwrap()
                .contains("FrameLayout")
        );
        assert!(Template::Layout.render("Bad.xml", "").is_err());
        assert!(Template::Folder.render("папка", "").unwrap().1.is_none());
    }
    #[test]
    fn collisions_never_replace_and_new_files_are_complete() {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), create("A.kt", "original")).unwrap();
        let p = prepare(d.path(), create("A.kt", "replacement")).unwrap();
        assert!(apply(d.path(), p).is_err());
        assert_eq!(
            std::fs::read_to_string(d.path().join("A.kt")).unwrap(),
            "original"
        );
        assert!(
            run(
                d.path(),
                Action::Create {
                    path: "A.kt".into(),
                    contents: None
                }
            )
            .is_err()
        );
    }
    #[test]
    fn stale_source_and_destination_conflict_preserve_both() {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), create("a", "one")).unwrap();
        let a = Action::Move {
            from: "a".into(),
            to: "b".into(),
        };
        let p = prepare(d.path(), a.clone()).unwrap();
        std::fs::write(d.path().join("a"), "changed").unwrap();
        assert!(apply(d.path(), p).is_err());
        run(d.path(), create("b", "two")).unwrap();
        assert!(run(d.path(), a).is_err());
        assert_eq!(std::fs::read_to_string(d.path().join("b")).unwrap(), "two");
        assert!(d.path().join("a").exists());
    }
    #[test]
    fn folders_move_and_trash_restore_across_restart() {
        let d = tempfile::tempdir().unwrap();
        run(
            d.path(),
            Action::Create {
                path: "dir".into(),
                contents: None,
            },
        )
        .unwrap();
        run(d.path(), create("dir/a", "one")).unwrap();
        run(
            d.path(),
            Action::Move {
                from: "dir".into(),
                to: "moved".into(),
            },
        )
        .unwrap();
        run(
            d.path(),
            Action::Trash {
                path: "moved".into(),
            },
        )
        .unwrap();
        assert!(!d.path().join("moved").exists());
        run(d.path(), Action::Restore).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("moved/a")).unwrap(),
            "one"
        );
        assert!(prepare(d.path(), Action::Restore).is_err());
    }
    #[test]
    fn restore_conflict_and_tamper_keep_payload() {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), create("a", "original")).unwrap();
        run(d.path(), Action::Trash { path: "a".into() }).unwrap();
        run(d.path(), create("a", "occupied")).unwrap();
        assert!(run(d.path(), Action::Restore).is_err());
        std::fs::remove_file(d.path().join("a")).unwrap();
        let r = Root::open(d.path()).unwrap();
        let (id, _) = r.latest().unwrap();
        drop(r);
        std::fs::write(
            d.path().join(".wex/file-history").join(id).join("payload"),
            "tamper",
        )
        .unwrap();
        assert!(prepare(d.path(), Action::Restore).is_err());
        assert!(!d.path().join("a").exists());
    }
    #[test]
    fn traversal_symlinks_managed_and_special_files_are_rejected() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), d.path().join("link")).unwrap();
        for path in ["../a", "/tmp/a", ".wex/a", "build/a", "link/a", ""] {
            assert!(prepare(d.path(), create(path, "x")).is_err(), "{path}");
        }
        assert!(
            prepare(
                d.path(),
                Action::Trash {
                    path: "link".into()
                }
            )
            .is_err()
        );
        run(
            d.path(),
            Action::Create {
                path: "folder".into(),
                contents: None,
            },
        )
        .unwrap();
        symlink(outside.path(), d.path().join("folder/link")).unwrap();
        assert!(
            prepare(
                d.path(),
                Action::Trash {
                    path: "folder".into()
                }
            )
            .is_err()
        );
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
    #[test]
    fn reviewed_source_parent_symlink_swap_is_rejected() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        run(d.path(), create("src/a", "one")).unwrap();
        let p = prepare(
            d.path(),
            Action::Trash {
                path: "src/a".into(),
            },
        )
        .unwrap();
        std::fs::rename(d.path().join("src"), d.path().join("old")).unwrap();
        symlink(outside.path(), d.path().join("src")).unwrap();
        assert!(apply(d.path(), p).is_err());
        assert!(d.path().join("old/a").is_file());
    }
    #[test]
    fn directory_self_move_and_busy_lock_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        assert!(
            prepare(
                d.path(),
                Action::Move {
                    from: "a".into(),
                    to: "a/child".into()
                }
            )
            .is_err()
        );
        let lock = Root::open(d.path()).unwrap();
        assert!(prepare(d.path(), create("b", "x")).is_err());
        drop(lock);
        run(d.path(), create("b", "x")).unwrap();
    }
}
