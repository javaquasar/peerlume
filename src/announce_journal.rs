//! Bounded live view plus an append-only, flushed JSONL journal of real announces.
pub use librqbit_tracker_comms::{AnnounceMetrics, AnnounceObservation, AnnounceReply};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

pub const LIVE_CAPACITY: usize = 1024;
pub const LIVE_BYTE_BUDGET: usize = 8 * 1024 * 1024;
pub const PAGE_SIZE: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    #[serde(skip)]
    summary_cache: OnceLock<String>,
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub announce: AnnounceObservation,
}
impl JournalEntry {
    pub fn summary(&self) -> &str {
        self.summary_cache.get_or_init(|| self.format_summary())
    }
    fn format_summary(&self) -> String {
        let a = &self.announce;
        format!(
            "#{} {} {} {} {} · actual u/d/l={}/{}/{} · sent={}/{}/{} · verified={} · peers={} · {} ms",
            self.sequence,
            a.started_unix_ms,
            a.tracker,
            a.event.as_deref().unwrap_or("none"),
            a.outcome,
            a.actual.uploaded,
            a.actual.downloaded,
            a.actual.left,
            a.sent.uploaded,
            a.sent.downloaded,
            a.sent.left,
            a.verified_bytes,
            a.reply.peers.len(),
            a.elapsed_ms
        )
    }
    fn retained_bytes(&self) -> usize {
        let _ = self.summary();
        let a = &self.announce;
        std::mem::size_of::<Self>()
            + 4 * std::mem::size_of::<usize>()
            + self.run_id.capacity()
            + self.summary_cache.get().map(String::capacity).unwrap_or(0)
            + [
                &a.info_hash,
                &a.peer_id_hex,
                &a.tracker,
                &a.transport,
                &a.mode,
                &a.outcome,
            ]
            .iter()
            .map(|s| s.capacity())
            .sum::<usize>()
            + [&a.event, &a.reply.warning, &a.reply.failure]
                .iter()
                .filter_map(|s| s.as_ref())
                .map(|s| s.capacity())
                .sum::<usize>()
            + a.reply.peers.capacity() * std::mem::size_of::<std::net::SocketAddr>()
    }
}
pub struct JournalPage {
    pub entries: Vec<Arc<JournalEntry>>,
    pub total: usize,
    pub retained_bytes: usize,
    pub skipped_oversize: u64,
}
struct Inner {
    entries: VecDeque<(Arc<JournalEntry>, usize)>,
    retained_bytes: usize,
    skipped_oversize: u64,
    max_entries: usize,
    max_bytes: usize,
    sequence: u64,
    writer: Option<BufWriter<File>>,
    path: Option<PathBuf>,
    error: Option<String>,
}
#[derive(Clone)]
pub struct AnnounceJournal {
    run_id: String,
    inner: Arc<Mutex<Inner>>,
}
static RUN: AtomicU64 = AtomicU64::new(0);
impl Default for AnnounceJournal {
    fn default() -> Self {
        Self::memory()
    }
}
impl AnnounceJournal {
    pub fn memory() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            run_id: format!(
                "{stamp}-{}-{}",
                std::process::id(),
                RUN.fetch_add(1, Ordering::Relaxed)
            ),
            inner: Arc::new(Mutex::new(Inner {
                entries: VecDeque::new(),
                retained_bytes: 0,
                skipped_oversize: 0,
                max_entries: LIVE_CAPACITY,
                max_bytes: LIVE_BYTE_BUDGET,
                sequence: 0,
                writer: None,
                path: None,
                error: None,
            })),
        }
    }
    pub fn open(directory: &Path) -> Self {
        let journal = Self::memory();
        let path = directory.join(format!("announces-{}.jsonl", journal.run_id));
        let result = std::fs::create_dir_all(directory)
            .and_then(|()| OpenOptions::new().create_new(true).write(true).open(&path));
        let mut inner = journal.inner.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(file) => {
                inner.writer = Some(BufWriter::new(file));
                inner.path = Some(path);
            }
            Err(e) => inner.error = Some(format!("JSONL logging unavailable: {e}")),
        }
        drop(inner);
        journal
    }
    pub fn observer(&self) -> librqbit_tracker_comms::Observer {
        let journal = self.clone();
        Arc::new(move |observation| journal.record(observation))
    }
    pub fn record(&self, announce: AnnounceObservation) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.sequence += 1;
        let entry = JournalEntry {
            summary_cache: OnceLock::new(),
            schema_version: 1,
            run_id: self.run_id.clone(),
            sequence: inner.sequence,
            announce,
        };
        if let Some(writer) = &mut inner.writer {
            let result = serde_json::to_writer(&mut *writer, &entry)
                .map_err(std::io::Error::other)
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.flush());
            if let Err(error) = result {
                inner.error = Some(format!(
                    "JSONL write failed; disk log is incomplete: {error}"
                ));
                inner.writer = None;
            }
        }
        let weight = entry.retained_bytes();
        if weight > inner.max_bytes {
            inner.skipped_oversize += 1;
            return;
        }
        while inner.entries.len() >= inner.max_entries
            || inner.retained_bytes.saturating_add(weight) > inner.max_bytes
        {
            if let Some((_, bytes)) = inner.entries.pop_front() {
                inner.retained_bytes -= bytes;
            }
        }
        inner.retained_bytes += weight;
        inner.entries.push_back((Arc::new(entry), weight));
    }
    pub fn entries(&self, hash: Option<&str>) -> Vec<Arc<JournalEntry>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .map(|(entry, _)| entry)
            .filter(|e| hash.is_none_or(|h| e.announce.info_hash == h))
            .cloned()
            .collect()
    }
    /// Newest first; snapshots share immutable entries instead of cloning peer lists.
    pub fn page(&self, hash: Option<&str>, offset: usize, limit: usize) -> JournalPage {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let matching = || {
            inner
                .entries
                .iter()
                .rev()
                .map(|(e, _)| e)
                .filter(|e| hash.is_none_or(|h| e.announce.info_hash == h))
        };
        JournalPage {
            entries: matching()
                .skip(offset)
                .take(limit.min(PAGE_SIZE))
                .cloned()
                .collect(),
            total: matching().count(),
            retained_bytes: inner.retained_bytes,
            skipped_oversize: inner.skipped_oversize,
        }
    }
    pub fn latest(&self, hash: &str) -> Option<Arc<JournalEntry>> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .iter()
            .rev()
            .map(|(entry, _)| entry)
            .find(|entry| entry.announce.info_hash == hash)
            .cloned()
    }
    pub fn path(&self) -> Option<PathBuf> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path
            .clone()
    }
    pub fn error(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
    }
    /// Streams the complete disk journal, not just the in-memory tail. Never overwrites a file.
    pub(crate) fn disk_snapshot(&self) -> Result<(File, u64), String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.error.is_some() {
            return Err("Disk journal is incomplete".into());
        }
        let path = inner.path.as_ref().ok_or("No disk journal is available")?;
        let file = File::open(path).map_err(|_| "Disk journal is unavailable")?;
        let length = file
            .metadata()
            .map_err(|_| "Disk journal is unavailable")?
            .len();
        Ok((file, length))
    }

    pub fn export(&self, destination: &Path, hash: Option<&str>) -> Result<u64, String> {
        let (path, length, error) = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let path = inner.path.clone().ok_or("No disk journal is available")?;
            let length = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            (path, length, inner.error.clone())
        };
        if let Some(error) = error {
            return Err(error);
        }
        let source = BufReader::new(File::open(path).map_err(|e| e.to_string())?.take(length));
        let mut out = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)
                .map_err(|e| e.to_string())?,
        );
        let mut count = 0;
        for line in source.lines() {
            let line = line.map_err(|e| e.to_string())?;
            let entry: JournalEntry = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if hash.is_none_or(|h| entry.announce.info_hash == h) {
                writeln!(out, "{line}").map_err(|e| e.to_string())?;
                count += 1;
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        Ok(count)
    }
}

pub fn torrent_hash(hashes: &[String]) -> Option<&str> {
    hashes.iter().find_map(|h| h.strip_prefix("v1: "))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn observation(hash: &str) -> AnnounceObservation {
        AnnounceObservation {
            started_unix_ms: 1,
            elapsed_ms: 2,
            info_hash: hash.into(),
            peer_id_hex: "00".into(),
            tracker: "http://localhost:1".into(),
            transport: "http".into(),
            mode: "normal".into(),
            event: Some("started".into()),
            port: 42,
            actual: AnnounceMetrics {
                downloaded: 7,
                uploaded: 3,
                left: 8,
            },
            verified_bytes: 70,
            full_size: 78,
            selected_bytes: Some(78),
            request_transmitted: Some(true),
            sent: AnnounceMetrics::default(),
            outcome: "ok".into(),
            reply: AnnounceReply {
                peers: vec!["127.0.0.1:45".parse().unwrap()],
                ..Default::default()
            },
        }
    }
    #[test]
    fn disk_history_survives_bounded_live_tail_and_exports_per_torrent() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AnnounceJournal::open(dir.path());
        for i in 0..LIVE_CAPACITY + 13 {
            journal.record(observation(if i % 2 == 0 { "a" } else { "b" }));
        }
        assert_eq!(journal.entries(None).len(), LIVE_CAPACITY);
        assert_eq!(journal.entries(None)[0].sequence, 14);
        let dest = dir.path().join("export.jsonl");
        let count = journal.export(&dest, Some("a")).unwrap();
        assert_eq!(count as usize, (LIVE_CAPACITY + 13).div_ceil(2));
        let entries: Vec<JournalEntry> = BufReader::new(File::open(&dest).unwrap())
            .lines()
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .collect();
        assert_eq!(entries[0].sequence, 1);
        assert_eq!(entries[0].announce.actual.downloaded, 7);
        assert_eq!(entries[0].announce.verified_bytes, 70);
        assert_eq!(entries[0].announce.reply.peers.len(), 1);
        assert!(journal.export(&dest, None).is_err());
    }
    #[test]
    fn disk_failure_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let journal = AnnounceJournal::open(&file);
        journal.record(observation("a"));
        assert!(journal.error().is_some());
        assert_eq!(journal.entries(Some("a")).len(), 1);
        assert!(journal.export(&dir.path().join("out"), None).is_err());
    }

    #[test]
    fn pages_and_latest_share_peer_allocations_and_cache_summaries() {
        let journal = AnnounceJournal::memory();
        for _ in 0..100 {
            journal.record(observation("a"));
        }
        let page = journal.page(Some("a"), 0, usize::MAX);
        let latest = journal.latest("a").unwrap();
        assert_eq!(page.entries.len(), PAGE_SIZE);
        assert_eq!(page.total, 100);
        assert_eq!(page.entries[0].sequence, 100);
        assert!(Arc::ptr_eq(&latest, &page.entries[0]));
        assert_eq!(
            latest.announce.reply.peers.as_ptr(),
            page.entries[0].announce.reply.peers.as_ptr()
        );
        assert_eq!(latest.summary().as_ptr(), latest.summary().as_ptr());
        assert_eq!(journal.page(Some("a"), 32, 32).entries[0].sequence, 68);
        assert!(journal.page(Some("missing"), 0, 32).entries.is_empty());
        assert!(journal.page(None, usize::MAX, 32).entries.is_empty());
        assert_eq!(Arc::strong_count(&latest), 3);
    }

    #[test]
    fn byte_budget_evicts_large_peer_lists_without_losing_disk_history() {
        let root = tempfile::tempdir().unwrap();
        let journal = AnnounceJournal::open(root.path());
        journal.inner.lock().unwrap().max_bytes = 32 * 1024;
        for _ in 0..20 {
            let mut entry = observation("a");
            entry.reply.peers = vec!["127.0.0.1:42".parse().unwrap(); 300];
            journal.record(entry);
            assert!(journal.page(None, 0, 32).retained_bytes <= 32 * 1024);
        }
        assert!(journal.entries(None).len() < 20);
        let mut oversized = observation("a");
        oversized.reply.peers = vec!["127.0.0.1:42".parse().unwrap(); 2000];
        journal.record(oversized);
        assert_eq!(journal.page(None, 0, 32).skipped_oversize, 1);
        assert_eq!(
            journal
                .export(&root.path().join("full.jsonl"), None)
                .unwrap(),
            21
        );
        let full: Vec<JournalEntry> =
            BufReader::new(File::open(root.path().join("full.jsonl")).unwrap())
                .lines()
                .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
                .collect();
        assert_eq!(full.last().unwrap().announce.reply.peers.len(), 2000);
    }
}
