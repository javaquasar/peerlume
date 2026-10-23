//! Bounded standalone-audit display history with a full append-only disk log.
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const AUDIT_ENTRIES: usize = 256;
pub(crate) const AUDIT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) trait RetainedSize {
    fn retained_bytes(&self) -> usize;
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AuditExchange {
    pub timestamp_unix_ms: u128,
    pub step_index: usize,
    pub tracker_host: String,
    pub info_hash_hex: String,
    pub peer_id: String,
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Option<crate::tracker::AnnounceEvent>,
    pub interval_seconds: Option<u64>,
    pub peers: Vec<String>,
    pub error: Option<String>,
}
impl RetainedSize for AuditExchange {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.tracker_host.capacity()
            + self.info_hash_hex.capacity()
            + self.peer_id.capacity()
            + self.peers.capacity() * std::mem::size_of::<String>()
            + self.peers.iter().map(String::capacity).sum::<usize>()
            + self.error.as_ref().map(String::capacity).unwrap_or(0)
    }
}
impl AuditExchange {
    pub fn execute(
        url: &str,
        allow_hosts: &[String],
        request: crate::tracker::AnnounceRequest,
        step_index: usize,
    ) -> Self {
        use crate::tracker::{announce_for_audit, compact_ipv4_peers, validate_audit_target};
        let mut exchange = Self {
            timestamp_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            step_index,
            tracker_host: validate_audit_target(url, allow_hosts)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "invalid target".into()),
            info_hash_hex: hex_bytes(&request.info_hash),
            peer_id: String::from_utf8_lossy(&request.peer_id).into_owned(),
            port: request.port,
            uploaded: request.uploaded,
            downloaded: request.downloaded,
            left: request.left,
            event: request.event,
            interval_seconds: None,
            peers: Vec::new(),
            error: None,
        };
        match announce_for_audit(url, &request, allow_hosts).and_then(|response| {
            let peers = compact_ipv4_peers(&response.compact_peers)?
                .into_iter()
                .map(|peer| peer.to_string())
                .collect();
            Ok((response.interval_seconds, peers))
        }) {
            Ok((interval, peers)) => {
                exchange.interval_seconds = Some(interval);
                exchange.peers = peers;
            }
            Err(error) => exchange.error = Some(error),
        }
        exchange
    }
    #[cfg(feature = "tui")]
    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return format!(
                "step {} failed: {}",
                self.step_index + 1,
                error.chars().take(512).collect::<String>()
            );
        }
        let peers = self
            .peers
            .iter()
            .take(8)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "step {}: up={} down={} left={} event={:?} -> interval={:?}s peers={} [{}]{}",
            self.step_index + 1,
            self.uploaded,
            self.downloaded,
            self.left,
            self.event,
            self.interval_seconds,
            self.peers.len(),
            peers,
            if self.peers.len() > 8 {
                " (full list in audit JSONL)"
            } else {
                ""
            }
        )
    }
}
pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

struct Disk {
    directory: PathBuf,
    path: Option<PathBuf>,
    writer: Option<BufWriter<File>>,
    error: Option<String>,
}
#[derive(Clone)]
pub(crate) struct LiveHistory<T> {
    recorded: u64,
    entries: VecDeque<(T, usize)>,
    retained_bytes: usize,
    skipped_oversize: u64,
    max_entries: usize,
    max_bytes: usize,
    disk: Option<Arc<Mutex<Disk>>>,
}
impl<T> Default for LiveHistory<T> {
    fn default() -> Self {
        #[cfg(not(test))]
        let directory = Some(crate::settings::data_directory().join("audit-logs"));
        #[cfg(test)]
        let directory = None;
        Self::new(directory, AUDIT_ENTRIES, AUDIT_BYTES)
    }
}
static RUN: AtomicU64 = AtomicU64::new(0);
impl<T> LiveHistory<T> {
    fn new(directory: Option<PathBuf>, max_entries: usize, max_bytes: usize) -> Self {
        assert!(max_entries > 0 && max_bytes > 0);
        Self {
            recorded: 0,
            entries: VecDeque::new(),
            retained_bytes: 0,
            skipped_oversize: 0,
            max_entries,
            max_bytes,
            disk: directory.map(|directory| {
                Arc::new(Mutex::new(Disk {
                    directory,
                    path: None,
                    writer: None,
                    error: None,
                }))
            }),
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn recorded(&self) -> u64 {
        self.recorded
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.entries.iter().map(|(value, _)| value)
    }
    pub fn clear(&mut self) {
        self.entries = VecDeque::new();
        self.retained_bytes = 0;
    }
    pub fn status(&self) -> String {
        let mut text = format!(
            "Live audit history: {} entries of {} observed · {} / {} retained bytes · {} oversized on disk only",
            self.len(),
            self.recorded(),
            self.retained_bytes,
            self.max_bytes,
            self.skipped_oversize
        );
        if let Some(disk) = &self.disk {
            let disk = disk.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(path) = &disk.path {
                text.push_str(&format!(" · JSONL: {}", path.display()));
            }
            if let Some(error) = &disk.error {
                text.push_str(&format!(" · {error}"));
            }
        }
        text
    }
}
impl<T: Serialize + RetainedSize> LiveHistory<T> {
    pub fn push(&mut self, value: T) {
        self.recorded += 1;
        if let Some(disk) = &self.disk {
            let mut disk = disk.lock().unwrap_or_else(|e| e.into_inner());
            if disk.writer.is_none() && disk.error.is_none() {
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let path = disk.directory.join(format!(
                    "audit-{stamp}-{}-{}.jsonl",
                    std::process::id(),
                    RUN.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir_all(&disk.directory)
                    .and_then(|()| OpenOptions::new().create_new(true).write(true).open(&path))
                {
                    Ok(file) => {
                        disk.writer = Some(BufWriter::new(file));
                        disk.path = Some(path);
                    }
                    Err(e) => disk.error = Some(format!("Audit JSONL unavailable: {e}")),
                }
            }
            if let Some(writer) = &mut disk.writer {
                let result = serde_json::to_writer(&mut *writer, &value)
                    .map_err(std::io::Error::other)
                    .and_then(|()| writer.write_all(b"\n"))
                    .and_then(|()| writer.flush());
                if let Err(error) = result {
                    disk.error = Some(format!("Audit JSONL incomplete: {error}"));
                    disk.writer = None;
                }
            }
        }
        let weight =
            value.retained_bytes() + std::mem::size_of::<(T, usize)>() - std::mem::size_of::<T>();
        if weight > self.max_bytes {
            self.skipped_oversize += 1;
            return;
        }
        while self.entries.len() >= self.max_entries
            || self.retained_bytes.saturating_add(weight) > self.max_bytes
        {
            if let Some((_, bytes)) = self.entries.pop_front() {
                self.retained_bytes -= bytes;
            }
        }
        self.retained_bytes += weight;
        self.entries.push_back((value, weight));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    impl RetainedSize for String {
        fn retained_bytes(&self) -> usize {
            std::mem::size_of::<String>() + self.capacity()
        }
    }
    #[test]
    fn live_limits_and_clear_do_not_truncate_full_audit_history() {
        let root = tempfile::tempdir().unwrap();
        let mut history = LiveHistory::new(Some(root.path().to_path_buf()), 3, 1000);
        for i in 0..20 {
            history.push(format!("exchange-{i}"));
        }
        assert_eq!(history.len(), 3);
        assert!(history.retained_bytes <= 1000);
        let path = history
            .disk
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .path
            .clone()
            .unwrap();
        let mut next = history.clone();
        history.clear();
        next.push("next run".to_owned());
        assert!(history.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 21);
        next.push("x".repeat(2000));
        assert_eq!(next.skipped_oversize, 1);
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 22);
        let mut bytes = LiveHistory::new(None, 1000, 200);
        for _ in 0..100 {
            bytes.push("x".repeat(100));
        }
        assert_eq!(bytes.len(), 1);
    }
    #[test]
    fn disk_failure_remains_visible_and_live_memory_is_still_bounded() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let mut history = LiveHistory::new(Some(file), 2, 1024);
        for _ in 0..20 {
            history.push("exchange".to_owned());
        }
        assert_eq!(history.len(), 2);
        assert!(history.status().contains("unavailable"));
    }
}
