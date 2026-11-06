//! Exact surviving-character anchors across immutable document revisions.
//!
//! IDs and changes never own a Rope, syntax tree, parser or parent revision.
//! Replacements do not invent identity for newly inserted code. A removed span is
//! explicit, and Undo restores its cached position in the actual historical state.
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Process-local identity, independent of mutable LSP clocks and content hashes.
#[derive(Clone, Debug)]
pub struct TextId(Arc<()>);
impl TextId {
    pub(crate) fn new() -> Self {
        Self(Arc::new(()))
    }
}
impl PartialEq for TextId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TextId {}
impl Hash for TextId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

#[derive(Debug)]
pub(crate) struct Edit {
    before: Range<usize>,
    after: Range<usize>,
}
#[derive(Debug)]
struct Map {
    edits: Vec<Edit>,
    removed: Vec<Range<usize>>,
    before: usize,
    after: usize,
    retire: AtomicBool,
}
impl Drop for Map {
    fn drop(&mut self) {
        let bytes = self
            .edits
            .capacity()
            .saturating_mul(std::mem::size_of::<Edit>())
            .saturating_add(
                self.removed
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Range<usize>>()),
            );
        if self.retire.load(Ordering::Acquire) && bytes >= crate::document_retirement::MIN_BYTES {
            crate::document_retirement::retire(crate::document_retirement::Payload::Tracking {
                edits: std::mem::take(&mut self.edits),
                removed: std::mem::take(&mut self.removed),
            });
        }
    }
}
impl Map {
    fn new(before: usize, edits: &[(Range<usize>, usize)]) -> Result<Self> {
        let mut result = Self {
            edits: Vec::with_capacity(edits.len()),
            removed: Vec::new(),
            before,
            after: before,
            retire: AtomicBool::new(false),
        };
        let (mut old_end, mut new_end) = (0, 0usize);
        for (range, inserted) in edits {
            ensure!(
                old_end <= range.start && range.start <= range.end && range.end <= before,
                "overlapping or invalid tracking edits"
            );
            let start = new_end
                .checked_add(range.start - old_end)
                .ok_or_else(|| anyhow::anyhow!("tracking coordinate overflow"))?;
            let end = start
                .checked_add(*inserted)
                .ok_or_else(|| anyhow::anyhow!("tracking coordinate overflow"))?;
            if !range.is_empty() {
                match result.removed.last_mut() {
                    Some(last) if last.end == range.start => last.end = range.end,
                    _ => result.removed.push(range.clone()),
                }
            }
            result.edits.push(Edit {
                before: range.clone(),
                after: start..end,
            });
            old_end = range.end;
            new_end = end;
        }
        result.after = new_end
            .checked_add(before - old_end)
            .ok_or_else(|| anyhow::anyhow!("tracking coordinate overflow"))?;
        Ok(result)
    }
    fn inverse(&self) -> Result<Self> {
        Self::new(
            self.after,
            &self
                .edits
                .iter()
                .map(|e| (e.after.clone(), e.before.len()))
                .collect::<Vec<_>>(),
        )
    }
    fn character(&self, pos: usize) -> usize {
        let n = self.edits.partition_point(|e| e.before.end <= pos);
        if n == 0 {
            pos
        } else {
            let edit = &self.edits[n - 1];
            edit.after.end + (pos - edit.before.end)
        }
    }
    fn span(&self, range: Range<usize>) -> Result<Option<Range<usize>>> {
        ensure!(
            range.start < range.end && range.end <= self.before,
            "tracking requires a nonempty in-bounds character span"
        );
        let mut first = range.start;
        let n = self.removed.partition_point(|r| r.end <= first);
        if let Some(r) = self.removed.get(n)
            && r.start <= first
        {
            first = r.end;
        }
        if first >= range.end {
            return Ok(None);
        }
        let mut last = range.end - 1;
        let n = self.removed.partition_point(|r| r.start <= last);
        if n > 0 && self.removed[n - 1].end > last {
            let Some(previous) = self.removed[n - 1].start.checked_sub(1) else {
                return Ok(None);
            };
            last = previous;
        }
        if first > last {
            return Ok(None);
        }
        Ok(Some(self.character(first)..self.character(last) + 1))
    }
}

/// One actual atomic edit (including a formatter/refactor batch), in char offsets.
#[derive(Debug)]
pub struct TextChange {
    before: TextId,
    after: TextId,
    forward: Map,
    backward: Map,
}
impl TextChange {
    pub(crate) fn defer_release(&self) {
        self.forward.retire.store(true, Ordering::Release);
        self.backward.retire.store(true, Ordering::Release);
    }
    pub(crate) fn new(
        before: TextId,
        after: TextId,
        len: usize,
        edits: &[(Range<usize>, usize)],
    ) -> Result<Self> {
        let forward = Map::new(len, edits)?;
        let backward = forward.inverse()?;
        Ok(Self {
            before,
            after,
            forward,
            backward,
        })
    }
}
#[derive(Clone)]
struct Edge {
    target: TextId,
    change: Arc<TextChange>,
    forward: bool,
}

/// Metadata-only snapshot of the current document and its bounded live history.
pub struct TrackingSnapshot {
    current: TextId,
    len: usize,
    edges: HashMap<TextId, Vec<Edge>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projection {
    Mapped(Range<usize>),
    Removed,
    /// External reload, unrelated document, or history no longer connecting states.
    Unrelated,
}

/// A UI-owned identity. Toggling/removing an anchor is NOT part of text Undo.
pub struct TextAnchor {
    origin: TextId,
    known: HashMap<TextId, Option<Range<usize>>>,
}
impl TrackingSnapshot {
    pub(crate) fn new(
        current: TextId,
        len: usize,
        changes: impl IntoIterator<Item = Arc<TextChange>>,
    ) -> Self {
        let mut result = Self {
            current,
            len,
            edges: HashMap::new(),
        };
        let mut seen = HashSet::new();
        for c in changes {
            if !seen.insert(c.after.clone()) {
                continue;
            }
            result
                .edges
                .entry(c.before.clone())
                .or_default()
                .push(Edge {
                    target: c.after.clone(),
                    change: c.clone(),
                    forward: true,
                });
            result.edges.entry(c.after.clone()).or_default().push(Edge {
                target: c.before.clone(),
                change: c,
                forward: false,
            });
        }
        result
    }
    pub fn id(&self) -> &TextId {
        &self.current
    }
    pub fn anchor(&self, range: Range<usize>) -> Result<TextAnchor> {
        ensure!(
            range.start < range.end && range.end <= self.len,
            "tracking requires a nonempty in-bounds character span"
        );
        Ok(TextAnchor {
            origin: self.current.clone(),
            known: HashMap::from([(self.current.clone(), Some(range))]),
        })
    }
    pub fn project(&self, anchor: &mut TextAnchor) -> Result<Projection> {
        if !anchor.known.contains_key(&self.current) {
            let mut queue = VecDeque::from([self.current.clone()]);
            let mut visited = HashSet::from([self.current.clone()]);
            let mut toward_current = HashMap::<TextId, Edge>::new();
            let mut found = None;
            while let Some(state) = queue.pop_front() {
                if anchor.known.contains_key(&state) {
                    found = Some(state);
                    break;
                }
                for edge in self.edges.get(&state).into_iter().flatten() {
                    if visited.insert(edge.target.clone()) {
                        toward_current.insert(
                            edge.target.clone(),
                            Edge {
                                target: state.clone(),
                                change: edge.change.clone(),
                                forward: !edge.forward,
                            },
                        );
                        queue.push_back(edge.target.clone());
                    }
                }
            }
            let Some(mut state) = found else {
                return Ok(Projection::Unrelated);
            };
            let mut span = anchor.known[&state].clone();
            while state != self.current {
                let edge = &toward_current[&state];
                let map = if edge.forward {
                    &edge.change.forward
                } else {
                    &edge.change.backward
                };
                span = match span {
                    Some(range) => map.span(range)?,
                    None => None,
                };
                state = edge.target.clone();
                anchor.known.insert(state.clone(), span.clone());
            }
        }
        // Cached positions cannot retain text or grow with the lifetime of an IDE.
        anchor.known.retain(|id, _| {
            id == &anchor.origin || id == &self.current || self.edges.contains_key(id)
        });
        Ok(match &anchor.known[&self.current] {
            Some(range) => Projection::Mapped(range.clone()),
            None => Projection::Removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_replacements_and_insertions_do_not_resurrect_removed_characters() {
        let m = Map::new(10, &[(2..4, 2), (4..4, 5), (4..6, 1)]).unwrap();
        assert_eq!(m.span(2..6).unwrap(), None);
        assert_eq!(m.span(3..8).unwrap(), Some(10..12));
        assert_eq!(m.span(1..7).unwrap(), Some(1..11));
        assert_eq!(m.inverse().unwrap().span(2..10).unwrap(), None);
    }
    #[test]
    fn repeated_insertions_and_deletions_map_boundaries_without_signed_offsets() {
        let m = Map::new(10, &[(0..2, 0), (2..2, 3), (2..2, 4), (8..10, 0)]).unwrap();
        assert_eq!(m.span(0..2).unwrap(), None);
        assert_eq!(m.span(2..3).unwrap(), Some(7..8));
        assert_eq!(m.span(7..10).unwrap(), Some(12..13));
        assert_eq!(m.after, 13);
        let inv = m.inverse().unwrap();
        assert_eq!(inv.span(7..13).unwrap(), Some(2..8));
        assert!(m.span(2..2).is_err());
        assert!(m.span(10..11).is_err());
        assert!(Map::new(10, &[(2..5, 1), (4..6, 0)]).is_err());
        assert!(Map::new(usize::MAX, &[(0..0, 1)]).is_err());
    }
    #[test]
    fn exhaustive_small_map_matches_an_independent_surviving_character_oracle() {
        for len in 1..10 {
            for start in 0..=len {
                for end in start..=len {
                    for inserted in 0..4 {
                        let map = Map::new(len, &[(start..end, inserted)]).unwrap();
                        let mut actual: Vec<Option<usize>> = (0..len).map(Some).collect();
                        actual.splice(start..end, std::iter::repeat_n(None, inserted));
                        for a in 0..len {
                            for b in a + 1..=len {
                                let positions: Vec<_> = actual
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, c)| {
                                        c.filter(|c| (a..b).contains(c)).map(|_| i)
                                    })
                                    .collect();
                                let expected = positions
                                    .first()
                                    .zip(positions.last())
                                    .map(|(a, b)| *a..*b + 1);
                                assert_eq!(
                                    map.span(a..b).unwrap(),
                                    expected,
                                    "{len} {start} {end} {inserted} {a} {b}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn multiple_atomic_edits_match_independent_forward_and_inverse_character_oracles() {
        let mut seed = 41u64;
        for _ in 0..1500 {
            let mut rand = |n| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as usize % n
            };
            let len = 1 + rand(20);
            let mut edits = vec![];
            let mut cursor = 0;
            for _ in 0..6 {
                let start = cursor + rand(len - cursor + 1);
                let end = start + rand(len - start + 1);
                edits.push((start..end, rand(4)));
                cursor = end;
            }
            let map = Map::new(len, &edits).unwrap();
            let mut oracle: Vec<Option<usize>> = (0..len).map(Some).collect();
            for (r, n) in edits.iter().rev() {
                oracle.splice(r.clone(), std::iter::repeat_n(None, *n));
            }
            assert_eq!(map.after, oracle.len());
            let inverse = map.inverse().unwrap();
            for start in 0..len {
                let positions: Vec<_> = oracle
                    .iter()
                    .enumerate()
                    .filter_map(|(i, old)| old.filter(|n| *n >= start).map(|_| i))
                    .collect();
                assert_eq!(
                    map.span(start..len).unwrap(),
                    positions
                        .first()
                        .zip(positions.last())
                        .map(|(a, b)| *a..*b + 1)
                );
            }
            for start in 0..oracle.len() {
                let positions: Vec<_> = oracle[start..].iter().copied().flatten().collect();
                assert_eq!(
                    inverse.span(start..oracle.len()).unwrap(),
                    positions
                        .first()
                        .zip(positions.last())
                        .map(|(a, b)| *a..*b + 1)
                );
            }
        }
    }
    #[test]
    fn retained_large_tracking_metadata_retires_its_arrays_on_the_existing_worker() {
        let before = crate::document_retirement::snapshot().tracking_reclaimed;
        let edits = vec![(0..0, 1); 10_000];
        let change = Arc::new(TextChange::new(TextId::new(), TextId::new(), 0, &edits).unwrap());
        let external = change.clone();
        change.defer_release();
        drop(change);
        assert_eq!(Arc::strong_count(&external), 1);
        drop(external);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let s = crate::document_retirement::snapshot();
            if s.tracking_reclaimed >= before + 2 {
                assert_ne!(
                    s.worker_thread.as_deref(),
                    Some(format!("{:?}", std::thread::current().id()).as_str())
                );
                break;
            }
            assert!(std::time::Instant::now() < end);
            std::thread::yield_now();
        }
    }
}
