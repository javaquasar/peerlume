//! PeerLume extension: per-session, per-torrent announce reporting policy.
//! Only outgoing tracker statistics are transformed; transfer statistics and
//! the returned peer stream are untouched.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use crate::TrackerCommsStats;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReportingMode {
    #[default]
    Accurate,
    FrozenProgress,
    UploadRatio15,
    HideDownloaded,
    FreezeAfter,
}

#[derive(Debug, Default)]
pub struct ReportingPolicy {
    pub mode: ReportingMode,
    pub full_size: u64,
    pub freeze_after_bytes: u64,
    frozen: Mutex<Option<crate::AnnounceMetrics>>,
}

impl ReportingPolicy {
    pub fn new(mode: ReportingMode, full_size: u64, freeze_after_bytes: u64) -> Self {
        Self {
            mode,
            full_size,
            freeze_after_bytes,
            frozen: Mutex::new(None),
        }
    }
    pub fn mode_id(&self) -> &'static str {
        match self.mode {
            ReportingMode::Accurate => "normal",
            ReportingMode::FrozenProgress => "no_progress",
            ReportingMode::UploadRatio15 => "upload_ratio_1_5",
            ReportingMode::HideDownloaded => "hide_downloaded",
            ReportingMode::FreezeAfter => "freeze_after",
        }
    }
    pub fn apply_metrics(&self, mut metrics: crate::AnnounceMetrics) -> crate::AnnounceMetrics {
        match self.mode {
            ReportingMode::Accurate => {}
            ReportingMode::FrozenProgress => {
                metrics = crate::AnnounceMetrics {
                    uploaded: 0,
                    downloaded: 0,
                    left: self.full_size,
                }
            }
            ReportingMode::UploadRatio15 => {
                metrics.uploaded =
                    (u128::from(metrics.downloaded) * 3 / 2).min(u128::from(u64::MAX)) as u64
            }
            ReportingMode::HideDownloaded => metrics.downloaded = 0,
            ReportingMode::FreezeAfter => {
                let mut frozen = self.frozen.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(value) = *frozen {
                    return value;
                }
                if metrics.downloaded >= self.freeze_after_bytes {
                    *frozen = Some(metrics);
                }
            }
        }
        metrics
    }
    pub fn apply(&self, mut stats: TrackerCommsStats) -> TrackerCommsStats {
        match self.mode {
            ReportingMode::Accurate => {}
            ReportingMode::FrozenProgress => {
                stats.uploaded_bytes = 0;
                stats.downloaded_bytes = 0;
                stats.total_bytes = self.full_size;
            }
            ReportingMode::UploadRatio15 => {
                stats.uploaded_bytes =
                    ((u128::from(stats.downloaded_bytes) * 3 / 2).min(u128::from(u64::MAX))) as u64;
            }
            ReportingMode::HideDownloaded | ReportingMode::FreezeAfter => {
                let metrics = self.apply_metrics(crate::AnnounceMetrics {
                    uploaded: stats.uploaded_bytes,
                    downloaded: stats.downloaded_bytes,
                    left: stats.get_left_to_download_bytes(),
                });
                stats.uploaded_bytes = metrics.uploaded;
                stats.downloaded_bytes = metrics.downloaded;
                stats.total_bytes = metrics.downloaded.saturating_add(metrics.left);
            }
        }
        stats
    }
}

type Key = ([u8; 20], [u8; 20]);
static POLICIES: LazyLock<Mutex<HashMap<Key, Arc<ReportingPolicy>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct ReportingRegistration {
    key: Key,
}

impl ReportingRegistration {
    /// Replace the registry entry while transports are paused. Existing in-flight
    /// requests retain their old snapshot; newly started transports use this policy.
    pub fn replace(&self, policy: ReportingPolicy) -> Result<(), &'static str> {
        let mut policies = POLICIES.lock().unwrap_or_else(|error| error.into_inner());
        let entry = policies.get_mut(&self.key).ok_or("Reporting registration is no longer active")?;
        *entry = Arc::new(policy);
        Ok(())
    }
}

impl Drop for ReportingRegistration {
    fn drop(&mut self) {
        POLICIES
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

pub fn register_reporting_policy(
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    policy: ReportingPolicy,
) -> Result<ReportingRegistration, &'static str> {
    let key = (info_hash, peer_id);
    let mut policies = POLICIES.lock().unwrap_or_else(|error| error.into_inner());
    if policies.contains_key(&key) {
        return Err("Torrent reporting policy is already registered in this session");
    }
    policies.insert(key, Arc::new(policy));
    Ok(ReportingRegistration { key })
}

pub(crate) fn policy_for(info_hash: [u8; 20], peer_id: [u8; 20]) -> Arc<ReportingPolicy> {
    POLICIES
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&(info_hash, peer_id))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    #[test]
    fn replacement_is_session_scoped_and_old_workers_keep_their_snapshot() {
        let hash = [201; 20]; let peer = [202; 20]; let other = [203; 20];
        let guard = register_reporting_policy(hash, peer, ReportingPolicy::new(ReportingMode::FrozenProgress, 100, 10)).unwrap();
        let other_guard = register_reporting_policy(hash, other, ReportingPolicy::default()).unwrap();
        let old_worker = policy_for(hash, peer);
        guard.replace(ReportingPolicy::new(ReportingMode::HideDownloaded, 100, 10)).unwrap();
        let actual = crate::AnnounceMetrics { uploaded: 7, downloaded: 20, left: 80 };
        assert_eq!(old_worker.apply_metrics(actual).uploaded, 0);
        assert_eq!(policy_for(hash, peer).apply_metrics(actual).uploaded, 7);
        assert_eq!(policy_for(hash, peer).apply_metrics(actual).downloaded, 0);
        assert_eq!(policy_for(hash, other).apply_metrics(actual), actual);
        drop(guard); drop(other_guard);
        assert_eq!(policy_for(hash, peer).apply_metrics(actual), actual);
    }

    #[test]
    fn explicit_replacement_resets_a_frozen_snapshot() {
        let hash = [204; 20]; let peer = [205; 20];
        let policy = || ReportingPolicy::new(ReportingMode::FreezeAfter, 100, 10);
        let guard = register_reporting_policy(hash, peer, policy()).unwrap();
        let old = policy_for(hash, peer);
        let first = crate::AnnounceMetrics { uploaded: 3, downloaded: 10, left: 90 };
        let next = crate::AnnounceMetrics { uploaded: 9, downloaded: 30, left: 70 };
        assert_eq!(old.apply_metrics(first), first);
        assert_eq!(old.apply_metrics(next), first);
        guard.replace(policy()).unwrap();
        assert_eq!(policy_for(hash, peer).apply_metrics(next), next);
        assert_eq!(old.apply_metrics(next), first);
    }
}
