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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateSample {
    pub down: u64,
    pub up: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub history: VecDeque<RateSample>,
    pub peers: Vec<PeerSnapshot>,
    previous: Option<Instant>,
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
        self.history.push_back(RateSample { down, up });
    }
}
pub fn activity(torrent: &TorrentItem) -> &'static str {
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
    }
}
