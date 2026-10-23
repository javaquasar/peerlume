//! UI-independent access to persistent state and the transfer session.
//!
//! Frontends (egui today, ratatui or another UI later) own presentation state,
//! while this type owns the infrastructure shared by every frontend.

use std::path::PathBuf;

use crate::{
    file_selection::FileSelectionModel,
    metainfo,
    session::{SessionCommand, SessionEvent, SessionRuntime},
    settings::{ClientSettings, TrackerReportingMode, data_directory},
    state::{StateStore, StoredFile, StoredTrackerAuditProfile, TorrentRecord},
    torrent::{EffectiveTorrentSettings, TorrentItem, TorrentOverrides, TorrentState},
};

pub struct ClientBackend {
    state_store: Option<StateStore>,
    session_runtime: Option<SessionRuntime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionNotice {
    None,
    Added { id: String },
    SettingsApplied { id: String },
    Error { id: String, message: String },
    Removed { id: String },
    Stopped,
}

pub fn apply_session_event(torrents: &mut [TorrentItem], event: SessionEvent) -> SessionNotice {
    match event {
        SessionEvent::Mode { id, application } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                torrent.runtime.mode = application.clone();
                match application {
                    crate::runtime_status::ModeApplication::Active { config, .. } => {
                        torrent.overrides.download_mode = config.mode;
                        torrent.overrides.freeze_after_bytes = config.freeze_after_bytes;
                        torrent.runtime.mode_draft = None;
                        return SessionNotice::SettingsApplied { id };
                    }
                    crate::runtime_status::ModeApplication::Failed { message, .. } => {
                        return SessionNotice::Error { id, message };
                    }
                    _ => {}
                }
            }
            SessionNotice::None
        }
        SessionEvent::RepairPolicy { id, policy } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                torrent.overrides.repair_policy = policy;
                if policy.enabled {
                    torrent.overrides.verify_after_download = true;
                }
            }
            SessionNotice::SettingsApplied { id }
        }
        SessionEvent::Repair { id, status } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                torrent.runtime.repair = status;
            }
            SessionNotice::None
        }
        SessionEvent::NetworkTotals {
            id,
            uploaded,
            downloaded,
        } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                torrent.runtime.network_uploaded = uploaded;
                torrent.runtime.network_downloaded = downloaded;
            }
            SessionNotice::None
        }
        SessionEvent::Verification { id, status } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                match &status {
                    crate::completion::CompletionStatus::Checking { .. } => {
                        torrent.state = TorrentState::Checking;
                        torrent.download_rate = 0;
                        torrent.upload_rate = 0;
                    }
                    crate::completion::CompletionStatus::Failed { message } => {
                        torrent.state = TorrentState::VerificationFailed;
                        torrent.completion = status.clone();
                        return SessionNotice::Error {
                            id,
                            message: message.clone(),
                        };
                    }
                    _ => {}
                }
                torrent.completion = status;
            }
            SessionNotice::None
        }
        SessionEvent::Added { id } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id)
                && torrent.state == TorrentState::WaitingForEngine
            {
                torrent.state = TorrentState::Downloading;
            }
            SessionNotice::Added { id }
        }
        SessionEvent::StateChanged { id, state } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                if state != "paused" {
                    torrent.completion = crate::completion::CompletionStatus::Pending;
                }
                torrent.state = if state == "paused" {
                    TorrentState::Paused
                } else {
                    TorrentState::Downloading
                };
            }
            SessionNotice::None
        }
        SessionEvent::Stats {
            id,
            downloaded_bytes,
            uploaded_bytes,
            download_rate,
            upload_rate,
            file_progress,
            peers,
            finished,
            paused,
        } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                if matches!(
                    torrent.completion,
                    crate::completion::CompletionStatus::Checking { .. }
                        | crate::completion::CompletionStatus::Failed { .. }
                ) {
                    return SessionNotice::None;
                }
                for (index, progress) in file_progress.into_iter().enumerate() {
                    torrent.files.set_file_downloaded(index, progress);
                }
                torrent.downloaded_bytes = if torrent.files.files().is_empty() {
                    downloaded_bytes
                } else {
                    torrent.files.selected_downloaded()
                };
                torrent.uploaded_bytes = uploaded_bytes;
                torrent.download_rate = download_rate;
                torrent.upload_rate = upload_rate;
                torrent.peers = peers;
                if finished && torrent.selected_size() > 0 {
                    torrent.completion = crate::completion::CompletionStatus::Verified;
                } else {
                    torrent.completion = crate::completion::CompletionStatus::Pending;
                }
                torrent.state = if paused {
                    TorrentState::Paused
                } else if finished {
                    TorrentState::Seeding
                } else {
                    TorrentState::Downloading
                };
            }
            SessionNotice::None
        }
        SessionEvent::Error { id, message } => {
            if let Some(torrent) = find_torrent_mut(torrents, &id) {
                torrent.completion = crate::completion::CompletionStatus::Pending;
                torrent.state = TorrentState::Error;
                torrent.download_rate = 0;
                torrent.upload_rate = 0;
            }
            SessionNotice::Error { id, message }
        }
        SessionEvent::Removed { id } => SessionNotice::Removed { id },
        SessionEvent::Stopped => SessionNotice::Stopped,
    }
}

fn find_torrent_mut<'a>(torrents: &'a mut [TorrentItem], id: &str) -> Option<&'a mut TorrentItem> {
    torrents.iter_mut().find(|torrent| {
        torrent.info_hashes.join(";") == id || torrent.info_hashes.iter().any(|hash| hash == id)
    })
}

pub fn file_selection_command(torrent: &TorrentItem) -> SessionCommand {
    SessionCommand::SetFiles {
        id: torrent.info_hashes.join(";"),
        only_files: torrent
            .files
            .files()
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.selected.then_some(i))
            .collect(),
    }
}

pub fn add_torrent_command(torrent: &TorrentItem, settings: &ClientSettings) -> SessionCommand {
    let effective =
        EffectiveTorrentSettings::resolve(settings, &torrent.overrides, torrent.is_private);
    let selected: Vec<_> = torrent
        .files
        .files()
        .iter()
        .enumerate()
        .filter_map(|(index, file)| file.selected.then_some(index))
        .collect();
    let only_files = (selected.len() != torrent.files.files().len()).then_some(selected);
    SessionCommand::AddTorrent {
        repair_policy: torrent.overrides.repair_policy,
        verify_after_download: torrent.overrides.verify_after_download
            || torrent.overrides.repair_policy.enabled,
        download_mode: torrent.overrides.download_mode,
        freeze_after_bytes: if torrent.overrides.freeze_after_bytes == 0 {
            crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES
        } else {
            torrent.overrides.freeze_after_bytes
        },
        full_size: torrent
            .files
            .files()
            .iter()
            .map(|file| file.size_bytes)
            .sum(),
        id: torrent.info_hashes.join(";"),
        source: torrent.source.clone(),
        output_folder: effective.download_directory,
        only_files,
        trackers: torrent.trackers.clone(),
        paused: matches!(
            torrent.state,
            TorrentState::Paused | TorrentState::Error | TorrentState::VerificationFailed
        ),
        peer_limit: (effective.max_peers > 0).then_some(effective.max_peers),
    }
}

pub fn torrent_to_record(
    torrent: &TorrentItem,
    tracker_audit: Option<StoredTrackerAuditProfile>,
) -> TorrentRecord {
    let hash = |prefix: &str| {
        torrent
            .info_hashes
            .iter()
            .find_map(|value| value.strip_prefix(prefix).map(str::to_owned))
    };
    TorrentRecord {
        repair_policy: torrent.overrides.repair_policy,
        verify_after_download: torrent.overrides.verify_after_download
            || torrent.overrides.repair_policy.enabled,
        download_mode: torrent.overrides.download_mode,
        freeze_after_bytes: torrent.overrides.freeze_after_bytes,
        id: torrent.info_hashes.join(";"),
        name: torrent.name.clone(),
        source: torrent.source.clone(),
        info_hash_v1: hash("v1: "),
        info_hash_v2: hash("v2: "),
        is_private: torrent.is_private,
        state: torrent.state.label().to_ascii_lowercase(),
        queue_position: 0,
        download_directory: torrent.overrides.download_directory.clone(),
        trackers: torrent.trackers.clone(),
        files: torrent
            .files
            .files()
            .iter()
            .enumerate()
            .map(|(index, file)| StoredFile {
                index: index as u32,
                path: file.path.clone(),
                size_bytes: file.size_bytes,
                downloaded_bytes: file.downloaded_bytes,
                selected: file.selected,
                priority: 0,
            })
            .collect(),
        verified_pieces: Vec::new(),
        downloaded_bytes: torrent.downloaded_bytes,
        uploaded_bytes: torrent.uploaded_bytes,
        tracker_audit,
    }
}

/// A draft becomes a saved setting only after the engine acknowledges it.
pub fn mode_change_command(
    torrent: &mut TorrentItem,
    config: crate::runtime_status::ModeConfig,
) -> Result<SessionCommand, String> {
    config.mode.validate_threshold(config.freeze_after_bytes)?;
    config.mode.validate_metadata(
        torrent.info_hashes.iter().any(|h| h.starts_with("v1: ")),
        torrent.files.files().iter().map(|f| f.size_bytes).sum(),
    )?;
    torrent.runtime.mode = crate::runtime_status::ModeApplication::Applying {
        requested: config,
        active: torrent.runtime.mode.active(),
    };
    Ok(SessionCommand::SetDownloadMode {
        id: torrent.info_hashes.join(";"),
        config,
    })
}

pub fn mode_submission_failed(
    torrent: &mut TorrentItem,
    config: crate::runtime_status::ModeConfig,
    message: String,
) {
    torrent.runtime.mode = crate::runtime_status::ModeApplication::Failed {
        requested: config,
        active: torrent.runtime.mode.active(),
        message,
    };
}

pub fn torrent_from_record(record: TorrentRecord, settings: &ClientSettings) -> TorrentItem {
    let files = FileSelectionModel::from_snapshot(record.files.into_iter().map(|file| {
        (
            file.path,
            file.size_bytes,
            file.downloaded_bytes,
            file.selected,
        )
    }));
    let downloaded_bytes = files.selected_downloaded();
    let mut info_hashes = Vec::new();
    if let Some(hash) = record.info_hash_v1 {
        info_hashes.push(format!("v1: {hash}"));
    }
    if let Some(hash) = record.info_hash_v2 {
        info_hashes.push(format!("v2: {hash}"));
    }
    TorrentItem {
        runtime: Default::default(),
        completion: crate::completion::CompletionStatus::Pending,
        name: record.name,
        source: record.source,
        info_hashes,
        trackers: record.trackers,
        peer_id: metainfo::generate_peer_id(&settings.bittorrent.peer_id_prefix),
        files,
        downloaded_bytes,
        uploaded_bytes: record.uploaded_bytes,
        download_rate: 0,
        upload_rate: 0,
        peers: 0,
        seeds: 0,
        state: match record.state.as_str() {
            "seeding" => TorrentState::Seeding,
            "downloading" | "waiting for transfer engine" => TorrentState::WaitingForEngine,
            "checking" => TorrentState::WaitingForEngine,
            "verification failed" => TorrentState::VerificationFailed,
            "error" => TorrentState::Error,
            _ => TorrentState::Paused,
        },
        is_private: record.is_private,
        overrides: TorrentOverrides {
            verify_after_download: record.verify_after_download || record.repair_policy.enabled,
            repair_policy: record.repair_policy,
            download_mode: record.download_mode,
            freeze_after_bytes: record.freeze_after_bytes,
            download_directory: record.download_directory,
            ..Default::default()
        },
    }
}

impl ClientBackend {
    pub fn journal(&self) -> Option<&crate::announce_journal::AnnounceJournal> {
        self.session_runtime.as_ref().map(|s| &s.journal)
    }
    pub fn offline() -> Self {
        Self {
            state_store: None,
            session_runtime: None,
        }
    }

    pub fn start(settings: &ClientSettings) -> Self {
        Self {
            state_store: StateStore::open(&data_directory().join("state.sqlite3")).ok(),
            session_runtime: SessionRuntime::start(
                256,
                PathBuf::from(&settings.download.download_directory),
                settings.privacy.tracker_reporting == TrackerReportingMode::Disabled,
            )
            .ok(),
        }
    }

    pub fn load_torrents(&self) -> Result<Vec<TorrentRecord>, String> {
        self.state_store.as_ref().map_or_else(
            || Ok(Vec::new()),
            |store| store.load_all_torrents().map_err(|error| error.to_string()),
        )
    }

    pub fn save_torrent(&mut self, torrent: &TorrentRecord) -> Result<(), String> {
        self.state_store.as_mut().map_or_else(
            || Ok(()),
            |store| {
                store
                    .save_torrent(torrent)
                    .map_err(|error| error.to_string())
            },
        )
    }

    pub fn remove_torrent(&self, id: &str) -> Result<(), String> {
        self.state_store.as_ref().map_or_else(
            || Ok(()),
            |store| {
                store
                    .remove_torrent(id)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )
    }

    pub fn send(&self, command: SessionCommand) -> Result<(), String> {
        self.session_runtime
            .as_ref()
            .ok_or_else(|| "Transfer engine is unavailable".to_owned())?
            .try_send(command)
    }

    pub fn try_recv(&self) -> Option<SessionEvent> {
        self.session_runtime
            .as_ref()
            .and_then(SessionRuntime::try_recv)
    }

    pub fn session_available(&self) -> bool {
        self.session_runtime.is_some()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn completion_stats(finished: bool, paused: bool) -> SessionEvent {
        SessionEvent::Stats {
            id: "v1: abc".into(),
            downloaded_bytes: 100,
            uploaded_bytes: 7,
            download_rate: 0,
            upload_rate: 0,
            file_progress: vec![100],
            peers: 0,
            finished,
            paused,
        }
    }

    #[test]
    fn done_is_engine_confirmed_and_checking_has_its_own_progress() {
        let mut torrents = vec![torrent()];
        apply_session_event(&mut torrents, completion_stats(false, false));
        assert_eq!(torrents[0].progress(), 1.0);
        assert!(!torrents[0].completed_successfully());
        apply_session_event(
            &mut torrents,
            SessionEvent::Verification {
                id: "v1: abc".into(),
                status: crate::completion::CompletionStatus::Checking {
                    checked_bytes: 25,
                    total_bytes: 100,
                },
            },
        );
        assert_eq!(torrents[0].display_progress(), 0.25);
        assert_eq!(torrents[0].progress_text(), "Checking — 25%");
        apply_session_event(&mut torrents, completion_stats(true, false));
        assert!(
            !torrents[0].completed_successfully(),
            "stale completion cannot interrupt a check"
        );
        apply_session_event(
            &mut torrents,
            SessionEvent::Verification {
                id: "v1: abc".into(),
                status: crate::completion::CompletionStatus::Verified,
            },
        );
        apply_session_event(&mut torrents, completion_stats(true, false));
        assert!(torrents[0].completed_successfully());
        assert_eq!(torrents[0].status_text(), "Done · Seeding");
        apply_session_event(&mut torrents, completion_stats(true, true));
        assert_eq!(torrents[0].status_text(), "Done · Paused");
    }

    #[test]
    fn failed_verification_never_turns_green_from_stale_stats() {
        let mut torrents = vec![torrent()];
        let status = crate::completion::CompletionStatus::Failed {
            message: "piece hash mismatch".into(),
        };
        apply_session_event(
            &mut torrents,
            SessionEvent::Verification {
                id: "v1: abc".into(),
                status,
            },
        );
        apply_session_event(&mut torrents, completion_stats(true, false));
        assert!(!torrents[0].completed_successfully());
        assert_eq!(torrents[0].progress_text(), "Verification failed");
        assert_eq!(torrents[0].state, TorrentState::VerificationFailed);
    }

    pub(crate) fn torrent() -> TorrentItem {
        TorrentItem {
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
            name: "test".into(),
            source: "test.torrent".into(),
            info_hashes: vec!["v1: abc".into()],
            trackers: Vec::new(),
            peer_id: "-PL0001-123456789012".into(),
            files: FileSelectionModel::from_files([("file.bin".into(), 100)]),
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_rate: 0,
            upload_rate: 0,
            peers: 0,
            seeds: 0,
            state: TorrentState::WaitingForEngine,
            is_private: false,
            overrides: TorrentOverrides::default(),
        }
    }

    #[test]
    fn unavailable_optional_services_have_frontend_friendly_semantics() {
        let mut backend = ClientBackend::offline();
        let record = TorrentRecord {
            repair_policy: Default::default(),
            verify_after_download: false,
            download_mode: Default::default(),
            freeze_after_bytes: 0,
            id: "test".into(),
            name: "test".into(),
            source: "test.torrent".into(),
            info_hash_v1: None,
            info_hash_v2: None,
            is_private: false,
            trackers: Vec::new(),
            files: Vec::new(),
            state: "paused".into(),
            queue_position: 0,
            download_directory: None,
            verified_pieces: Vec::new(),
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            tracker_audit: None,
        };

        assert!(backend.load_torrents().unwrap().is_empty());
        assert!(backend.save_torrent(&record).is_ok());
        assert!(backend.remove_torrent("test").is_ok());
        assert!(!backend.session_available());
        assert!(backend.send(SessionCommand::Shutdown).is_err());
    }

    #[test]
    fn shared_event_reducer_updates_every_frontend_model() {
        let mut torrents = vec![torrent()];
        let id = "v1: abc".to_owned();

        assert_eq!(
            apply_session_event(&mut torrents, SessionEvent::Added { id: id.clone() }),
            SessionNotice::Added { id: id.clone() }
        );
        assert_eq!(torrents[0].state, TorrentState::Downloading);

        assert_eq!(
            apply_session_event(
                &mut torrents,
                SessionEvent::Stats {
                    id: id.clone(),
                    downloaded_bytes: 40,
                    uploaded_bytes: 7,
                    download_rate: 20,
                    upload_rate: 3,
                    file_progress: vec![40],
                    peers: 5,
                    finished: false,
                    paused: false,
                }
            ),
            SessionNotice::None
        );
        assert_eq!(torrents[0].downloaded_bytes, 40);
        assert_eq!(torrents[0].uploaded_bytes, 7);
        assert_eq!(torrents[0].peers, 5);

        let message = "network".to_owned();
        assert_eq!(
            apply_session_event(
                &mut torrents,
                SessionEvent::Error {
                    id: id.clone(),
                    message: message.clone(),
                }
            ),
            SessionNotice::Error { id, message }
        );
        assert_eq!(torrents[0].state, TorrentState::Error);
        assert_eq!(torrents[0].download_rate, 0);

        assert_eq!(
            apply_session_event(
                &mut torrents,
                SessionEvent::StateChanged {
                    id: "v1: abc".into(),
                    state: "paused",
                }
            ),
            SessionNotice::None
        );
        assert_eq!(torrents[0].state, TorrentState::Paused);

        assert_eq!(
            apply_session_event(
                &mut torrents,
                SessionEvent::Removed {
                    id: "v1: abc".into(),
                }
            ),
            SessionNotice::Removed {
                id: "v1: abc".into(),
            }
        );
        assert_eq!(
            apply_session_event(&mut torrents, SessionEvent::Stopped),
            SessionNotice::Stopped
        );
    }

    #[test]
    fn checked_complete_files_do_not_override_the_paused_state() {
        let mut torrents = vec![torrent()];
        apply_session_event(
            &mut torrents,
            SessionEvent::Stats {
                id: "v1: abc".into(),
                downloaded_bytes: 100,
                uploaded_bytes: 0,
                download_rate: 0,
                upload_rate: 0,
                file_progress: vec![100],
                peers: 0,
                finished: true,
                paused: true,
            },
        );
        assert_eq!(torrents[0].state, TorrentState::Paused);
        assert_eq!(torrents[0].downloaded_bytes, 100);
    }

    #[test]
    fn shared_record_and_command_mapping_preserve_frontend_choices() {
        let mut torrent = torrent();
        torrent.overrides.verify_after_download = true;
        torrent.files.set_selected(0, false);
        torrent.overrides.download_directory = Some("special".into());
        torrent.overrides.download_mode = crate::download_mode::DownloadMode::NoProgress;
        let settings = ClientSettings::default();

        let command = add_torrent_command(&torrent, &settings);
        let SessionCommand::AddTorrent {
            verify_after_download,
            download_mode,
            output_folder,
            only_files,
            paused,
            ..
        } = command
        else {
            panic!("expected add command");
        };
        assert_eq!(output_folder, "special");
        assert!(verify_after_download);
        assert_eq!(
            download_mode,
            crate::download_mode::DownloadMode::NoProgress
        );
        assert_eq!(only_files, Some(Vec::new()));
        assert!(!paused);

        let record = torrent_to_record(&torrent, None);
        let restored = torrent_from_record(record, &settings);
        assert!(restored.overrides.verify_after_download);
        assert!(
            !restored.completed_successfully(),
            "restored progress is not a fresh integrity confirmation"
        );
        assert_eq!(
            restored.overrides.download_mode,
            crate::download_mode::DownloadMode::NoProgress
        );
        assert!(!restored.files.files()[0].selected);
        assert_eq!(
            restored.overrides.download_directory.as_deref(),
            Some("special")
        );
    }
    #[test]
    fn mode_is_saved_only_after_ack_and_restart_requires_fresh_confirmation() {
        use crate::runtime_status::{ModeApplication, ModeConfig, RepairPolicy};
        let mut item = torrent();
        let id = item.info_hashes.join(";");
        let config = ModeConfig {
            mode: crate::download_mode::DownloadMode::NoProgress,
            freeze_after_bytes: 123,
        };
        item.runtime.mode = ModeApplication::Active {
            config: ModeConfig::default(),
            tracker_enabled: true,
        };
        let command = mode_change_command(&mut item, config).unwrap();
        assert!(matches!(command, SessionCommand::SetDownloadMode { .. }));
        assert_eq!(
            item.overrides.download_mode,
            crate::download_mode::DownloadMode::Normal
        );
        mode_submission_failed(&mut item, config, "Queue unavailable".into());
        assert_eq!(item.runtime.mode.active(), Some(ModeConfig::default()));
        assert_eq!(
            item.overrides.download_mode,
            crate::download_mode::DownloadMode::Normal
        );
        let mut items = vec![item];
        assert_eq!(
            apply_session_event(
                &mut items,
                SessionEvent::Mode {
                    id: id.clone(),
                    application: ModeApplication::Active {
                        config,
                        tracker_enabled: true
                    }
                }
            ),
            SessionNotice::SettingsApplied { id: id.clone() }
        );
        let policy = RepairPolicy {
            enabled: true,
            max_attempts: 4,
        };
        apply_session_event(&mut items, SessionEvent::RepairPolicy { id, policy });
        assert!(items[0].overrides.verify_after_download);
        let restored = torrent_from_record(
            torrent_to_record(&items[0], None),
            &ClientSettings::default(),
        );
        assert_eq!(restored.mode_config(), config);
        assert_eq!(restored.overrides.repair_policy, policy);
        assert_eq!(restored.runtime.mode, ModeApplication::AwaitingEngine);
    }
}
