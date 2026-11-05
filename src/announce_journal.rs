//! Bounded live view and nonblocking admission to one ordered disk writer.
pub(crate) mod writer;
pub use writer::{JournalOptions, JournalStatus};
#[cfg(test)]
mod async_tests;
pub use librqbit_tracker_comms::{AnnounceMetrics, AnnounceObservation, AnnounceReply};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
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
    pub(crate) fn retained_bytes(&self) -> usize {
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
#[derive(Clone, Serialize, Deserialize)]
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
    path: Option<PathBuf>,
    error: Option<String>,
    remote_run_id: Option<String>,
}
#[derive(Clone)]
pub struct AnnounceJournal {
    run_id: String,
    inner: Arc<Mutex<Inner>>,
    dispatch: Arc<Mutex<()>>,
    disk: Option<Arc<writer::DiskWriter>>,
    restored: Option<Arc<writer::Snapshot>>,
    frontend: bool,
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
                path: None,
                error: None,
                remote_run_id: None,
            })),
            dispatch: Arc::new(Mutex::new(())),
            disk: None,
            restored: None,
            frontend: false,
        }
    }
    pub fn open(directory: &Path) -> Self {
        Self::open_with_options(directory, JournalOptions::default())
    }
    pub fn open_with_options(directory: &Path, options: JournalOptions) -> Self {
        let mut journal = Self::memory();
        match writer::DiskWriter::start(directory, &journal.run_id, writer::Kind::Announce, options)
        {
            Ok(disk) => {
                journal
                    .inner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .path = Some(disk.path());
                journal.disk = Some(disk);
            }
            Err(error) => {
                journal
                    .inner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .error = Some(error)
            }
        }
        journal
    }
    pub fn observer(&self) -> librqbit_tracker_comms::Observer {
        let journal = self.clone();
        Arc::new(move |observation| journal.record(observation))
    }
    pub fn record(&self, announce: AnnounceObservation) {
        if self.frontend || self.restored.is_some() {
            self.inner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .error = Some("Read-only journal cannot record observations".into());
            return;
        }
        // Serialize concurrent producers, never readers or disk I/O. This keeps
        // sequence allocation and bounded admission in exactly the same order.
        let _dispatch = self
            .dispatch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let sequence = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            let Some(next) = inner.sequence.checked_add(1) else {
                inner.error = Some("Journal sequence overflow".into());
                return;
            };
            inner.sequence = next;
            next
        };
        let entry = Arc::new(JournalEntry {
            summary_cache: OnceLock::new(),
            schema_version: 1,
            run_id: self.run_id.clone(),
            sequence,
            announce,
        });
        let weight = entry.retained_bytes();
        let admission_error = self
            .disk
            .as_ref()
            .and_then(|disk| disk.enqueue(Some(sequence), entry.clone(), weight).err());
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(error) = admission_error {
            inner.error.get_or_insert(error);
        }
        retain(&mut inner, entry, weight);
    }
    pub fn status(&self) -> JournalStatus {
        let mut status = self.disk.as_ref().map_or_else(
            || {
                self.restored.as_ref().map_or_else(
                    || JournalStatus {
                        error: self
                            .inner
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .error
                            .clone(),
                        ..Default::default()
                    },
                    |snapshot| snapshot.status(),
                )
            },
            |disk| disk.status(),
        );
        if status.error.is_none() {
            status.error = self
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .error
                .clone();
        }
        status
    }
    pub fn run_id(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remote_run_id
            .clone()
            .unwrap_or_else(|| self.run_id.clone())
    }
    pub fn manifest_path(&self) -> Option<PathBuf> {
        self.disk
            .as_ref()
            .map(|disk| disk.manifest_path())
            .or_else(|| {
                self.restored
                    .as_ref()
                    .map(|snapshot| snapshot.manifest_path())
            })
    }
    /// A barrier includes every record admitted before it; it never holds the live mutex.
    pub fn flush(&self) -> Result<(), String> {
        if self.frontend {
            return Err("Frontend journal exports and flushes belong to its owner".into());
        }
        if self.disk.is_none() && self.restored.is_none() {
            return self.error().map_or(Ok(()), Err);
        }
        self.snapshot(None).map(|_| ())
    }
    pub fn finish(&self) -> Result<(), String> {
        if self.frontend {
            return Err("Frontend journal exports and flushes belong to its owner".into());
        }
        let finished = self.disk.as_ref().map_or(Ok(()), |disk| disk.finish());
        finished.and(self.error().map_or(Ok(()), Err))
    }
    pub fn frontend_view() -> Self {
        let mut journal = Self::memory();
        journal.frontend = true;
        journal
    }
    pub fn replace_frontend_entries(
        &self,
        mut entries: Vec<JournalEntry>,
        source_path: Option<PathBuf>,
        error: Option<String>,
    ) -> Result<(), String> {
        if !self.frontend {
            return Err("Only a frontend projection can replace owner observations".into());
        }
        if entries.len() > LIVE_CAPACITY {
            return Err("Frontend journal exceeds its entry bound".into());
        }
        entries.sort_by_key(|entry| entry.sequence);
        if entries
            .windows(2)
            .any(|pair| pair[0].sequence == pair[1].sequence)
            || entries.iter().any(|entry| {
                entry.schema_version != 1
                    || entry.sequence == 0
                    || entry.run_id != entries[0].run_id
            })
        {
            return Err("Frontend journal identity or sequences are invalid".into());
        }
        let mut retained = 0usize;
        let mut shared = VecDeque::new();
        for entry in entries {
            let weight = entry.retained_bytes();
            retained = retained.saturating_add(weight);
            if retained > LIVE_BYTE_BUDGET {
                return Err("Frontend journal exceeds its byte bound".into());
            }
            shared.push_back((Arc::new(entry), weight));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.remote_run_id = shared.front().map(|(entry, _)| entry.run_id.clone());
        inner.sequence = shared.back().map_or(0, |(entry, _)| entry.sequence);
        inner.entries = shared;
        inner.retained_bytes = retained;
        inner.skipped_oversize = 0;
        inner.path = source_path;
        inner.error = error;
        Ok(())
    }
    pub fn reopen(manifest_path: &Path) -> Result<Self, String> {
        let snapshot = writer::Snapshot::reopen(manifest_path)?;
        let mut journal = Self::memory();
        journal.run_id = snapshot.run_id().to_owned();
        snapshot.stream(&mut std::io::sink(), None, |entry| {
            let weight = entry.retained_bytes();
            let mut inner = journal
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            inner.sequence = entry.sequence;
            retain(&mut inner, Arc::new(entry), weight);
        })?;
        journal
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .path = Some(snapshot.first_path());
        journal.restored = Some(Arc::new(snapshot));
        Ok(journal)
    }
    fn snapshot(&self, limit: Option<u64>) -> Result<writer::Snapshot, String> {
        if self.frontend {
            return Err("Frontend journal export must be requested from its owner".into());
        }
        if self.error().is_some() {
            return Err("Disk journal is incomplete".into());
        }
        if let Some(disk) = &self.disk {
            return disk.snapshot(limit);
        }
        if let Some(snapshot) = &self.restored {
            if limit.is_some_and(|limit| snapshot.length().map_or(true, |length| length > limit)) {
                return Err(writer::EXPORT_TOO_LARGE.into());
            }
            return Ok((**snapshot).clone());
        }
        Err("No disk journal is available".into())
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
            .or_else(|| self.disk.as_ref().and_then(|disk| disk.status().error))
    }
    /// Streams the complete disk journal, not just the in-memory tail. Never overwrites a file.
    pub(crate) fn disk_snapshot(&self) -> Result<(File, u64), String> {
        self.snapshot(Some(512 * 1024 * 1024))?
            .file(512 * 1024 * 1024)
    }

    pub fn export(&self, destination: &Path, hash: Option<&str>) -> Result<u64, String> {
        let snapshot = self.snapshot(None)?;
        let mut out = BufWriter::new(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)
                .map_err(|e| e.to_string())?,
        );
        let count = snapshot.stream(&mut out, hash, |_| {})?;
        out.flush().map_err(|e| e.to_string())?;
        out.get_ref()
            .sync_all()
            .map_err(|error| error.to_string())?;
        Ok(count)
    }
}

fn retain(inner: &mut Inner, entry: Arc<JournalEntry>, weight: usize) {
    if weight > inner.max_bytes {
        inner.skipped_oversize += 1;
        return;
    }
    while inner.entries.len() >= inner.max_entries
        || inner.retained_bytes.saturating_add(weight) > inner.max_bytes
    {
        if let Some((_, bytes)) = inner.entries.pop_front() {
            inner.retained_bytes -= bytes;
        } else {
            break;
        }
    }
    inner.retained_bytes += weight;
    inner.entries.push_back((entry, weight));
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
