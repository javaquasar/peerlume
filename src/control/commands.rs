//! Validated commands for existing torrents. Acceptance is never completion.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::Controller;
#[cfg(test)]
use crate::client::torrent_to_record;
use crate::state::StoredTrackerAuditProfile;
use crate::torrent::TorrentState;
use crate::{
    client::{ClientBackend, mode_submission_failed},
    runtime_status::{ModeApplication, ModeConfig, RepairPolicy},
    session::{SessionCommand, SessionSendError},
    torrent::TorrentItem,
};

pub const COMMAND_ACCEPTED: &str = "Command queued; awaiting engine confirmation";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TorrentCommand {
    Pause,
    Resume,
    Recheck,
    RecheckAndRepair,
    SetMode(ModeConfig),
    SetRepairPolicy(RepairPolicy),
    SetVerifyAfterDownload(bool),
    SetFiles(Vec<usize>),
    SetTransferSettings(crate::transfer_settings::TransferSettings),
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCommand {
    pub torrent_id: String,
    /// In-process compare-and-set token, not an engine result or operation ID.
    pub command_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandError {
    UnknownTorrent,
    RevisionConflict { current_revision: u64 },
    RevisionExhausted,
    InvalidMode,
    InvalidRepairPolicy,
    InvalidFileSelection,
    InvalidTransferSettings,
    EngineUnavailable,
    QueueFull,
    EngineClosed,
    OperationInProgress,
    PersistenceUnavailable,
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownTorrent => "Torrent is unavailable",
            Self::RevisionConflict { .. } => "Torrent commands changed; refresh before retrying",
            Self::RevisionExhausted => "Command revision space is exhausted",
            Self::InvalidMode => "Invalid download mode metadata or freeze threshold",
            Self::InvalidRepairPolicy => "Repair attempts must be between 1 and 10",
            Self::InvalidFileSelection => "File selection contains duplicate or invalid indices",
            Self::InvalidTransferSettings => {
                "Invalid transfer limits, peer count, priority or seed limits"
            }
            Self::EngineUnavailable => "Transfer engine is unavailable",
            Self::QueueFull => "Transfer command queue is full; retry after refreshing",
            Self::EngineClosed => "Transfer engine command channel is closed",
            Self::OperationInProgress => "Another torrent operation is awaiting confirmation",
            Self::PersistenceUnavailable => "Persistent state store is unavailable",
        })
    }
}

impl std::error::Error for CommandError {}

impl From<SessionSendError> for CommandError {
    fn from(error: SessionSendError) -> Self {
        match error {
            SessionSendError::Unavailable => Self::EngineUnavailable,
            SessionSendError::Full => Self::QueueFull,
            SessionSendError::Closed => Self::EngineClosed,
        }
    }
}

impl TorrentCommand {
    pub(crate) fn feedback_action(&self) -> Option<crate::runtime_status::TransferAction> {
        use crate::runtime_status::TransferAction;
        match self {
            Self::Pause => Some(TransferAction::Pause),
            Self::Resume => Some(TransferAction::Resume),
            Self::Recheck | Self::RecheckAndRepair => Some(TransferAction::Recheck),
            _ => None,
        }
    }
    fn prepare(&self, torrent: &TorrentItem, id: String) -> Result<SessionCommand, CommandError> {
        Ok(match self {
            Self::SetTransferSettings(settings) => {
                settings
                    .validate()
                    .map_err(|_| CommandError::InvalidTransferSettings)?;
                SessionCommand::SetTransferSettings {
                    id,
                    settings: *settings,
                }
            }
            Self::Pause => SessionCommand::Pause { id },
            Self::Resume => SessionCommand::Resume { id },
            Self::Recheck => SessionCommand::Recheck { id },
            Self::RecheckAndRepair => SessionCommand::RecheckAndRepair { id },
            Self::Remove => SessionCommand::Remove { id },
            Self::SetMode(config) => {
                config.validate().map_err(|_| CommandError::InvalidMode)?;
                let full_size = torrent
                    .files
                    .checked_total_size()
                    .ok_or(CommandError::InvalidMode)?;
                config
                    .mode
                    .validate_metadata(
                        torrent
                            .info_hashes
                            .iter()
                            .any(|hash| hash.starts_with("v1: ")),
                        full_size,
                    )
                    .map_err(|_| CommandError::InvalidMode)?;
                SessionCommand::SetDownloadMode {
                    id,
                    config: *config,
                }
            }
            Self::SetRepairPolicy(policy) => {
                policy
                    .validate()
                    .map_err(|_| CommandError::InvalidRepairPolicy)?;
                SessionCommand::SetRepairPolicy {
                    id,
                    policy: *policy,
                }
            }
            Self::SetVerifyAfterDownload(enabled) => SessionCommand::SetVerifyAfterDownload {
                id,
                enabled: *enabled,
            },
            Self::SetFiles(indices) => {
                if indices.len() > torrent.files.file_count()
                    || indices
                        .iter()
                        .any(|index| *index >= torrent.files.file_count())
                {
                    return Err(CommandError::InvalidFileSelection);
                }
                let mut only_files = indices.clone();
                only_files.sort_unstable();
                if only_files.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err(CommandError::InvalidFileSelection);
                }
                SessionCommand::SetFiles { id, only_files }
            }
        })
    }
}

impl Controller {
    /// Revision changes on registration and accepted commands, not telemetry.
    /// It is local to this controller lifetime; durable operations are a later slice.
    pub fn command_revision(&self, id: &str) -> Result<u64, CommandError> {
        let matches = |torrent: &&crate::torrent::TorrentItem| {
            torrent.info_hashes.join(";") == id || torrent.info_hashes.iter().any(|hash| hash == id)
        };
        let torrent = self
            .torrent_indices
            .get(id)
            .and_then(|index| self.torrents.get(*index))
            .filter(matches)
            .or_else(|| self.torrents.iter().find(matches))
            .ok_or(CommandError::UnknownTorrent)?;
        self.observed
            .get(&torrent.info_hashes.join(";"))
            .map(|row| row.command_revision)
            .ok_or(CommandError::UnknownTorrent)
    }

    pub fn submit(
        &mut self,
        id: &str,
        command: TorrentCommand,
        expected_revision: Option<u64>,
    ) -> Result<AcceptedCommand, CommandError> {
        if let Some(client) = self.backend.attached_client() {
            return client.submit(id, &command, expected_revision);
        }
        self.submit_via(id, command, expected_revision, |backend, command| {
            backend.send_checked(command).map_err(Into::into)
        })
    }

    fn submit_via(
        &mut self,
        id: &str,
        command: TorrentCommand,
        expected_revision: Option<u64>,
        send: impl FnOnce(&ClientBackend, SessionCommand) -> Result<(), CommandError>,
    ) -> Result<AcceptedCommand, CommandError> {
        let index = self.torrent_index(id).ok_or(CommandError::UnknownTorrent)?;
        self.reconcile();
        if self.is_busy(&self.torrents[index].info_hashes.join(";")) {
            return Err(CommandError::OperationInProgress);
        }
        let result = self.submit_registered_via(index, command, expected_revision, send);
        if result.is_ok() {
            self.publish();
        }
        result
    }

    fn submit_registered_via(
        &mut self,
        index: usize,
        command: TorrentCommand,
        expected_revision: Option<u64>,
        send: impl FnOnce(&ClientBackend, SessionCommand) -> Result<(), CommandError>,
    ) -> Result<AcceptedCommand, CommandError> {
        let id = self.torrents[index].info_hashes.join(";");
        if matches!(command, TorrentCommand::Remove) && !self.backend.persistence_available() {
            return Err(CommandError::PersistenceUnavailable);
        }
        let revision = self.observed[&id].command_revision;
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(CommandError::RevisionConflict {
                current_revision: revision,
            });
        }
        let prepared = command.prepare(&self.torrents[index], id.clone())?;
        let next = self
            .command_clock
            .checked_add(1)
            .ok_or(CommandError::RevisionExhausted)?;
        let feedback_action = command.feedback_action();
        if feedback_action.is_some()
            && self.torrents[index]
                .runtime
                .command
                .pending_action()
                .is_some()
        {
            return Err(CommandError::OperationInProgress);
        }
        if let Err(error) = send(&self.backend, prepared) {
            if let Some(action) = feedback_action {
                self.torrents[index]
                    .runtime
                    .command
                    .reject(action, &error.to_string());
            }
            if let TorrentCommand::SetMode(config) = command {
                mode_submission_failed(&mut self.torrents[index], config, error.to_string());
            }
            return Err(error);
        }
        self.command_clock = next;
        if let Some(action) = feedback_action {
            self.torrents[index].runtime.command.begin(action);
        }
        self.observed
            .get_mut(&id)
            .expect("registered above")
            .command_revision = next;
        if let TorrentCommand::SetMode(config) = command {
            let torrent = &mut self.torrents[index];
            torrent.runtime.mode = ModeApplication::Applying {
                requested: config,
                active: torrent.runtime.mode.active(),
            };
        }
        self.dirty = true;
        Ok(AcceptedCommand {
            torrent_id: id,
            command_revision: next,
        })
    }

    /// Trusted local retry, using this owner's saved settings and existing engine.
    /// No new IPC command or grant. A failed registration is not a resumable handle.
    pub(crate) fn retry_start(
        &mut self,
        id: &str,
        settings: &crate::settings::ClientSettings,
    ) -> Result<(), String> {
        if let Some(client) = self.backend.attached_client() {
            return client.retry_start(id, settings);
        }
        use crate::runtime_status::TransferAction;
        let index = self.torrent_index(id).ok_or("Torrent is unavailable")?;
        self.reconcile();
        if self.is_busy(id)
            || self.torrents[index]
                .runtime
                .command
                .pending_action()
                .is_some()
        {
            return Err(CommandError::OperationInProgress.to_string());
        }
        let torrent = &self.torrents[index];
        if torrent.state != TorrentState::Error
            || torrent.runtime.registered
            || torrent.runtime.last_error.is_none()
        {
            return Err("Retry start requires a failed registration reported by the engine".into());
        }
        if !self.backend.persistence_available() {
            return Err(CommandError::PersistenceUnavailable.to_string());
        }
        if !settings.validate().is_empty() {
            return Err("Correct invalid client settings before retrying".into());
        }
        let next = self
            .command_clock
            .checked_add(1)
            .ok_or("Command revision space is exhausted")?;
        let mut retry = torrent.clone();
        retry.state = TorrentState::WaitingForEngine;
        if let Err(error) = self
            .backend
            .send_checked(crate::client::add_torrent_command(&retry, settings))
        {
            let message = CommandError::from(error).to_string();
            self.torrents[index]
                .runtime
                .command
                .reject(TransferAction::Start, &message);
            return Err(message);
        }
        self.torrents[index]
            .runtime
            .command
            .begin(TransferAction::Start);
        self.command_clock = next;
        self.observed
            .get_mut(id)
            .expect("registered above")
            .command_revision = next;
        self.dirty = true;
        self.publish();
        Ok(())
    }

    /// The audit profile is presentation-independent data supplied by the UI.
    /// Call after SettingsApplied or StateApplied, never merely after enqueueing.
    /// Owner-only API: ReadHandle observers cannot checkpoint application state.
    pub fn persist_confirmed_update(
        &mut self,
        id: &str,
        audit: Option<StoredTrackerAuditProfile>,
    ) -> Result<(), String> {
        let index = self.torrent_index(id).ok_or("Torrent is unavailable")?;
        let torrent = &self.torrents[index];
        let Some(intent) = self.unpersisted_updates.get(id).copied() else {
            return Err("No confirmed update to persist".into());
        };
        if !self.backend.persistence_available() {
            return Err("Persistent state store is unavailable".into());
        }
        self.backend
            .persist_torrent_update(torrent, intent, audit.as_ref())?;
        self.unpersisted_updates.remove(id);
        Ok(())
    }

    /// Best-effort batch: report partial acceptance and never claim all transfers
    /// have changed state. The same validation/send path handles each request.
    #[cfg(any(test, feature = "gui", feature = "tui"))]
    pub(crate) fn submit_batch(&mut self, resume: bool) -> String {
        if let Some(client) = self.backend.attached_client() {
            let command = if resume {
                TorrentCommand::Resume
            } else {
                TorrentCommand::Pause
            };
            let mut queued = 0;
            let mut failed = 0;
            for item in &self.torrents {
                let eligible = if resume {
                    matches!(
                        item.state,
                        TorrentState::Paused
                            | TorrentState::Error
                            | TorrentState::VerificationFailed
                    )
                } else {
                    matches!(
                        item.state,
                        TorrentState::Downloading
                            | TorrentState::WaitingForEngine
                            | TorrentState::Seeding
                            | TorrentState::Checking
                    )
                };
                if eligible {
                    let id = item.info_hashes.join(";");
                    match client.enqueue_submit(
                        &id,
                        command.clone(),
                        self.command_revision(&id).ok(),
                    ) {
                        Ok(()) => queued += 1,
                        Err(_) => failed += 1,
                    }
                }
            }
            return format!("Awaiting background owner: {queued} queued, {failed} not queued");
        }
        self.submit_batch_via(resume, |backend, command| {
            backend.send_checked(command).map_err(Into::into)
        })
    }

    #[cfg(any(test, feature = "gui", feature = "tui"))]
    fn submit_batch_via(
        &mut self,
        resume: bool,
        mut send: impl FnMut(&ClientBackend, SessionCommand) -> Result<(), CommandError>,
    ) -> String {
        self.reconcile();
        let indices: Vec<_> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, torrent)| {
                if resume {
                    matches!(
                        torrent.state,
                        TorrentState::Paused
                            | TorrentState::Error
                            | TorrentState::VerificationFailed
                    )
                } else {
                    matches!(
                        torrent.state,
                        TorrentState::Downloading
                            | TorrentState::WaitingForEngine
                            | TorrentState::Seeding
                            | TorrentState::Checking
                    )
                }
            })
            .map(|(index, _)| index)
            .collect();
        let mut accepted = 0;
        let mut failed = 0;
        for index in indices {
            let command = if resume {
                TorrentCommand::Resume
            } else {
                TorrentCommand::Pause
            };
            match self.submit_registered_via(index, command, None, |backend, command| {
                send(backend, command)
            }) {
                Ok(_) => accepted += 1,
                Err(_) => failed += 1,
            }
        }
        self.publish();
        format!("Commands queued: {accepted}; rejected: {failed}. Awaiting engine confirmation.")
    }
}

#[cfg(test)]
mod tests;
