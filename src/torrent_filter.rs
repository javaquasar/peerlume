//! Read-only status filtering shared by the desktop and terminal frontends.
use crate::torrent::{TorrentItem, TorrentState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Downloading,
    Seeding,
    Paused,
    Checking,
    Done,
    WaitingForEngine,
    VerificationFailed,
    Error,
}

impl StatusFilter {
    pub const ALL: [Self; 9] = [
        Self::All,
        Self::Downloading,
        Self::Seeding,
        Self::Paused,
        Self::Checking,
        Self::Done,
        Self::WaitingForEngine,
        Self::VerificationFailed,
        Self::Error,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Downloading => TorrentState::Downloading.label(),
            Self::Seeding => TorrentState::Seeding.label(),
            Self::Paused => TorrentState::Paused.label(),
            Self::Checking => TorrentState::Checking.label(),
            Self::Done => "Done",
            Self::WaitingForEngine => TorrentState::WaitingForEngine.label(),
            Self::VerificationFailed => TorrentState::VerificationFailed.label(),
            Self::Error => TorrentState::Error.label(),
        }
    }

    pub fn matches(self, torrent: &TorrentItem) -> bool {
        match self {
            Self::All => true,
            Self::Downloading => torrent.state == TorrentState::Downloading,
            Self::Seeding => torrent.state == TorrentState::Seeding,
            Self::Paused => torrent.state == TorrentState::Paused,
            Self::Checking => torrent.state == TorrentState::Checking,
            Self::Done => torrent.completed_successfully(),
            Self::WaitingForEngine => torrent.state == TorrentState::WaitingForEngine,
            Self::VerificationFailed => torrent.state == TorrentState::VerificationFailed,
            Self::Error => torrent.state == TorrentState::Error,
        }
    }

    /// Original model indices, never filtered row positions. Commands retain
    /// their canonical target even when earlier rows are hidden.
    pub fn indices(self, torrents: &[TorrentItem]) -> impl Iterator<Item = usize> + '_ {
        torrents
            .iter()
            .enumerate()
            .filter_map(move |(index, torrent)| self.matches(torrent).then_some(index))
    }

    pub fn count(self, torrents: &[TorrentItem]) -> usize {
        self.indices(torrents).count()
    }

    pub fn selected_index(self, torrents: &[TorrentItem], selected: usize) -> Option<usize> {
        torrents
            .get(selected)
            .filter(|torrent| self.matches(torrent))
            .map(|_| selected)
            .or_else(|| self.indices(torrents).next())
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|filter| *filter == self)
            .expect("known filter");
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionStatus;

    #[test]
    fn every_engine_state_has_an_exact_filter_and_all_never_hides_any_state() {
        let states = [
            TorrentState::Downloading,
            TorrentState::Seeding,
            TorrentState::Paused,
            TorrentState::Checking,
            TorrentState::WaitingForEngine,
            TorrentState::VerificationFailed,
            TorrentState::Error,
        ];
        let mut torrent = crate::client::tests::torrent();
        for state in states {
            torrent.state = state;
            assert!(StatusFilter::All.matches(&torrent));
            let matched: Vec<_> = StatusFilter::ALL
                .into_iter()
                .filter(|filter| {
                    *filter != StatusFilter::All
                        && *filter != StatusFilter::Done
                        && filter.matches(&torrent)
                })
                .collect();
            assert_eq!(matched.len(), 1);
            assert_eq!(matched[0].label(), state.label());
        }
    }

    #[test]
    fn done_requires_verified_selected_payload_not_a_rounded_percentage_or_seeding_state() {
        let mut torrent = crate::client::tests::torrent();
        torrent.downloaded_bytes = torrent.selected_size();
        for state in [TorrentState::Seeding, TorrentState::Paused] {
            torrent.state = state;
            torrent.completion = CompletionStatus::Pending;
            assert!(!StatusFilter::Done.matches(&torrent));
            torrent.completion = CompletionStatus::Verified;
            assert!(StatusFilter::Done.matches(&torrent));
            torrent.downloaded_bytes -= 1;
            assert!(!StatusFilter::Done.matches(&torrent));
            torrent.downloaded_bytes += 1;
        }
        torrent.state = TorrentState::Checking;
        assert!(!StatusFilter::Done.matches(&torrent));
        torrent.state = TorrentState::Seeding;
        torrent.files.select_all(false);
        assert!(!StatusFilter::Done.matches(&torrent));
    }

    #[test]
    fn filtering_preserves_original_indices_identity_order_and_visible_selection() {
        let mut torrents = vec![crate::client::tests::torrent(); 5];
        for (index, torrent) in torrents.iter_mut().enumerate() {
            torrent.info_hashes = vec![format!("v1: {index}")];
            torrent.state = if index % 2 == 0 {
                TorrentState::Paused
            } else {
                TorrentState::Downloading
            };
        }
        let filter = StatusFilter::Downloading;
        assert_eq!(filter.indices(&torrents).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(filter.count(&torrents), 2);
        assert_eq!(filter.selected_index(&torrents, 3), Some(3));
        assert_eq!(filter.selected_index(&torrents, 0), Some(1));
        assert_eq!(filter.selected_index(&torrents, usize::MAX), Some(1));
        assert_eq!(torrents[3].info_hashes, ["v1: 3"]);
        torrents[3].state = TorrentState::Paused;
        assert_eq!(filter.selected_index(&torrents, 3), Some(1));
        torrents.remove(1);
        assert_eq!(filter.selected_index(&torrents, 1), None);
        assert_eq!(filter.count(&torrents), 0);
        assert_eq!(
            StatusFilter::All.indices(&torrents).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn empty_lists_are_safe_and_filter_cycle_returns_to_all() {
        let mut filter = StatusFilter::default();
        for expected in StatusFilter::ALL {
            assert_eq!(filter, expected);
            assert_eq!(filter.count(&[]), 0);
            assert_eq!(filter.selected_index(&[], usize::MAX), None);
            filter = filter.next();
        }
        assert_eq!(filter, StatusFilter::All);
    }
}
