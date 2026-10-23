//! Bounded live view plus an append-only, flushed JSONL journal of real announces.
pub use librqbit_tracker_comms::{AnnounceMetrics, AnnounceObservation, AnnounceReply};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

pub const LIVE_CAPACITY: usize = 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub announce: AnnounceObservation,
}
impl JournalEntry {
    pub fn summary(&self) -> String {
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
}
struct Inner {
    entries: VecDeque<JournalEntry>,
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
        if inner.entries.len() == LIVE_CAPACITY {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }
    pub fn entries(&self, hash: Option<&str>) -> Vec<JournalEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter(|e| hash.is_none_or(|h| e.announce.info_hash == h))
            .cloned()
            .collect()
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
mod tests {
    use super::*;
    fn observation(hash: &str) -> AnnounceObservation {
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
}
