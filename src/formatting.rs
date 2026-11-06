//! Bounded, read-only formatter review. Only an explicit Apply creates an unsaved edit.
use crate::{editor::Document, kotlin::Range};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::Read,
    ops::Range as Chars,
    path::{Path, PathBuf},
};
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    K2,
    GoogleJava,
}
impl Backend {
    pub fn for_path(path: &Path) -> Result<Self> {
        match path.extension().and_then(|s| s.to_str()) {
            Some("kt" | "kts") => Ok(Self::K2),
            Some("java") => Ok(Self::GoogleJava),
            _ => anyhow::bail!("Formatting needs Kotlin or Java source"),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::K2 => "K2 · Kotlin",
            Self::GoogleJava => "google-java-format 1.37.0 · Java/AOSP",
        }
    }
}
const MAX_EDITS: usize = 100_000;
const MAX_LINES: usize = 100_000;
pub(crate) fn rpc_request(
    capabilities: &Value,
    path: &Path,
    range: Option<&Range>,
) -> Result<(&'static str, Value)> {
    ensure!(
        path.extension().is_some_and(|s| s == "kt" || s == "kts"),
        "K2 formats Kotlin; Java uses its separate pinned formatter"
    );
    let (method, key) = if range.is_some() {
        (
            "textDocument/rangeFormatting",
            "documentRangeFormattingProvider",
        )
    } else {
        ("textDocument/formatting", "documentFormattingProvider")
    };
    let cap = &capabilities["capabilities"][key];
    ensure!(
        cap == true || cap.is_object(),
        "K2 does not advertise the requested formatter"
    );
    let mut params = serde_json::json!({"textDocument":{"uri":crate::kotlin::file_uri(path)?},"options":{"tabSize":4,"insertSpaces":true,"trimTrailingWhitespace":true,"insertFinalNewline":true}});
    if let Some(range) = range {
        params["range"] = serde_json::to_value(range)?;
    }
    Ok((method, params))
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub root: PathBuf,
    pub path: PathBuf,
    pub text: String,
    pub version: i32,
    pub hash: blake3::Hash,
    pub disk_hash: blake3::Hash,
}
fn disk(path: &Path) -> Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.len() <= MAX_BYTES as u64,
        "formatting needs a regular source <=8MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "formatting source exceeds8MiB");
    Ok(bytes)
}
impl Capture {
    pub fn read(root: &Path, path: &Path, text: String, version: i32) -> Result<Self> {
        ensure!(text.len() <= MAX_BYTES, "formatting snapshot exceeds8MiB");
        let root = root.canonicalize()?;
        ensure!(
            path.is_absolute() && path.starts_with(&root) && path.canonicalize()? == path,
            "format source is outside project or aliased"
        );
        let bytes = disk(path)?;
        std::str::from_utf8(&bytes).context("format source disk is not UTF8")?;
        Ok(Self {
            root,
            path: path.to_owned(),
            version,
            hash: blake3::hash(text.as_bytes()),
            text,
            disk_hash: blake3::hash(&bytes),
        })
    }
    pub fn verify_disk(&self) -> Result<()> {
        ensure!(
            self.root.canonicalize()? == self.root && self.path.canonicalize()? == self.path,
            "formatting source/root moved"
        );
        ensure!(
            blake3::hash(&disk(&self.path)?) == self.disk_hash,
            "Source changed on disk; request formatting again"
        );
        Ok(())
    }
    pub fn review(self, value: &Value, requested: Option<Range>) -> Result<Review> {
        self.verify_disk()?;
        let mut doc = Document::new(&self.text)?;
        let raw = if value.is_null() {
            &[][..]
        } else {
            value
                .as_array()
                .context("formatter returned non-array edits")?
                .as_slice()
        };
        ensure!(
            raw.len() <= MAX_EDITS,
            "formatting edit count exceeds100000"
        );
        let mut edits = Vec::with_capacity(raw.len());
        let mut replacement_bytes = 0usize;
        for e in raw {
            let range: Range = serde_json::from_value(e["range"].clone())?;
            let start = doc.char_at_lsp(range.start.line, range.start.character)?;
            let end = doc.char_at_lsp(range.end.line, range.end.character)?;
            ensure!(start <= end, "reversed formatting edit");
            let text = e["newText"]
                .as_str()
                .context("formatter edit missing newText")?;
            replacement_bytes = replacement_bytes
                .checked_add(text.len())
                .context("format expansion overflow")?;
            ensure!(
                replacement_bytes <= MAX_BYTES,
                "formatter replacement text exceeds8MiB"
            );
            edits.push((start..end, text.to_owned()));
        }
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        for pair in edits.windows(2) {
            ensure!(
                pair[0].0.end <= pair[1].0.start && pair[0].0.start != pair[1].0.start,
                "overlapping or ambiguous formatter edits"
            );
        }
        doc.edit_batch_parsed(&edits)?;
        let after = doc.rope().to_string();
        ensure!(
            after.len() <= MAX_BYTES * 2,
            "formatted result exceeds16MiB"
        );
        let outside_selection_changed = if let Some(range) = &requested {
            let before = Document::new(&self.text)?;
            let a = before.char_at_lsp(range.start.line, range.start.character)?;
            let b = before.char_at_lsp(range.end.line, range.end.character)?;
            ensure!(a <= b, "reversed format selection");
            let prefix = before.rope().slice(..a).to_string();
            let suffix = before.rope().slice(b..).to_string();
            after.len() < prefix.len() + suffix.len()
                || !after.starts_with(&prefix)
                || !after.ends_with(&suffix)
        } else {
            false
        };
        let unchanged = after == self.text;
        let (rows, abbreviated) = preview(&self.text, &after)?;
        self.verify_disk()?;
        let preview_columns = rows
            .iter()
            .map(|r| r.before.chars().count().max(r.after.chars().count()))
            .max()
            .unwrap_or(0)
            + 12;
        let crlf = (
            self.text.matches("\r\n").count(),
            after.matches("\r\n").count(),
        );
        Ok(Review {
            backend: Backend::K2,
            capture: self,
            edits,
            after,
            requested,
            unchanged,
            outside_selection_changed,
            rows,
            abbreviated,
            preview_columns,
            crlf,
        })
    }
}
#[derive(Debug, Clone)]
pub struct Row {
    pub before_line: Option<usize>,
    pub after_line: Option<usize>,
    pub before: String,
    pub after: String,
    pub changed: bool,
}
#[derive(Debug, Clone)]
pub struct Review {
    pub backend: Backend,
    pub capture: Capture,
    pub edits: Vec<(Chars<usize>, String)>,
    pub after: String,
    pub requested: Option<Range>,
    pub unchanged: bool,
    pub outside_selection_changed: bool,
    pub rows: Vec<Row>,
    pub abbreviated: bool,
    pub preview_columns: usize,
    pub crlf: (usize, usize),
}
fn preview(before: &str, after: &str) -> Result<(Vec<Row>, bool)> {
    let a = before.lines().take(MAX_LINES + 1).collect::<Vec<_>>();
    let b = after.lines().take(MAX_LINES + 1).collect::<Vec<_>>();
    ensure!(
        a.len() <= MAX_LINES && b.len() <= MAX_LINES,
        "Formatting preview exceeds100000 lines"
    );
    let head = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = head.saturating_sub(2);
    let ae = (a.len() - tail + 2).min(a.len());
    let be = (b.len() - tail + 2).min(b.len());
    let mut rows = Vec::new();
    let mut abbreviated = false;
    for n in 0..(ae - start).max(be - start) {
        let av = a.get(start + n).filter(|_| start + n < ae);
        let bv = b.get(start + n).filter(|_| start + n < be);
        let mut short = |s: Option<&&str>| {
            let s = s.copied().unwrap_or("");
            let mut x = s.chars().take(400).collect::<String>();
            if s.chars().nth(400).is_some() {
                x.push('…');
                abbreviated = true;
            }
            x.replace('\t', "⇥   ")
        };
        rows.push(Row {
            before_line: av.map(|_| start + n + 1),
            after_line: bv.map(|_| start + n + 1),
            before: short(av),
            after: short(bv),
            changed: av != bv,
        });
    }
    if before.ends_with('\n') != after.ends_with('\n') {
        rows.push(Row {
            before_line: None,
            after_line: None,
            before: format!("Final newline: {}", before.ends_with('\n')),
            after: format!("Final newline: {}", after.ends_with('\n')),
            changed: true,
        });
    }
    Ok((rows, abbreviated))
}
impl Review {
    pub fn apply(&self, doc: &Document) -> Result<Document> {
        ensure!(!self.unchanged, "No formatting changes to apply");
        ensure!(
            doc.path.as_ref() == Some(&self.capture.path)
                && doc.version == self.capture.version
                && doc.content_hash() == self.capture.hash,
            "Source snapshot changed; preview again"
        );
        ensure!(
            doc.disk_hash() == Some(self.capture.disk_hash),
            "Source disk baseline changed; preview again"
        );
        self.capture.verify_disk()?;
        let mut next = doc.fork()?;
        next.edit_batch_parsed(&self.edits)?;
        ensure!(*next.rope() == self.after, "formatted candidate mismatch");
        self.capture.verify_disk()?;
        Ok(next)
    }
    pub fn map_position(&self, pos: usize, right_bias: bool) -> usize {
        let mut delta = 0isize;
        for (range, text) in &self.edits {
            if pos < range.start {
                break;
            }
            let count = text.chars().count();
            if pos > range.end || pos == range.end && !range.is_empty() {
                delta += count as isize - range.len() as isize;
                continue;
            }
            if range.is_empty() {
                return (range.start as isize + delta) as usize
                    + if right_bias { count } else { 0 };
            }
            return (range.start as isize + delta) as usize + (pos - range.start).min(count);
        }
        (pos as isize + delta).max(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, PathBuf, Document) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("Main.kt");
        std::fs::write(&p, "val x=1\n").unwrap();
        let doc = Document::open(&p).unwrap();
        (d, p, doc)
    }
    fn edit(a: u32, b: u32, t: &str) -> Value {
        json!({"range":{"start":{"line":0,"character":a},"end":{"line":0,"character":b}},"newText":t})
    }
    fn capture(d: &Path, p: &Path, doc: &Document) -> Capture {
        Capture::read(d, p, doc.rope().to_string(), doc.version).unwrap()
    }
    #[test]
    fn format_is_single_undo_dirty_overlay_and_never_saves() {
        let (d, p, mut doc) = fixture();
        doc.edit(7..7, " + 2").unwrap();
        let text = doc.rope().to_string();
        let r = capture(d.path(), &p, &doc)
            .review(&json!([edit(5, 5, " "), edit(6, 6, " ")]), None)
            .unwrap();
        let mut after = r.apply(&doc).unwrap();
        assert_eq!(after.rope().to_string(), "val x = 1 + 2\n");
        assert!(after.dirty());
        assert!(after.undo().unwrap());
        assert_eq!(after.rope().to_string(), text);
        assert!(after.undo().unwrap());
        assert_eq!(after.rope().to_string(), "val x=1\n");
        assert_eq!(std::fs::read_to_string(p).unwrap(), "val x=1\n");
    }
    #[test]
    fn null_empty_and_identical_are_successful_noop() {
        let (d, p, doc) = fixture();
        for v in [Value::Null, json!([]), json!([edit(0, 3, "val")])] {
            let r = capture(d.path(), &p, &doc).review(&v, None).unwrap();
            assert!(r.unchanged);
            assert!(r.apply(&doc).is_err());
        }
    }
    #[test]
    fn malformed_overlapping_and_utf16_split_reject_all() {
        let (d, p, doc) = fixture();
        for v in [
            json!({}),
            json!([edit(99, 99, "")]),
            json!([edit(4, 7, "a"), edit(6, 7, "b")]),
            json!([edit(4, 4, "a"), edit(4, 4, "b")]),
        ] {
            assert!(capture(d.path(), &p, &doc).review(&v, None).is_err());
        }
        std::fs::write(&p, "😀x\n").unwrap();
        let doc = Document::open(&p).unwrap();
        assert!(
            capture(d.path(), &p, &doc)
                .review(&json!([edit(1, 2, "a")]), None)
                .is_err()
        );
    }
    #[test]
    fn external_changes_and_new_history_reject_before_apply() {
        let (d, p, mut doc) = fixture();
        let r = capture(d.path(), &p, &doc)
            .review(&json!([edit(5, 5, " ")]), None)
            .unwrap();
        doc.edit(0..0, "//").unwrap();
        assert!(r.apply(&doc).is_err());
        std::fs::write(&p, "val x=2\n").unwrap();
        assert!(r.capture.verify_disk().is_err());
    }
    #[test]
    fn unrelated_invalid_source_does_not_block_single_file_format() {
        let (d, p, doc) = fixture();
        std::fs::write(d.path().join("Bad.kt"), [0xff]).unwrap();
        assert!(
            capture(d.path(), &p, &doc)
                .review(&json!([edit(5, 5, " ")]), None)
                .is_ok()
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_and_fifo_reject_without_following() {
        use std::os::unix::fs::symlink;
        let (d, p, _) = fixture();
        let a = d.path().join("Alias.kt");
        symlink(&p, &a).unwrap();
        assert!(Capture::read(d.path(), &a, "x".into(), 1).is_err());
        let f = d.path().join("Pipe.kt");
        let c = std::ffi::CString::new(f.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(Capture::read(d.path(), &f, "x".into(), 1).is_err());
    }
    #[test]
    fn range_expansion_is_visible_and_mapping_tracks_insertions() {
        let (d, p, doc) = fixture();
        let scope = Range {
            start: crate::kotlin::Position {
                line: 0,
                character: 4,
            },
            end: crate::kotlin::Position {
                line: 0,
                character: 5,
            },
        };
        let r = capture(d.path(), &p, &doc)
            .review(&json!([edit(5, 5, " "), edit(6, 6, " ")]), Some(scope))
            .unwrap();
        assert!(r.outside_selection_changed);
        assert_eq!(r.map_position(7, true), 9);
        assert_eq!(r.map_position(5, false), 5);
        assert_eq!(r.map_position(5, true), 6);
    }
    #[test]
    fn rpc_capabilities_and_java_are_not_false_noop() {
        let p = Path::new("/tmp/Main.kt");
        let c = json!({"capabilities":{"documentFormattingProvider":true,"documentRangeFormattingProvider":{}}});
        let (method, v) = rpc_request(&c, p, None).unwrap();
        assert_eq!(method, "textDocument/formatting");
        assert_eq!(v["options"]["tabSize"], 4);
        assert!(rpc_request(&Value::Null, p, None).is_err());
        assert!(rpc_request(&c, Path::new("/tmp/Main.java"), None).is_err());
    }
    #[test]
    fn crlf_normalization_is_exposed_in_review_metadata() {
        let (d, p, _) = fixture();
        std::fs::write(&p, "val x=1\r\n").unwrap();
        let doc = Document::open(&p).unwrap();
        let r=capture(d.path(),&p,&doc).review(&json!([{"range":{"start":{"line":0,"character":0},"end":{"line":1,"character":0}},"newText":"val x=1\n"}]),None).unwrap();
        assert_eq!(r.crlf, (1, 0));
        assert!(!r.unchanged);
    }
}
