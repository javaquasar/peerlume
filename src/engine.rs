use crate::settings::ClientSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyMode {
    Live,
    NewTorrentsOnly,
    ReconnectPeers,
    RestartApplication,
}

impl ApplyMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "stored defaults; existing transfers unchanged",
            Self::NewTorrentsOnly => "used by new torrents",
            Self::ReconnectPeers => "restart required where supported",
            Self::RestartApplication => "application restart required",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingChange {
    pub key: &'static str,
    pub mode: ApplyMode,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsDiff {
    pub changes: Vec<SettingChange>,
}

impl SettingsDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn count_by_mode(&self, mode: ApplyMode) -> usize {
        self.changes
            .iter()
            .filter(|change| change.mode == mode)
            .count()
    }

    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "No runtime changes".into();
        }
        [
            ApplyMode::Live,
            ApplyMode::NewTorrentsOnly,
            ApplyMode::ReconnectPeers,
            ApplyMode::RestartApplication,
        ]
        .into_iter()
        .filter_map(|mode| {
            let count = self.count_by_mode(mode);
            (count > 0).then(|| format!("{count} {}", mode.label()))
        })
        .collect::<Vec<_>>()
        .join(", ")
    }
}

#[derive(Debug)]
pub struct EngineRuntime {
    applied: ClientSettings,
    pub command_log: Vec<SettingsDiff>,
}

impl EngineRuntime {
    pub fn new(settings: ClientSettings) -> Self {
        Self {
            applied: settings,
            command_log: Vec::new(),
        }
    }

    pub fn apply_settings(&mut self, settings: &ClientSettings) -> SettingsDiff {
        let diff = diff_settings(&self.applied, settings);
        if !diff.is_empty() {
            self.command_log.push(diff.clone());
            self.applied = settings.clone();
        }
        diff
    }
}

pub fn diff_settings(old: &ClientSettings, new: &ClientSettings) -> SettingsDiff {
    let mut changes = Vec::new();
    let mut push = |condition, key, mode| {
        if condition {
            changes.push(SettingChange { key, mode });
        }
    };

    push(
        old.download != new.download,
        "download",
        ApplyMode::NewTorrentsOnly,
    );
    push(old.queue != new.queue, "queue", ApplyMode::Live);
    push(old.speed != new.speed, "speed", ApplyMode::Live);

    push(
        old.connection.max_connections_global != new.connection.max_connections_global
            || old.connection.max_connections_per_torrent
                != new.connection.max_connections_per_torrent
            || old.connection.port_forwarding != new.connection.port_forwarding
            || old.connection.connection_timeout_seconds
                != new.connection.connection_timeout_seconds
            || old.connection.retry_count != new.connection.retry_count,
        "connection_limits",
        ApplyMode::Live,
    );
    push(
        old.connection.listen_port != new.connection.listen_port
            || old.connection.random_port_on_start != new.connection.random_port_on_start
            || old.connection.tcp_enabled != new.connection.tcp_enabled
            || old.connection.utp_enabled != new.connection.utp_enabled
            || old.connection.prefer_utp != new.connection.prefer_utp
            || old.connection.network_interface != new.connection.network_interface
            || old.connection.bind_address != new.connection.bind_address
            || old.connection.proxy_url != new.connection.proxy_url
            || old.connection.proxy_peer_connections != new.connection.proxy_peer_connections,
        "connection_endpoint",
        ApplyMode::ReconnectPeers,
    );

    push(
        old.bittorrent != new.bittorrent,
        "bittorrent",
        ApplyMode::Live,
    );
    push(old.seeding != new.seeding, "seeding", ApplyMode::Live);
    push(
        old.privacy.encryption != new.privacy.encryption,
        "privacy.encryption",
        ApplyMode::ReconnectPeers,
    );
    push(
        old.privacy.tracker_reporting != new.privacy.tracker_reporting,
        "privacy.tracker_reporting",
        ApplyMode::RestartApplication,
    );
    push(
        old.privacy.blocklist_enabled != new.privacy.blocklist_enabled
            || old.privacy.blocklist_url != new.privacy.blocklist_url
            || old.privacy.blocklist_auto_update != new.privacy.blocklist_auto_update
            || old.privacy.redact_tracker_credentials != new.privacy.redact_tracker_credentials,
        "privacy.runtime",
        ApplyMode::Live,
    );
    push(
        old.storage.cache_size_mib != new.storage.cache_size_mib
            || old.storage.max_queued_write_mib != new.storage.max_queued_write_mib,
        "storage.cache",
        ApplyMode::RestartApplication,
    );
    push(
        old.storage.resume_save_interval_seconds != new.storage.resume_save_interval_seconds
            || old.storage.verify_sleep_milliseconds != new.storage.verify_sleep_milliseconds
            || old.storage.check_free_space != new.storage.check_free_space
            || old.storage.minimum_free_space_mib != new.storage.minimum_free_space_mib
            || old.storage.no_space_action != new.storage.no_space_action,
        "storage.runtime",
        ApplyMode::Live,
    );
    push(
        old.storage.move_completed != new.storage.move_completed
            || old.storage.completed_directory != new.storage.completed_directory,
        "storage.completed",
        ApplyMode::NewTorrentsOnly,
    );
    push(
        old.automation != new.automation,
        "automation",
        ApplyMode::Live,
    );

    SettingsDiff { changes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_settings_by_application_mode() {
        let old = ClientSettings::default();
        let mut new = old.clone();
        new.speed.download_limit_kib = 1000;
        new.download.download_directory = "other".into();
        new.connection.listen_port = 60000;
        new.storage.cache_size_mib = 256;
        new.privacy.tracker_reporting = crate::settings::TrackerReportingMode::Disabled;

        let diff = diff_settings(&old, &new);
        assert_eq!(diff.count_by_mode(ApplyMode::Live), 1);
        assert_eq!(diff.count_by_mode(ApplyMode::NewTorrentsOnly), 1);
        assert_eq!(diff.count_by_mode(ApplyMode::ReconnectPeers), 1);
        assert_eq!(diff.count_by_mode(ApplyMode::RestartApplication), 2);
        assert!(diff.changes.contains(&SettingChange {
            key: "privacy.tracker_reporting",
            mode: ApplyMode::RestartApplication,
        }));
    }

    #[test]
    fn runtime_logs_only_real_changes() {
        let settings = ClientSettings::default();
        let mut runtime = EngineRuntime::new(settings.clone());
        assert!(runtime.apply_settings(&settings).is_empty());
        assert!(runtime.command_log.is_empty());

        let mut changed = settings;
        changed.queue.max_active_downloads = 8;
        assert!(!runtime.apply_settings(&changed).is_empty());
        assert_eq!(runtime.command_log.len(), 1);
        assert!(!runtime.command_log[0].is_empty());
    }
}
