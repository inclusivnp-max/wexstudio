//! Editor view state in Unicode scalar offsets; movement uses extended graphemes.
//! Preedit is transient and never enters the Rope, undo history, disk or LSP.
use crate::editor::Document;
use anyhow::Result;
use ropey::Rope;
use std::ops::Range;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

#[derive(Clone, Debug)]
pub struct Composition {
    pub range: Range<usize>,
    pub text: String,
}
#[derive(Clone, Debug, Default)]
pub struct EditState {
    pub cursor: usize,
    pub anchor: usize,
    pub preferred_x: Option<f32>,
    pub composition: Option<Composition>,
    pub ime_enabled: bool,
    reject_commit: bool,
    pub scroll_x: f32,
    pub scroll_y: f32,
}
impl EditState {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
    pub fn selected(&self) -> bool {
        self.anchor != self.cursor
    }
    pub fn collapse(&mut self, pos: usize) {
        self.cancel_composition();
        self.cursor = pos;
        self.anchor = pos;
        self.preferred_x = None;
        self.composition = None;
    }
    pub fn select(&mut self, anchor: usize, cursor: usize) {
        self.cancel_composition();
        self.anchor = anchor;
        self.cursor = cursor;
        self.preferred_x = None;
        self.composition = None;
    }
    pub fn clamp(&mut self, doc: &Document) {
        let n = doc.rope().len_chars();
        self.cursor = snap(doc.rope(), self.cursor.min(n));
        self.anchor = snap(doc.rope(), self.anchor.min(n));
    }
    pub fn move_to(&mut self, rope: &Rope, pos: usize, extend: bool) {
        self.cancel_composition();
        let p = snap(rope, pos.min(rope.len_chars()));
        if !extend {
            self.anchor = p;
        }
        self.cursor = p;
        self.preferred_x = None;
        self.composition = None;
    }
    pub fn horizontal(&mut self, rope: &Rope, right: bool, word: bool, extend: bool) {
        let r = self.range();
        let p = if !extend && self.selected() && !word {
            if right { r.end } else { r.start }
        } else if word {
            word_boundary(rope, self.cursor, right)
        } else {
            boundary(rope, self.cursor, right)
        };
        self.move_to(rope, p, extend);
    }
    pub fn select_word(&mut self, rope: &Rope, pos: usize) {
        let r = word_range(rope, pos);
        self.select(r.start, r.end);
    }
    pub fn select_line(&mut self, rope: &Rope, line: usize) {
        let line = line.min(rope.len_lines() - 1);
        self.select(
            rope.line_to_char(line),
            if line + 1 < rope.len_lines() {
                rope.line_to_char(line + 1)
            } else {
                rope.len_chars()
            },
        );
    }
    pub fn selected_text(&self, doc: &Document) -> String {
        doc.rope().slice(self.range()).to_string()
    }
    pub fn replace(&mut self, doc: &mut Document, text: &str) -> Result<()> {
        let r = self.range();
        self.replace_range(doc, r, text)
    }
    pub fn replace_range(&mut self, doc: &mut Document, r: Range<usize>, text: &str) -> Result<()> {
        doc.set_view(self.anchor, self.cursor);
        let cursor = r.start + text.chars().count();
        doc.edit(r, text)?;
        self.collapse(cursor);
        doc.set_view(self.anchor, self.cursor);
        Ok(())
    }
    pub fn delete(&mut self, doc: &mut Document, forward: bool, word: bool) -> Result<()> {
        let r = if self.selected() {
            self.range()
        } else {
            let p = if word {
                word_boundary(doc.rope(), self.cursor, forward)
            } else {
                boundary(doc.rope(), self.cursor, forward)
            };
            p.min(self.cursor)..p.max(self.cursor)
        };
        self.replace_range(doc, r, "")
    }
    pub fn undo(&mut self, doc: &mut Document, redo: bool) -> Result<bool> {
        doc.set_view(self.anchor, self.cursor);
        let changed = if redo { doc.redo()? } else { doc.undo()? };
        if changed {
            if let Some((a, c)) = doc.view() {
                self.select(a, c);
            }
            self.clamp(doc);
            self.composition = None;
        }
        Ok(changed)
    }
    pub fn enable_ime(&mut self) {
        self.ime_enabled = true;
        self.reject_commit = false;
    }
    pub fn preedit(&mut self, text: String) {
        self.reject_commit = false;
        let range = self
            .composition
            .as_ref()
            .map(|c| c.range.clone())
            .unwrap_or_else(|| self.range());
        self.composition = Some(Composition { range, text });
        self.ime_enabled = true;
    }
    pub fn commit(&mut self, doc: &mut Document, text: &str) -> Result<()> {
        if self.reject_commit {
            return Ok(());
        }
        if text.is_empty() {
            self.composition = None;
            self.ime_enabled = false;
            return Ok(());
        }
        let range = self
            .composition
            .as_ref()
            .map(|c| c.range.clone())
            .unwrap_or_else(|| self.range());
        self.replace_range(doc, range, text)?;
        self.reject_commit = false;
        Ok(())
    }
    pub fn cancel_composition(&mut self) {
        if self.composition.is_some() {
            self.reject_commit = true;
        }
        self.composition = None;
        self.ime_enabled = false;
    }
    /// Keep line/column when a clean document is reloaded, not stale absolute offsets.
    pub fn rebase(&mut self, old: &Document, next: &Document) {
        let map = |p: usize| {
            let p = p.min(old.rope().len_chars());
            let line = old.rope().char_to_line(p);
            let col = p - old.rope().line_to_char(line);
            let line = line.min(next.rope().len_lines() - 1);
            next.rope().line_to_char(line) + col.min(line_length(next.rope(), line))
        };
        let a = map(self.anchor);
        let c = map(self.cursor);
        self.select(a, c);
        self.clamp(next);
        self.cancel_composition();
    }
    pub fn indent(&mut self, doc: &mut Document, outdent: bool) -> Result<()> {
        if !outdent && !self.selected() {
            return self.replace(doc, "    ");
        }
        let r = self.range();
        let first = doc.rope().char_to_line(r.start);
        let last = doc
            .rope()
            .char_to_line(if r.end > r.start { r.end - 1 } else { r.end });
        let mut edits = Vec::new();
        for line in first..=last {
            let start = doc.rope().line_to_char(line);
            if outdent {
                let text = doc.rope().line(line);
                let n = if text.chars().next() == Some('\t') {
                    1
                } else {
                    text.chars().take(4).take_while(|c| *c == ' ').count()
                };
                if n > 0 {
                    edits.push((start..start + n, String::new()));
                }
            } else {
                edits.push((start..start, "    ".into()));
            }
        }
        doc.set_view(self.anchor, self.cursor);
        let a = map_position(self.anchor, &edits);
        let c = map_position(self.cursor, &edits);
        doc.edit_batch(&edits)?;
        self.select(a, c);
        doc.set_view(a, c);
        Ok(())
    }
}
pub fn map_position(pos: usize, edits: &[(Range<usize>, String)]) -> usize {
    let mut delta = 0isize;
    for (r, t) in edits {
        if pos < r.start {
            break;
        }
        let n = t.chars().count();
        if pos <= r.end {
            return (r.start as isize + delta) as usize + n;
        }
        delta += n as isize - (r.end - r.start) as isize;
    }
    (pos as isize + delta).max(0) as usize
}
pub fn line_length(rope: &Rope, line: usize) -> usize {
    let s = rope.line(line);
    let mut n = s.len_chars();
    while n > 0 && matches!(s.char(n - 1), '\r' | '\n') {
        n -= 1;
    }
    n
}
pub fn boundary(rope: &Rope, pos: usize, right: bool) -> usize {
    let pos = pos.min(rope.len_chars());
    let byte = rope.char_to_byte(pos);
    if (right && byte == rope.len_bytes()) || (!right && byte == 0) {
        return pos;
    }
    let mut cursor = GraphemeCursor::new(byte, rope.len_bytes(), true);
    let (mut chunk, mut start, _, _) =
        rope.chunk_at_byte(if right { byte } else { byte.saturating_sub(1) });
    loop {
        let result = if right {
            cursor.next_boundary(chunk, start)
        } else {
            cursor.prev_boundary(chunk, start)
        };
        match result {
            Ok(Some(b)) => return rope.byte_to_char(b),
            Ok(None) => return if right { rope.len_chars() } else { 0 },
            Err(GraphemeIncomplete::NextChunk) => {
                (chunk, start, _, _) =
                    rope.chunk_at_byte((start + chunk.len()).min(rope.len_bytes()));
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, start, _, _) = rope.chunk_at_byte(start.saturating_sub(1));
            }
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (c, s, _, _) = rope.chunk_at_byte(end.saturating_sub(1));
                cursor.provide_context(&c[..end - s], s);
            }
            Err(GraphemeIncomplete::InvalidOffset) => {
                unreachable!("Rope chunk and grapheme cursor disagree")
            }
        }
    }
}
pub fn snap(rope: &Rope, pos: usize) -> usize {
    if pos == 0 || pos >= rope.len_chars() {
        return pos.min(rope.len_chars());
    }
    let before = boundary(rope, pos, false);
    let after = boundary(rope, before, true);
    if after == pos { pos } else { before }
}
fn class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        1
    } else if c.is_whitespace() {
        0
    } else {
        2
    }
}
// Consecutive ASCII scalars are grapheme boundaries except CR/LF. Stop one
// scalar before any Unicode edge, then use the existing full GraphemeCursor.
fn ascii_run(rope: &Rope, mut p: usize, kind: u8, right: bool) -> usize {
    let mut chars = rope.chars_at(p);
    if right {
        let Some(mut here) = chars.next() else {
            return p;
        };
        loop {
            if !here.is_ascii() || matches!(here, '\r' | '\n') || class(here) != kind {
                return p;
            }
            let Some(next) = chars.next() else {
                return p + 1;
            };
            if !next.is_ascii() {
                return p;
            }
            p += 1;
            here = next;
        }
    } else {
        let Some(mut prev) = chars.prev() else {
            return p;
        };
        loop {
            if !prev.is_ascii() || matches!(prev, '\r' | '\n') || class(prev) != kind {
                return p;
            }
            let Some(before) = chars.prev() else {
                return p - 1;
            };
            if !before.is_ascii() || matches!(before, '\r' | '\n') {
                return p;
            }
            p -= 1;
            prev = before;
        }
    }
}
pub fn word_range(rope: &Rope, pos: usize) -> Range<usize> {
    if rope.len_chars() == 0 {
        return 0..0;
    }
    let pos = snap(rope, pos.min(rope.len_chars() - 1));
    let kind = class(rope.char(pos));
    let mut a = pos;
    let mut b = boundary(rope, pos, true);
    while a > 0 {
        let prev = boundary(rope, a, false);
        if class(rope.char(prev)) != kind || matches!(rope.char(prev), '\n' | '\r') {
            break;
        }
        a = ascii_run(rope, prev, kind, false);
    }
    while b < rope.len_chars() {
        if class(rope.char(b)) != kind || matches!(rope.char(b), '\n' | '\r') {
            break;
        }
        let end = ascii_run(rope, b, kind, true);
        b = if end == b {
            boundary(rope, b, true)
        } else {
            end
        };
    }
    a..b
}
pub fn word_boundary(rope: &Rope, pos: usize, right: bool) -> usize {
    let n = rope.len_chars();
    let mut p = pos.min(n);
    if right {
        if p == n {
            return n;
        }
        let kind = class(rope.char(p));
        while p < n && class(rope.char(p)) == kind {
            let end = ascii_run(rope, p, kind, true);
            p = if end == p {
                boundary(rope, p, true)
            } else {
                end
            };
        }
        while p < n && class(rope.char(p)) == 0 {
            let end = ascii_run(rope, p, 0, true);
            p = if end == p {
                boundary(rope, p, true)
            } else {
                end
            };
        }
    } else {
        while p > 0 {
            let prev = boundary(rope, p, false);
            if class(rope.char(prev)) != 0 {
                break;
            }
            p = ascii_run(rope, prev, 0, false);
        }
        if p > 0 {
            let kind = class(rope.char(boundary(rope, p, false)));
            while p > 0 {
                let prev = boundary(rope, p, false);
                if class(rope.char(prev)) != kind {
                    break;
                }
                p = ascii_run(rope, prev, kind, false);
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_replacement_and_undo_restore_range() {
        let mut d = Document::new("αβ\nпривет\n").unwrap();
        let mut e = EditState::default();
        e.select(3, 9);
        e.replace(&mut d, "世界").unwrap();
        assert_eq!(d.rope().to_string(), "αβ\n世界\n");
        assert_eq!(e.range(), 5..5);
        e.undo(&mut d, false).unwrap();
        assert_eq!(e.range(), 3..9);
        assert_eq!(e.selected_text(&d), "привет");
        e.undo(&mut d, true).unwrap();
        assert_eq!(e.cursor, 5);
    }
    #[test]
    fn keyboard_extends_reverses_and_collapses() {
        let r = Rope::from_str("ab cd\nEF");
        let mut e = EditState::default();
        e.horizontal(&r, true, false, true);
        e.horizontal(&r, true, false, true);
        assert_eq!(e.range(), 0..2);
        e.horizontal(&r, false, false, false);
        assert_eq!(e.range(), 0..0);
        e.horizontal(&r, true, true, true);
        assert_eq!(e.range(), 0..3);
        e.move_to(&r, 1, true);
        assert_eq!(e.range(), 0..1);
    }
    #[test]
    fn extended_graphemes_and_crlf_are_atomic() {
        let text = "e\u{301}👩🏽‍💻🇲🇩\r\nX";
        let r = Rope::from_str(text);
        let mut p = 0;
        let mut bounds = vec![0];
        while p < r.len_chars() {
            p = boundary(&r, p, true);
            bounds.push(p);
        }
        assert_eq!(bounds, vec![0, 2, 6, 8, 10, 11]);
        for pair in bounds.windows(2) {
            assert_eq!(boundary(&r, pair[1], false), pair[0]);
        }
        let mut d = Document::new(text).unwrap();
        let mut e = EditState::default();
        e.collapse(6);
        e.delete(&mut d, false, false).unwrap();
        assert_eq!(d.rope().to_string(), "e\u{301}🇲🇩\r\nX");
    }
    #[test]
    fn graphemes_cross_rope_chunks() {
        let text = format!(
            "{}{}{}",
            "a".repeat(1000),
            "e\u{301}".repeat(1500),
            "👩‍👩‍👧‍👦".repeat(1500)
        );
        let r = Rope::from_str(&text);
        let expected =
            unicode_segmentation::UnicodeSegmentation::grapheme_indices(text.as_str(), true)
                .map(|(b, _)| r.byte_to_char(b))
                .chain(std::iter::once(r.len_chars()))
                .collect::<Vec<_>>();
        let mut got = vec![0];
        let mut p = 0;
        while p < r.len_chars() {
            p = boundary(&r, p, true);
            got.push(p);
        }
        assert_eq!(got, expected);
        for pair in got.windows(2) {
            assert_eq!(boundary(&r, pair[1], false), pair[0]);
        }
    }
    #[test]
    fn unicode_words_and_line_gestures() {
        let r = Rope::from_str("val привет_2 = 世界\nnext");
        let mut e = EditState::default();
        e.select_word(&r, 7);
        assert_eq!(r.slice(e.range()).to_string(), "привет_2");
        e.select_line(&r, 0);
        assert_eq!(e.range(), 0..18);
    }
    #[test]
    fn preedit_is_ephemeral_commit_is_one_undo_and_cancel_is_lossless() {
        let mut d = Document::new("val x = hello\n").unwrap();
        let mut e = EditState::default();
        e.select(8, 13);
        let version = d.version;
        let revision = d.revision;
        e.enable_ime();
        e.preedit("に".into());
        e.preedit("日本".into());
        assert_eq!(d.version, version);
        assert_eq!(d.revision, revision);
        assert!(!d.dirty());
        e.commit(&mut d, "日本語").unwrap();
        assert_eq!(d.rope().to_string(), "val x = 日本語\n");
        e.undo(&mut d, false).unwrap();
        assert_eq!(d.rope().to_string(), "val x = hello\n");
        assert_eq!(e.range(), 8..13);
        e.preedit("discard".into());
        e.cancel_composition();
        e.commit(&mut d, "late stale commit").unwrap();
        assert_eq!(d.rope().to_string(), "val x = hello\n");
        e.enable_ime();
        e.commit(&mut d, "новое").unwrap();
        assert!(d.rope().to_string().contains("новое"));
    }
    #[test]
    fn indent_batch_is_one_undo() {
        let mut d = Document::new("one\ntwo\nthree\n").unwrap();
        let mut e = EditState::default();
        e.select(0, 8);
        e.indent(&mut d, false).unwrap();
        assert_eq!(d.rope().to_string(), "    one\n    two\nthree\n");
        e.undo(&mut d, false).unwrap();
        assert_eq!(d.rope().to_string(), "one\ntwo\nthree\n");
        assert_eq!(e.range(), 0..8);
    }
    #[test]
    fn reload_rebases_selection_to_line_column() {
        let old = Document::new("abc\nabcdef\n").unwrap();
        let next = Document::new("x\nuvwxyz\nmore\n").unwrap();
        let mut e = EditState::default();
        e.select(5, 8);
        e.rebase(&old, &next);
        assert_eq!(e.range(), 3..6);
    }
}

#[cfg(test)]
mod ascii_word_tests {
    use super::*;
    fn old_word_range(rope: &Rope, pos: usize) -> Range<usize> {
        if rope.len_chars() == 0 {
            return 0..0;
        }
        let pos = snap(rope, pos.min(rope.len_chars() - 1));
        let kind = class(rope.char(pos));
        let mut a = pos;
        let mut b = boundary(rope, pos, true);
        while a > 0 {
            let prev = boundary(rope, a, false);
            if class(rope.char(prev)) != kind || matches!(rope.char(prev), '\n' | '\r') {
                break;
            }
            a = prev;
        }
        while b < rope.len_chars() {
            if class(rope.char(b)) != kind || matches!(rope.char(b), '\n' | '\r') {
                break;
            }
            b = boundary(rope, b, true);
        }
        a..b
    }
    fn old_word_boundary(rope: &Rope, pos: usize, right: bool) -> usize {
        let n = rope.len_chars();
        let mut p = pos.min(n);
        if right {
            if p == n {
                return n;
            }
            let kind = class(rope.char(p));
            while p < n && class(rope.char(p)) == kind {
                p = boundary(rope, p, true);
            }
            while p < n && class(rope.char(p)) == 0 {
                p = boundary(rope, p, true);
            }
        } else {
            while p > 0 {
                let prev = boundary(rope, p, false);
                if class(rope.char(prev)) != 0 {
                    break;
                }
                p = prev;
            }
            if p > 0 {
                let kind = class(rope.char(boundary(rope, p, false)));
                while p > 0 {
                    let prev = boundary(rope, p, false);
                    if class(rope.char(prev)) != kind {
                        break;
                    }
                    p = prev;
                }
            }
        }
        p
    }
    #[test]
    fn ascii_runs_match_original_grapheme_algorithm_at_every_unicode_boundary() {
        let cases = [
            "alpha beta___42... \r\nnext\n",
            "e\u{301}abc a\u{301}b 中文xyz\r\n end",
            "\u{600}abc \u{600}a e\u{301}\u{301}x",
            "👩\u{200d}💻abc 🇯🇵x 🇺🇦 y",
            "한글abc देवनागरीabc a\u{200d}b\n",
        ];
        for text in cases {
            let r = Rope::from_str(&text.repeat(200));
            for p in 0..=r.len_chars() {
                assert_eq!(word_range(&r, p), old_word_range(&r, p), "{text:?}/{p}");
                for right in [false, true] {
                    assert_eq!(
                        word_boundary(&r, p, right),
                        old_word_boundary(&r, p, right),
                        "{text:?}/{p}/{right}"
                    );
                }
            }
        }
    }
    #[test]
    fn real_million_ascii_word_stays_exact_without_per_scalar_tree_seeks() {
        let r = Rope::from_str(&format!("prefix {} suffix\n", "x".repeat(1000000)));
        assert_eq!(word_range(&r, 10), 7..1000007);
        assert_eq!(word_boundary(&r, 7, true), 1000008);
        assert_eq!(word_boundary(&r, 1000007, false), 7);
    }
}
