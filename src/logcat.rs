//! Incremental Android logger_entry v1/v3/v4 decoder and byte-budgeted retention.
use anyhow::{Result, ensure};
use bytes::{Buf, BytesMut};
use regex::Regex;
use serde::Serialize;
use std::collections::VecDeque;

const MAX_PAYLOAD: usize = 5 * 1024;
#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    pub seconds: u32,
    pub nanos: u32,
    pub pid: u32,
    pub tid: u32,
    pub buffer: u32,
    pub uid: Option<u32>,
    pub priority: u8,
    pub tag: String,
    pub message: String,
    pub truncated: bool,
}
impl LogEntry {
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.tag.capacity() + self.message.capacity()
    }
    pub fn level(&self) -> &'static str {
        match self.priority {
            2 => "V",
            3 => "D",
            4 => "I",
            5 => "W",
            6 => "E",
            7 => "F",
            _ => "?",
        }
    }
}
#[derive(Default)]
pub struct Decoder {
    pending: BytesMut,
}
impl Decoder {
    /// Caller supplies bounded network chunks. A bad frame aborts the stream.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<LogEntry>> {
        ensure!(chunk.len() <= 65536, "logcat input chunk exceeds 64 KiB");
        self.pending.extend_from_slice(chunk);
        let result = self.decode();
        if result.is_err() {
            self.pending.clear();
        }
        result
    }
    pub fn finish(&self) -> Result<()> {
        ensure!(self.pending.is_empty(), "truncated logcat frame");
        Ok(())
    }
    fn decode(&mut self) -> Result<Vec<LogEntry>> {
        let mut result = Vec::new();
        while self.pending.len() >= 4 {
            let payload_len = u16::from_le_bytes(self.pending[0..2].try_into()?) as usize;
            let raw_header = u16::from_le_bytes(self.pending[2..4].try_into()?) as usize;
            let header_len = if raw_header == 0 { 20 } else { raw_header };
            ensure!(
                [20, 24, 28].contains(&header_len),
                "unsupported logger header {header_len}"
            );
            ensure!(
                (3..=MAX_PAYLOAD).contains(&payload_len),
                "invalid logcat payload length {payload_len}"
            );
            if self.pending.len() < header_len + payload_len {
                break;
            }
            let frame = &self.pending[..header_len + payload_len];
            let word = |offset| u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap());
            let nanos = word(16);
            ensure!(nanos < 1_000_000_000, "invalid log timestamp");
            let payload = &frame[header_len..];
            let priority = payload[0];
            let end_tag = payload[1..]
                .iter()
                .position(|b| *b == 0)
                .map(|n| n + 1)
                .ok_or_else(|| anyhow::anyhow!("unterminated log tag"))?;
            // liblog clamps the complete iovec to LOGGER_ENTRY_MAX_PAYLOAD,
            // so a full4068-byte record can legitimately lose its final NUL.
            let truncated = payload.last() != Some(&0);
            ensure!(
                !truncated || payload_len == 4068,
                "unterminated log message"
            );
            let tag = String::from_utf8_lossy(&payload[1..end_tag]).into_owned();
            let message = String::from_utf8_lossy(
                &payload[end_tag + 1..payload.len() - usize::from(!truncated)],
            )
            .into_owned();
            result.push(LogEntry {
                seconds: word(12),
                nanos,
                pid: word(4),
                tid: word(8),
                buffer: if header_len >= 24 { word(20) } else { 0 },
                uid: if header_len >= 28 {
                    Some(word(24))
                } else {
                    None
                },
                priority,
                tag,
                message,
                truncated,
            });
            self.pending.advance(header_len + payload_len);
        }
        Ok(result)
    }
}
pub struct LogRing {
    entries: VecDeque<LogEntry>,
    used: usize,
    budget: usize,
    pub dropped: u64,
}
impl LogRing {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            used: 0,
            budget,
            dropped: 0,
        }
    }
    pub fn push(&mut self, entry: LogEntry) {
        let size = entry.retained_bytes();
        if size > self.budget {
            self.dropped += 1;
            return;
        }
        while self.used + size > self.budget {
            self.used -= self.entries.pop_front().unwrap().retained_bytes();
            self.dropped += 1;
        }
        self.used += size;
        self.entries.push_back(entry);
    }
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &LogEntry> + ExactSizeIterator {
        self.entries.iter()
    }
    pub fn used_bytes(&self) -> usize {
        self.used
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}
#[derive(Default)]
pub struct LogFilter {
    pub minimum_priority: u8,
    pub pid: Option<u32>,
    pub expression: Option<Regex>,
}
impl LogFilter {
    pub fn matches(&self, entry: &LogEntry) -> bool {
        entry.priority >= self.minimum_priority
            && self.pid.is_none_or(|pid| entry.pid == pid)
            && self
                .expression
                .as_ref()
                .is_none_or(|r| r.is_match(&entry.tag) || r.is_match(&entry.message))
    }
}
#[derive(Debug, Serialize)]
pub struct Replay {
    pub bytes: u64,
    pub entries: u64,
    pub truncated_messages: u64,
    pub elapsed_micros: u128,
}
/// Replay real raw captures through the same bounded streaming decoder as the IDE.
pub fn replay_file(path: &std::path::Path) -> Result<Replay> {
    use std::io::Read;
    let start = std::time::Instant::now();
    let mut f = std::fs::File::open(path)?;
    let mut report = Replay {
        bytes: 0,
        entries: 0,
        truncated_messages: 0,
        elapsed_micros: 0,
    };
    let mut decoder = Decoder::default();
    let mut chunk = [0; 16384];
    loop {
        let n = f.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        report.bytes += n as u64;
        for e in decoder.feed(&chunk[..n])? {
            report.entries += 1;
            report.truncated_messages += u64::from(e.truncated);
        }
    }
    decoder.finish()?;
    report.elapsed_micros = start.elapsed().as_micros();
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame(header: usize) -> Vec<u8> {
        let mut b = Vec::new();
        let p = b"\x04Tag\0hello\0";
        b.extend((p.len() as u16).to_le_bytes());
        b.extend((header as u16).to_le_bytes());
        for n in [12u32, 13, 100, 42, 0, 1000].iter().take((header - 4) / 4) {
            b.extend(n.to_le_bytes());
        }
        b.extend(p);
        b
    }
    #[test]
    fn full_android_payload_can_lose_terminator_without_losing_last_byte_or_next_frame() {
        let mut f = frame(28);
        f.truncate(28 + 5);
        f.resize(28 + 4068, b'x');
        f[0..2].copy_from_slice(&4068u16.to_le_bytes());
        let stream = [f, frame(28)].concat();
        let mut decoder = Decoder::default();
        let mut entries = Vec::new();
        for c in stream.chunks(7) {
            entries.extend(decoder.feed(c).unwrap());
        }
        decoder.finish().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].truncated);
        assert_eq!(entries[0].message.len(), 4063);
        assert!(entries[0].message.ends_with('x'));
        assert_eq!(entries[1].message, "hello");
        assert!(!entries[1].truncated);
    }
    #[test]
    fn short_unterminated_message_is_still_rejected() {
        let mut f = frame(28);
        *f.last_mut().unwrap() = b'x';
        assert!(Decoder::default().feed(&f).is_err());
    }
    #[test]
    fn all_versions_and_byte_fragmentation() {
        for h in [20, 24, 28] {
            let mut d = Decoder::default();
            let mut out = Vec::new();
            for b in frame(h) {
                out.extend(d.feed(&[b]).unwrap());
            }
            d.finish().unwrap();
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].tag, "Tag");
            assert_eq!(out[0].message, "hello");
            assert_eq!(out[0].pid, 12);
            assert_eq!(out[0].uid, if h == 28 { Some(1000) } else { None });
        }
    }
    #[test]
    fn multiple_frames() {
        let mut d = Decoder::default();
        assert_eq!(d.feed(&[frame(28), frame(20)].concat()).unwrap().len(), 2);
    }
    #[test]
    fn malformed_header_is_rejected_and_cleared() {
        let mut d = Decoder::default();
        assert!(d.feed(&[3, 0, 255, 255]).is_err());
        assert!(d.finish().is_ok());
    }
    #[test]
    fn truncated_frame_is_reported() {
        let mut d = Decoder::default();
        d.feed(&frame(28)[..10]).unwrap();
        assert!(d.finish().is_err());
    }
    #[test]
    fn legacy_zero_header() {
        let mut b = frame(20);
        b[2] = 0;
        assert_eq!(Decoder::default().feed(&b).unwrap().len(), 1);
    }
    #[test]
    fn budget_evicts_old_entries() {
        let e = Decoder::default().feed(&frame(28)).unwrap().remove(0);
        let size = e.retained_bytes();
        let mut r = LogRing::new(size * 2);
        for _ in 0..100 {
            r.push(e.clone());
        }
        assert_eq!(r.entries().len(), 2);
        assert_eq!(r.dropped, 98);
        assert!(r.used_bytes() <= size * 2);
    }
    #[test]
    fn zero_budget_drops() {
        let mut r = LogRing::new(0);
        r.push(Decoder::default().feed(&frame(20)).unwrap().remove(0));
        assert_eq!(r.dropped, 1);
    }
    #[test]
    fn filters_pid_priority_regex() {
        let e = Decoder::default().feed(&frame(28)).unwrap().remove(0);
        let mut f = LogFilter {
            minimum_priority: 4,
            pid: Some(12),
            expression: Some(Regex::new("hello").unwrap()),
        };
        assert!(f.matches(&e));
        f.minimum_priority = 5;
        assert!(!f.matches(&e));
    }
}
