use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 2;
pub const DEFAULT_BLOCKLIST_URL: &str =
    "https://github.com/Naunter/BT_BlockLists/raw/master/bt_blocklists.gz";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentLayout {
    #[default]
    Original,
    Subfolder,
    NoSubfolder,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreallocationMode {
    Off,
    #[default]
    Fast,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode {
    #[default]
    Fast,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceStrategy {
    #[default]
    RarestFirst,
    Sequential,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMode {
    #[default]
    Prefer,
    Require,
    AllowPlaintext,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerReportingMode {
    #[default]
    Accurate,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChokingAlgorithm {
    #[default]
    TitForTat,
    FixedSlots,
    RateBased,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoSpaceAction {
    #[default]
    PauseTorrent,
    WarnOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    pub download_directory: String,
    pub incomplete_directory_enabled: bool,
    pub incomplete_directory: String,
    pub rename_partial_files: bool,
    pub start_when_added: bool,
    pub show_add_dialog: bool,
    pub select_all_files: bool,
    pub content_layout: ContentLayout,
    pub preallocation: PreallocationMode,
    pub verify_on_add: VerifyMode,
    pub verify_on_completion: bool,
    pub sequential_download: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            download_directory: "downloads".into(),
            incomplete_directory_enabled: false,
            incomplete_directory: "downloads/incomplete".into(),
            rename_partial_files: true,
            start_when_added: true,
            show_add_dialog: true,
            select_all_files: true,
            content_layout: ContentLayout::Original,
            preallocation: PreallocationMode::Fast,
            verify_on_add: VerifyMode::Fast,
            verify_on_completion: false,
            sequential_download: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueSettings {
    pub enabled: bool,
    pub max_active_downloads: usize,
    pub max_active_checks: usize,
    pub add_to_top: bool,
    pub stalled_enabled: bool,
    pub stalled_minutes: u64,
    pub ignore_inactive: bool,
    pub start_next_automatically: bool,
    pub prefer_downloads: bool,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_active_downloads: 3,
            max_active_checks: 1,
            add_to_top: false,
            stalled_enabled: true,
            stalled_minutes: 30,
            ignore_inactive: true,
            start_next_automatically: true,
            prefer_downloads: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeedSettings {
    pub download_limit_kib: u64,
    pub upload_limit_kib: u64,
    pub alternate_enabled: bool,
    pub alternate_download_kib: u64,
    pub alternate_upload_kib: u64,
    pub schedule_enabled: bool,
    pub schedule_begin_minutes: u16,
    pub schedule_end_minutes: u16,
    pub schedule_days: u8,
    pub limit_local_peers: bool,
}

impl Default for SpeedSettings {
    fn default() -> Self {
        Self {
            download_limit_kib: 0,
            upload_limit_kib: 0,
            alternate_enabled: false,
            alternate_download_kib: 50,
            alternate_upload_kib: 50,
            schedule_enabled: false,
            schedule_begin_minutes: 540,
            schedule_end_minutes: 1020,
            schedule_days: 127,
            limit_local_peers: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionSettings {
    pub max_connections_global: usize,
    pub max_connections_per_torrent: usize,
    pub listen_port: u16,
    pub random_port_on_start: bool,
    pub port_forwarding: bool,
    pub tcp_enabled: bool,
    pub utp_enabled: bool,
    pub prefer_utp: bool,
    pub connection_timeout_seconds: u64,
    pub retry_count: usize,
    pub network_interface: String,
    pub bind_address: String,
    pub proxy_url: String,
    pub proxy_peer_connections: bool,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            max_connections_global: 200,
            max_connections_per_torrent: 50,
            listen_port: 51413,
            random_port_on_start: false,
            port_forwarding: true,
            tcp_enabled: true,
            utp_enabled: true,
            prefer_utp: true,
            connection_timeout_seconds: 10,
            retry_count: 3,
            network_interface: String::new(),
            bind_address: String::new(),
            proxy_url: String::new(),
            proxy_peer_connections: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BitTorrentSettings {
    pub peer_id_prefix: String,
    pub piece_strategy: PieceStrategy,
    pub prioritize_first_last: bool,
    pub endgame: bool,
    pub dht: bool,
    pub pex: bool,
    pub lsd: bool,
    pub max_outstanding_requests: usize,
    pub outgoing_connections_per_second: usize,
    pub public_extra_trackers: String,
}

impl Default for BitTorrentSettings {
    fn default() -> Self {
        Self {
            peer_id_prefix: "-PL0001-".into(),
            piece_strategy: PieceStrategy::RarestFirst,
            prioritize_first_last: false,
            endgame: true,
            dht: true,
            pex: true,
            lsd: false,
            max_outstanding_requests: 200,
            outgoing_connections_per_second: 20,
            public_extra_trackers: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeedingSettings {
    pub max_active_seeds: usize,
    pub ratio_limit_enabled: bool,
    pub ratio_limit: f64,
    pub time_limit_enabled: bool,
    pub time_limit_minutes: u64,
    pub idle_limit_enabled: bool,
    pub idle_limit_minutes: u64,
    pub upload_slots_per_torrent: usize,
    pub choking_algorithm: ChokingAlgorithm,
    pub prefer_seeds: bool,
}

impl Default for SeedingSettings {
    fn default() -> Self {
        Self {
            max_active_seeds: 10,
            ratio_limit_enabled: false,
            ratio_limit: 2.0,
            time_limit_enabled: false,
            time_limit_minutes: 1440,
            idle_limit_enabled: false,
            idle_limit_minutes: 30,
            upload_slots_per_torrent: 14,
            choking_algorithm: ChokingAlgorithm::TitForTat,
            prefer_seeds: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacySettings {
    pub encryption: EncryptionMode,
    pub tracker_reporting: TrackerReportingMode,
    pub blocklist_enabled: bool,
    pub blocklist_url: String,
    pub blocklist_auto_update: bool,
    pub redact_tracker_credentials: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            encryption: EncryptionMode::Prefer,
            tracker_reporting: TrackerReportingMode::Accurate,
            blocklist_enabled: true,
            blocklist_url: DEFAULT_BLOCKLIST_URL.into(),
            blocklist_auto_update: true,
            redact_tracker_credentials: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    pub cache_size_mib: u64,
    pub max_queued_write_mib: u64,
    pub resume_save_interval_seconds: u64,
    pub verify_sleep_milliseconds: u64,
    pub check_free_space: bool,
    pub minimum_free_space_mib: u64,
    pub no_space_action: NoSpaceAction,
    pub move_completed: bool,
    pub completed_directory: String,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            cache_size_mib: 64,
            max_queued_write_mib: 32,
            resume_save_interval_seconds: 180,
            verify_sleep_milliseconds: 100,
            check_free_space: true,
            minimum_free_space_mib: 1024,
            no_space_action: NoSpaceAction::PauseTorrent,
            move_completed: false,
            completed_directory: "downloads/completed".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationSettings {
    pub watch_directory_enabled: bool,
    pub watch_directory: String,
    pub remove_original_torrent: bool,
    pub script_on_added: String,
    pub script_on_completed: String,
    pub script_on_seeding_completed: String,
    pub backup_torrents_enabled: bool,
    pub backup_directory: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub last_preferences_tab: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientSettings {
    pub version: u32,
    pub download: DownloadSettings,
    pub queue: QueueSettings,
    pub speed: SpeedSettings,
    pub connection: ConnectionSettings,
    pub bittorrent: BitTorrentSettings,
    pub seeding: SeedingSettings,
    pub privacy: PrivacySettings,
    pub storage: StorageSettings,
    pub automation: AutomationSettings,
    pub ui: UiSettings,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            download: DownloadSettings::default(),
            queue: QueueSettings::default(),
            speed: SpeedSettings::default(),
            connection: ConnectionSettings::default(),
            bittorrent: BitTorrentSettings::default(),
            seeding: SeedingSettings::default(),
            privacy: PrivacySettings::default(),
            storage: StorageSettings::default(),
            automation: AutomationSettings::default(),
            ui: UiSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub field: &'static str,
    pub message: String,
}

impl ClientSettings {
    pub fn from_toml(contents: &str) -> Result<Self, toml::de::Error> {
        let value: toml::Value = toml::from_str(contents)?;
        if value.get("download").is_some() || value.get("version").is_some() {
            let mut settings: Self = toml::from_str(contents)?;
            settings.version = CONFIG_VERSION;
            Ok(settings)
        } else {
            let legacy: LegacySettings = toml::from_str(contents)?;
            Ok(legacy.into_current())
        }
    }

    pub fn load() -> Self {
        Self::load_from(&settings_path()).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let contents = fs::read_to_string(path)?;
        Self::from_toml(&contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&settings_path())
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut current = self.clone();
        current.version = CONFIG_VERSION;
        let contents = toml::to_string_pretty(&current)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, contents)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(temporary, path)
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let mut push = |field, message: &str| {
            issues.push(ValidationIssue {
                field,
                message: message.into(),
            })
        };

        if self.download.download_directory.trim().is_empty() {
            push(
                "download.download_directory",
                "download directory cannot be empty",
            );
        }
        if self.download.incomplete_directory_enabled
            && self.download.incomplete_directory.trim().is_empty()
        {
            push(
                "download.incomplete_directory",
                "incomplete directory cannot be empty",
            );
        }
        if self.queue.enabled && self.queue.max_active_downloads == 0 {
            push(
                "queue.max_active_downloads",
                "must be at least 1 when queueing is enabled",
            );
        }
        if self.queue.max_active_checks == 0 {
            push("queue.max_active_checks", "must be at least 1");
        }
        if self.speed.schedule_begin_minutes >= 1440 || self.speed.schedule_end_minutes >= 1440 {
            push("speed.schedule", "schedule times must fall within one day");
        }
        if self.speed.schedule_enabled && self.speed.schedule_days == 0 {
            push("speed.schedule_days", "select at least one schedule day");
        }
        if self.connection.max_connections_global == 0 {
            push("connection.max_connections_global", "must be at least 1");
        }
        if self.connection.max_connections_per_torrent == 0
            || self.connection.max_connections_per_torrent > self.connection.max_connections_global
        {
            push(
                "connection.max_connections_per_torrent",
                "must be between 1 and the global limit",
            );
        }
        if !self.connection.tcp_enabled && !self.connection.utp_enabled {
            push(
                "connection.transports",
                "at least TCP or uTP must be enabled",
            );
        }
        if self.connection.connection_timeout_seconds == 0 {
            push(
                "connection.connection_timeout_seconds",
                "must be at least 1 second",
            );
        }
        let proxy = self.connection.proxy_url.trim();
        if !proxy.is_empty()
            && ![
                "http://",
                "https://",
                "socks4://",
                "socks4h://",
                "socks5://",
                "socks5h://",
            ]
            .iter()
            .any(|prefix| proxy.starts_with(prefix))
        {
            push(
                "connection.proxy_url",
                "proxy URL has an unsupported scheme",
            );
        }
        if self.bittorrent.max_outstanding_requests == 0 {
            push("bittorrent.max_outstanding_requests", "must be at least 1");
        }
        if crate::metainfo::normalize_peer_id_prefix(&self.bittorrent.peer_id_prefix).is_err() {
            push(
                "bittorrent.peer_id_prefix",
                "peer ID prefix must contain 1 to 8 printable ASCII bytes",
            );
        }
        if self.seeding.ratio_limit_enabled && self.seeding.ratio_limit <= 0.0 {
            push("seeding.ratio_limit", "must be greater than zero");
        }
        if self.seeding.upload_slots_per_torrent == 0 {
            push("seeding.upload_slots_per_torrent", "must be at least 1");
        }
        let blocklist_url = self.privacy.blocklist_url.trim();
        if self.privacy.blocklist_enabled
            && !(blocklist_url.starts_with("https://") || blocklist_url.starts_with("http://"))
        {
            push(
                "privacy.blocklist_url",
                "blocklist URL must use HTTP or HTTPS",
            );
        }
        if self.storage.move_completed && self.storage.completed_directory.trim().is_empty() {
            push(
                "storage.completed_directory",
                "completed directory cannot be empty",
            );
        }
        if self.automation.watch_directory_enabled
            && self.automation.watch_directory.trim().is_empty()
        {
            push(
                "automation.watch_directory",
                "watch directory cannot be empty",
            );
        }
        if self.automation.backup_torrents_enabled
            && self.automation.backup_directory.trim().is_empty()
        {
            push(
                "automation.backup_directory",
                "backup directory cannot be empty",
            );
        }
        issues
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct LegacySettings {
    download_directory: String,
    start_when_added: bool,
    max_active_downloads: usize,
    max_peer_connections: usize,
    download_limit_kib: u64,
    upload_limit_kib: u64,
    dht: bool,
    pex: bool,
    lsd: bool,
    piece_strategy: PieceStrategy,
    encryption: EncryptionMode,
    blocklist_enabled: bool,
    blocklist_url: String,
    blocklist_auto_update: bool,
}

impl LegacySettings {
    fn into_current(self) -> ClientSettings {
        let mut settings = ClientSettings::default();
        if !self.download_directory.is_empty() {
            settings.download.download_directory = self.download_directory;
        }
        settings.download.start_when_added = self.start_when_added;
        if self.max_active_downloads > 0 {
            settings.queue.max_active_downloads = self.max_active_downloads;
        }
        if self.max_peer_connections > 0 {
            settings.connection.max_connections_global = self.max_peer_connections;
            settings.connection.max_connections_per_torrent = settings
                .connection
                .max_connections_per_torrent
                .min(self.max_peer_connections);
        }
        settings.speed.download_limit_kib = self.download_limit_kib;
        settings.speed.upload_limit_kib = self.upload_limit_kib;
        settings.bittorrent.dht = self.dht;
        settings.bittorrent.pex = self.pex;
        settings.bittorrent.lsd = self.lsd;
        settings.bittorrent.piece_strategy = self.piece_strategy;
        settings.privacy.encryption = self.encryption;
        settings.privacy.blocklist_enabled = self.blocklist_enabled;
        if !self.blocklist_url.is_empty() {
            settings.privacy.blocklist_url = self.blocklist_url;
        }
        settings.privacy.blocklist_auto_update = self.blocklist_auto_update;
        settings
    }
}

pub fn data_directory() -> PathBuf {
    if let Some(directory) = PROFILE_DIRECTORY.get() {
        return directory.clone();
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("PeerLume")
}

static PROFILE_DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Explicit installed-profile override, set once before starting application workers.
pub fn configure_profile_from_args() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--profile") {
        let directory = args.get(index + 1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--profile requires a directory",
            )
        })?;
        let path = PathBuf::from(directory);
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Profile directory must be absolute",
            ));
        }
        PROFILE_DIRECTORY
            .set(path)
            .map_err(|_| io::Error::other("Profile is already configured"))?;
    }
    Ok(())
}

fn settings_path() -> PathBuf {
    data_directory().join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_settings_round_trip_through_toml() {
        let mut settings = ClientSettings::default();
        settings.queue.max_active_downloads = 7;
        settings.connection.listen_port = 60000;
        settings.privacy.tracker_reporting = TrackerReportingMode::Disabled;
        settings.automation.watch_directory_enabled = true;
        settings.automation.watch_directory = "watch".into();

        let encoded = toml::to_string_pretty(&settings).unwrap();
        let decoded = ClientSettings::from_toml(&encoded).unwrap();
        assert_eq!(decoded, settings);
        assert_eq!(
            decoded.privacy.tracker_reporting,
            TrackerReportingMode::Disabled
        );
        assert!(decoded.validate().is_empty());
    }

    #[test]
    fn migrates_flat_v1_settings() {
        let legacy = r#"
download_directory = "D:/data"
start_when_added = false
max_active_downloads = 9
max_peer_connections = 120
download_limit_kib = 5000
upload_limit_kib = 500
dht = false
pex = true
lsd = false
piece_strategy = "sequential"
encryption = "require"
blocklist_enabled = true
blocklist_url = "https://example.test/list.gz"
blocklist_auto_update = false
"#;
        let migrated = ClientSettings::from_toml(legacy).unwrap();
        assert_eq!(migrated.version, CONFIG_VERSION);
        assert_eq!(migrated.download.download_directory, "D:/data");
        assert_eq!(migrated.queue.max_active_downloads, 9);
        assert_eq!(
            migrated.bittorrent.piece_strategy,
            PieceStrategy::Sequential
        );
        assert_eq!(migrated.privacy.encryption, EncryptionMode::Require);
    }

    #[test]
    fn validation_rejects_conflicting_and_empty_values() {
        let mut settings = ClientSettings::default();
        settings.download.download_directory.clear();
        settings.connection.tcp_enabled = false;
        settings.connection.utp_enabled = false;
        settings.connection.max_connections_per_torrent = 500;
        settings.bittorrent.peer_id_prefix = "too-long-prefix".into();
        settings.privacy.blocklist_url = "file:///list.gz".into();
        let fields: Vec<_> = settings
            .validate()
            .into_iter()
            .map(|issue| issue.field)
            .collect();
        assert!(fields.contains(&"download.download_directory"));
        assert!(fields.contains(&"connection.transports"));
        assert!(fields.contains(&"connection.max_connections_per_torrent"));
        assert!(fields.contains(&"bittorrent.peer_id_prefix"));
        assert!(fields.contains(&"privacy.blocklist_url"));
    }
}
