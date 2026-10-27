use crate::{
    file_selection::FileSelectionModel,
    settings::{ClientSettings, PieceStrategy, TrackerReportingMode},
};

#[cfg(test)]
mod reliability_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TorrentState {
    Downloading,
    WaitingForEngine,
    Paused,
    Seeding,
    Checking,
    VerificationFailed,
    Error,
}

impl TorrentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Downloading => "Downloading",
            Self::WaitingForEngine => "Waiting for transfer engine",
            Self::Paused => "Paused",
            Self::Seeding => "Seeding",
            Self::Checking => "Checking",
            Self::VerificationFailed => "Verification failed",
            Self::Error => "Error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TorrentPriority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TorrentOverrides {
    pub repair_policy: crate::runtime_status::RepairPolicy,
    pub verify_after_download: bool,
    pub download_mode: crate::download_mode::DownloadMode,
    /// Zero means use the shared default (1 GiB); only relevant to FreezeAfter.
    pub freeze_after_bytes: u64,
    pub upload_ratio: crate::download_mode::UploadRatioRange,
    pub download_directory: Option<String>,
    pub sequential_download: Option<bool>,
    pub priority: TorrentPriority,
    pub download_limit_kib: Option<u64>,
    pub upload_limit_kib: Option<u64>,
    pub max_peers: Option<usize>,
    pub ratio_limit: Option<f64>,
    pub seeding_time_limit_minutes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveTorrentSettings {
    pub download_directory: String,
    pub sequential_download: bool,
    pub piece_strategy: PieceStrategy,
    pub download_limit_kib: u64,
    pub upload_limit_kib: u64,
    pub max_peers: usize,
    pub ratio_limit: Option<f64>,
    pub seeding_time_limit_minutes: Option<u64>,
    pub dht: bool,
    pub pex: bool,
    pub lsd: bool,
    pub tracker_enabled: bool,
}

impl EffectiveTorrentSettings {
    pub fn resolve(
        global: &ClientSettings,
        overrides: &TorrentOverrides,
        is_private: bool,
    ) -> Self {
        Self {
            download_directory: overrides
                .download_directory
                .clone()
                .unwrap_or_else(|| global.download.download_directory.clone()),
            sequential_download: overrides
                .sequential_download
                .unwrap_or(global.download.sequential_download),
            piece_strategy: if overrides
                .sequential_download
                .unwrap_or(global.download.sequential_download)
            {
                PieceStrategy::Sequential
            } else {
                global.bittorrent.piece_strategy
            },
            download_limit_kib: overrides
                .download_limit_kib
                .unwrap_or(global.speed.download_limit_kib),
            upload_limit_kib: overrides
                .upload_limit_kib
                .unwrap_or(global.speed.upload_limit_kib),
            max_peers: overrides
                .max_peers
                .unwrap_or(global.connection.max_connections_per_torrent),
            ratio_limit: overrides.ratio_limit.or_else(|| {
                global
                    .seeding
                    .ratio_limit_enabled
                    .then_some(global.seeding.ratio_limit)
            }),
            seeding_time_limit_minutes: overrides.seeding_time_limit_minutes.or_else(|| {
                global
                    .seeding
                    .time_limit_enabled
                    .then_some(global.seeding.time_limit_minutes)
            }),
            dht: !is_private && global.bittorrent.dht,
            pex: !is_private && global.bittorrent.pex,
            lsd: !is_private && global.bittorrent.lsd,
            tracker_enabled: global.privacy.tracker_reporting == TrackerReportingMode::Accurate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TorrentItem {
    pub runtime: crate::runtime_status::RuntimeStatus,
    pub completion: crate::completion::CompletionStatus,
    pub name: String,
    pub source: String,
    pub info_hashes: Vec<String>,
    pub trackers: Vec<String>,
    pub peer_id: String,
    pub files: FileSelectionModel,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_rate: u64,
    pub upload_rate: u64,
    pub peers: usize,
    pub seeds: usize,
    pub state: TorrentState,
    pub is_private: bool,
    pub overrides: TorrentOverrides,
}

impl TorrentItem {
    pub fn mode_config(&self) -> crate::runtime_status::ModeConfig {
        crate::runtime_status::ModeConfig {
            upload_ratio: self.overrides.upload_ratio,
            mode: self.overrides.download_mode,
            freeze_after_bytes: if self.overrides.freeze_after_bytes == 0 {
                crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES
            } else {
                self.overrides.freeze_after_bytes
            },
        }
    }
    pub fn completed_successfully(&self) -> bool {
        self.completion == crate::completion::CompletionStatus::Verified
            && self.selected_size() > 0
            && self.downloaded_bytes >= self.selected_size()
            && matches!(self.state, TorrentState::Seeding | TorrentState::Paused)
    }

    pub fn display_progress(&self) -> f32 {
        self.completion
            .checking_progress()
            .unwrap_or_else(|| self.progress())
    }

    pub fn progress_text(&self) -> String {
        if let Some(progress) = self.completion.checking_progress() {
            format!("Checking — {:.0}%", progress * 100.0)
        } else if self.completed_successfully() {
            "Done".into()
        } else if matches!(
            self.completion,
            crate::completion::CompletionStatus::Failed { .. }
        ) {
            "Verification failed".into()
        } else {
            // Do not round an unfinished download up to 100%.
            let total = self.selected_size();
            let percent = if total == 0 {
                0
            } else {
                self.downloaded_bytes.min(total) as u128 * 100 / total as u128
            };
            format!("{percent}%")
        }
    }

    pub fn status_text(&self) -> String {
        if self.completed_successfully() {
            format!("Done · {}", self.state.label())
        } else {
            self.state.label().into()
        }
    }

    pub fn selected_size(&self) -> u64 {
        self.files.selected_size()
    }

    pub fn progress(&self) -> f32 {
        let size = self.selected_size();
        if size == 0 {
            0.0
        } else {
            (self.downloaded_bytes as f64 / size as f64).clamp(0.0, 1.0) as f32
        }
    }

    pub fn ratio(&self) -> f64 {
        if self.downloaded_bytes == 0 {
            0.0
        } else {
            self.uploaded_bytes as f64 / self.downloaded_bytes as f64
        }
    }

    pub fn eta_seconds(&self) -> Option<u64> {
        if self.state != TorrentState::Downloading || self.download_rate == 0 {
            return None;
        }
        Some(self.selected_size().saturating_sub(self.downloaded_bytes) / self.download_rate)
    }

    pub fn tick(&mut self, seconds: f64) {
        match self.state {
            TorrentState::Downloading => {
                let increment = (self.download_rate as f64 * seconds) as u64;
                self.files.apply_downloaded_bytes(increment);
                self.downloaded_bytes = self.files.selected_downloaded();
                self.uploaded_bytes = self
                    .uploaded_bytes
                    .saturating_add((self.upload_rate as f64 * seconds) as u64);
                if self.downloaded_bytes >= self.selected_size() && self.selected_size() > 0 {
                    self.state = TorrentState::Seeding;
                    self.download_rate = 0;
                }
            }
            TorrentState::Seeding => {
                self.uploaded_bytes = self
                    .uploaded_bytes
                    .saturating_add((self.upload_rate as f64 * seconds) as u64);
            }
            TorrentState::WaitingForEngine => {}
            TorrentState::Paused => {}
            TorrentState::Error => {}
            TorrentState::Checking | TorrentState::VerificationFailed => {}
        }
    }
}

pub fn demo_torrents() -> Vec<TorrentItem> {
    let mut first = TorrentItem {
        runtime: Default::default(),
        completion: crate::completion::CompletionStatus::Pending,
        name: "Open Movie Collection".into(),
        source: "demo".into(),
        info_hashes: vec!["v1: demo-public-movie".into()],
        trackers: vec!["https://tracker.example/announce".into()],
        peer_id: "-PL0001-demo00000001".into(),
        files: FileSelectionModel::from_files([
            ("video/main-feature.mkv".into(), 2_684_354_560),
            ("video/trailer.mp4".into(), 84_934_656),
            ("subtitles/en.srt".into(), 81_920),
            ("subtitles/de.srt".into(), 86_016),
            ("extras/poster.jpg".into(), 2_621_440),
        ]),
        downloaded_bytes: 1_158_463_488,
        uploaded_bytes: 326_107_136,
        download_rate: 4_800_000,
        upload_rate: 620_000,
        peers: 18,
        seeds: 42,
        state: TorrentState::Downloading,
        is_private: false,
        overrides: TorrentOverrides::default(),
    };
    first.files.set_total_downloaded(first.downloaded_bytes);
    first.downloaded_bytes = first.files.selected_downloaded();

    let mut second = TorrentItem {
        runtime: Default::default(),
        completion: crate::completion::CompletionStatus::Pending,
        name: "Linux Distribution 2026.09".into(),
        source: "demo".into(),
        info_hashes: vec!["v1: demo-linux-distribution".into()],
        trackers: vec!["https://tracker.example/announce".into()],
        peer_id: "-PL0001-demo00000002".into(),
        files: FileSelectionModel::from_files([
            ("images/desktop-x86_64.iso".into(), 4_294_967_296),
            ("images/server-x86_64.iso".into(), 2_147_483_648),
            ("checksums/SHA256SUMS".into(), 4_096),
            ("docs/release-notes.pdf".into(), 3_670_016),
        ]),
        downloaded_bytes: 6_446_454_784,
        uploaded_bytes: 9_310_683_136,
        download_rate: 0,
        upload_rate: 1_150_000,
        peers: 6,
        seeds: 115,
        state: TorrentState::Seeding,
        is_private: false,
        overrides: TorrentOverrides::default(),
    };
    second.files.set_total_downloaded(second.downloaded_bytes);
    second.downloaded_bytes = second.files.selected_downloaded();

    let mut third = TorrentItem {
        runtime: Default::default(),
        completion: crate::completion::CompletionStatus::Pending,
        name: "Private Archive".into(),
        source: "demo".into(),
        info_hashes: vec!["v1: demo-private-archive".into()],
        trackers: vec!["https://tracker.example/passkey/announce".into()],
        peer_id: "-PL0001-demo00000003".into(),
        files: FileSelectionModel::from_files([
            ("archive/part-01.bin".into(), 734_003_200),
            ("archive/part-02.bin".into(), 734_003_200),
            ("archive/readme.txt".into(), 12_288),
        ]),
        downloaded_bytes: 188_743_680,
        uploaded_bytes: 52_428_800,
        download_rate: 0,
        upload_rate: 0,
        peers: 0,
        seeds: 8,
        state: TorrentState::Paused,
        is_private: true,
        overrides: TorrentOverrides::default(),
    };
    third.files.set_total_downloaded(third.downloaded_bytes);
    third.downloaded_bytes = third.files.selected_downloaded();
    vec![first, second, third]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_torrent_forces_tracker_only_discovery() {
        let settings = ClientSettings::default();
        let effective =
            EffectiveTorrentSettings::resolve(&settings, &TorrentOverrides::default(), true);
        assert!(!effective.dht);
        assert!(!effective.pex);
        assert!(!effective.lsd);
        assert!(effective.tracker_enabled);
    }

    #[test]
    fn disabled_reporting_disables_tracker_traffic() {
        let mut settings = ClientSettings::default();
        settings.privacy.tracker_reporting = TrackerReportingMode::Disabled;

        let effective =
            EffectiveTorrentSettings::resolve(&settings, &TorrentOverrides::default(), true);

        assert!(!effective.tracker_enabled);
        assert!(!effective.dht);
        assert!(!effective.pex);
        assert!(!effective.lsd);
    }

    #[test]
    fn per_torrent_values_override_global_defaults() {
        let settings = ClientSettings::default();
        let overrides = TorrentOverrides {
            download_directory: Some("special".into()),
            sequential_download: Some(true),
            download_limit_kib: Some(777),
            max_peers: Some(12),
            ..TorrentOverrides::default()
        };
        let effective = EffectiveTorrentSettings::resolve(&settings, &overrides, false);
        assert_eq!(effective.download_directory, "special");
        assert_eq!(effective.piece_strategy, PieceStrategy::Sequential);
        assert_eq!(effective.download_limit_kib, 777);
        assert_eq!(effective.max_peers, 12);
    }
}
