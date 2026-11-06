//! Validated project-wide LSP edits. Planning never writes files or mutates buffers.
use crate::{
    editor::Document,
    kotlin::{Range, uri_path},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ops::Range as CharRange,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone)]
pub struct Buffer {
    pub path: PathBuf,
    pub text: String,
    pub version: i32,
}
#[derive(Debug, Clone)]
pub struct Source {
    pub text: String,
    pub hash: blake3::Hash,
    pub disk_hash: blake3::Hash,
    pub version: Option<i32>,
}
#[derive(Debug, Clone)]
pub struct Capture {
    pub root: PathBuf,
    pub sources: BTreeMap<PathBuf, Source>,
}
#[derive(Debug, Clone)]
pub struct FileEdit {
    pub path: PathBuf,
    pub before: Source,
    pub edits: Vec<(CharRange<usize>, String)>,
    pub after: String,
}
#[derive(Debug, Clone)]
pub struct Plan {
    pub files: Vec<FileEdit>,
    pub edit_count: usize,
}
impl Capture {
    pub fn read(root: &Path, buffers: &[Buffer]) -> Result<Self> {
        let root = root.canonicalize()?;
        let files = crate::project::source_files(&root);
        ensure!(files.len() <= 10_000, "refactor source count exceeds 10000");
        let mut sources = BTreeMap::new();
        let mut bytes = 0usize;
        for path in files {
            ensure!(
                path.canonicalize()? == path && path.starts_with(&root),
                "source path alias/outside workspace"
            );
            let text = std::fs::read_to_string(&path)?;
            bytes += text.len();
            ensure!(
                bytes <= 64 * 1024 * 1024,
                "refactor snapshot exceeds 64 MiB"
            );
            let hash = blake3::hash(text.as_bytes());
            sources.insert(
                path,
                Source {
                    text,
                    hash,
                    disk_hash: hash,
                    version: None,
                },
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for buffer in buffers {
            ensure!(seen.insert(&buffer.path), "duplicate open buffer");
            let s = sources
                .get_mut(&buffer.path)
                .context("open buffer is not an editable project source")?;
            bytes = bytes - s.text.len() + buffer.text.len();
            ensure!(
                bytes <= 64 * 1024 * 1024,
                "refactor snapshot exceeds 64 MiB"
            );
            s.text.clone_from(&buffer.text);
            s.hash = blake3::hash(s.text.as_bytes());
            s.version = Some(buffer.version);
        }
        Ok(Self { root, sources })
    }
    /// Detect source additions/removals and disk changes during semantic analysis/review.
    pub fn verify_disk(&self) -> Result<()> {
        let files = crate::project::source_files(&self.root);
        ensure!(
            files.len() == self.sources.len() && files.iter().all(|p| self.sources.contains_key(p)),
            "project sources changed; request refactoring again"
        );
        for (path, s) in &self.sources {
            ensure!(
                path.canonicalize()? == *path && blake3::hash(&std::fs::read(path)?) == s.disk_hash,
                "source changed on disk: {}",
                path.display()
            );
        }
        Ok(())
    }
    pub fn plan(&self, value: &Value) -> Result<Plan> {
        ensure!(!value.is_null(), "K2 returned no rename edits");
        ensure!(value.is_object(), "invalid WorkspaceEdit");
        let mut grouped: BTreeMap<PathBuf, Vec<Value>> = BTreeMap::new();
        let mut add = |uri: &str, version: Option<i32>, edits: &Value| -> Result<()> {
            let path = uri_path(uri)?;
            let before = self
                .sources
                .get(&path)
                .context("edit targets an unknown/generated/non-project source")?;
            ensure!(path.canonicalize()? == path, "edit path is an alias");
            if let Some(version) = version {
                ensure!(
                    before.version == Some(version),
                    "stale LSP document version"
                );
            }
            ensure!(!grouped.contains_key(&path), "duplicate edit document");
            grouped.insert(
                path,
                edits
                    .as_array()
                    .context("text edits must be an array")?
                    .clone(),
            );
            Ok(())
        };
        ensure!(
            !(value.get("changes").is_some() && value.get("documentChanges").is_some()),
            "ambiguous WorkspaceEdit forms"
        );
        if let Some(changes) = value.get("changes") {
            for (uri, edits) in changes.as_object().context("changes must be an object")? {
                add(uri, None, edits)?;
            }
        } else if let Some(changes) = value.get("documentChanges") {
            for change in changes
                .as_array()
                .context("documentChanges must be an array")?
            {
                ensure!(
                    change.get("kind").is_none(),
                    "resource operations require a separate file-operation review"
                );
                let doc = &change["textDocument"];
                let version = match &doc["version"] {
                    Value::Null => None,
                    v => Some(i32::try_from(
                        v.as_i64().context("invalid document version")?,
                    )?),
                };
                add(
                    doc["uri"].as_str().context("missing document URI")?,
                    version,
                    &change["edits"],
                )?;
            }
        } else {
            bail!("WorkspaceEdit has no changes");
        }
        let mut plan = Plan {
            files: vec![],
            edit_count: 0,
        };
        for (path, raw) in grouped {
            let before = self.sources[&path].clone();
            let mut doc = Document::new(&before.text)?;
            let mut edits = Vec::new();
            for edit in raw {
                let range: Range = serde_json::from_value(edit["range"].clone())?;
                let start = doc.char_at_lsp(range.start.line, range.start.character)?;
                let end = doc.char_at_lsp(range.end.line, range.end.character)?;
                ensure!(start <= end, "reversed edit range");
                edits.push((
                    start..end,
                    edit["newText"]
                        .as_str()
                        .context("missing newText")?
                        .to_owned(),
                ));
                plan.edit_count += 1;
                ensure!(
                    plan.edit_count <= 100_000,
                    "refactor edit count exceeds 100000"
                );
            }
            edits.sort_by_key(|(r, _)| (r.start, r.end));
            for pair in edits.windows(2) {
                ensure!(
                    pair[0].0.end <= pair[1].0.start && pair[0].0.start != pair[1].0.start,
                    "overlapping or ambiguous edits"
                );
            }
            doc.edit_batch_parsed(&edits)?;
            let after = doc.rope().to_string();
            if after != before.text {
                plan.files.push(FileEdit {
                    path,
                    before,
                    edits,
                    after,
                });
            }
        }
        ensure!(!plan.files.is_empty(), "rename has no text changes");
        Ok(plan)
    }
}
impl FileEdit {
    /// Build a replacement off-thread. Existing history, disk baseline and view are retained.
    pub fn apply(&self, doc: &Document) -> Result<Document> {
        ensure!(
            doc.path.as_ref() == Some(&self.path) && doc.content_hash() == self.before.hash,
            "buffer changed after preview: {}",
            self.path.display()
        );
        ensure!(
            doc.disk_hash() == Some(self.before.disk_hash),
            "unreviewed external source change"
        );
        let mut next = doc.fork()?;
        next.edit_batch_parsed(&self.edits)?;
        ensure!(*next.rope() == self.after, "refactor result mismatch");
        Ok(next)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, Capture, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A.kt");
        let b = d.path().join("B.kt");
        std::fs::write(&a, "fun greet() = 7\n").unwrap();
        std::fs::write(&b, "val x = greet()\n").unwrap();
        let c = Capture::read(d.path(), &[]).unwrap();
        (d, c, a, b)
    }
    fn edit(start: u32, end: u32, text: &str) -> Value {
        json!({"range":{"start":{"line":0,"character":start},"end":{"line":0,"character":end}},"newText":text})
    }
    #[test]
    fn project_edit_is_all_validated_then_single_undo_per_document_without_disk_writes() {
        let (_d, c, a, b) = fixture();
        let au = crate::kotlin::file_uri(&a).unwrap();
        let bu = crate::kotlin::file_uri(&b).unwrap();
        let p = c
            .plan(&json!({"changes":{au:[edit(4,9,"welcome")],bu:[edit(8,13,"welcome")]}}))
            .unwrap();
        assert_eq!(p.files.len(), 2);
        assert_eq!(p.edit_count, 2);
        for f in p.files {
            let before = Document::open(&f.path).unwrap();
            let mut after = f.apply(&before).unwrap();
            assert!(after.dirty());
            assert!(after.rope().to_string().contains("welcome"));
            assert!(after.undo().unwrap());
            assert_eq!(after.rope(), before.rope());
            assert_eq!(std::fs::read_to_string(&f.path).unwrap(), f.before.text);
        }
    }
    #[test]
    fn invalid_second_file_never_mutates_the_first() {
        let (_d, c, a, b) = fixture();
        let au = crate::kotlin::file_uri(&a).unwrap();
        let bu = crate::kotlin::file_uri(&b).unwrap();
        assert!(
            c.plan(&json!({"changes":{au:[edit(4,9,"welcome")],bu:[edit(800,900,"welcome")]}}))
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(a).unwrap(), "fun greet() = 7\n");
    }
    #[test]
    fn dirty_buffers_and_utf16_are_used_instead_of_disk_contents() {
        let (d, _, a, _) = fixture();
        let text = "//😀 greet\n";
        let c = Capture::read(
            d.path(),
            &[Buffer {
                path: a.clone(),
                text: text.into(),
                version: 42,
            }],
        )
        .unwrap();
        let uri = crate::kotlin::file_uri(&a).unwrap();
        let p=c.plan(&json!({"documentChanges":[{"textDocument":{"uri":uri,"version":42},"edits":[edit(5,10,"welcome")]}]})).unwrap();
        assert_eq!(p.files[0].after, "//😀 welcome\n");
        assert!(c.plan(&json!({"documentChanges":[{"textDocument":{"uri":uri,"version":41},"edits":[edit(5,10,"welcome")]}]})).is_err());
        assert!(c.plan(&json!({"changes":{uri:[edit(3,4,"x")]}})).is_err());
    }
    #[test]
    fn overlap_resource_ops_and_outside_files_are_rejected() {
        let (_d, c, a, _) = fixture();
        let uri = crate::kotlin::file_uri(&a).unwrap();
        assert!(
            c.plan(&json!({"changes":{uri.clone():[edit(4,9,"x"),edit(8,10,"y")]}}))
                .is_err()
        );
        assert!(
            c.plan(&json!({"documentChanges":[{"kind":"delete","uri":uri}]}))
                .is_err()
        );
        assert!(
            c.plan(&json!({"changes":{"file:///tmp/not-project.kt":[edit(0,0,"x")]}}))
                .is_err()
        );
    }
    #[test]
    fn disk_changes_additions_and_stale_live_buffers_invalidate_preview() {
        let (d, c, a, _) = fixture();
        let uri = crate::kotlin::file_uri(&a).unwrap();
        let p = c
            .plan(&json!({"changes":{uri:[edit(4,9,"welcome")]}}))
            .unwrap();
        let mut doc = Document::open(&a).unwrap();
        doc.edit(0..0, "//new\n").unwrap();
        assert!(p.files[0].apply(&doc).is_err());
        std::fs::write(d.path().join("New.kt"), "val y = 1").unwrap();
        assert!(c.verify_disk().is_err());
        std::fs::remove_file(d.path().join("New.kt")).unwrap();
        std::fs::write(a, "fun changed() = 0\n").unwrap();
        assert!(c.verify_disk().is_err());
    }
}
