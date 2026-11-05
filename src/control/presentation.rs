//! Cheap change detection for summary telemetry; never walks file paths or rows.
use crate::{
    completion::CompletionStatus,
    runtime_status::{TransferAction, TransferFeedback},
    storage::relocation::StorageStatus,
    torrent::{TorrentItem, TorrentState},
};

#[derive(PartialEq, Eq)]
pub(super) struct TelemetryStamp {
    counters: [u64; 9],
    peers: usize,
    seeds: usize,
    state: TorrentState,
    completion: std::mem::Discriminant<CompletionStatus>,
    storage: std::mem::Discriminant<StorageStatus>,
    command: std::mem::Discriminant<TransferFeedback>,
    pending: Option<TransferAction>,
    registered: bool,
    error: bool,
}
impl TelemetryStamp {
    pub fn capture(item: &TorrentItem) -> Self {
        let (checked, total) = match item.completion {
            CompletionStatus::Checking {
                checked_bytes,
                total_bytes,
            } => (checked_bytes, total_bytes),
            _ => (0, 0),
        };
        Self {
            counters: [
                item.downloaded_bytes,
                item.uploaded_bytes,
                item.download_rate,
                item.upload_rate,
                item.runtime.network_uploaded,
                item.runtime.network_downloaded,
                item.runtime.seeded_seconds,
                checked,
                total,
            ],
            peers: item.peers,
            seeds: item.seeds,
            state: item.state,
            completion: std::mem::discriminant(&item.completion),
            storage: std::mem::discriminant(&item.runtime.storage),
            command: std::mem::discriminant(&item.runtime.command),
            pending: item.runtime.command.pending_action(),
            registered: item.runtime.registered,
            error: item.runtime.last_error.is_some(),
        }
    }
}
