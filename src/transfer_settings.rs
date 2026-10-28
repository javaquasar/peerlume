//! Validated, persisted controls shared by frontends and the real session.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferSettings {
    /// Zero is unlimited; units are KiB/s, not tracker-reported bytes.
    pub download_kib: u32,
    pub upload_kib: u32,
    pub max_peers: u16,
    pub sequential: bool,
    pub priority: Priority,
    /// Zero disables the seeding stop condition. Fixed-point, 1000 = ratio 1.
    pub ratio_milli: u32,
    pub seed_minutes: u32,
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            download_kib: 0,
            upload_kib: 0,
            max_peers: 128,
            sequential: false,
            priority: Priority::Normal,
            ratio_milli: 0,
            seed_minutes: 0,
        }
    }
}

impl TransferSettings {
    pub(crate) fn seed_limit_reached(
        self,
        uploaded: u64,
        size: u64,
        seeded: std::time::Duration,
    ) -> bool {
        (self.ratio_milli > 0
            && size > 0
            && u128::from(uploaded) * 1000 >= u128::from(size) * u128::from(self.ratio_milli))
            || (self.seed_minutes > 0 && seeded.as_secs() >= u64::from(self.seed_minutes) * 60)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.download_kib > u32::MAX / 1024 || self.upload_kib > u32::MAX / 1024 {
            return Err("Rate must be 0..4194303 KiB/s (0 means unlimited)".into());
        }
        if !(1..=4096).contains(&self.max_peers) {
            return Err("Max peers must be 1..4096".into());
        }
        if self.ratio_milli > 1_000_000 {
            return Err("Seed ratio must be 0..1000 (0 disables)".into());
        }
        Ok(())
    }

    pub(crate) fn engine(self) -> librqbit::TransferSettings {
        librqbit::TransferSettings {
            limits: librqbit::limits::LimitsConfig {
                download_bps: std::num::NonZeroU32::new(self.download_kib * 1024),
                upload_bps: std::num::NonZeroU32::new(self.upload_kib * 1024),
            },
            sequential: self.sequential,
            priority: match self.priority {
                Priority::Low => -1,
                Priority::Normal => 0,
                Priority::High => 1,
            },
        }
    }

    pub(crate) fn from_item(
        item: &crate::torrent::TorrentItem,
        global: &crate::settings::ClientSettings,
    ) -> Self {
        if let Some(settings) = item.overrides.transfer_settings {
            return settings;
        }
        let effective = crate::torrent::EffectiveTorrentSettings::resolve(
            global,
            &item.overrides,
            item.is_private,
        );
        Self {
            download_kib: effective.download_limit_kib.min(u64::from(u32::MAX / 1024)) as u32,
            upload_kib: effective.upload_limit_kib.min(u64::from(u32::MAX / 1024)) as u32,
            max_peers: effective.max_peers.clamp(1, 4096) as u16,
            sequential: effective.sequential_download,
            priority: match item.overrides.priority {
                crate::torrent::TorrentPriority::Low => Priority::Low,
                crate::torrent::TorrentPriority::Normal => Priority::Normal,
                crate::torrent::TorrentPriority::High => Priority::High,
            },
            ratio_milli: effective
                .ratio_limit
                .map(|ratio| (ratio * 1000.0).round().clamp(0.0, 1_000_000.0) as u32)
                .unwrap_or(0),
            seed_minutes: effective
                .seeding_time_limit_minutes
                .unwrap_or(0)
                .min(u64::from(u32::MAX)) as u32,
        }
    }

    pub(crate) fn apply(self, item: &mut crate::torrent::TorrentItem) {
        item.overrides.transfer_settings = Some(self);
        item.overrides.download_limit_kib = Some(u64::from(self.download_kib));
        item.overrides.upload_limit_kib = Some(u64::from(self.upload_kib));
        item.overrides.max_peers = Some(usize::from(self.max_peers));
        item.overrides.sequential_download = Some(self.sequential);
        item.overrides.priority = match self.priority {
            Priority::Low => crate::torrent::TorrentPriority::Low,
            Priority::Normal => crate::torrent::TorrentPriority::Normal,
            Priority::High => crate::torrent::TorrentPriority::High,
        };
        item.overrides.ratio_limit = Some(f64::from(self.ratio_milli) / 1000.0);
        item.overrides.seeding_time_limit_minutes = Some(u64::from(self.seed_minutes));
    }
}

#[derive(Default)]
pub(crate) struct SeedingClock {
    elapsed: std::time::Duration,
    previous: Option<std::time::Instant>,
}
impl SeedingClock {
    pub(crate) fn restored(seconds: u64) -> Self {
        Self {
            elapsed: std::time::Duration::from_secs(seconds),
            previous: None,
        }
    }
    pub(crate) fn sample(&mut self, now: std::time::Instant, active: bool) -> std::time::Duration {
        if let Some(previous) = self.previous.take() {
            self.elapsed += now.saturating_duration_since(previous);
        }
        if active {
            self.previous = Some(now);
        }
        self.elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_point_ratios_survive_round_trip_through_the_gui_override_draft() {
        let mut item = crate::client::tests::torrent();
        for ratio_milli in [0, 1, 999, 1001, 2001, 123_457, 1_000_000] {
            let settings = TransferSettings {
                ratio_milli,
                ..Default::default()
            };
            settings.apply(&mut item);
            item.overrides.transfer_settings = None;
            assert_eq!(
                TransferSettings::from_item(&item, &crate::settings::ClientSettings::default()),
                settings
            );
        }
    }
    #[test]
    fn validation_prevents_overflow_and_zero_peer_deadlock() {
        let mut settings = TransferSettings::default();
        assert!(settings.validate().is_ok());
        settings.download_kib = u32::MAX;
        assert!(settings.validate().is_err());
        settings.download_kib = 1;
        assert_eq!(settings.engine().limits.download_bps.unwrap().get(), 1024);
        settings.max_peers = 0;
        assert!(settings.validate().is_err());
        assert!(serde_json::from_str::<TransferSettings>("{\"download_kib\":-1}").is_err());
    }

    #[test]
    fn seed_limits_use_real_counts_and_do_not_count_paused_time() {
        let mut settings = TransferSettings {
            ratio_milli: 2000,
            seed_minutes: 1,
            ..Default::default()
        };
        assert!(!settings.seed_limit_reached(199, 100, std::time::Duration::ZERO));
        assert!(settings.seed_limit_reached(200, 100, std::time::Duration::ZERO));
        assert!(settings.seed_limit_reached(0, 100, std::time::Duration::from_secs(60)));
        let now = std::time::Instant::now();
        let mut clock = SeedingClock::default();
        assert_eq!(clock.sample(now, true).as_secs(), 0);
        assert_eq!(
            clock
                .sample(now + std::time::Duration::from_secs(10), false)
                .as_secs(),
            10
        );
        assert_eq!(
            clock
                .sample(now + std::time::Duration::from_secs(100), true)
                .as_secs(),
            10
        );
        assert_eq!(
            clock
                .sample(now + std::time::Duration::from_secs(105), true)
                .as_secs(),
            15
        );
        settings.ratio_milli = 0;
        settings.seed_minutes = 0;
        assert!(!settings.seed_limit_reached(u64::MAX, 1, std::time::Duration::MAX));
    }
}
