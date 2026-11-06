//! Cancellable immutable-Rope search; Unicode options and bounded result storage.
use anyhow::{Result, ensure};
use regex::{Regex, RegexBuilder};
use ropey::Rope;
use std::ops::Range;
pub const MATCH_LIMIT: usize = 50_000;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub within: Option<Range<usize>>,
}
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub matches: Vec<Range<usize>>,
    pub truncated: bool,
    pub error: Option<String>,
}
pub(crate) fn compile(o: &SearchOptions) -> Result<Regex> {
    Ok(RegexBuilder::new(&if o.regex {
        o.query.clone()
    } else {
        regex::escape(&o.query)
    })
    .case_insensitive(!o.case_sensitive)
    .multi_line(true)
    .size_limit(4 * 1024 * 1024)
    .dfa_size_limit(8 * 1024 * 1024)
    .build()?)
}
fn snapshot(rope: &Rope, cancel: &impl Fn() -> bool) -> Result<String> {
    let mut text = String::with_capacity(rope.len_bytes());
    for chunk in rope.chunks() {
        ensure!(!cancel(), "search cancelled");
        text.push_str(chunk);
    }
    Ok(text)
}
pub(crate) fn in_scope(
    text: &str,
    start: usize,
    end: usize,
    o: &SearchOptions,
    rope: &Rope,
) -> bool {
    if let Some(r) = &o.within
        && (start < rope.char_to_byte(r.start.min(rope.len_chars()))
            || end > rope.char_to_byte(r.end.min(rope.len_chars())))
    {
        return false;
    }
    let word = |c: char| {
        static WORD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        WORD.get_or_init(|| Regex::new(r"^\w$").expect("constant Unicode word regex"))
            .is_match(c.encode_utf8(&mut [0; 4]))
    };
    !o.whole_word
        || (!text[..start].chars().next_back().is_some_and(word)
            && !text[end..].chars().next().is_some_and(word))
}
pub fn search(rope: &Rope, o: &SearchOptions, cancel: impl Fn() -> bool) -> Result<SearchResults> {
    if o.query.is_empty() {
        return Ok(SearchResults::default());
    }
    let re = match compile(o) {
        Ok(re) => re,
        Err(e) => {
            return Ok(SearchResults {
                error: Some(e.to_string()),
                ..Default::default()
            });
        }
    };
    let text = snapshot(rope, &cancel)?;
    let mut out = SearchResults::default();
    for m in re.find_iter(&text) {
        ensure!(!cancel(), "search cancelled");
        if !in_scope(&text, m.start(), m.end(), o, rope) {
            continue;
        }
        if out.matches.len() == MATCH_LIMIT {
            out.truncated = true;
            break;
        }
        out.matches
            .push(rope.byte_to_char(m.start())..rope.byte_to_char(m.end()));
    }
    Ok(out)
}
fn replacement_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(c) => {
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}
pub fn replacements(
    rope: &Rope,
    o: &SearchOptions,
    replacement: &str,
    only: Option<Range<usize>>,
    cancel: impl Fn() -> bool,
) -> Result<Vec<(Range<usize>, String)>> {
    ensure!(!o.query.is_empty(), "search query is empty");
    let re = compile(o)?;
    let text = snapshot(rope, &cancel)?;
    let repl = if o.regex {
        replacement_escapes(replacement)
    } else {
        replacement.to_string()
    };
    let mut out = Vec::new();
    let mut bytes = 0;
    for caps in re.captures_iter(&text) {
        ensure!(!cancel(), "replacement cancelled");
        let m = caps.get(0).unwrap();
        if !in_scope(&text, m.start(), m.end(), o, rope) {
            continue;
        }
        let range = rope.byte_to_char(m.start())..rope.byte_to_char(m.end());
        if only.as_ref().is_some_and(|r| *r != range) {
            continue;
        }
        ensure!(
            out.len() < MATCH_LIMIT,
            "more than 50,000 matches; refine the query before Replace All"
        );
        let value = if o.regex {
            let mut value = String::new();
            caps.expand(&repl, &mut value);
            value
        } else {
            repl.clone()
        };
        bytes += value.len();
        ensure!(
            bytes <= 64 * 1024 * 1024,
            "replacement output exceeds 64 MiB; refine the query"
        );
        out.push((range, value));
        if only.is_some() {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_case_word_and_scope_use_character_offsets() {
        let r = Rope::from_str("Привет приветствие привет\nworld");
        let mut o = SearchOptions {
            query: "привет".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(
            search(&r, &o, || false).unwrap().matches,
            vec![0..6, 19..25]
        );
        o.case_sensitive = true;
        assert_eq!(search(&r, &o, || false).unwrap().matches, vec![19..25]);
        o.within = Some(0..20);
        assert!(search(&r, &o, || false).unwrap().matches.is_empty());
    }
    #[test]
    fn invalid_regex_empty_query_and_cancellation() {
        let r = Rope::from_str("x");
        assert!(
            search(
                &r,
                &SearchOptions {
                    query: "[".into(),
                    regex: true,
                    ..Default::default()
                },
                || false
            )
            .unwrap()
            .error
            .is_some()
        );
        assert!(
            search(&r, &Default::default(), || false)
                .unwrap()
                .matches
                .is_empty()
        );
        assert!(
            search(
                &r,
                &SearchOptions {
                    query: "x".into(),
                    ..Default::default()
                },
                || true
            )
            .is_err()
        );
    }
    #[test]
    fn regex_captures_multiline_and_zero_width_replacements() {
        let r = Rope::from_str("val item1 = 1\nval item2 = 2\n");
        let o = SearchOptions {
            query: r"item(\d)".into(),
            regex: true,
            ..Default::default()
        };
        let e = replacements(&r, &o, "name$1\\n", None, || false).unwrap();
        assert_eq!(e[0], (4..9, "name1\n".into()));
        let o = SearchOptions {
            query: "^".into(),
            regex: true,
            ..Default::default()
        };
        assert_eq!(
            search(&r, &o, || false).unwrap().matches,
            vec![0..0, 14..14, 28..28]
        );
        assert_eq!(
            replacements(&r, &o, "// ", None, || false).unwrap().len(),
            3
        );
    }
    #[test]
    fn result_cap_is_explicit_and_replace_all_does_not_silently_truncate() {
        let r = Rope::from_str(&"x ".repeat(MATCH_LIMIT + 1));
        let o = SearchOptions {
            query: "x".into(),
            ..Default::default()
        };
        let found = search(&r, &o, || false).unwrap();
        assert_eq!(found.matches.len(), MATCH_LIMIT);
        assert!(found.truncated);
        assert!(replacements(&r, &o, "a", None, || false).is_err());
        assert_eq!(
            replacements(&r, &o, "a", Some(0..1), || false)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn literal_replacement_is_literal_and_batch_undoable() {
        let mut d = crate::editor::Document::new("foo\nfoo\n").unwrap();
        let o = SearchOptions {
            query: "foo".into(),
            ..Default::default()
        };
        let e = replacements(d.rope(), &o, r"$1\n", None, || false).unwrap();
        d.edit_batch(&e).unwrap();
        assert_eq!(d.rope().to_string(), "$1\\n\n$1\\n\n");
        d.undo().unwrap();
        assert_eq!(d.rope().to_string(), "foo\nfoo\n");
    }
    #[test]
    fn whole_word_preserves_combining_marks_and_connector_context() {
        let rope = Rope::from_str("e\u{301} e e\u{203f}x");
        let options = SearchOptions {
            query: "e".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(
            search(&rope, &options, || false).unwrap().matches,
            vec![3..4]
        );
    }
}
