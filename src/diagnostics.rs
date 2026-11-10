//! Bounded measured history. A zero rate alone is not evidence of a port or disk fault.
use crate::{
    announce_journal::{JournalEntry, torrent_hash},
    completion::CompletionStatus,
    runtime_status::{ModeApplication, RepairStatus},
    storage::relocation::StorageStatus,
    torrent::{TorrentItem, TorrentState},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerSnapshot {
    pub address: String,
    pub state: String,
    pub client: String,
    pub downloaded: u64,
    pub uploaded: u64,
    pub errors: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RateSample {
    pub elapsed_ms: u64,
    pub down: u64,
    pub up: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub history: VecDeque<RateSample>,
    pub peers: Vec<PeerSnapshot>,
    previous: Option<Instant>,
    origin: Option<Instant>,
    pub queue_waiting: bool,
}
impl Diagnostics {
    pub fn sample(&mut self, now: Instant, down: u64, up: u64) {
        if self.previous.is_some_and(|previous| {
            now.saturating_duration_since(previous) < Duration::from_secs(1)
        }) {
            return;
        }
        self.previous = Some(now);
        if self.history.len() == 120 {
            self.history.pop_front();
        }
        let origin = *self.origin.get_or_insert(now);
        self.history.push_back(RateSample {
            elapsed_ms: now
                .saturating_duration_since(origin)
                .as_millis()
                .min(u64::MAX as u128) as u64,
            down,
            up,
        });
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub reason: &'static str,
    pub next_action: &'static str,
}

/// Explain the latest observation without presenting an inferred fault as fact.
/// A recorded announce may explain what to inspect, but cannot prove why a
/// current transfer is idle. A disconnected view cannot establish live state.
pub fn readiness(
    torrent: &TorrentItem,
    last_announce: Option<&JournalEntry>,
    owner_connected: bool,
) -> Readiness {
    let runtime = &torrent.runtime;
    let (reason, next_action) = match torrent.state {
        _ if !owner_connected => (
            "Background client disconnected. Showing last known data.",
            "Reconnect to the background client before acting on this status.",
        ),
        TorrentState::VerificationFailed => (
            "Integrity verification failed",
            "Inspect the reported verification error before requesting repair.",
        ),
        TorrentState::Error => (
            "Transfer error reported",
            "Inspect the last reported error and correct it before retrying.",
        ),
        _ if matches!(runtime.storage, StorageStatus::Copying { .. }) => (
            "Storage operation in progress",
            "Wait for the storage operation to finish before changing the data location.",
        ),
        _ if matches!(runtime.storage, StorageStatus::Checking { .. }) => (
            "Storage destination verification in progress",
            "Wait for the engine to verify the destination.",
        ),
        _ if torrent.state == TorrentState::Checking
            || matches!(torrent.completion, CompletionStatus::Checking { .. })
            || matches!(runtime.repair, RepairStatus::Checking { .. }) =>
        {
            (
                "Integrity check in progress",
                "Wait for the check result. See Pieces for measured verification progress.",
            )
        }
        _ if runtime.command.pending_action().is_some() => (
            "Transfer command awaiting engine confirmation",
            "Wait for the confirmation or error before sending another command.",
        ),
        _ if !runtime.registered => (
            "Engine registration is not yet confirmed",
            "Wait for registration. Check the status if the engine reports an error.",
        ),
        TorrentState::WaitingForEngine => (
            "Transfer readiness is not yet confirmed",
            "Check the registration and metadata status before starting a transfer.",
        ),
        TorrentState::Paused => ("Paused", "Resume if you want to start transferring."),
        TorrentState::Downloading if runtime.diagnostics.queue_waiting => (
            "Waiting for a download queue slot (engine-confirmed)",
            "Wait for an active slot or review the configured download queue limit.",
        ),
        _ if torrent.files.file_count() > 0 && torrent.files.selected_count() == 0 => (
            "No files selected",
            "Choose files on the Files tab before expecting payload transfer.",
        ),
        _ if torrent.download_rate > 0 => (
            "Receiving payload in the latest sample",
            "No action required while download progress continues.",
        ),
        _ if torrent.upload_rate > 0 => (
            "Sending payload in the latest sample",
            "No action required while upload progress continues.",
        ),
        TorrentState::Seeding => (
            "Seeding with no measured upload",
            "No action is required. See Peers if you want to inspect connections.",
        ),
        _ if matches!(runtime.storage, StorageStatus::Failed { .. }) => (
            "The last storage operation failed",
            "Inspect the storage error and current data location before retrying.",
        ),
        _ if torrent.files.file_count() == 0 => (
            "File metadata is unavailable in this view",
            "Wait for metadata before selecting files.",
        ),
        _ if runtime.diagnostics.history.is_empty() => (
            "No measured rate history in this view",
            "Open Status and wait for a transfer sample.",
        ),
        _ if torrent.peers == 0
            && last_announce.is_some_and(|entry| {
                torrent_hash(&torrent.info_hashes)
                    .is_some_and(|hash| entry.announce.info_hash.eq_ignore_ascii_case(hash))
                    && matches!(
                        entry.announce.outcome.as_str(),
                        "timeout"
                            | "transport_error"
                            | "http_error"
                            | "invalid_response"
                            | "response_read_error"
                            | "tracker_failure"
                            | "udp_error"
                    )
            }) =>
        {
            (
                "No connected peers in the latest sample. Last recorded announce failed.",
                "Inspect the announce result on Log. It may not explain the current wait.",
            )
        }
        _ if torrent.peers == 0
            && matches!(
                runtime.mode,
                ModeApplication::Active {
                    tracker_enabled: false,
                    ..
                }
            ) =>
        {
            (
                "No connected peers in the latest sample. Tracker communication is disabled.",
                "Review this torrent's active mode and allowed peer discovery settings.",
            )
        }
        _ if torrent.peers == 0 => (
            "No connected peers in the latest sample",
            "See Trackers and Peers for observations. The cause is unknown.",
        ),
        _ => (
            "Peers connected. No payload rate measured in the latest sample.",
            "See Peers and Pieces for observations. The cause is unknown.",
        ),
    };
    Readiness {
        reason,
        next_action,
    }
}

pub fn activity(torrent: &TorrentItem) -> &'static str {
    readiness(torrent, None, true).reason
}

/// Explicit local export: no torrent names, paths, URLs, peer addresses or free-form errors.
pub fn export(torrent: &TorrentItem, path: &std::path::Path) -> std::io::Result<()> {
    export_for_connection(torrent, path, true)
}

pub fn export_for_connection(
    torrent: &TorrentItem,
    path: &std::path::Path,
    owner_connected: bool,
) -> std::io::Result<()> {
    use std::io::Write;
    let readiness = readiness(torrent, None, owner_connected);
    let report = serde_json::json!({
        "schema":1, "state":torrent.state.label(), "activity":readiness.reason,
        "next_action":readiness.next_action, "owner_connected":owner_connected,
        "downloaded":torrent.runtime.network_downloaded, "uploaded":torrent.runtime.network_uploaded,
        "peers":torrent.peers, "queue_waiting":torrent.runtime.diagnostics.queue_waiting,
        "samples":torrent.runtime.diagnostics.history,
        "scope":"Sampled local diagnostics; not a complete event history or network diagnosis"
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_download() -> TorrentItem {
        let mut torrent = crate::client::tests::torrent();
        torrent.state = TorrentState::Downloading;
        torrent.runtime.registered = true;
        torrent.files =
            crate::file_selection::FileSelectionModel::from_files([("payload.bin".into(), 8192)]);
        torrent.download_rate = 0;
        torrent.upload_rate = 0;
        torrent.peers = 0;
        torrent
    }

    #[test]
    fn checking_and_connection_evidence_outrank_old_queue_and_transfer_samples() {
        let mut torrent = registered_download();
        torrent.runtime.diagnostics.queue_waiting = true;
        torrent.download_rate = 17;
        assert_eq!(
            readiness(&torrent, None, true).reason,
            "Waiting for a download queue slot (engine-confirmed)"
        );
        torrent.state = TorrentState::Checking;
        assert_eq!(
            readiness(&torrent, None, true).reason,
            "Integrity check in progress"
        );
        assert_eq!(
            readiness(&torrent, None, false).reason,
            "Background client disconnected. Showing last known data."
        );
        torrent.state = TorrentState::Downloading;
        torrent.runtime.diagnostics.queue_waiting = false;
        assert_eq!(
            readiness(&torrent, None, true).reason,
            "Receiving payload in the latest sample"
        );
    }

    #[test]
    fn no_sample_is_unknown_and_idle_paused_or_seeding_need_no_repair() {
        let mut torrent = registered_download();
        assert_eq!(
            readiness(&torrent, None, true).reason,
            "No measured rate history in this view"
        );
        torrent.runtime.diagnostics.sample(Instant::now(), 0, 0);
        assert_eq!(
            readiness(&torrent, None, true).reason,
            "No connected peers in the latest sample"
        );
        torrent.peers = 1;
        assert!(
            readiness(&torrent, None, true)
                .next_action
                .contains("cause is unknown")
        );

        torrent.state = TorrentState::Paused;
        assert_eq!(readiness(&torrent, None, true).reason, "Paused");
        torrent.state = TorrentState::Seeding;
        assert!(
            readiness(&torrent, None, true)
                .next_action
                .starts_with("No action")
        );
    }

    #[test]
    fn last_tracker_failure_is_historical_evidence_for_only_the_matching_torrent() {
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut torrent = registered_download();
        torrent.info_hashes = vec![format!("v1: {hash}")];
        torrent.runtime.diagnostics.sample(Instant::now(), 0, 0);
        let journal = crate::announce_journal::AnnounceJournal::memory();
        let mut observation = crate::announce_journal::tests::observation(hash);
        observation.tracker = "https://user:SECRET@tracker.test/announce?key=SECRET".into();
        observation.outcome = "http_error".into();
        journal.record(observation.clone());
        let latest = journal.latest(hash).unwrap();
        let diagnosis = readiness(&torrent, Some(&latest), true);
        assert!(diagnosis.reason.contains("Last recorded announce failed"));
        assert!(diagnosis.next_action.contains("may not explain"));
        assert!(!format!("{diagnosis:?}").contains("SECRET"));

        torrent.info_hashes = vec![format!("v1: {}", "b".repeat(40))];
        assert!(
            !readiness(&torrent, Some(&latest), true)
                .reason
                .contains("announce")
        );
        torrent.info_hashes = vec![format!("v1: {hash}")];
        observation.outcome = "cancelled".into();
        journal.record(observation);
        let latest = journal.latest(hash).unwrap();
        assert!(
            !readiness(&torrent, Some(&latest), true)
                .reason
                .contains("announce")
        );
    }

    #[test]
    fn samples_are_bounded_and_do_not_invent_missing_measurements() {
        let mut diagnostics = Diagnostics::default();
        let start = Instant::now();
        for n in 0..500 {
            diagnostics.sample(start + Duration::from_millis(n * 500), n, 2 * n);
        }
        assert_eq!(diagnostics.history.len(), 120);
        assert_eq!(diagnostics.history.back().unwrap().down, 498);
        let mut gap = Diagnostics::default();
        gap.sample(start, 1, 2);
        gap.sample(start + Duration::from_secs(10), 3, 4);
        assert_eq!(gap.history[1].elapsed_ms, 10_000);
    }
    #[test]
    fn export_excludes_identifying_and_secret_fields_and_never_overwrites() {
        let mut torrent = crate::client::tests::torrent();
        torrent.name = "SECRET_NAME".into();
        torrent.source = "SECRET_PATH".into();
        torrent.trackers = vec!["https://user:SECRET_PASSWORD@tracker.test/SECRET_QUERY".into()];
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.json");
        export_for_connection(&torrent, &path, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("tracker.test"));
        let report: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["owner_connected"], false);
        assert!(
            report["activity"]
                .as_str()
                .unwrap()
                .contains("last known data")
        );
        assert_eq!(
            export(&torrent, &path).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }
}
