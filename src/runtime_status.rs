//! Shared live control acknowledgements, bounded recovery, and status-card data.
use crate::{
    announce_journal::JournalEntry,
    download_mode::{DEFAULT_FREEZE_AFTER_BYTES, DownloadMode},
    torrent::TorrentItem,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeConfig {
    pub mode: DownloadMode,
    pub freeze_after_bytes: u64,
}
impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            mode: DownloadMode::Normal,
            freeze_after_bytes: DEFAULT_FREEZE_AFTER_BYTES,
        }
    }
}
impl ModeConfig {
    pub fn label(self) -> String {
        if self.mode == DownloadMode::FreezeAfter {
            format!("{} ({} bytes)", self.mode.label(), self.freeze_after_bytes)
        } else {
            self.mode.label().into()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModeApplication {
    #[default]
    AwaitingEngine,
    Applying {
        requested: ModeConfig,
        active: Option<ModeConfig>,
    },
    Active {
        config: ModeConfig,
        tracker_enabled: bool,
    },
    Failed {
        requested: ModeConfig,
        active: Option<ModeConfig>,
        message: String,
    },
}
impl ModeApplication {
    pub fn active(&self) -> Option<ModeConfig> {
        match self {
            Self::Active { config, .. } => Some(*config),
            Self::Applying { active, .. } | Self::Failed { active, .. } => *active,
            _ => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::AwaitingEngine => "Awaiting engine",
            Self::Applying { .. } => "Applying",
            Self::Active {
                tracker_enabled: false,
                ..
            } => "Active policy; tracker communication disabled",
            Self::Active { .. } => "Active",
            Self::Failed { .. } => "Failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepairPolicy {
    pub enabled: bool,
    pub max_attempts: u32,
}
impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: 3,
        }
    }
}
impl RepairPolicy {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=10).contains(&self.max_attempts) {
            Err("Repair attempts must be between 1 and 10".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RepairStatus {
    #[default]
    Idle,
    Downloading {
        attempt: u32,
        limit: u32,
    },
    Checking {
        attempt: u32,
        limit: u32,
    },
    Succeeded {
        attempts: u32,
    },
    Exhausted {
        attempts: u32,
        limit: u32,
        message: String,
    },
}
#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    pub mode: ModeApplication,
    pub mode_draft: Option<ModeConfig>,
    pub repair: RepairStatus,
    pub network_downloaded: u64,
    pub network_uploaded: u64,
}

pub const REPAIR_STALL_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) struct RepairBudget {
    pub used: u32,
    progress: u64,
    changed_at: Instant,
}
impl RepairBudget {
    pub fn resume(&mut self, now: Instant) {
        self.changed_at = now;
    }
    pub fn new(now: Instant) -> Self {
        Self {
            used: 0,
            progress: 0,
            changed_at: now,
        }
    }
    pub fn start(&mut self, policy: RepairPolicy, progress: u64, now: Instant) -> bool {
        if !policy.enabled || self.used >= policy.max_attempts {
            return false;
        }
        self.used += 1;
        self.progress = progress;
        self.changed_at = now;
        true
    }
    pub fn stalled(&mut self, progress: u64, now: Instant) -> bool {
        if progress > self.progress {
            self.progress = progress;
            self.changed_at = now;
        }
        now.saturating_duration_since(self.changed_at) >= REPAIR_STALL_TIMEOUT
    }
}

pub fn status_card(torrent: &TorrentItem, last: Option<&JournalEntry>) -> Vec<(String, String)> {
    let mut rows = vec![
        ("Saved mode".into(), torrent.mode_config().label()),
        ("Policy status".into(), torrent.runtime.mode.label().into()),
        (
            "Active mode".into(),
            torrent
                .runtime
                .mode
                .active()
                .map(ModeConfig::label)
                .unwrap_or_else(|| "Not confirmed by engine".into()),
        ),
        (
            "Transfer".into(),
            format!("{} · {}", torrent.status_text(), torrent.progress_text()),
        ),
        (
            "Session network totals (uploaded/downloaded)".into(),
            format!(
                "{} / {} bytes",
                torrent.runtime.network_uploaded, torrent.runtime.network_downloaded
            ),
        ),
        (
            "Checked/selected payload".into(),
            format!(
                "{} / {} bytes",
                torrent.files.selected_downloaded(),
                torrent.selected_size()
            ),
        ),
        (
            "Automatic repair".into(),
            format!(
                "{} · limit {} · {:?}",
                if torrent.overrides.repair_policy.enabled {
                    "Enabled"
                } else {
                    "Disabled"
                },
                torrent.overrides.repair_policy.max_attempts,
                torrent.runtime.repair
            ),
        ),
    ];
    match &torrent.runtime.mode {
        ModeApplication::Applying { requested, .. } => {
            rows.push(("Requested mode".into(), requested.label()))
        }
        ModeApplication::Failed {
            requested, message, ..
        } => rows.push((
            "Mode application error".into(),
            format!("{}: {message}", requested.label()),
        )),
        _ => {}
    }
    if let Some(last) = last {
        let a = &last.announce;
        rows.push((
            "Last announce".into(),
            format!(
                "#{} · {} · mode={} · event={} · {}",
                last.sequence,
                a.started_unix_ms,
                a.mode,
                a.event.as_deref().unwrap_or("none"),
                a.outcome
            ),
        ));
        // Tracker paths, userinfo and query values are never shown in this card.
        rows.push(("Tracker".into(), redacted_endpoint(&a.tracker)));
        rows.push((
            "Actual announce u/d/left".into(),
            format!(
                "{} / {} / {}",
                a.actual.uploaded, a.actual.downloaded, a.actual.left
            ),
        ));
        rows.push((
            "Sent announce u/d/left".into(),
            format!(
                "{} / {} / {}",
                a.sent.uploaded, a.sent.downloaded, a.sent.left
            ),
        ));
        rows.push((
            "Tracker reply".into(),
            format!(
                "HTTP {:?} · peers {} · interval {:?} s · min interval {:?} s",
                a.reply.http_status,
                a.reply.peers.len(),
                a.reply.interval,
                a.reply.min_interval
            ),
        ));
    } else {
        rows.push((
            "Last announce".into(),
            "No real announce observed in this session".into(),
        ));
    }
    rows
}

fn redacted_endpoint(value: &str) -> String {
    match url::Url::parse(value) {
        Ok(url) => format!(
            "{}://{}{} /[redacted]",
            url.scheme(),
            url.host_str().unwrap_or("unknown"),
            url.port().map(|p| format!(":{p}")).unwrap_or_default()
        ),
        Err(_) => "[redacted tracker endpoint]".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repair_is_bounded_and_only_verified_progress_restarts_stall_timer() {
        let now = Instant::now();
        let policy = RepairPolicy {
            enabled: true,
            max_attempts: 3,
        };
        let mut budget = RepairBudget::new(now);
        for _ in 0..3 {
            assert!(budget.start(policy, 0, now));
        }
        assert!(!budget.start(policy, 0, now));
        assert!(!budget.stalled(1, now + REPAIR_STALL_TIMEOUT));
        assert!(budget.stalled(1, now + REPAIR_STALL_TIMEOUT * 2));
        assert!(
            RepairPolicy {
                enabled: true,
                max_attempts: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            RepairPolicy {
                enabled: true,
                max_attempts: 11
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn status_endpoint_never_displays_credentials_or_paths() {
        for url in [
            "https://user:password@tracker.test/private-passkey/announce?uk=secret",
            "not a URL: secret",
        ] {
            let redacted = redacted_endpoint(url);
            for secret in ["user", "password", "private-passkey", "secret"] {
                assert!(!redacted.contains(secret));
            }
        }
    }
}
