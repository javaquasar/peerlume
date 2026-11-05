//! Bounded standalone-audit display history with a full append-only disk log.
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    writer: Option<Arc<crate::announce_journal::writer::DiskWriter>>,
    error: Option<String>,
    #[cfg(test)]
    registry: Arc<Mutex<AuditRegistry>>,
}
#[derive(Clone)]
pub(crate) struct LiveHistory<T> {
    recorded: u64,
    entries: VecDeque<(Arc<T>, usize)>,
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
const MAX_AUDIT_DIRECTORIES: usize = 8;
#[derive(Default)]
struct AuditRegistry {
    writers: HashMap<PathBuf, Arc<crate::announce_journal::writer::DiskWriter>>,
    initialization_errors: HashMap<PathBuf, String>,
    admission_error: Option<String>,
    closing: bool,
}
static WRITERS: OnceLock<Mutex<AuditRegistry>> = OnceLock::new();
#[cfg(not(test))]
fn audit_writer(
    directory: &std::path::Path,
) -> Result<Arc<crate::announce_journal::writer::DiskWriter>, String> {
    audit_writer_in(WRITERS.get_or_init(Default::default), directory)
}
fn audit_writer_in(
    registry: &Mutex<AuditRegistry>,
    directory: &std::path::Path,
) -> Result<Arc<crate::announce_journal::writer::DiskWriter>, String> {
    let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
    if registry.closing {
        return Err("Audit journal admission is closed for process shutdown".into());
    }
    if let Some(writer) = registry.writers.get(directory) {
        return Ok(writer.clone());
    }
    if let Some(error) = registry.initialization_errors.get(directory) {
        return Err(error.clone());
    }
    if registry.writers.len() + registry.initialization_errors.len() >= MAX_AUDIT_DIRECTORIES {
        let error = format!(
            "Audit journal directory limit ({MAX_AUDIT_DIRECTORIES}) exceeded; complete history is not confirmed"
        );
        registry.admission_error.get_or_insert(error.clone());
        return Err(error);
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let run_id = format!(
        "{stamp}-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::Relaxed)
    );
    let writer = match crate::announce_journal::writer::DiskWriter::start(
        directory,
        &run_id,
        crate::announce_journal::writer::Kind::Audit,
        Default::default(),
    ) {
        Ok(writer) => writer,
        Err(error) => {
            registry
                .initialization_errors
                .insert(directory.to_owned(), error.clone());
            return Err(error);
        }
    };
    registry
        .writers
        .insert(directory.to_owned(), writer.clone());
    Ok(writer)
}

/// Call after cancelling and draining this process's standalone-audit workers.
/// Existing display clones cannot defer the durable barrier. This closes new
/// process admission permanently; it is an exit API, not a per-scenario reset.
pub fn finish_all_audit_writers(timeout: Duration) -> Result<(), String> {
    finish_registry(WRITERS.get_or_init(Default::default), timeout)
}
fn finish_registry(registry: &Mutex<AuditRegistry>, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(30));
    let (writers, mut errors) = loop {
        let locked = match registry.try_lock() {
            Ok(registry) => Some(registry),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut registry) = locked {
            registry.closing = true;
            let mut errors: Vec<_> = registry.initialization_errors.values().cloned().collect();
            errors.extend(registry.admission_error.clone());
            break (
                registry.writers.values().cloned().collect::<Vec<_>>(),
                errors,
            );
        }
        if Instant::now() >= deadline {
            return Err(
                "Audit journal shutdown timed out waiting for its initialization lock".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    for writer in writers {
        if let Err(error) =
            writer.finish_with_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Audit journal shutdown failed: {}",
            errors.join("; ")
        ))
    }
}
impl<T> LiveHistory<T> {
    #[cfg(test)]
    pub(crate) fn new_in_for_test(directory: PathBuf) -> Self {
        Self::new(Some(directory), AUDIT_ENTRIES, AUDIT_BYTES)
    }
    /// Empty live tail for a worker, sharing its originating view's disk slot.
    #[cfg(any(feature = "gui", test))]
    pub fn producer(&self) -> Self {
        Self {
            recorded: 0,
            entries: VecDeque::new(),
            retained_bytes: 0,
            skipped_oversize: 0,
            max_entries: self.max_entries,
            max_bytes: self.max_bytes,
            disk: self.disk.clone(),
        }
    }
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
                    writer: None,
                    error: None,
                    #[cfg(test)]
                    registry: Default::default(),
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
        self.entries.iter().map(|(value, _)| value.as_ref())
    }
    pub fn clear(&mut self) {
        self.entries = VecDeque::new();
        self.retained_bytes = 0;
    }
    pub fn status(&self) -> String {
        let mut text = format!(
            "Live audit history: {} entries of {} observed · {} / {} retained bytes · {} oversized outside live view",
            self.len(),
            self.recorded(),
            self.retained_bytes,
            self.max_bytes,
            self.skipped_oversize
        );
        if let Some(disk) = &self.disk {
            let disk = disk.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(writer) = &disk.writer {
                let status = writer.status();
                text.push_str(&format!(" · JSONL first segment: {} · manifest: {} · accepted/flushed={}/{} · queued={}",writer.path().display(),writer.manifest_path().display(),status.accepted_sequence,status.flushed_sequence,status.queued_entries));
                if let Some(error) = status.error {
                    text.push_str(&format!(" · {error}"));
                }
            }
            if let Some(error) = &disk.error {
                text.push_str(&format!(" · {error}"));
            }
        }
        text
    }
}
impl<T: Serialize + RetainedSize + Send + Sync + 'static> LiveHistory<T> {
    /// Display a worker-recorded exchange without appending it to disk twice.
    #[cfg(any(feature = "gui", test))]
    pub fn push_view(&mut self, value: T) {
        self.recorded += 1;
        let value = Arc::new(value);
        let weight = value.retained_bytes()
            + std::mem::size_of::<(Arc<T>, usize)>()
            + 2 * std::mem::size_of::<usize>();
        self.retain_value(value, weight);
    }
    pub fn push(&mut self, value: T) {
        self.recorded += 1;
        let value = Arc::new(value);
        let weight = value.retained_bytes()
            + std::mem::size_of::<(Arc<T>, usize)>()
            + 2 * std::mem::size_of::<usize>();
        if let Some(disk) = &self.disk {
            let initialize = {
                let disk = disk.lock().unwrap_or_else(|error| error.into_inner());
                (disk.writer.is_none() && disk.error.is_none()).then(|| disk.directory.clone())
            };
            if let Some(directory) = initialize {
                #[cfg(not(test))]
                let writer = audit_writer(&directory);
                #[cfg(test)]
                let writer = {
                    let registry = disk
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .registry
                        .clone();
                    audit_writer_in(&registry, &directory)
                };
                let mut disk = disk.lock().unwrap_or_else(|error| error.into_inner());
                if disk.writer.is_none() && disk.error.is_none() {
                    match writer {
                        Ok(writer) => disk.writer = Some(writer),
                        Err(error) => {
                            disk.error = Some(format!("Audit JSONL unavailable: {error}"))
                        }
                    }
                }
            }
            let writer = disk
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .writer
                .clone();
            if let Some(writer) = writer
                && let Err(error) = writer.enqueue(None, value.clone(), weight)
            {
                disk.lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .error
                    .get_or_insert(error);
            }
        }
        self.retain_value(value, weight);
    }
    fn retain_value(&mut self, value: Arc<T>, weight: usize) {
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
    #[cfg(test)]
    pub(crate) fn flush_disk(&self) -> Result<(), String> {
        if let Some(disk) = &self.disk {
            let writer = disk
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .writer
                .clone();
            if let Some(writer) = writer {
                writer.snapshot(None)?;
            }
        }
        Ok(())
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
    fn isolated_history(
        registry: &Mutex<AuditRegistry>,
        directory: &std::path::Path,
    ) -> LiveHistory<String> {
        let writer = audit_writer_in(registry, directory).unwrap();
        let history = LiveHistory::new(Some(directory.to_owned()), 8, 2048);
        history.disk.as_ref().unwrap().lock().unwrap().writer = Some(writer);
        history
    }

    #[test]
    fn process_barrier_flushes_retained_producer_and_view_clones_without_duplicate_records() {
        let root = tempfile::tempdir().unwrap();
        let registry = Mutex::new(AuditRegistry::default());
        let mut view = isolated_history(&registry, root.path());
        let mut producer = view.producer();
        producer.push("controlled exchange".to_owned());
        view.push_view("controlled exchange".to_owned());
        let retained = producer.clone();
        let writer = retained
            .disk
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .writer
            .clone()
            .unwrap();
        finish_registry(&registry, Duration::from_secs(5)).unwrap();
        assert_eq!(producer.recorded(), 1);
        assert_eq!(view.recorded(), 1);
        assert_eq!(view.len(), 1);
        let status = writer.status();
        assert!(status.closed);
        assert_eq!(status.accepted_sequence, 1);
        assert_eq!(status.durable_sequence, 1);
        assert_eq!(
            std::fs::read_to_string(writer.path())
                .unwrap()
                .lines()
                .count(),
            1
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(writer.manifest_path()).unwrap()).unwrap();
        assert_eq!(manifest["clean_shutdown"], true);
        assert_eq!(manifest["incomplete"], false);
        assert!(audit_writer_in(&registry, &root.path().join("later")).is_err());
        producer.push("late exchange".to_owned());
        assert!(view.status().contains("closed"));
        assert_eq!(writer.status().accepted_sequence, 1);
        assert_eq!(writer.status().rejected_records, 1);
        assert_eq!(
            std::fs::read_to_string(writer.path())
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn process_barrier_propagates_manifest_failure_while_clones_are_still_alive() {
        let root = tempfile::tempdir().unwrap();
        let registry = Mutex::new(AuditRegistry::default());
        let mut history = isolated_history(&registry, root.path());
        history.push("controlled exchange".to_owned());
        let writer = history
            .disk
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .writer
            .clone()
            .unwrap();
        history.flush_disk().unwrap();
        std::fs::remove_file(writer.manifest_path()).unwrap();
        std::fs::create_dir(writer.manifest_path()).unwrap();
        assert!(finish_registry(&registry, Duration::from_secs(5)).is_err());
        assert!(writer.status().closed);
        assert!(writer.status().error.is_some());
        assert!(history.status().contains("Cannot replace journal manifest"));
    }

    #[test]
    fn process_barrier_has_a_deadline_when_writer_initialization_holds_registry_lock() {
        let registry = Arc::new(Mutex::new(AuditRegistry::default()));
        let held = registry.lock().unwrap();
        let registry_for_worker = registry.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            sender
                .send(finish_registry(
                    &registry_for_worker,
                    Duration::from_millis(30),
                ))
                .unwrap();
        });
        let error = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("Registry barrier blocked without honoring its deadline")
            .unwrap_err();
        assert!(error.contains("initialization lock"));
        drop(held);
        worker.join().unwrap();
    }

    #[test]
    fn process_barrier_preserves_failed_open_and_bounds_directory_error_records() {
        let root = tempfile::tempdir().unwrap();
        let registry = Mutex::new(AuditRegistry::default());
        for index in 0..MAX_AUDIT_DIRECTORIES {
            let path = root.path().join(format!("invalid-{index}"));
            std::fs::write(&path, "not a directory").unwrap();
            assert!(audit_writer_in(&registry, &path).is_err());
        }
        let extra = root.path().join("over-limit");
        assert!(
            audit_writer_in(&registry, &extra)
                .err()
                .unwrap()
                .contains("directory limit")
        );
        assert!(!extra.exists());
        assert_eq!(
            registry.lock().unwrap().initialization_errors.len(),
            MAX_AUDIT_DIRECTORIES
        );
        assert!(finish_registry(&registry, Duration::from_secs(5)).is_err());
    }

    #[test]
    fn process_registry_preserves_accepted_records_after_every_live_view_drops() {
        let root = tempfile::tempdir().unwrap();
        let registry = Mutex::new(AuditRegistry::default());
        let (path, manifest_path) = {
            let mut history = isolated_history(&registry, root.path());
            history.push("accepted before view close".to_owned());
            let writer = history
                .disk
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .writer
                .clone()
                .unwrap();
            (writer.path(), writer.manifest_path())
        };
        assert_eq!(registry.lock().unwrap().writers.len(), 1);
        finish_registry(&registry, Duration::from_secs(5)).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["clean_shutdown"], true);
        assert_eq!(manifest["segments"][0]["last_sequence"], 1);
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
            .writer
            .as_ref()
            .unwrap()
            .path();
        let mut next = history.clone();
        history.clear();
        next.push("next run".to_owned());
        next.flush_disk().unwrap();
        assert!(history.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 21);
        next.push("x".repeat(2000));
        next.flush_disk().unwrap();
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
