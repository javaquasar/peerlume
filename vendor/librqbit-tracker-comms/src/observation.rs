//! Transport observations. No tracker credentials or raw URLs cross this boundary.
use serde_derive::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnounceMetrics {
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnnounceReply {
    pub http_status: Option<u16>,
    pub interval: Option<u64>,
    pub min_interval: Option<u64>,
    pub seeders: Option<u64>,
    pub leechers: Option<u64>,
    pub warning: Option<String>,
    pub failure: Option<String>,
    pub peers: Vec<SocketAddr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnnounceObservation {
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    pub info_hash: String,
    pub peer_id_hex: String,
    pub tracker: String,
    pub transport: String,
    pub mode: String,
    pub event: Option<String>,
    pub port: u16,
    pub actual: AnnounceMetrics,
    /// Checked pieces on disk, NOT bytes received in this transfer generation.
    pub verified_bytes: u64,
    pub full_size: u64,
    pub selected_bytes: Option<u64>,
    /// HTTP transport errors cannot reliably tell whether a request reached the server.
    pub request_transmitted: Option<bool>,
    pub sent: AnnounceMetrics,
    pub outcome: String,
    pub reply: AnnounceReply,
}

pub type Observer = Arc<dyn Fn(AnnounceObservation) + Send + Sync>;
#[derive(Clone, Copy, Default)]
pub struct TransferSnapshot {
    pub downloaded: u64,
    pub uploaded: u64,
    pub verified_bytes: Option<u64>,
    pub full_size: Option<u64>,
    pub selected_bytes: Option<u64>,
}
pub type TransferMetricsProvider = Arc<dyn Fn([u8; 20]) -> TransferSnapshot + Send + Sync>;
#[derive(Clone)]
pub(crate) struct Hooks {
    pub observer: Observer,
    pub transfers: TransferMetricsProvider,
    pub completions: CompletionHistory,
}
pub(crate) struct CompletionState {
    pub baseline: Option<u64>,
    pub settled: bool,
}
type CompletionHistory = Arc<Mutex<HashMap<([u8; 20], String), Arc<Mutex<CompletionState>>>>>;
static HOOKS: LazyLock<Mutex<HashMap<[u8; 20], Hooks>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub struct ObservationRegistration([u8; 20]);
impl Drop for ObservationRegistration {
    fn drop(&mut self) {
        HOOKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}
pub fn register_observer(
    peer_id: [u8; 20],
    observer: Observer,
    transfers: TransferMetricsProvider,
) -> Result<ObservationRegistration, &'static str> {
    let mut hooks = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    if hooks.contains_key(&peer_id) {
        return Err("Session observer is already registered");
    }
    hooks.insert(
        peer_id,
        Hooks {
            observer,
            transfers,
            completions: Default::default(),
        },
    );
    Ok(ObservationRegistration(peer_id))
}
pub(crate) fn hooks_for(peer_id: [u8; 20]) -> Option<Hooks> {
    HOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&peer_id)
        .cloned()
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

/// Logs cancellation too, and records replies before any peer-stream backpressure.
pub(crate) struct Attempt {
    pub record: AnnounceObservation,
    observer: Option<Observer>,
    start: Instant,
}
impl Attempt {
    pub fn new(record: AnnounceObservation, observer: Option<Observer>) -> Self {
        Self {
            record,
            observer,
            start: Instant::now(),
        }
    }
    pub fn finish(mut self) {
        self.emit();
    }
    fn emit(&mut self) {
        if let Some(observer) = self.observer.take() {
            self.record.elapsed_ms =
                self.start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            // Move the potentially large peer vector into the journal. A cheap
            // empty replacement keeps Drop cancellation logging exactly-once.
            let peers = std::mem::take(&mut self.record.reply.peers);
            let mut emitted = self.record.clone();
            emitted.reply.peers = peers;
            observer(emitted);
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.emit();
    }
}

pub(crate) fn redacted_tracker(url: &url::Url) -> String {
    // Paths, userinfo and ALL query values may contain a private passkey.
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host().map(|h| h.to_string()).unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}
pub(crate) fn safe_message(text: &str, tracker: &url::Url) -> String {
    let mut result = text.replace(tracker.as_str(), "[redacted tracker URL]");
    for secret in tracker
        .path_segments()
        .into_iter()
        .flatten()
        .chain(std::iter::once(tracker.username()))
        .chain(tracker.password())
    {
        if !secret.is_empty() {
            result = result.replace(secret, "[redacted]");
        }
    }
    for (_, value) in tracker.query_pairs() {
        if !value.is_empty() {
            result = result.replace(value.as_ref(), "[redacted]");
        }
    }
    for part in tracker.query().unwrap_or_default().split('&') {
        if let Some((_, value)) = part.split_once('=')
            && !value.is_empty()
        {
            result = result.replace(value, "[redacted]");
        }
    }
    result.chars().take(1024).collect()
}
