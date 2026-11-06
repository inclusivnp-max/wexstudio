//! Rope-backed UTF-8 text with incremental Kotlin parsing and bounded undo.
use crate::text_tracking::{TextChange, TextId, TrackingSnapshot};
use anyhow::{Context, Result, ensure};
use ropey::Rope;
use std::{
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tree_sitter::{InputEdit, Parser, Point, Tree};

/// Immutable revision shared by the active buffer, history and background forks.
/// A hash is computed at most once for these exact bytes, never invalidated in place.
#[derive(Clone)]
struct SyntaxSeed {
    tree: Tree,
    edits: Vec<InputEdit>,
}
struct TextState {
    rope: Rope,
    id: TextId,
    origin: Option<Arc<TextChange>>,
    tree: OnceLock<Tree>,
    // No parent TextState link: pending edit chains cannot retain unbounded history.
    seed: Mutex<Option<SyntaxSeed>>,
    hash: OnceLock<blake3::Hash>,
    retire_on_drop: AtomicBool,
}
impl Drop for TextState {
    fn drop(&mut self) {
        if self.retire_on_drop.load(Ordering::Acquire) {
            if let Some(change) = &self.origin {
                change.defer_release();
            }
            let seed = self
                .seed
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            crate::document_retirement::retire(crate::document_retirement::Payload::Text {
                rope: std::mem::take(&mut self.rope),
                tree: self.tree.take(),
                seed: seed.map(|s| (s.tree, s.edits)),
                tracking: self.origin.take(),
            });
        }
    }
}
impl TextState {
    fn defer_release(&self) {
        if self.rope.len_bytes() >= crate::document_retirement::MIN_BYTES {
            self.retire_on_drop.store(true, Ordering::Release);
        }
    }
    fn hash(&self) -> blake3::Hash {
        *self.hash.get_or_init(|| hash(&self.rope))
    }
}
#[derive(Clone)]
struct History {
    text: Arc<TextState>,
    view: Option<(usize, usize)>,
}

pub struct Document {
    text: Arc<TextState>,
    parser: Parser,
    parser_bytes: usize,
    undo: Vec<History>,
    redo: Vec<History>,
    view: Option<(usize, usize)>,
    pub revision: u64,
    pub version: i32,
    pub path: Option<PathBuf>,
    saved_hash: blake3::Hash,
    // None means the accepted disk baseline is absent, including after Undo.
    saved_text: Option<Arc<TextState>>,
    disk_hash: Option<blake3::Hash>,
    dirty: bool,
    disk_baseline: Arc<()>,
    deferred_syntax: bool,
}
impl Drop for Document {
    fn drop(&mut self) {
        if self.deferred_syntax && self.parser_bytes >= crate::document_retirement::MIN_BYTES {
            crate::document_retirement::retire(crate::document_retirement::Payload::Parser(
                std::mem::replace(&mut self.parser, Parser::new()),
            ));
        }
    }
}
/// Immutable write request; neither parser nor undo history travels to the writer.
pub struct SaveSnapshot {
    text: Arc<TextState>,
    path: PathBuf,
    expected: Option<blake3::Hash>,
    baseline: Arc<()>,
    pub revision: u64,
}
pub struct SaveReceipt {
    text: Arc<TextState>,
    path: PathBuf,
    hash: blake3::Hash,
    baseline: Arc<()>,
    pub revision: u64,
}
/// Cheap immutable capture for the breakpoint worker. Constructing graphs,
/// projecting spans, line scans and hashes happen on that worker, not the GPU.
pub struct TrackingDocument {
    path: Option<PathBuf>,
    text: Arc<TextState>,
    saved: Option<Arc<TextState>>,
    saved_hash: blake3::Hash,
    changes: Vec<Arc<TextChange>>,
}
impl TrackingDocument {
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn id(&self) -> &TextId {
        &self.text.id
    }
    pub fn current(&self) -> (&Rope, TrackingSnapshot) {
        (
            &self.text.rope,
            TrackingSnapshot::new(
                self.text.id.clone(),
                self.text.rope.len_chars(),
                self.changes.iter().cloned(),
            ),
        )
    }
    pub fn saved(&self) -> Option<(&Rope, TrackingSnapshot, blake3::Hash)> {
        self.saved.as_ref().map(|s| {
            (
                &s.rope,
                TrackingSnapshot::new(
                    s.id.clone(),
                    s.rope.len_chars(),
                    self.changes.iter().cloned(),
                ),
                self.saved_hash,
            )
        })
    }
}
impl SaveReceipt {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn hash(&self) -> blake3::Hash {
        self.hash
    }
}
impl SaveSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn verify(&self, cancel: &impl Fn() -> bool) -> Result<Option<std::fs::Permissions>> {
        ensure!(!cancel(), "Save remaining cancelled");
        if self.expected.is_none() {
            return match std::fs::symlink_metadata(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Ok(_) => anyhow::bail!("file appeared on disk; review before saving"),
                Err(e) => Err(e).context("disk state changed; review before saving"),
            };
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options
            .open(&self.path)
            .context("disk state changed; review before saving")?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "Only regular text files can be saved");
        let mut h = blake3::Hasher::new();
        let mut buffer = [0; 65536];
        loop {
            ensure!(!cancel(), "Save remaining cancelled");
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            h.update(&buffer[..n]);
        }
        ensure!(
            Some(h.finalize()) == self.expected,
            "file changed on disk; review external changes before saving"
        );
        Ok(Some(meta.permissions()))
    }
    /// File I/O/hash/fsync runs on the writer. Cancellation before persist removes
    /// the private temp file; a completed atomic rename always returns its receipt.
    pub fn write(self, cancel: impl Fn() -> bool) -> Result<SaveReceipt> {
        let permissions = self.verify(&cancel)?;
        let mut tmp =
            tempfile::NamedTempFile::new_in(self.path.parent().context("missing parent")?)?;
        if let Some(p) = permissions {
            tmp.as_file().set_permissions(p)?;
        }
        let mut hash = blake3::Hasher::new();
        for chunk in self.text.rope.chunks() {
            ensure!(!cancel(), "Save remaining cancelled");
            tmp.write_all(chunk.as_bytes())?;
            hash.update(chunk.as_bytes());
        }
        tmp.flush()?;
        tmp.as_file().sync_all()?;
        let hash = hash.finalize();
        let _ = self.text.hash.set(hash);
        self.verify(&cancel)?;
        ensure!(!cancel(), "Save remaining cancelled");
        if self.expected.is_none() {
            tmp.persist_noclobber(&self.path)?;
        } else {
            tmp.persist(&self.path)?;
        }
        Ok(SaveReceipt {
            text: self.text,
            path: self.path,
            hash,
            baseline: self.baseline,
            revision: self.revision,
        })
    }
}
/// Exact immutable text and captured syntax readiness for a background renderer.
#[derive(Clone)]
pub struct RenderSnapshot {
    // Drop the extra tree handle before TextState transfers its last owner.
    tree: Option<Tree>,
    text: Arc<TextState>,
}
impl RenderSnapshot {
    pub fn rope(&self) -> &Rope {
        &self.text.rope
    }
    pub fn ready(&self) -> bool {
        self.tree.is_some()
    }
    pub fn matches(&self, doc: &Document) -> bool {
        Arc::ptr_eq(&self.text, &doc.text)
    }
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.text, &other.text) && self.ready() == other.ready()
    }
    pub fn leaf_spans(&self, range: Range<usize>) -> Vec<(Range<usize>, String)> {
        visible_leaves(self.tree.as_ref(), range)
    }
}

/// Cheap immutable syntax job. Publication enriches only its exact text state;
/// it never installs a document, moves a cursor or changes versions/history/disk.
#[derive(Clone)]
pub struct SyntaxSnapshot {
    text: Arc<TextState>,
}
impl SyntaxSnapshot {
    pub fn matches(&self, doc: &Document) -> bool {
        Arc::ptr_eq(&self.text, &doc.text)
    }
    pub fn ready(&self) -> bool {
        self.text.tree.get().is_some()
    }
    pub fn bytes(&self) -> usize {
        self.text.rope.len_bytes()
    }
    pub(crate) fn parse(&self, parser: &mut Parser, cancel: impl Fn() -> bool) -> Result<()> {
        if self.ready() {
            return Ok(());
        }
        ensure!(!cancel(), "Syntax parse cancelled");
        let seed = self.text.seed.lock().unwrap().clone();
        let old = if let Some(mut seed) = seed {
            for edit in &seed.edits {
                ensure!(!cancel(), "Syntax parse cancelled");
                seed.tree.edit(edit);
            }
            Some(seed.tree)
        } else {
            None
        };
        let tree = parse_cancellable(parser, &self.text.rope, old.as_ref(), Some(&cancel))?;
        ensure!(!cancel(), "Syntax parse cancelled");
        let _ = self.text.tree.set(tree);
        let old = self.text.seed.lock().unwrap().take();
        drop(old);
        Ok(())
    }
}
fn parse(parser: &mut Parser, rope: &Rope, old: Option<&Tree>) -> Result<Tree> {
    parse_cancellable(parser, rope, old, None)
}
fn parse_cancellable(
    parser: &mut Parser,
    rope: &Rope,
    old: Option<&Tree>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Tree> {
    let mut progress = |_: &tree_sitter::ParseState| cancel.is_some_and(|f| f());
    parser
        .parse_with_options(
            &mut |byte, _| {
                if byte >= rope.len_bytes() {
                    return &[][..];
                }
                let (chunk, start, _, _) = rope.chunk_at_byte(byte);
                &chunk.as_bytes()[byte - start..]
            },
            old,
            cancel.map(|_| tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        )
        .context("Kotlin parsing was cancelled")
}
fn hash(rope: &Rope) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    for c in rope.chunks() {
        h.update(c.as_bytes());
    }
    h.finalize()
}
impl Document {
    pub fn new(text: &str) -> Result<Self> {
        Self::from_rope(Rope::from_str(text), None)
    }
    fn from_rope(rope: Rope, cancel: Option<&dyn Fn() -> bool>) -> Result<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())?;
        ensure!(!cancel.is_some_and(|f| f()), "Opening document cancelled");
        let tree = parse_cancellable(&mut parser, &rope, None, cancel)?;
        let mut h = blake3::Hasher::new();
        for chunk in rope.chunks() {
            ensure!(!cancel.is_some_and(|f| f()), "Opening document cancelled");
            h.update(chunk.as_bytes());
        }
        let saved_hash = h.finalize();
        let parser_bytes = rope.len_bytes();
        let text = Arc::new(TextState {
            rope,
            id: TextId::new(),
            origin: None,
            tree: OnceLock::from(tree),
            seed: Mutex::new(None),
            hash: OnceLock::from(saved_hash),
            retire_on_drop: AtomicBool::new(false),
        });
        Ok(Self {
            text: text.clone(),
            parser,
            parser_bytes,
            undo: Vec::new(),
            redo: Vec::new(),
            view: None,
            revision: 1,
            version: 1,
            path: None,
            saved_hash,
            saved_text: Some(text),
            disk_hash: None,
            dirty: false,
            disk_baseline: Arc::new(()),
            deferred_syntax: false,
        })
    }
    /// Construct an off-thread document from already verified no-follow disk bytes.
    pub(crate) fn from_verified_text(
        path: PathBuf,
        text: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Self> {
        let mut doc = Self::from_rope(Rope::from_str(text), Some(cancel))?;
        doc.path = Some(path);
        doc.disk_hash = Some(doc.saved_hash);
        doc.defer_syntax();
        Ok(doc)
    }
    /// A project history action must be the actual next per-document history step.
    pub fn history_target_matches(&self, target: &RenderSnapshot, redo: bool) -> bool {
        let history = if redo { &self.redo } else { &self.undo };
        history
            .last()
            .is_some_and(|h| Arc::ptr_eq(&h.text, &target.text))
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_cancellable(path, || false)
    }
    /// Streaming UTF-8/Rope construction and cancellable Tree-sitter parse for background open.
    pub fn open_cancellable(path: impl AsRef<Path>, cancel: impl Fn() -> bool) -> Result<Self> {
        struct Reader<'a> {
            file: std::fs::File,
            cancel: &'a dyn Fn() -> bool,
            bytes: u64,
        }
        impl Read for Reader<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                if (self.cancel)() {
                    return Err(std::io::Error::other("Opening document cancelled"));
                }
                let n = self.file.read(b)?;
                self.bytes += n as u64;
                if self.bytes > u32::MAX as u64 {
                    return Err(std::io::Error::other(
                        "Document exceeds Tree-sitter byte address space",
                    ));
                }
                Ok(n)
            }
        }
        ensure!(!cancel(), "Opening document cancelled");
        let path = path.as_ref().canonicalize()?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = options.open(&path)?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "Only regular text files can be opened");
        ensure!(
            meta.len() <= u32::MAX as u64,
            "Document exceeds Tree-sitter byte address space"
        );
        let rope = Rope::from_reader(Reader {
            file,
            cancel: &cancel,
            bytes: 0,
        })
        .with_context(|| format!("Read UTF-8 {}", path.display()))?;
        let mut doc = Self::from_rope(rope, Some(&cancel))?;
        // Bind the completed parse to a current disk checkpoint, not bytes that
        // changed while a large syntax tree was being built.
        let mut current_options = std::fs::OpenOptions::new();
        current_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            current_options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut current = current_options.open(&path)?;
        ensure!(
            current.metadata()?.is_file(),
            "Source stopped being a regular file while opening"
        );
        ensure!(
            current.metadata()?.len() == doc.text.rope.len_bytes() as u64,
            "File changed while opening · retry"
        );
        let mut h = blake3::Hasher::new();
        let mut bytes = [0; 65536];
        loop {
            ensure!(!cancel(), "Opening document cancelled");
            let n = current.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            h.update(&bytes[..n]);
        }
        ensure!(
            h.finalize() == doc.saved_hash,
            "File changed while opening · retry"
        );
        doc.disk_hash = Some(doc.saved_hash);
        doc.path = Some(path);
        Ok(doc)
    }
    pub fn rope(&self) -> &Rope {
        &self.text.rope
    }
    /// Exact char-coordinate changes for UI-owned anchors, without owning text.
    /// Current/Undo/Redo/accepted Save states stay distinct from mutable LSP clocks.
    pub fn tracking_snapshot(&self) -> TrackingSnapshot {
        TrackingSnapshot::new(
            self.text.id.clone(),
            self.text.rope.len_chars(),
            std::iter::once(&self.text)
                .chain(self.undo.iter().map(|h| &h.text))
                .chain(self.redo.iter().map(|h| &h.text))
                .chain(self.saved_text.iter())
                .filter_map(|state| state.origin.clone()),
        )
    }
    pub fn saved_text_id(&self) -> Option<&TextId> {
        self.saved_text.as_ref().map(|s| &s.id)
    }
    pub fn text_id(&self) -> &TextId {
        &self.text.id
    }
    pub fn tracking_capture(&self) -> TrackingDocument {
        TrackingDocument {
            path: self.path.clone(),
            text: self.text.clone(),
            saved: self.saved_text.clone(),
            saved_hash: self.saved_hash,
            changes: std::iter::once(&self.text)
                .chain(self.undo.iter().map(|h| &h.text))
                .chain(self.redo.iter().map(|h| &h.text))
                .chain(self.saved_text.iter())
                .filter_map(|s| s.origin.clone())
                .collect(),
        }
    }
    /// None means this exact text revision has not finished parsing; never stale ranges.
    pub fn tree(&self) -> Option<&Tree> {
        self.text.tree.get()
    }
    pub fn defer_syntax(&mut self) {
        if !self.deferred_syntax {
            self.text.defer_release();
            for h in self.undo.iter().chain(&self.redo) {
                h.text.defer_release();
            }
            if let Some(saved) = &self.saved_text {
                saved.defer_release();
            }
        }
        self.deferred_syntax = true;
    }
    pub fn syntax_snapshot(&self) -> Option<SyntaxSnapshot> {
        self.tree().is_none().then(|| SyntaxSnapshot {
            text: self.text.clone(),
        })
    }
    fn seed_for_edit(&self, edit: InputEdit) -> Option<SyntaxSeed> {
        let mut seed = if let Some(tree) = self.tree() {
            SyntaxSeed {
                tree: tree.clone(),
                edits: vec![],
            }
        } else {
            self.text.seed.lock().unwrap().clone()?
        };
        if let Some(last) = seed.edits.last_mut() {
            // Adjacent pure insertions/deletions compose exactly, including Unicode
            // and newline points; general replacements stay separate and bounded.
            if last.start_byte == last.old_end_byte
                && edit.start_byte == edit.old_end_byte
                && edit.start_byte == last.new_end_byte
            {
                last.new_end_byte = edit.new_end_byte;
                last.new_end_position = edit.new_end_position;
                return Some(seed);
            }
            if last.start_byte == last.new_end_byte
                && edit.start_byte == edit.new_end_byte
                && edit.old_end_byte == last.start_byte
            {
                last.start_byte = edit.start_byte;
                last.start_position = edit.start_position;
                last.new_end_byte = edit.new_end_byte;
                last.new_end_position = edit.new_end_position;
                return Some(seed);
            }
        }
        if seed.edits.len() >= 64 {
            return None;
        }
        seed.edits.push(edit);
        Some(seed)
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
    fn point(&self, c: usize) -> Point {
        let byte = self.text.rope.char_to_byte(c);
        let row = self.text.rope.char_to_line(c);
        Point {
            row,
            column: byte - self.text.rope.line_to_byte(row),
        }
    }
    fn next_clocks(&self) -> Result<(i32, u64)> {
        Ok((
            self.version
                .checked_add(1)
                .context("document version exhausted")?,
            self.revision
                .checked_add(1)
                .context("document revision exhausted")?,
        ))
    }
    fn history(&self) -> History {
        History {
            text: self.text.clone(),
            view: self.view,
        }
    }
    fn is_saved(&self) -> bool {
        self.saved_text.as_ref().is_some_and(|saved| {
            Arc::ptr_eq(saved, &self.text)
                || (saved.rope.len_bytes() == self.text.rope.len_bytes()
                    && self.text.hash() == self.saved_hash)
        })
    }
    pub fn edit(&mut self, range: Range<usize>, text: &str) -> Result<()> {
        ensure!(
            range.start <= range.end && range.end <= self.text.rope.len_chars(),
            "edit range out of bounds"
        );
        if self.text.rope.slice(range.clone()) == text {
            return Ok(());
        }
        let (version, revision) = self.next_clocks()?;
        let start_byte = self.text.rope.char_to_byte(range.start);
        let old_end_byte = self.text.rope.char_to_byte(range.end);
        let new_len = (self.text.rope.len_bytes() - (old_end_byte - start_byte))
            .checked_add(text.len())
            .context("document size exhausted")?;
        ensure!(
            new_len <= u32::MAX as usize,
            "Document exceeds Tree-sitter byte address space"
        );
        let start_position = self.point(range.start);
        let old_end_position = self.point(range.end);
        let lines = text.bytes().filter(|b| *b == b'\n').count();
        let new_end_position = if lines == 0 {
            Point {
                row: start_position.row,
                column: start_position.column + text.len(),
            }
        } else {
            Point {
                row: start_position.row + lines,
                column: text.rsplit('\n').next().unwrap().len(),
            }
        };
        // Work on persistent clones: the prior Rope AND parsed tree stay immutable.
        // A parse failure must not publish text/history/clocks from a partial edit.
        let mut rope = self.text.rope.clone();
        rope.remove(range.clone());
        rope.insert(range.start, text);
        let edit = InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + text.len(),
            start_position,
            old_end_position,
            new_end_position,
        };
        let id = TextId::new();
        let origin = Arc::new(TextChange::new(
            self.text.id.clone(),
            id.clone(),
            self.text.rope.len_chars(),
            &[(range, text.chars().count())],
        )?);
        let next = Arc::new(TextState {
            rope,
            id,
            origin: Some(origin),
            tree: OnceLock::new(),
            seed: Mutex::new(self.seed_for_edit(edit)),
            hash: OnceLock::new(),
            retire_on_drop: AtomicBool::new(
                self.deferred_syntax
                    && (new_len >= crate::document_retirement::MIN_BYTES
                        || self.text.retire_on_drop.load(Ordering::Acquire)),
            ),
        });
        if !self.deferred_syntax {
            self.parser_bytes = self.parser_bytes.max(new_len);
            SyntaxSnapshot { text: next.clone() }.parse(&mut self.parser, || false)?;
        }
        self.undo.push(self.history());
        if self.undo.len() > 128 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.text = next;
        self.version = version;
        self.revision = revision;
        self.dirty = true;
        Ok(())
    }
    pub fn undo(&mut self) -> Result<bool> {
        if self.undo.is_empty() {
            return Ok(false);
        }
        let (version, revision) = self.next_clocks()?;
        let previous = self.undo.pop().unwrap();
        self.redo.push(self.history());
        self.text = previous.text;
        self.view = previous.view;
        self.version = version;
        self.revision = revision;
        self.dirty = !self.is_saved();
        Ok(true)
    }
    pub fn redo(&mut self) -> Result<bool> {
        if self.redo.is_empty() {
            return Ok(false);
        }
        let (version, revision) = self.next_clocks()?;
        let next = self.redo.pop().unwrap();
        self.undo.push(self.history());
        self.text = next.text;
        self.view = next.view;
        self.version = version;
        self.revision = revision;
        self.dirty = !self.is_saved();
        Ok(true)
    }
    /// Capture exact bytes/baseline without hashing, parsing or filesystem access.
    pub fn save_snapshot(&self) -> Result<SaveSnapshot> {
        Ok(SaveSnapshot {
            text: self.text.clone(),
            path: self.path.clone().context("document has no path")?,
            expected: self.disk_hash,
            baseline: self.disk_baseline.clone(),
            revision: self.revision,
        })
    }
    /// Merge only the written baseline; never replace current text/history/view/clocks.
    pub fn accept_saved(&mut self, receipt: &SaveReceipt) -> Result<()> {
        ensure!(
            self.path.as_ref() == Some(&receipt.path)
                && Arc::ptr_eq(&self.disk_baseline, &receipt.baseline),
            "Saved snapshot baseline changed; buffer retained · review disk"
        );
        self.saved_hash = receipt.hash;
        if self.deferred_syntax {
            receipt.text.defer_release();
        }
        self.saved_text = Some(receipt.text.clone());
        self.disk_hash = Some(receipt.hash);
        self.disk_baseline = Arc::new(());
        // A different, not-yet-hashed state stays conservatively dirty. Do not hash
        // a newer giant buffer on the GPU thread just to accept an older Save.
        self.dirty = !(Arc::ptr_eq(&self.text, &receipt.text)
            || self.text.hash.get().is_some_and(|h| *h == receipt.hash));
        Ok(())
    }
    /// Synchronous library helper. Desktop production uses DocumentSaver instead.
    pub fn save(&mut self) -> Result<()> {
        let receipt = self.save_snapshot()?.write(|| false)?;
        self.accept_saved(&receipt)
    }
    pub fn disk_hash(&self) -> Option<blake3::Hash> {
        self.disk_hash
    }
    /// Exact BLAKE3, shared and memoized per immutable text/tree state.
    /// First hashing of new dirty bytes is linear; repeated calls/history/forks reuse it.
    pub fn content_hash(&self) -> blake3::Hash {
        self.text.hash()
    }
    pub fn set_view(&mut self, anchor: usize, cursor: usize) {
        self.view = Some((anchor, cursor));
    }
    pub fn view(&self) -> Option<(usize, usize)> {
        self.view
    }
    /// Explicitly accept an observed remote baseline, without writing or clearing undo.
    /// Save rechecks this exact hash (or absence); a later disk change still conflicts.
    pub fn acknowledge_disk(&mut self, disk: Option<&Document>) {
        self.disk_hash = disk.and_then(|d| d.disk_hash);
        if let Some(d) = disk {
            if self.deferred_syntax
                && let Some(saved) = &d.saved_text
            {
                saved.defer_release();
            }
            self.disk_baseline = d.disk_baseline.clone();
            self.saved_hash = d.saved_hash;
            self.saved_text = d.saved_text.clone();
            self.dirty = !self.is_saved();
        } else {
            self.disk_baseline = Arc::new(());
            self.saved_text = None;
            self.dirty = true;
        }
    }
    /// Cheap immutable job snapshot: Rope/tree/history share their backing storage.
    pub fn fork(&self) -> Result<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())?;
        Ok(Self {
            text: self.text.clone(),
            parser,
            parser_bytes: 0,
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            view: self.view,
            revision: self.revision,
            version: self.version,
            path: self.path.clone(),
            saved_hash: self.saved_hash,
            saved_text: self.saved_text.clone(),
            disk_hash: self.disk_hash,
            dirty: self.dirty,
            disk_baseline: self.disk_baseline.clone(),
            deferred_syntax: self.deferred_syntax,
        })
    }
    /// One Undo step. UI batches defer parsing too; worker jobs can request an
    /// aligned tree explicitly without changing the document's future UI mode.
    pub fn edit_batch(&mut self, edits: &[(Range<usize>, String)]) -> Result<()> {
        self.edit_batch_impl(edits, !self.deferred_syntax)
    }
    pub fn edit_batch_parsed(&mut self, edits: &[(Range<usize>, String)]) -> Result<()> {
        self.edit_batch_impl(edits, true)
    }
    fn edit_batch_impl(&mut self, edits: &[(Range<usize>, String)], parse_now: bool) -> Result<()> {
        let mut end = 0;
        for (r, _) in edits {
            ensure!(
                r.start >= end && r.end >= r.start && r.end <= self.text.rope.len_chars(),
                "overlapping or invalid batch edits"
            );
            end = r.end;
        }
        if edits.is_empty() {
            return Ok(());
        }
        let mut next = self.text.rope.clone();
        for (r, text) in edits.iter().rev() {
            next.remove(r.clone());
            next.insert(r.start, text);
        }
        if next == self.text.rope {
            return Ok(());
        }
        let (version, revision) = self.next_clocks()?;
        ensure!(
            next.len_bytes() <= u32::MAX as usize,
            "Document exceeds Tree-sitter byte address space"
        );
        let tree = if parse_now {
            self.parser_bytes = self.parser_bytes.max(next.len_bytes());
            OnceLock::from(parse(&mut self.parser, &next, None)?)
        } else {
            OnceLock::new()
        };
        let id = TextId::new();
        let changes = edits
            .iter()
            .filter(|(r, text)| self.text.rope.slice(r.clone()) != text.as_str())
            .map(|(r, text)| (r.clone(), text.chars().count()))
            .collect::<Vec<_>>();
        let origin = Arc::new(TextChange::new(
            self.text.id.clone(),
            id.clone(),
            self.text.rope.len_chars(),
            &changes,
        )?);
        self.undo.push(self.history());
        if self.undo.len() > 128 {
            self.undo.remove(0);
        }
        self.redo.clear();
        let retire = self.deferred_syntax
            && (next.len_bytes() >= crate::document_retirement::MIN_BYTES
                || self.text.retire_on_drop.load(Ordering::Acquire));
        self.text = Arc::new(TextState {
            rope: next,
            id,
            origin: Some(origin),
            tree,
            seed: Mutex::new(None),
            hash: OnceLock::new(),
            retire_on_drop: AtomicBool::new(retire),
        });
        self.version = version;
        self.revision = revision;
        self.dirty = true;
        Ok(())
    }
    /// LSP character positions are UTF-16 code units, not bytes or Unicode scalars.
    pub fn lsp_position(&self, c: usize) -> Result<(u32, u32)> {
        ensure!(c <= self.text.rope.len_chars(), "position out of bounds");
        let line = self.text.rope.char_to_line(c);
        let start = self.text.rope.line_to_char(line);
        let units: usize = self
            .text
            .rope
            .slice(start..c)
            .chars()
            .map(char::len_utf16)
            .sum();
        Ok((line as u32, units as u32))
    }
    pub fn char_at_lsp(&self, line: u32, units: u32) -> Result<usize> {
        let line = line as usize;
        ensure!(line < self.text.rope.len_lines(), "line out of bounds");
        let start = self.text.rope.line_to_char(line);
        let mut offset = 0;
        for (i, ch) in self.text.rope.line(line).chars().enumerate() {
            if offset == units {
                return Ok(start + i);
            }
            ensure!(ch != '\n' && ch != '\r', "column beyond line");
            offset += ch.len_utf16() as u32;
            ensure!(offset <= units, "position splits UTF-16 surrogate pair");
        }
        ensure!(offset == units, "column out of bounds");
        Ok(start + self.text.rope.line(line).len_chars())
    }
    pub fn leaf_spans(&self, range: Range<usize>) -> Vec<(Range<usize>, String)> {
        visible_leaves(self.tree(), range)
    }
    /// Cheap immutable paint input; no parser, history, hash or full text copy.
    pub fn render_snapshot(&self) -> RenderSnapshot {
        RenderSnapshot {
            tree: self.tree().cloned(),
            text: self.text.clone(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_unicode_edit_matches_fresh_parse() {
        let mut d = Document::new("fun main() {\n println(\"👋\")\n}\n").unwrap();
        let start = d
            .rope()
            .to_string()
            .chars()
            .position(|c| c == '👋')
            .unwrap();
        d.edit(start..start + 1, "привет\nмир").unwrap();
        let fresh = Document::new(&d.rope().to_string()).unwrap();
        assert_eq!(
            d.tree().unwrap().root_node().to_sexp(),
            fresh.tree().unwrap().root_node().to_sexp()
        );
    }
    #[test]
    fn undo_redo_and_dirty() {
        let mut d = Document::new("val x = 1\n").unwrap();
        d.edit(8..9, "2").unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        assert!(!d.dirty());
        d.redo().unwrap();
        assert_eq!(d.rope().to_string(), "val x = 2\n");
    }
    #[test]
    fn utf16_round_trip() {
        let d = Document::new("a😀б\nfun x() {}\n").unwrap();
        assert_eq!(d.lsp_position(2).unwrap(), (0, 3));
        assert_eq!(d.char_at_lsp(0, 3).unwrap(), 2);
        assert!(d.char_at_lsp(0, 2).is_err());
        assert!(d.char_at_lsp(0, 7).is_err());
    }
    #[test]
    fn invalid_edit_is_nonmutating() {
        let mut d = Document::new("abc").unwrap();
        assert!(d.edit(2..4, "x").is_err());
        assert_eq!(d.rope().to_string(), "abc");
    }
    #[test]
    fn save_and_external_change_guard() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("X.kt");
        std::fs::write(&p, "val x = 1").unwrap();
        let mut d = Document::open(&p).unwrap();
        d.edit(8..9, "2").unwrap();
        d.save().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "val x = 2");
        d.edit(8..9, "3").unwrap();
        std::fs::write(&p, "external").unwrap();
        assert!(d.save().is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "external");
    }
    #[test]
    fn ten_thousand_lines() {
        let text = "val value = 1\n".repeat(10000);
        let mut d = Document::new(&text).unwrap();
        d.edit(0..0, "// header\n").unwrap();
        assert_eq!(d.rope().len_lines(), 10002);
        assert!(!d.tree().unwrap().root_node().has_error());
    }
    #[test]
    fn visible_highlights_are_restricted_to_viewport() {
        let d = Document::new("val x = 1\n".repeat(10000).as_str()).unwrap();
        let start = d.rope().line_to_byte(5000);
        let spans = d.leaf_spans(start..start + 10);
        assert!(spans.len() > 2 && spans.len() < 10);
        assert!(
            spans
                .iter()
                .all(|(r, _)| r.start >= start && r.end <= start + 10)
        );
    }
    #[test]
    fn undo_history_is_bounded_and_noop_does_not_increment_version() {
        let mut d = Document::new("val x = 1").unwrap();
        d.edit(8..9, "1").unwrap();
        assert_eq!(d.version, 1);
        for i in 0..200 {
            d.edit(8..9, if i % 2 == 0 { "2" } else { "1" }).unwrap();
        }
        assert_eq!(d.undo.len(), 128);
        let mut undone = 0;
        while d.undo().unwrap() {
            undone += 1;
        }
        assert_eq!(undone, 128);
    }
    #[test]
    fn accepted_remote_baseline_still_rejects_a_newer_disk_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Main.kt");
        std::fs::write(&p, "original").unwrap();
        let mut d = Document::open(&p).unwrap();
        d.edit(0..8, "local").unwrap();
        std::fs::write(&p, "remote").unwrap();
        let remote = Document::open(&p).unwrap();
        d.acknowledge_disk(Some(&remote));
        assert!(d.dirty());
        assert_eq!(d.rope(), "local");
        std::fs::write(&p, "newer remote").unwrap();
        assert!(d.save().is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "newer remote");
        let remote = Document::open(&p).unwrap();
        d.acknowledge_disk(Some(&remote));
        d.save().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "local");
        assert!(d.undo().unwrap());
        assert_eq!(d.rope(), "original");
    }
    #[test]
    fn explicit_recreate_checks_expected_absence() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Main.kt");
        std::fs::write(&p, "retained").unwrap();
        let mut d = Document::open(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        d.acknowledge_disk(None);
        assert!(d.dirty());
        std::fs::write(&p, "appeared").unwrap();
        assert!(d.save().is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "appeared");
        std::fs::remove_file(&p).unwrap();
        d.save().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "retained");
    }
    #[test]
    fn invalid_batch_never_mutates_document_or_history() {
        let mut d = Document::new("abcdef").unwrap();
        let revision = d.revision;
        assert!(
            d.edit_batch(&[(1..4, "x".into()), (3..5, "y".into())])
                .is_err()
        );
        assert_eq!(d.rope(), "abcdef");
        assert_eq!(d.revision, revision);
        assert!(!d.undo().unwrap());
    }
}

#[cfg(test)]
mod open_tests {
    use super::*;
    #[test]
    fn streaming_open_preserves_utf8_hash_and_can_save_undo() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("Unicode.kt");
        let text = "// 日本語 · русский · 😀\nval example = 42\n".repeat(1000);
        std::fs::write(&p, &text).unwrap();
        let mut doc = Document::open_cancellable(&p, || false).unwrap();
        assert_eq!(doc.rope().to_string(), text);
        assert_eq!(doc.disk_hash(), Some(blake3::hash(text.as_bytes())));
        doc.edit(0..0, "// edit\n").unwrap();
        assert!(doc.undo().unwrap());
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), text);
    }
    #[test]
    fn cancel_stream_and_parser_without_publishing_partial_document() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("Large.kt");
        std::fs::write(&p, "val example = 42\n".repeat(40000)).unwrap();
        assert!(Document::open_cancellable(&p, || true).is_err());
        let calls = std::cell::Cell::new(0);
        let result = Document::open_cancellable(&p, || {
            let n = calls.get() + 1;
            calls.set(n);
            n > 12
        });
        assert!(result.is_err());
        assert!(calls.get() > 12);
        let rope = Rope::from_str(&"val example = 42\n".repeat(40000));
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(
            parse_cancellable(
                &mut parser,
                &rope,
                None,
                Some(&|| {
                    calls.set(calls.get() + 1);
                    true
                })
            )
            .is_err()
        );
        assert!(calls.get() > 0);
    }
}

#[cfg(test)]
mod open_race_tests {
    use super::*;
    #[test]
    fn changed_file_is_not_published_after_slow_open() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("A.kt");
        std::fs::write(&p, "val source=1\n".repeat(20000)).unwrap();
        let calls = std::cell::Cell::new(0);
        let result = Document::open_cancellable(&p, || {
            let n = calls.get() + 1;
            calls.set(n);
            if n == 5 {
                std::fs::write(&p, "val replacement=2\n").unwrap();
            }
            false
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(p).unwrap(), "val replacement=2\n");
    }
    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_blocking_reader() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("pipe.kt");
        let name = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(Document::open(&p).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod history_snapshot_tests {
    use super::*;
    fn same_parse(d: &Document) {
        fn nodes(n: tree_sitter::Node<'_>, out: &mut Vec<(String, Range<usize>, Point, Point)>) {
            out.push((
                n.kind().into(),
                n.byte_range(),
                n.start_position(),
                n.end_position(),
            ));
            let mut c = n.walk();
            for child in n.children(&mut c) {
                nodes(child, out);
            }
        }
        let fresh = Document::new(&d.rope().to_string()).unwrap();
        let (mut actual, mut expected) = (vec![], vec![]);
        nodes(d.tree().unwrap().root_node(), &mut actual);
        nodes(fresh.tree().unwrap().root_node(), &mut expected);
        assert_eq!(actual, expected);
    }
    #[test]
    fn undo_redo_restore_exact_persistent_tree_and_selection_without_reparse() {
        let mut d = Document::new("fun greet() = \"😀\"\n").unwrap();
        d.set_view(4, 9);
        let before = d.text.clone();
        d.edit(4..9, "hello").unwrap();
        d.set_view(9, 9);
        let after = d.text.clone();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(d.undo().unwrap());
        assert!(Arc::ptr_eq(&before, &d.text));
        assert_eq!(d.view(), Some((4, 9)));
        assert_eq!(d.version, 3);
        same_parse(&d);
        assert!(d.redo().unwrap());
        assert!(Arc::ptr_eq(&after, &d.text));
        assert_eq!(d.view(), Some((9, 9)));
        assert_eq!(d.version, 4);
        same_parse(&d);
        // The previous tree was never mutated by tree.edit on its clone.
        assert_eq!(
            before.tree.get().unwrap().root_node().end_byte(),
            before.rope.len_bytes()
        );
    }
    #[test]
    fn immutable_hash_cache_is_shared_by_history_and_forks_not_new_edits() {
        let mut d = Document::new("val x = 1\n").unwrap();
        d.edit(8..9, "2").unwrap();
        assert!(d.text.hash.get().is_none());
        let old = d.text.clone();
        let mut fork = d.fork().unwrap();
        assert!(Arc::ptr_eq(&fork.text, &old));
        let expected = blake3::hash(b"val x = 2\n");
        assert_eq!(fork.content_hash(), expected);
        assert_eq!(old.hash.get(), Some(&expected));
        for _ in 0..50 {
            assert_eq!(d.content_hash(), expected);
        }
        fork.edit(8..9, "3").unwrap();
        assert!(!Arc::ptr_eq(&fork.text, &old));
        assert!(fork.text.hash.get().is_none());
        assert_eq!(d.content_hash(), expected);
        assert_eq!(fork.content_hash(), blake3::hash(b"val x = 3\n"));
        assert!(fork.undo().unwrap());
        assert!(Arc::ptr_eq(&fork.text, &old));
        assert_eq!(fork.content_hash(), expected);
        assert!(fork.redo().unwrap());
        assert_eq!(fork.content_hash(), blake3::hash(b"val x = 3\n"));
    }
    #[test]
    fn common_insertion_history_needs_no_dirty_hash_or_parse_on_undo() {
        let mut d = Document::new("val x = 1\n").unwrap();
        let base = d.text.clone();
        d.edit(0..0, "// first\n").unwrap();
        let first = d.text.clone();
        d.edit(0..0, "// second\n").unwrap();
        assert!(first.hash.get().is_none());
        d.undo().unwrap();
        assert!(Arc::ptr_eq(&first, &d.text));
        assert!(d.dirty());
        assert!(first.hash.get().is_none());
        d.undo().unwrap();
        assert!(Arc::ptr_eq(&base, &d.text));
        assert!(!d.dirty());
        d.redo().unwrap();
        assert!(Arc::ptr_eq(&first, &d.text));
        assert!(first.hash.get().is_none());
    }
    #[test]
    fn save_in_middle_of_history_recomputes_dirty_against_new_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("A.kt");
        std::fs::write(&p, "val x = 1\n").unwrap();
        let mut d = Document::open(&p).unwrap();
        d.edit(8..9, "2").unwrap();
        d.save().unwrap();
        let saved = d.text.clone();
        d.edit(8..9, "3").unwrap();
        d.undo().unwrap();
        assert!(Arc::ptr_eq(&saved, &d.text));
        assert!(!d.dirty());
        d.undo().unwrap();
        assert!(d.dirty());
        assert_eq!(d.content_hash(), blake3::hash(b"val x = 1\n"));
        d.redo().unwrap();
        assert!(!d.dirty());
        d.redo().unwrap();
        assert!(d.dirty());
        d.undo().unwrap();
        d.edit(8..9, "4").unwrap();
        assert!(!d.redo().unwrap());
        assert_eq!(std::fs::read_to_string(p).unwrap(), "val x = 2\n");
    }
    #[test]
    fn absent_disk_baseline_stays_dirty_through_history_until_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("A.kt");
        std::fs::write(&p, "val x = 1\n").unwrap();
        let mut d = Document::open(&p).unwrap();
        d.edit(8..9, "2").unwrap();
        std::fs::remove_file(&p).unwrap();
        d.acknowledge_disk(None);
        d.undo().unwrap();
        assert!(d.dirty());
        assert!(d.saved_text.is_none());
        d.redo().unwrap();
        assert!(d.dirty());
        d.save().unwrap();
        assert!(!d.dirty());
        d.undo().unwrap();
        assert!(d.dirty());
        d.redo().unwrap();
        assert!(!d.dirty());
    }
    #[test]
    fn equivalent_remote_baseline_and_manual_revert_use_exact_hash_not_identity() {
        let mut d = Document::new("val x = 1\n").unwrap();
        d.edit(8..9, "2").unwrap();
        d.edit(8..9, "1").unwrap();
        let equal = d.text.clone();
        d.edit(8..9, "3").unwrap();
        d.undo().unwrap();
        assert!(Arc::ptr_eq(&equal, &d.text));
        assert!(!d.dirty());
        let remote = Document::new("val x = 1\n").unwrap();
        assert!(!Arc::ptr_eq(&remote.text, &d.text));
        d.acknowledge_disk(Some(&remote));
        assert!(!d.dirty());
        d.undo().unwrap();
        assert!(d.dirty());
        d.redo().unwrap();
        assert!(!d.dirty());
    }
    #[test]
    fn exhausted_clocks_leave_edit_batch_undo_redo_and_history_unchanged() {
        for revision_exhausted in [false, true] {
            let mut d = Document::new("abc").unwrap();
            d.edit(0..1, "x").unwrap();
            d.undo().unwrap();
            if revision_exhausted {
                d.revision = u64::MAX;
            } else {
                d.version = i32::MAX;
            }
            let state = d.text.clone();
            let clocks = (d.version, d.revision);
            let sizes = (d.undo.len(), d.redo.len());
            assert!(d.edit(0..1, "y").is_err());
            assert!(d.edit_batch(&[(0..1, "z".into())]).is_err());
            assert!(d.redo().is_err());
            assert!(Arc::ptr_eq(&state, &d.text));
            assert_eq!((d.version, d.revision), clocks);
            assert_eq!((d.undo.len(), d.redo.len()), sizes);
            assert!(d.edit(0..1, "a").is_ok());
            assert!(d.edit_batch(&[]).is_ok());
            // An actual undo has the same all-or-nothing clock guard.
            d.version = 10;
            d.revision = 10;
            d.redo().unwrap();
            let state = d.text.clone();
            d.version = i32::MAX;
            assert!(d.undo().is_err());
            assert!(Arc::ptr_eq(&state, &d.text));
            assert_eq!(d.undo.len(), 1);
        }
    }
    #[test]
    fn mixed_unicode_edits_batches_and_history_match_fresh_tree_ranges() {
        let mut d = Document::new("fun greet() {\n println(\"👋\")\n}\n").unwrap();
        let mut states = vec![(d.text.clone(), d.content_hash())];
        for n in 0..24 {
            if n % 3 == 0 {
                d.edit_batch(&[
                    (0..0, format!("// comment{n} 日本語\n")),
                    (d.rope().len_chars()..d.rope().len_chars(), "\n".into()),
                ])
                .unwrap();
            } else {
                let p = d.rope().len_chars();
                d.edit(p..p, &format!("// 😀 русский{n}\n")).unwrap();
            }
            same_parse(&d);
            states.push((d.text.clone(), d.content_hash()));
        }
        for (state, h) in states.iter().rev().skip(1) {
            assert!(d.undo().unwrap());
            assert!(Arc::ptr_eq(state, &d.text));
            assert_eq!(d.content_hash(), *h);
            same_parse(&d);
        }
        assert!(!d.dirty());
        assert!(!d.undo().unwrap());
        for (state, h) in states.iter().skip(1) {
            assert!(d.redo().unwrap());
            assert!(Arc::ptr_eq(state, &d.text));
            assert_eq!(d.content_hash(), *h);
            same_parse(&d);
        }
        assert!(!d.redo().unwrap());
    }
}

#[cfg(test)]
mod save_snapshot_tests {
    use super::*;
    fn doc() -> (tempfile::TempDir, Document) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("A.kt");
        std::fs::write(&p, "val x = 1\n").unwrap();
        let d = Document::open(&p).unwrap();
        (dir, d)
    }
    #[test]
    fn save_receipt_preserves_newer_text_history_view_and_clocks_without_hashing() {
        let (_dir, mut d) = doc();
        d.edit(8..9, "2").unwrap();
        d.set_view(8, 9);
        let snapshot = d.save_snapshot().unwrap();
        d.edit(8..9, "3").unwrap();
        d.set_view(9, 9);
        let (v, r) = (d.version, d.revision);
        assert!(d.text.hash.get().is_none());
        let receipt = snapshot.write(|| false).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path.as_ref().unwrap()).unwrap(),
            "val x = 2\n"
        );
        d.accept_saved(&receipt).unwrap();
        assert_eq!(d.rope(), "val x = 3\n");
        assert!(d.dirty());
        assert_eq!(d.view(), Some((9, 9)));
        assert_eq!((d.version, d.revision), (v, r));
        assert!(d.text.hash.get().is_none());
        d.undo().unwrap();
        assert_eq!(d.rope(), "val x = 2\n");
        assert!(!d.dirty());
        assert_eq!(d.view(), Some((8, 9)));
        d.redo().unwrap();
        assert!(d.dirty());
        d.save().unwrap();
        assert!(!d.dirty());
    }
    #[test]
    fn duplicate_or_changed_baseline_receipt_never_overwrites_current_baseline() {
        let (_dir, mut d) = doc();
        d.edit(8..9, "2").unwrap();
        let snapshot = d.save_snapshot().unwrap();
        let remote = Document::open(d.path.as_ref().unwrap()).unwrap();
        d.acknowledge_disk(Some(&remote));
        let baseline = d.disk_hash();
        let receipt = snapshot.write(|| false).unwrap();
        assert!(d.accept_saved(&receipt).is_err());
        assert_eq!(d.disk_hash(), baseline);
        assert!(d.dirty());
        let remote = Document::open(d.path.as_ref().unwrap()).unwrap();
        d.acknowledge_disk(Some(&remote));
        let receipt = d.save_snapshot().unwrap().write(|| false).unwrap();
        d.accept_saved(&receipt).unwrap();
        assert!(d.accept_saved(&receipt).is_err());
        assert!(!d.dirty());
    }
    #[test]
    fn background_fork_inherits_exact_current_baseline_token() {
        let (_dir, mut d) = doc();
        d.edit(8..9, "2").unwrap();
        let snapshot = d.save_snapshot().unwrap();
        let mut fork = d.fork().unwrap();
        fork.edit_batch(&[(8..9, "3".into())]).unwrap();
        fork.acknowledge_disk(Some(&d));
        let receipt = snapshot.write(|| false).unwrap();
        fork.accept_saved(&receipt).unwrap();
        assert_eq!(fork.rope(), "val x = 3\n");
        assert!(fork.dirty());
        fork.undo().unwrap();
        assert!(!fork.dirty());
    }
    #[test]
    fn cancellation_and_external_change_leave_disk_and_temp_inventory_unchanged() {
        let (dir, mut d) = doc();
        d.edit(0..0, &"// comment\n".repeat(5000)).unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(
            d.save_snapshot()
                .unwrap()
                .write(|| {
                    calls.set(calls.get() + 1);
                    calls.get() > 6
                })
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(d.path.as_ref().unwrap()).unwrap(),
            "val x = 1\n"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let snapshot = d.save_snapshot().unwrap();
        std::fs::write(d.path.as_ref().unwrap(), "external\n").unwrap();
        assert!(snapshot.write(|| false).is_err());
        assert_eq!(
            std::fs::read_to_string(d.path.as_ref().unwrap()).unwrap(),
            "external\n"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn absent_target_recreate_refuses_a_later_file_and_preserves_modes() {
        let (_dir, mut d) = doc();
        let path = d.path.clone().unwrap();
        std::fs::remove_file(&path).unwrap();
        d.acknowledge_disk(None);
        let snapshot = d.save_snapshot().unwrap();
        std::fs::write(&path, "appeared").unwrap();
        assert!(snapshot.write(|| false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "appeared");
        std::fs::remove_file(&path).unwrap();
        d.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            d.edit(8..9, "2").unwrap();
            d.save().unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_save_targets_reject_without_blocking_or_replacement() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let (_dir, d) = doc();
        let path = d.path.clone().unwrap();
        std::fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(d.save_snapshot().unwrap().write(|| false).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        std::fs::remove_file(&path).unwrap();
        symlink(path.with_file_name("missing"), &path).unwrap();
        assert!(d.save_snapshot().unwrap().write(|| false).is_err());
        assert!(
            std::fs::symlink_metadata(path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[cfg(test)]
mod deferred_syntax_tests {
    use super::*;
    fn parser() -> Parser {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        p
    }
    fn equivalent(d: &Document) {
        fn nodes(n: tree_sitter::Node<'_>, out: &mut Vec<(String, Range<usize>, Point, Point)>) {
            out.push((
                n.kind().into(),
                n.byte_range(),
                n.start_position(),
                n.end_position(),
            ));
            let mut c = n.walk();
            for x in n.children(&mut c) {
                nodes(x, out);
            }
        }
        let fresh = Document::new(&d.rope().to_string()).unwrap();
        let (mut a, mut b) = (vec![], vec![]);
        nodes(d.tree().unwrap().root_node(), &mut a);
        nodes(fresh.tree().unwrap().root_node(), &mut b);
        assert_eq!(a, b);
    }
    #[test]
    fn deferred_edit_publishes_rope_clocks_history_but_never_stale_spans() {
        let mut d = Document::new("val x=1\n").unwrap();
        let original = d.text.clone();
        d.defer_syntax();
        d.set_view(6, 7);
        d.edit(6..7, "日本語😀").unwrap();
        assert_eq!(d.version, 2);
        assert_eq!(d.revision, 2);
        assert!(d.tree().is_none());
        assert!(d.leaf_spans(0..d.rope().len_bytes()).is_empty());
        assert_eq!(original.tree.get().unwrap().root_node().end_byte(), 8);
        let snapshot = d.syntax_snapshot().unwrap();
        let clocks = (d.version, d.revision);
        let view = d.view();
        snapshot.parse(&mut parser(), || false).unwrap();
        assert!(snapshot.matches(&d));
        assert_eq!((d.version, d.revision), clocks);
        assert_eq!(d.view(), view);
        equivalent(&d);
        assert!(!d.leaf_spans(0..d.rope().len_bytes()).is_empty());
        d.undo().unwrap();
        assert!(Arc::ptr_eq(&original, &d.text));
        assert!(d.tree().is_some());
        assert!(!d.dirty());
        d.redo().unwrap();
        equivalent(&d);
    }
    #[test]
    fn adjacent_insertions_and_backspaces_compose_exact_unicode_newline_points() {
        for deletion in [false, true] {
            let mut d = Document::new("// abc😀\n// 日本語\nval x=1\n").unwrap();
            d.defer_syntax();
            if deletion {
                d.edit(3..7, "").unwrap();
                d.edit(0..3, "").unwrap();
            } else {
                d.edit(0..0, "// added\n").unwrap();
                let p = "// added\n".chars().count();
                d.edit(p..p, "// 😀日本語\n").unwrap();
            }
            assert_eq!(d.text.seed.lock().unwrap().as_ref().unwrap().edits.len(), 1);
            d.syntax_snapshot()
                .unwrap()
                .parse(&mut parser(), || false)
                .unwrap();
            assert!(d.text.seed.lock().unwrap().is_none());
            equivalent(&d);
        }
    }
    #[test]
    fn long_coalesced_unicode_burst_has_no_parent_state_chain_and_matches_fresh() {
        let mut d = Document::new("val x=1\n").unwrap();
        let original = d.text.clone();
        d.defer_syntax();
        for n in 0..320 {
            let p = d.rope().len_chars();
            d.edit(p..p, &format!("// 日本語😀 {n}\r\n")).unwrap();
        }
        assert_eq!(d.text.seed.lock().unwrap().as_ref().unwrap().edits.len(), 1);
        assert_eq!(d.undo.len(), 128);
        assert_eq!(Arc::strong_count(&original), 2);
        d.syntax_snapshot()
            .unwrap()
            .parse(&mut parser(), || false)
            .unwrap();
        equivalent(&d);
        assert_eq!(original.tree.get().unwrap().root_node().end_byte(), 8);
    }
    #[test]
    fn nonadjacent_seed_is_bounded_and_falls_back_to_actual_fresh_parse() {
        let mut d = Document::new("fun value() = \"😀\"\n").unwrap();
        d.defer_syntax();
        for n in 0..90 {
            let p = if n % 2 == 0 { 0 } else { d.rope().len_chars() };
            d.edit(p..p, "// x\n").unwrap();
            assert!(
                d.text
                    .seed
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_none_or(|s| s.edits.len() <= 64)
            );
        }
        assert!(d.text.seed.lock().unwrap().is_none());
        assert!(d.tree().is_none());
        d.syntax_snapshot()
            .unwrap()
            .parse(&mut parser(), || false)
            .unwrap();
        equivalent(&d);
    }
    #[test]
    fn stale_snapshot_only_enriches_its_own_history_state_never_current_bytes() {
        let mut d = Document::new("val x=1\n").unwrap();
        d.defer_syntax();
        d.edit(6..7, "2").unwrap();
        let old = d.syntax_snapshot().unwrap();
        d.edit(6..7, "3").unwrap();
        let latest = d.syntax_snapshot().unwrap();
        old.parse(&mut parser(), || false).unwrap();
        assert!(!old.matches(&d));
        assert!(d.tree().is_none());
        assert_eq!(d.rope(), "val x=3\n");
        latest.parse(&mut parser(), || false).unwrap();
        equivalent(&d);
        d.undo().unwrap();
        assert!(old.matches(&d));
        equivalent(&d);
        d.redo().unwrap();
        equivalent(&d);
    }
    #[test]
    fn cancellation_retry_and_save_do_not_require_or_publish_partial_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A.kt");
        std::fs::write(&path, "val x=1\n".repeat(20000)).unwrap();
        let mut d = Document::open(&path).unwrap();
        d.defer_syntax();
        d.edit(0..0, "// newer 😀\n").unwrap();
        let snapshot = d.syntax_snapshot().unwrap();
        let calls = std::cell::Cell::new(0);
        let mut p = parser();
        assert!(
            snapshot
                .parse(&mut p, || {
                    calls.set(calls.get() + 1);
                    calls.get() > 3
                })
                .is_err()
        );
        assert!(d.tree().is_none());
        d.save().unwrap();
        assert!(!d.dirty());
        assert!(d.tree().is_none());
        assert_eq!(std::fs::read_to_string(path).unwrap(), d.rope().to_string());
        p.reset();
        snapshot.parse(&mut p, || false).unwrap();
        equivalent(&d);
        assert!(!d.dirty());
    }
    #[test]
    fn foreground_batch_defers_parsing_and_preserves_one_undo_step() {
        let mut d = Document::new("val a=1\nval b=2\n").unwrap();
        d.defer_syntax();
        d.edit_batch(&[(0..0, "    ".into()), (8..8, "    ".into())])
            .unwrap();
        assert!(d.tree().is_none());
        assert!(d.leaf_spans(0..100).is_empty());
        d.syntax_snapshot()
            .unwrap()
            .parse(&mut parser(), || false)
            .unwrap();
        equivalent(&d);
        d.undo().unwrap();
        assert_eq!(d.rope(), "val a=1\nval b=2\n");
        assert!(d.tree().is_some());
        assert!(!d.dirty());
    }
    #[test]
    fn batch_worker_returns_aligned_tree_even_for_a_deferred_fork() {
        let mut d = Document::new("val x=1\nval y=2\n").unwrap();
        d.defer_syntax();
        d.edit(6..7, "3").unwrap();
        let mut fork = d.fork().unwrap();
        fork.edit_batch_parsed(&[(6..7, "4".into()), (14..15, "5".into())])
            .unwrap();
        assert!(fork.tree().is_some());
        equivalent(&fork);
        assert!(d.tree().is_none());
        assert_eq!(d.rope(), "val x=3\nval y=2\n");
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    fn large() -> Document {
        Document::new(&"val number = 123\n".repeat(5000)).unwrap()
    }
    fn flagged(state: &Arc<TextState>) -> bool {
        state.retire_on_drop.load(Ordering::Acquire)
    }
    #[test]
    fn desktop_marks_existing_history_without_text_view_clock_or_tree_mutation() {
        let mut d = large();
        d.edit(0..0, "// first\n").unwrap();
        d.undo().unwrap();
        d.set_view(1, 4);
        let before = (
            d.content_hash(),
            d.version,
            d.revision,
            d.view(),
            d.tree().unwrap().root_node().to_sexp(),
        );
        assert!(!flagged(&d.text));
        d.defer_syntax();
        assert!(flagged(&d.text) && flagged(d.saved_text.as_ref().unwrap()));
        assert!(d.undo.iter().chain(&d.redo).all(|h| flagged(&h.text)));
        assert_eq!(
            before,
            (
                d.content_hash(),
                d.version,
                d.revision,
                d.view(),
                d.tree().unwrap().root_node().to_sexp()
            )
        );
    }
    #[test]
    fn tiny_replacement_retains_large_seed_retirement_even_after_history_eviction() {
        let mut d = large();
        d.defer_syntax();
        d.edit(0..d.rope().len_chars(), "x").unwrap();
        assert!(flagged(&d.text));
        assert!(d.text.seed.lock().unwrap().is_some());
        let tiny = Arc::downgrade(&d.text);
        for _ in 0..140 {
            d.edit(d.rope().len_chars()..d.rope().len_chars(), "x")
                .unwrap();
        }
        assert_eq!(d.undo.len(), 128);
        assert!(tiny.upgrade().is_none());
        assert!(flagged(&d.text));
    }
    #[test]
    fn shared_fork_and_pending_syntax_hold_exact_state_until_last_owner() {
        let mut d = large();
        d.defer_syntax();
        d.edit(0..0, "// dirty\n").unwrap();
        let weak = Arc::downgrade(&d.text);
        let snapshot = d.syntax_snapshot().unwrap();
        let fork = d.fork().unwrap();
        assert_eq!(fork.parser_bytes, 0);
        assert!(flagged(&fork.text));
        d.undo().unwrap();
        d.edit(0..0, "// new branch\n").unwrap();
        assert!(weak.upgrade().is_some());
        drop(fork);
        assert!(weak.upgrade().is_some());
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        snapshot.parse(&mut parser, || false).unwrap();
        assert!(snapshot.ready());
        assert!(!snapshot.matches(&d));
        drop(snapshot);
        assert!(weak.upgrade().is_none());
        assert_eq!(d.rope().line(0), "// new branch\n");
    }
    #[test]
    fn external_saved_baseline_and_small_batches_do_not_reintroduce_large_main_thread_drops() {
        let mut d = Document::new("val small=1").unwrap();
        d.defer_syntax();
        let remote = large();
        d.acknowledge_disk(Some(&remote));
        assert!(flagged(d.saved_text.as_ref().unwrap()));
        assert!(flagged(&remote.text));
        let mut parsed = large();
        parsed.defer_syntax();
        parsed
            .edit_batch_parsed(&[(0..parsed.rope().len_chars(), "val small=1".into())])
            .unwrap();
        assert!(flagged(&parsed.text));
        assert!(parsed.parser_bytes >= crate::document_retirement::MIN_BYTES);
        parsed.undo().unwrap();
        assert!(parsed.tree().is_some());
    }
    #[test]
    fn small_never_large_desktop_and_synchronous_library_revisions_stay_inline() {
        let mut d = Document::new("val small=1").unwrap();
        d.defer_syntax();
        d.edit(0..0, "// comment\n").unwrap();
        assert!(!flagged(&d.text));
        assert!(d.parser_bytes < crate::document_retirement::MIN_BYTES);
        let mut sync = large();
        sync.edit(0..0, "// sync\n").unwrap();
        assert!(!flagged(&sync.text));
        assert!(sync.tree().is_some());
        assert!(!sync.deferred_syntax);
    }
}

fn visible_leaves(tree: Option<&Tree>, range: Range<usize>) -> Vec<(Range<usize>, String)> {
    fn visit(n: tree_sitter::Node<'_>, r: &Range<usize>, out: &mut Vec<(Range<usize>, String)>) {
        if n.end_byte() <= r.start || n.start_byte() >= r.end {
            return;
        }
        if n.child_count() == 0 {
            out.push((n.byte_range(), n.kind().to_owned()));
        } else {
            let mut c = n.walk();
            if c.goto_first_child_for_byte(r.start).is_some() {
                loop {
                    let child = c.node();
                    if child.start_byte() >= r.end {
                        break;
                    }
                    visit(child, r, out);
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(tree) = tree {
        visit(tree.root_node(), &range, &mut out);
    }
    out
}

#[cfg(test)]
mod render_snapshot_tests {
    use super::*;
    #[test]
    fn render_snapshot_freezes_bytes_syntax_readiness_and_state_identity() {
        let mut d = Document::new("// exact Ж😀\nval n = 1\n").unwrap();
        d.defer_syntax();
        let original = d.render_snapshot();
        let leaves = original.leaf_spans(0..d.rope().len_bytes());
        d.edit(0..0, "// newer\n").unwrap();
        let pending = d.render_snapshot();
        assert!(!pending.ready() && !original.matches(&d));
        assert_eq!(original.rope().to_string(), "// exact Ж😀\nval n = 1\n");
        assert_eq!(original.leaf_spans(0..original.rope().len_bytes()), leaves);
        let work = d.syntax_snapshot().unwrap();
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        work.parse(&mut parser, || false).unwrap();
        assert!(d.render_snapshot().ready());
        assert!(!pending.ready() && pending.matches(&d));
        assert!(!pending.same(&d.render_snapshot()));
        assert!(pending.leaf_spans(0..pending.rope().len_bytes()).is_empty());
        d.undo().unwrap();
        assert!(original.same(&d.render_snapshot()));
    }
    #[test]
    fn tracking_metadata_and_anchors_do_not_retain_text_states_or_parent_chains() {
        use crate::text_tracking::Projection;
        let mut d = Document::new("target();\n").unwrap();
        d.defer_syntax();
        d.acknowledge_disk(None);
        let weak = Arc::downgrade(&d.text);
        let initial = d.tracking_snapshot();
        let mut anchor = initial.anchor(0..9).unwrap();
        for _ in 0..150 {
            d.edit(0..0, " ").unwrap();
            assert!(matches!(
                d.tracking_snapshot().project(&mut anchor).unwrap(),
                Projection::Mapped(_)
            ));
        }
        assert!(weak.upgrade().is_none());
        let current = Arc::downgrade(&d.text);
        let snapshot = d.tracking_snapshot();
        let fork = d.fork().unwrap();
        drop(d);
        assert!(current.upgrade().is_some());
        drop(fork);
        assert!(current.upgrade().is_none());
        // Both snapshots and a live anchor survive destruction of ALL text owners.
        assert_ne!(initial.id(), snapshot.id());
        assert!(matches!(
            snapshot.project(&mut anchor).unwrap(),
            Projection::Mapped(_)
        ));
    }
}
