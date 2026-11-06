//! Reviewed local branch operations. Git owns checkout collision/lock/hook semantics;
//! no force, reset, stash, remote guessing or implicit save is performed here.
use super::*;
#[derive(Clone, Debug)]
pub struct Branch {
    pub name: OsString,
    pub oid: String,
    pub current: bool,
    pub worktree: Option<PathBuf>,
}
#[derive(Clone, Debug)]
pub struct Catalog {
    pub snapshot: Arc<Snapshot>,
    pub items: Vec<Branch>,
    raw: Vec<u8>,
}
#[derive(Clone)]
pub enum Action {
    Create(String),
    Switch(usize),
}
fn refs(root: &Path, stop: &AtomicBool) -> Result<Vec<u8>> {
    success(
        root,
        &[
            os("for-each-ref"),
            os("--sort=refname"),
            os("--format=%(refname)%00%(objectname)%00%(HEAD)%00%(worktreepath)%00"),
            os("refs/heads/"),
        ],
        None,
        None,
        stop,
    )
}
fn parse(raw: &[u8]) -> Result<Vec<Branch>> {
    let mut rest = raw;
    let mut items = vec![];
    while !rest.is_empty() {
        let mut fields = vec![];
        for _ in 0..4 {
            let at = rest
                .iter()
                .position(|b| *b == 0)
                .context("Malformed branch record")?;
            fields.push(&rest[..at]);
            rest = &rest[at + 1..];
        }
        ensure!(rest.first() == Some(&b'\n'), "Malformed branch terminator");
        rest = &rest[1..];
        let name = fields[0]
            .strip_prefix(b"refs/heads/")
            .filter(|s| !s.is_empty())
            .context("Invalid local branch")?;
        ensure!(
            [40, 64].contains(&fields[1].len()) && fields[1].iter().all(u8::is_ascii_hexdigit),
            "Invalid branch object ID"
        );
        ensure!(
            fields[2] == b"*" || fields[2] == b" ",
            "Malformed current branch flag"
        );
        items.push(Branch {
            name: OsString::from_vec(name.to_vec()),
            oid: String::from_utf8(fields[1].to_vec())?,
            current: fields[2] == b"*",
            worktree: (!fields[3].is_empty())
                .then(|| PathBuf::from(OsString::from_vec(fields[3].to_vec()))),
        });
        ensure!(items.len() <= FILES, "Branch list exceeds20,000 entries");
    }
    Ok(items)
}
pub fn read(project: &Path, stop: &AtomicBool) -> Result<Catalog> {
    let snapshot = Arc::new(refresh(project, stop)?);
    let raw = refs(&snapshot.root, stop)?;
    let items = parse(&raw)?;
    ensure!(
        snapshot.same_state(&refresh(&snapshot.root, stop)?) && raw == refs(&snapshot.root, stop)?,
        "Branches/repository changed while reading; Refresh"
    );
    Ok(Catalog {
        snapshot,
        items,
        raw,
    })
}
impl Catalog {
    pub fn same_review(&self, other: &Self) -> bool {
        self.raw == other.raw && self.snapshot.same_state(&other.snapshot)
    }
    pub fn can_create(&self) -> bool {
        self.snapshot.head.starts_with(b"born:")
    }
    fn check(&self, stop: &AtomicBool) -> Result<()> {
        self.snapshot.check(stop)?;
        ensure!(
            self.snapshot
                .same_state(&refresh(&self.snapshot.root, stop)?)
                && self.raw == refs(&self.snapshot.root, stop)?,
            "Branch review changed; Refresh before applying"
        );
        Ok(())
    }
}
pub fn apply(c: &Catalog, action: Action, stop: &AtomicBool) -> Result<(Catalog, String)> {
    c.check(stop)?;
    let root = &c.snapshot.root;
    let note = match action {
        Action::Create(name) => {
            ensure!(
                c.can_create(),
                "Create the first commit before creating another branch"
            );
            ensure!(
                !name.is_empty() && name.len() <= 4096 && !name.starts_with('-'),
                "Enter a literal branch name (max4096 bytes)"
            );
            let normalized = success(
                root,
                &[os("check-ref-format"), os("--branch"), os(&name)],
                None,
                None,
                stop,
            )?;
            ensure!(
                normalized == [name.as_bytes(), b"\n"].concat(),
                "Use a literal branch name, not a previous-checkout expression"
            );
            let oid = success(
                root,
                &[os("rev-parse"), os("--verify"), os("HEAD")],
                None,
                None,
                stop,
            )?;
            let oid = std::str::from_utf8(&oid)?.trim();
            c.check(stop)?;
            success(
                root,
                &[os("branch"), os("--no-track"), os("--"), os(&name), os(oid)],
                None,
                None,
                stop,
            )
            .context("Branch creation did not finish; Refresh actual state")?;
            format!("Created {name} · current branch and working files kept")
        }
        Action::Switch(at) => {
            let b = c.items.get(at).context("Choose a reviewed local branch")?;
            ensure!(!b.current, "Already on this branch");
            ensure!(
                b.worktree.is_none(),
                "Branch is already checked out in another worktree"
            );
            success(
                root,
                &[
                    os("switch"),
                    os("--no-guess"),
                    os("--no-recurse-submodules"),
                    os("--"),
                    &b.name,
                ],
                None,
                None,
                stop,
            )
            .context("Switch did not finish; no force/stash used. Refresh actual state")?;
            let actual = read(root, stop)?;
            ensure!(
                actual
                    .items
                    .iter()
                    .any(|n| n.current && n.name == b.name && n.oid == b.oid),
                "Target branch moved during switch; Refresh actual repository state"
            );
            return Ok((
                actual,
                format!(
                    "Switched to {} · compatible local changes kept; commit draft retained",
                    b.name.to_string_lossy()
                ),
            ));
        }
    };
    Ok((read(root, stop)?, note))
}
#[cfg(test)]
mod tests {
    use super::super::tests::Repo;
    use super::*;
    fn stop() -> AtomicBool {
        AtomicBool::new(false)
    }
    #[test]
    fn real_create_and_switch_keeps_disk_edits_and_does_not_commit() {
        let r = Repo::new(true);
        let before = r.git(&["rev-parse", "HEAD"]);
        let c = read(r.p(), &stop()).unwrap();
        let (c, _) = apply(&c, Action::Create("feature/猫".into()), &stop()).unwrap();
        assert!(
            !c.items
                .iter()
                .find(|b| b.name == "feature/猫")
                .unwrap()
                .current
        );
        fs::write(r.p().join("a.txt"), "dirty kept\n").unwrap();
        let c = read(r.p(), &stop()).unwrap();
        let n = c.items.iter().position(|b| b.name == "feature/猫").unwrap();
        let (c, _) = apply(&c, Action::Switch(n), &stop()).unwrap();
        assert!(c.items[n].current);
        assert_eq!(
            fs::read_to_string(r.p().join("a.txt")).unwrap(),
            "dirty kept\n"
        );
        assert_eq!(r.git(&["rev-parse", "HEAD"]), before);
    }
    #[test]
    fn stale_head_ref_and_worktree_bytes_reject_whole_action() {
        let r = Repo::new(true);
        r.git(&["branch", "other"]);
        let c = read(r.p(), &stop()).unwrap();
        r.git(&["switch", "-q", "other"]);
        assert!(apply(&c, Action::Create("wrong".into()), &stop()).is_err());
        assert!(r.git(&["branch", "--list", "wrong"]).is_empty());
        let c = read(r.p(), &stop()).unwrap();
        fs::write(r.p().join("a.txt"), "outside\n").unwrap();
        assert!(apply(&c, Action::Create("stale".into()), &stop()).is_err());
    }
    #[test]
    fn target_ref_move_and_duplicate_invalid_expression_names_rejected() {
        let r = Repo::new(true);
        r.git(&["branch", "target"]);
        let c = read(r.p(), &stop()).unwrap();
        fs::write(r.p().join("b.txt"), "next").unwrap();
        r.git(&["add", "b.txt"]);
        r.git(&["commit", "-qm", "next"]);
        let oid = r.git(&["rev-parse", "HEAD"]);
        r.git(&[
            "update-ref",
            "refs/heads/target",
            std::str::from_utf8(&oid).unwrap().trim(),
        ]);
        assert!(apply(&c, Action::Switch(0), &stop()).is_err());
        let c = read(r.p(), &stop()).unwrap();
        for name in ["target", "-bad", "bad..name", "@{-1}", "has space", ""] {
            assert!(
                apply(&c, Action::Create(name.into()), &stop()).is_err(),
                "{name}"
            );
        }
    }
    #[test]
    fn collision_and_foreign_lock_leave_local_files_and_lock_untouched() {
        let r = Repo::new(true);
        r.git(&["branch", "old"]);
        fs::write(r.p().join("a.txt"), "new commit\n").unwrap();
        r.git(&["add", "a.txt"]);
        r.git(&["commit", "-qm", "new"]);
        fs::write(r.p().join("a.txt"), "uncommitted\n").unwrap();
        let c = read(r.p(), &stop()).unwrap();
        let n = c.items.iter().position(|b| b.name == "old").unwrap();
        assert!(apply(&c, Action::Switch(n), &stop()).is_err());
        assert_eq!(
            fs::read_to_string(r.p().join("a.txt")).unwrap(),
            "uncommitted\n"
        );
        r.git(&["restore", "a.txt"]);
        let c = read(r.p(), &stop()).unwrap();
        let lock = r.p().join(".git/index.lock");
        fs::write(&lock, "foreign").unwrap();
        assert!(apply(&c, Action::Switch(n), &stop()).is_err());
        assert_eq!(fs::read_to_string(lock).unwrap(), "foreign");
    }
    #[test]
    fn unborn_detached_and_linked_worktree_are_explicit() {
        let empty = Repo::new(false);
        assert!(!read(empty.p(), &stop()).unwrap().can_create());
        assert!(
            apply(
                &read(empty.p(), &stop()).unwrap(),
                Action::Create("new".into()),
                &stop()
            )
            .is_err()
        );
        let r = Repo::new(true);
        r.git(&["branch", "linked"]);
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("wt");
        r.git(&["worktree", "add", "-q", other.to_str().unwrap(), "linked"]);
        let c = read(r.p(), &stop()).unwrap();
        let n = c.items.iter().position(|b| b.name == "linked").unwrap();
        assert_eq!(c.items[n].worktree.as_ref(), Some(&other));
        assert!(apply(&c, Action::Switch(n), &stop()).is_err());
        r.git(&["switch", "--detach", "-q"]);
        let c = read(r.p(), &stop()).unwrap();
        assert!(c.items.iter().all(|b| !b.current));
        let (c, _) = apply(&c, Action::Create("from-detached".into()), &stop()).unwrap();
        assert!(c.items.iter().any(|b| b.name == "from-detached"));
    }
    #[test]
    fn parser_keeps_raw_names_and_newlines_in_worktree_paths() {
        let mut raw = b"refs/heads/bad-\xff\0".to_vec();
        raw.extend([b'a'; 40]);
        raw.extend(b"\0 \0/tmp/with\nline\0\n");
        let items = parse(&raw).unwrap();
        assert_eq!(items[0].name.as_bytes(), b"bad-\xff");
        assert_eq!(
            items[0].worktree.as_ref().unwrap().as_os_str().as_bytes(),
            b"/tmp/with\nline"
        );
        assert!(parse(&raw[..raw.len() - 1]).is_err());
    }
    #[test]
    fn target_only_ref_move_rejects_switch_before_checkout() {
        let r = Repo::new(true);
        let old = r.git(&["rev-parse", "HEAD"]);
        r.git(&["branch", "target"]);
        fs::write(r.p().join("a.txt"), "second\n").unwrap();
        r.git(&["add", "a.txt"]);
        r.git(&["commit", "-qm", "second"]);
        let next = r.git(&["rev-parse", "HEAD"]);
        r.git(&["reset", "--hard", std::str::from_utf8(&old).unwrap().trim()]);
        let c = read(r.p(), &stop()).unwrap();
        let n = c.items.iter().position(|b| b.name == "target").unwrap();
        r.git(&[
            "update-ref",
            "refs/heads/target",
            std::str::from_utf8(&next).unwrap().trim(),
        ]);
        assert!(apply(&c, Action::Switch(n), &stop()).is_err());
        assert_eq!(r.git(&["rev-parse", "HEAD"]), old);
        assert_eq!(fs::read_to_string(r.p().join("a.txt")).unwrap(), "before\n");
    }
    #[test]
    fn create_foreign_ref_lock_and_cancel_leave_no_new_ref_or_lock_deletion() {
        let r = Repo::new(true);
        let c = read(r.p(), &stop()).unwrap();
        let lock = r.p().join(".git/refs/heads/new.lock");
        fs::write(&lock, "foreign").unwrap();
        assert!(apply(&c, Action::Create("new".into()), &stop()).is_err());
        assert_eq!(fs::read_to_string(&lock).unwrap(), "foreign");
        assert!(r.git(&["branch", "--list", "new"]).is_empty());
        assert!(
            apply(
                &c,
                Action::Create("cancelled".into()),
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(r.git(&["branch", "--list", "cancelled"]).is_empty());
    }
    #[test]
    fn different_tree_switch_updates_modified_added_deleted_files_and_ref() {
        let r = Repo::new(true);
        let original = r.git(&["symbolic-ref", "--short", "HEAD"]);
        let name = std::str::from_utf8(&original).unwrap().trim();
        fs::write(r.p().join("Removed.kt"), "class Removed\n").unwrap();
        r.git(&["add", "."]);
        r.git(&["commit", "-qm", "main tree"]);
        r.git(&["switch", "-c", "feature/model"]);
        fs::write(r.p().join("a.txt"), "different tree\n").unwrap();
        fs::remove_file(r.p().join("Removed.kt")).unwrap();
        fs::write(r.p().join("Added.kt"), "class Added\n").unwrap();
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "feature tree"]);
        let oid = r.git(&["rev-parse", "HEAD"]);
        r.git(&["switch", name]);
        let c = read(r.p(), &stop()).unwrap();
        let n = c
            .items
            .iter()
            .position(|b| b.name == "feature/model")
            .unwrap();
        let (c, _) = apply(&c, Action::Switch(n), &stop()).unwrap();
        assert_eq!(r.git(&["rev-parse", "HEAD"]), oid);
        assert!(c.items[n].current);
        assert_eq!(
            fs::read_to_string(r.p().join("a.txt")).unwrap(),
            "different tree\n"
        );
        assert!(!r.p().join("Removed.kt").exists());
        assert_eq!(
            fs::read_to_string(r.p().join("Added.kt")).unwrap(),
            "class Added\n"
        );
        assert!(c.snapshot.entries.is_empty());
    }
}
