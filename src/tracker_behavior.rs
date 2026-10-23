//! Shared, externally editable tracker audit presets. Resolving a preset performs
//! no network operations; applying it creates a per-torrent snapshot.

use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    settings::data_directory,
    state::{StoredTrackerAuditProfile, StoredTrackerAuditStep},
};

const TEMPLATE: &str = include_str!("../config/tracker-behaviors.example.toml");
const MAX_SECONDS: u64 = 604_800;
const MAX_STEPS: usize = 1024;
const MAX_FILE_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorMode {
    #[default]
    Manual,
    StartedOnly,
    Scenario,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeValue {
    FullSize,
    SelectedSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetricValue {
    Bytes(u64),
    Size(SizeValue),
}

impl Default for MetricValue {
    fn default() -> Self {
        Self::Bytes(0)
    }
}

impl MetricValue {
    fn resolve(self, full_size: u64, selected_size: u64) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Size(SizeValue::FullSize) => full_size,
            Self::Size(SizeValue::SelectedSize) => selected_size,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorEvent {
    #[default]
    None,
    Started,
    Completed,
    Stopped,
}

impl BehaviorEvent {
    fn stored(self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Started => Some("started".into()),
            Self::Completed => Some("completed".into()),
            Self::Stopped => Some("stopped".into()),
        }
    }
}

fn one() -> usize {
    1
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorStep {
    #[serde(default)]
    pub delay_seconds: u64,
    #[serde(default = "one")]
    pub repeat: usize,
    #[serde(default)]
    pub uploaded: MetricValue,
    #[serde(default)]
    pub downloaded: MetricValue,
    pub left: MetricValue,
    #[serde(default)]
    pub event: BehaviorEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorProfile {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub mode: BehaviorMode,
    #[serde(default)]
    pub allowed_host: String,
    pub port: Option<u16>,
    #[serde(default)]
    pub observation_seconds: u64,
    #[serde(default)]
    pub steps: Vec<BehaviorStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorCatalog {
    pub version: u32,
    #[serde(default = "default_download_modes")]
    pub download_modes: Vec<crate::download_mode::DownloadMode>,
    pub profiles: Vec<BehaviorProfile>,
}

fn default_download_modes() -> Vec<crate::download_mode::DownloadMode> {
    crate::download_mode::DownloadMode::ALL.to_vec()
}

impl BehaviorCatalog {
    pub fn from_toml(contents: &str) -> Result<Self, String> {
        let mut catalog: Self = toml::from_str(contents).map_err(|error| error.to_string())?;
        // Upgrade only the exact legacy built-in list; preserve custom subsets/order.
        if catalog.download_modes == crate::download_mode::DownloadMode::ALL[..3] {
            catalog
                .download_modes
                .extend_from_slice(&crate::download_mode::DownloadMode::ALL[3..]);
        }
        catalog.validate()?;
        Ok(catalog)
    }

    /// Uses an explicit override when provided. Otherwise initializes the default
    /// file once, without overwriting existing user configuration.
    pub fn load() -> Result<Self, String> {
        if let Some(path) = std::env::var_os("PEERLUME_TRACKER_BEHAVIORS") {
            return Self::load_from(Path::new(&path));
        }
        let path = data_directory().join("tracker-behaviors.toml");
        if !path.exists() {
            fs::create_dir_all(path.parent().expect("data directory"))
                .map_err(|error| error.to_string())?;
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => file
                    .write_all(TEMPLATE.as_bytes())
                    .map_err(|error| error.to_string())?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        // Bound file input before allocating/parsing it.
        use std::io::Read;
        let file = fs::File::open(path).map_err(|error| error.to_string())?;
        let mut contents = String::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut contents)
            .map_err(|error| error.to_string())?;
        if contents.len() as u64 > MAX_FILE_BYTES {
            return Err("Tracker behavior file exceeds 1 MiB".into());
        }
        Self::from_toml(&contents)
    }

    pub fn resolve(
        &self,
        id: &str,
        full_size: u64,
        selected_size: u64,
        fallback_port: u16,
    ) -> Result<StoredTrackerAuditProfile, String> {
        self.validate()?;
        if selected_size > full_size || full_size > i64::MAX as u64 {
            return Err("Invalid torrent byte sizes".into());
        }
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| format!("Unknown tracker behavior profile: {id}"))?;
        let port = profile.port.unwrap_or(fallback_port);
        if port == 0 {
            return Err("Announced port must be greater than zero".into());
        }
        let started_only = profile.mode == BehaviorMode::StartedOnly;
        let steps = if started_only {
            vec![StoredTrackerAuditStep {
                delay_seconds: 0,
                uploaded: 0,
                downloaded: 0,
                left: full_size,
                event: Some("started".into()),
            }]
        } else {
            profile
                .steps
                .iter()
                .flat_map(|step| {
                    (0..step.repeat).map(move |_| StoredTrackerAuditStep {
                        delay_seconds: step.delay_seconds,
                        uploaded: step.uploaded.resolve(full_size, selected_size),
                        downloaded: step.downloaded.resolve(full_size, selected_size),
                        left: step.left.resolve(full_size, selected_size),
                        event: step.event.stored(),
                    })
                })
                .collect()
        };
        Ok(StoredTrackerAuditProfile {
            allowed_host: profile.allowed_host.clone(),
            port,
            started_only,
            observation_seconds: profile.observation_seconds,
            steps,
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.download_modes.first() != Some(&crate::download_mode::DownloadMode::Normal)
            || self.download_modes.len() > crate::download_mode::DownloadMode::ALL.len()
            || self
                .download_modes
                .iter()
                .enumerate()
                .any(|(index, mode)| self.download_modes[..index].contains(mode))
        {
            return Err(
                "download_modes must start with normal and contain no duplicate modes".into(),
            );
        }
        if self.version != 1 || self.profiles.is_empty() || self.profiles.len() > 128 {
            return Err("Expected behavior version=1 and 1..=128 profiles".into());
        }
        let mut ids = HashSet::new();
        for profile in &self.profiles {
            let invalid = |message: &str| format!("Profile '{}': {message}", profile.id);
            if profile.id.is_empty()
                || !profile
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
                || !ids.insert(&profile.id)
                || profile.label.trim().is_empty()
            {
                return Err(invalid(
                    "id must be unique ASCII letters/digits/-/_ and label must not be empty",
                ));
            }
            if profile.port == Some(0) || profile.observation_seconds > MAX_SECONDS {
                return Err(invalid(
                    "port must be nonzero; observation cannot exceed seven days",
                ));
            }
            if !profile.allowed_host.is_empty()
                && (profile.allowed_host.trim() != profile.allowed_host
                    || url::Host::parse(&profile.allowed_host).is_err()
                    || profile.allowed_host.contains(['/', '?', '#', '@']))
            {
                return Err(invalid(
                    "allowed_host must be an exact hostname or IP, without URL/path/port",
                ));
            }
            if profile.mode == BehaviorMode::StartedOnly {
                if !profile.steps.is_empty() || profile.observation_seconds < 60 {
                    return Err(invalid(
                        "started_only requires no explicit steps and observation_seconds in 60..=604800",
                    ));
                }
                continue;
            }
            if profile.observation_seconds != 0 {
                return Err(invalid(
                    "observation_seconds applies only to started_only mode",
                ));
            }
            if profile.steps.is_empty()
                || (profile.mode == BehaviorMode::Manual
                    && (profile.steps.len() != 1 || profile.steps[0].repeat != 1))
            {
                return Err(invalid(
                    "scenario needs steps; manual needs exactly one non-repeating step",
                ));
            }
            let mut count = 0usize;
            let mut duration = profile.observation_seconds;
            for step in &profile.steps {
                if step.repeat == 0 || step.repeat > MAX_STEPS || step.delay_seconds > MAX_SECONDS {
                    return Err(invalid(
                        "repeat must be 1..=1024; delay cannot exceed seven days",
                    ));
                }
                count = count.saturating_add(step.repeat);
                duration =
                    duration.saturating_add(step.delay_seconds.saturating_mul(step.repeat as u64));
                if count > MAX_STEPS || duration > MAX_SECONDS {
                    return Err(invalid(
                        "expanded scenario exceeds 1024 steps or seven days",
                    ));
                }
                for value in [step.uploaded, step.downloaded, step.left] {
                    if matches!(value, MetricValue::Bytes(bytes) if bytes > i64::MAX as u64) {
                        return Err(invalid("metric exceeds SQLite integer range"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Shared cancellable wait used by both frontends' scenario runners.
pub fn wait_for_delay(seconds: u64, cancel: &AtomicBool) -> bool {
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(seconds)) else {
        return false;
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_legacy_mode_list_is_extended_without_changing_custom_subsets() {
        let current = "download_modes = [\"normal\", \"no_progress\", \"upload_ratio_1_5\", \"hide_downloaded\", \"freeze_after\"]";
        let old = TEMPLATE.replace(
            current,
            "download_modes = ['normal','no_progress','upload_ratio_1_5']",
        );
        assert_eq!(
            BehaviorCatalog::from_toml(&old).unwrap().download_modes,
            crate::download_mode::DownloadMode::ALL
        );
        let custom = TEMPLATE.replace(current, "download_modes = ['normal','hide_downloaded']");
        assert_eq!(
            BehaviorCatalog::from_toml(&custom)
                .unwrap()
                .download_modes
                .len(),
            2
        );
    }

    #[test]
    fn bundled_profiles_resolve_dynamic_sizes_and_repeat_delays() {
        let catalog = BehaviorCatalog::from_toml(TEMPLATE).unwrap();
        assert_eq!(
            catalog.download_modes,
            crate::download_mode::DownloadMode::ALL
        );
        let started = catalog.resolve("started-only", 1000, 100, 51413).unwrap();
        assert!(started.started_only);
        assert_eq!(started.steps[0].left, 1000);
        assert_eq!(started.steps[0].uploaded, 0);
        assert_eq!(started.steps[0].downloaded, 0);
        let scenario = catalog.resolve("periodic-test", 1000, 100, 51413).unwrap();
        assert_eq!(scenario.steps.len(), 4);
        assert_eq!(scenario.steps[1].event, None);
        assert_eq!(scenario.steps[2].delay_seconds, 1800);
        assert_eq!(scenario.steps[3].event.as_deref(), Some("stopped"));
        assert!(catalog.resolve("missing", 1000, 100, 51413).is_err());
    }

    #[test]
    fn rejects_typos_unknown_modes_duplicate_ids_and_invalid_limits() {
        for contents in [
            TEMPLATE.replace("mode = \"manual\"", "mode = \"typo\""),
            TEMPLATE.replace("uploaded = 0", "uploded = 0"),
            TEMPLATE.replace("id = \"started-only\"", "id = \"manual\""),
            TEMPLATE.replace("repeat = 2", "repeat = 0"),
            TEMPLATE.replace("repeat = 2", "repeat = 1025"),
            TEMPLATE.replace("delay_seconds = 1800", "delay_seconds = 604801"),
            TEMPLATE.replace("observation_seconds = 14400", "observation_seconds = 1"),
            TEMPLATE.replace("version = 1", "version = 2"),
            TEMPLATE.replace(
                "allowed_host = \"\"",
                "allowed_host = \"https://tracker.test/path\"",
            ),
            TEMPLATE.replace("label = \"Manual audit / normal transfer\"", "label = \"\""),
            TEMPLATE.replace("repeat = 2", "repeat = 1024"),
            TEMPLATE.replace("left = \"full_size\"", "left = -1"),
            TEMPLATE.replace("left = \"full_size\"", "left = \"typo\""),
        ] {
            assert!(BehaviorCatalog::from_toml(&contents).is_err(), "{contents}");
        }
    }

    #[test]
    fn resolved_snapshot_round_trips_through_sqlite() {
        let catalog = BehaviorCatalog::from_toml(
            &TEMPLATE.replace("left = \"full_size\"", "left = \"selected_size\""),
        )
        .unwrap();
        let profile = catalog.resolve("manual", 4096, 512, 1234).unwrap();
        assert_eq!(profile.steps[0].left, 512);
        let mut store = crate::state::StateStore::in_memory().unwrap();
        let record = crate::state::TorrentRecord {
            verify_after_download: false,
            download_mode: Default::default(),
            freeze_after_bytes: 0,
            id: "v1: abc".into(),
            name: "test".into(),
            source: "test.torrent".into(),
            info_hash_v1: Some("abc".into()),
            info_hash_v2: None,
            is_private: false,
            state: "paused".into(),
            queue_position: 0,
            download_directory: None,
            trackers: Vec::new(),
            files: Vec::new(),
            verified_pieces: Vec::new(),
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            tracker_audit: Some(profile.clone()),
        };
        store.save_torrent(&record).unwrap();
        assert_eq!(
            store.load_all_torrents().unwrap()[0].tracker_audit,
            Some(profile)
        );
    }

    #[test]
    fn per_torrent_resolution_is_independent_and_checks_port_and_sizes() {
        let catalog = BehaviorCatalog::from_toml(TEMPLATE).unwrap();
        let first = catalog.resolve("manual", 1000, 100, 1234).unwrap();
        let second = catalog.resolve("manual", 2000, 200, 4321).unwrap();
        assert_eq!(first.steps[0].left, 1000);
        assert_eq!(second.steps[0].left, 2000);
        assert_eq!(first.port, 1234);
        assert_eq!(second.port, 4321);
        assert!(catalog.resolve("manual", 10, 11, 1234).is_err());
        assert!(catalog.resolve("manual", 10, 10, 0).is_err());
        let custom = BehaviorCatalog::from_toml(
            &TEMPLATE.replace("mode = \"manual\"", "mode = \"manual\"\nport = 9000"),
        )
        .unwrap();
        assert_eq!(custom.resolve("manual", 10, 5, 1234).unwrap().port, 9000);
    }

    #[test]
    fn reloading_external_file_does_not_change_existing_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "peerlume-behavior-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("profiles.toml");
        fs::write(&path, TEMPLATE).unwrap();
        let snapshot = BehaviorCatalog::load_from(&path)
            .unwrap()
            .resolve("manual", 100, 50, 1234)
            .unwrap();
        fs::write(&path, TEMPLATE.replace("uploaded = 0", "uploaded = 77")).unwrap();
        let updated = BehaviorCatalog::load_from(&path)
            .unwrap()
            .resolve("manual", 100, 50, 1234)
            .unwrap();
        assert_eq!(snapshot.steps[0].uploaded, 0);
        assert_eq!(updated.steps[0].uploaded, 77);
        fs::write(&path, "invalid TOML = [").unwrap();
        assert!(BehaviorCatalog::load_from(&path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn cancellable_delay_handles_immediate_and_pre_cancelled_runs() {
        assert!(wait_for_delay(0, &AtomicBool::new(false)));
        assert!(!wait_for_delay(14400, &AtomicBool::new(true)));
    }
}
