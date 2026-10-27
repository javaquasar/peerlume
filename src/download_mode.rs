//! Download-time announce behavior, shared by the GUI, TUI and persistence.

pub use librqbit_tracker_comms::UploadRatioRange;
use serde::{Deserialize, Serialize};
pub const DEFAULT_FREEZE_AFTER_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadMode {
    #[default]
    Normal,
    NoProgress,
    #[serde(rename = "upload_ratio_1_5")]
    UploadRatio15,
    HideDownloaded,
    FreezeAfter,
}

impl DownloadMode {
    pub const ALL: [Self; 5] = [
        Self::Normal,
        Self::NoProgress,
        Self::UploadRatio15,
        Self::HideDownloaded,
        Self::FreezeAfter,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::NoProgress => "no_progress",
            Self::UploadRatio15 => "upload_ratio_1_5",
            Self::HideDownloaded => "hide_downloaded",
            Self::FreezeAfter => "freeze_after",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal download",
            Self::NoProgress => "Download without reporting progress",
            Self::UploadRatio15 => "Download; random reported-upload multiplier",
            Self::HideDownloaded => "Hide downloaded only; actual upload and left",
            Self::FreezeAfter => "Freeze metrics after a received-byte threshold",
        }
    }

    pub fn from_id(id: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.id() == id)
            .ok_or_else(|| format!("Unknown download mode: {id}"))
    }

    pub fn reporting_policy(self, full_size: u64) -> librqbit_tracker_comms::ReportingPolicy {
        self.reporting_policy_with_threshold(full_size, DEFAULT_FREEZE_AFTER_BYTES)
    }
    pub fn validate_threshold(self, threshold: u64) -> Result<(), String> {
        if self == Self::FreezeAfter && (threshold == 0 || threshold > i64::MAX as u64) {
            return Err("Freeze threshold must be between 1 and 9223372036854775807 bytes".into());
        }
        Ok(())
    }
    pub fn reporting_policy_with_threshold(
        self,
        full_size: u64,
        threshold: u64,
    ) -> librqbit_tracker_comms::ReportingPolicy {
        librqbit_tracker_comms::ReportingPolicy::new(self.reporting_mode(), full_size, threshold)
    }

    pub(crate) fn reporting_mode(self) -> librqbit_tracker_comms::ReportingMode {
        use librqbit_tracker_comms::ReportingMode;
        match self {
            Self::Normal => ReportingMode::Accurate,
            Self::NoProgress => ReportingMode::FrozenProgress,
            Self::UploadRatio15 => ReportingMode::UploadRatio15,
            Self::HideDownloaded => ReportingMode::HideDownloaded,
            Self::FreezeAfter => ReportingMode::FreezeAfter,
        }
    }

    pub fn validate_metadata(self, has_v1: bool, full_size: u64) -> Result<(), String> {
        if self != Self::Normal && (!has_v1 || full_size == 0) {
            return Err(
                "This download mode requires a v1/hybrid .torrent with a nonzero full size".into(),
            );
        }
        Ok(())
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Normal => "Download and announce actual transfer metrics.",
            Self::NoProgress => {
                "Download normally; announce uploaded=0, downloaded=0, left=full torrent size. Do not report completed."
            }
            Self::UploadRatio15 => {
                "Download normally; announce actual downloaded/left and uploaded=floor(downloaded × multiplier). Choose a new random multiplier from the configured range for every announce. Equal bounds give a fixed multiplier. Reported upload may decrease; local transfer totals remain actual."
            }
            Self::HideDownloaded => {
                "Download normally; announce downloaded=0 while uploaded and left remain actual. Existing files and checked progress are logged separately from network bytes."
            }
            Self::FreezeAfter => {
                "Announce actual metrics until the first announce at or above the received-byte threshold. Keep that uploaded/downloaded/left snapshot across pause, recheck and repair. Explicitly applying a mode or restarting the application resets the snapshot."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librqbit_tracker_comms::{TrackerCommsStats, TrackerCommsStatsState};

    fn stats(downloaded: u64) -> TrackerCommsStats {
        TrackerCommsStats {
            uploaded_bytes: 7,
            downloaded_bytes: downloaded,
            total_bytes: 100,
            torrent_state: TrackerCommsStatsState::Live,
        }
    }

    #[test]
    fn policies_change_only_announced_values() {
        let normal = DownloadMode::Normal.reporting_policy(100).apply(stats(20));
        assert_eq!(
            (
                normal.uploaded_bytes,
                normal.downloaded_bytes,
                normal.get_left_to_download_bytes()
            ),
            (7, 20, 80)
        );
        let hidden = DownloadMode::NoProgress
            .reporting_policy(100)
            .apply(stats(100));
        assert_eq!(
            (
                hidden.uploaded_bytes,
                hidden.downloaded_bytes,
                hidden.get_left_to_download_bytes()
            ),
            (0, 0, 100)
        );
        assert!(!hidden.is_completed());
        let ratio = crate::runtime_status::ModeConfig {
            mode: DownloadMode::UploadRatio15,
            upload_ratio: UploadRatioRange {
                min_milli: 1500,
                max_milli: 1500,
            },
            ..Default::default()
        }
        .reporting_policy(100)
        .unwrap()
        .apply(stats(21));
        assert_eq!(
            (
                ratio.uploaded_bytes,
                ratio.downloaded_bytes,
                ratio.get_left_to_download_bytes()
            ),
            (31, 21, 79)
        );
        let maximum = DownloadMode::UploadRatio15
            .reporting_policy(u64::MAX)
            .apply(stats(u64::MAX));
        assert_eq!(maximum.uploaded_bytes, u64::MAX);
    }

    #[test]
    fn normal_is_default_and_every_id_round_trips() {
        assert_eq!(DownloadMode::default(), DownloadMode::Normal);
        for mode in DownloadMode::ALL {
            assert_eq!(DownloadMode::from_id(mode.id()).unwrap(), mode);
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.id())
            );
        }
        assert!(DownloadMode::from_id("typo").is_err());
        assert!(
            DownloadMode::NoProgress
                .validate_metadata(false, 1)
                .is_err()
        );
        assert!(DownloadMode::NoProgress.validate_metadata(true, 0).is_err());
    }

    #[test]
    fn hiding_downloaded_preserves_upload_and_remaining_independently() {
        let actual = librqbit_tracker_comms::AnnounceMetrics {
            uploaded: 31,
            downloaded: 20,
            left: 50,
        };
        let sent = DownloadMode::HideDownloaded
            .reporting_policy(100)
            .apply_metrics(actual);
        assert_eq!((sent.uploaded, sent.downloaded, sent.left), (31, 0, 50));
        assert_eq!(actual.downloaded, 20);
    }

    #[test]
    fn freeze_latches_all_metrics_at_first_announce_at_or_above_threshold() {
        use librqbit_tracker_comms::AnnounceMetrics;
        let policy = DownloadMode::FreezeAfter.reporting_policy_with_threshold(100, 25);
        let before = AnnounceMetrics {
            uploaded: 3,
            downloaded: 24,
            left: 76,
        };
        let frozen = AnnounceMetrics {
            uploaded: 4,
            downloaded: 30,
            left: 70,
        };
        assert_eq!(policy.apply_metrics(before), before);
        assert_eq!(policy.apply_metrics(frozen), frozen);
        assert_eq!(
            policy.apply_metrics(AnnounceMetrics {
                uploaded: 500,
                downloaded: 100,
                left: 0
            }),
            frozen
        );
        // Counter rollback also cannot unfreeze a latched snapshot.
        assert_eq!(policy.apply_metrics(before), frozen);
        assert!(DownloadMode::FreezeAfter.validate_threshold(0).is_err());
        assert!(
            DownloadMode::FreezeAfter
                .validate_threshold(u64::MAX)
                .is_err()
        );
    }
}
