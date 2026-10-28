//! Bounded measured history. A zero rate alone is not evidence of a port or disk fault.
use crate::torrent::{TorrentItem, TorrentState};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerSnapshot {
    pub address: String,
    pub state: String,
    pub client: String,
    pub downloaded: u64,
    pub uploaded: u64,
    pub errors: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
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
pub fn activity(torrent: &TorrentItem) -> &'static str {
    if torrent.runtime.diagnostics.queue_waiting && torrent.state == TorrentState::Downloading {
        return "Waiting for a download queue slot (engine-confirmed)";
    }
    match torrent.state {
        TorrentState::Paused => "Paused by user, queue or configured seed limit",
        TorrentState::Checking => "Reading and hashing local files",
        TorrentState::Error | TorrentState::VerificationFailed => {
            "Engine/check error; inspect the last error before resuming"
        }
        TorrentState::WaitingForEngine => "Awaiting engine registration or metadata",
        _ if torrent.download_rate > 0 => "Receiving payload",
        _ if torrent.upload_rate > 0 => "Sending payload",
        _ if torrent.selected_size() == 0 => "No selected payload; metadata or selection required",
        _ if torrent.files.selected_downloaded() >= torrent.selected_size() => {
            "Selected payload complete; waiting for peer requests"
        }
        _ if torrent.peers == 0 => {
            "No connected peers; tracker reachability and piece availability not yet proven"
        }
        _ => {
            "Connected but idle; peer choking, piece availability or queue may limit progress (not proven)"
        }
    }
}

/// Explicit local export: no torrent names, paths, URLs, peer addresses or free-form errors.
pub fn export(torrent: &TorrentItem, path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    let report = serde_json::json!({
        "schema":1, "state":torrent.state.label(), "activity":activity(torrent),
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
        export(&torrent, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("tracker.test"));
        assert_eq!(
            export(&torrent, &path).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }
}
