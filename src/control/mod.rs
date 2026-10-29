//! Internal application controller. This is not a supported standalone library API.
//!
//! The controller is the only consumer of transfer events. Frontends retain their
//! presentation state and (temporarily) their existing local command/persistence
//! paths. Read handles cannot send commands, consume session events, or own an
//! engine. Local IPC and adapters use the same owner through validated, authorized
//! requests. The installed host independently pumps events between frontend frames.

pub mod access;
pub mod adapters;
pub mod agent;
mod commands;
mod export;
mod filesystem;
#[cfg(any(feature = "gui", feature = "tui"))]
pub(crate) mod frontends;
pub mod host;
mod observation;
pub mod operations;
pub mod wire;
pub use commands::{AcceptedCommand, COMMAND_ACCEPTED, CommandError, TorrentCommand};

use observation::UpdateKind;
pub use observation::{
    MAX_PAGE_SIZE, Observation, ObservedCompletion, ObservedModeState, ObservedState,
    PROTOCOL_VERSION, ReadHandle, ReadPage, ReadPageError, TorrentObservation,
};

use std::collections::BTreeMap;

use crate::{
    client::{ClientBackend, SessionNotice, apply_session_event},
    session::SessionEvent,
    torrent::TorrentItem,
};

pub struct Controller {
    // Transitional crate-local access preserves disjoint borrows in UI callbacks.
    // External adapters must use ReadHandle, never these fields.
    pub(crate) backend: ClientBackend,
    pub(crate) torrents: Vec<TorrentItem>,
    observed: BTreeMap<String, TorrentObservation>,
    reader: ReadHandle,
    dirty: bool,
    command_clock: u64,
    unpersisted_updates: std::collections::BTreeSet<String>,
    pub(crate) agent: Option<agent::AgentService>,
    pending_removals: std::collections::BTreeSet<String>,
    notices: std::collections::VecDeque<SessionNotice>,
    last_checkpoint: std::time::Instant,
}

impl Controller {
    pub fn new(backend: ClientBackend, torrents: Vec<TorrentItem>) -> Self {
        let mut controller = Self {
            backend,
            torrents,
            observed: BTreeMap::new(),
            reader: ReadHandle::default(),
            dirty: false,
            command_clock: 0,
            unpersisted_updates: Default::default(),
            agent: None,
            pending_removals: Default::default(),
            notices: Default::default(),
            last_checkpoint: std::time::Instant::now(),
        };
        controller.publish();
        controller
    }

    /// A read-only, independently polled observer. Dropping it cannot stop transfers.
    pub fn read_handle(&self) -> ReadHandle {
        self.reader.clone()
    }

    /// Owner-side pump; headless in-project hosts may drive it without a frontend.
    /// Handle each notice, then call publish after draining the current batch.
    /// Observers use ReadHandle and never consume this stream.
    pub fn poll_next(&mut self) -> Option<SessionNotice> {
        if let Some(notice) = self.notices.pop_front() {
            return Some(notice);
        }
        let event = self.backend.try_recv()?;
        Some(self.apply(event))
    }

    fn apply(&mut self, event: SessionEvent) -> SessionNotice {
        let acknowledgement = if self.agent.is_some()
            && !matches!(
                event,
                SessionEvent::Stats { .. }
                    | SessionEvent::NetworkTotals { .. }
                    | SessionEvent::Peers { .. }
                    | SessionEvent::Storage {
                        status: crate::storage::relocation::StorageStatus::Copying { .. },
                        ..
                    }
            ) {
            Some(event.clone())
        } else {
            None
        };
        let kind = UpdateKind::from_event(&event);
        // Build the public projection only from engine events. Optimistic UI
        // mode drafts, restored counters and local errors are not acknowledgements.
        let event_id = match &event {
            SessionEvent::Mode { id, .. }
            | SessionEvent::FilePrioritiesApplied { id, .. }
            | SessionEvent::Storage { id, .. }
            | SessionEvent::Peers { id, .. }
            | SessionEvent::RepairPolicy { id, .. }
            | SessionEvent::Repair { id, .. }
            | SessionEvent::NetworkTotals { id, .. }
            | SessionEvent::Verification { id, .. }
            | SessionEvent::Added { id }
            | SessionEvent::StateChanged { id, .. }
            | SessionEvent::Stats { id, .. }
            | SessionEvent::Error { id, .. }
            | SessionEvent::Removed { id } => Some(id.clone()),
            SessionEvent::VerifyAfterDownload { id, .. }
            | SessionEvent::TransferSettingsApplied { id, .. }
            | SessionEvent::FilesApplied { id, .. } => Some(id.clone()),
            SessionEvent::Stopped => None,
        };
        let notice = apply_session_event(&mut self.torrents, event);
        if let SessionNotice::Added { id } = &notice
            && let Some(torrent) = self
                .torrents
                .iter()
                .find(|torrent| torrent.info_hashes.join(";") == *id)
            && !torrent.files.high_priority().is_empty()
        {
            let _ = self
                .backend
                .send(crate::session::SessionCommand::SetFilePriorities {
                    id: id.clone(),
                    high: torrent.files.high_priority(),
                });
        }
        let old_len = self.observed.len();
        for torrent in &self.torrents {
            let id = torrent.info_hashes.join(";");
            if event_id
                .as_ref()
                .is_some_and(|event_id| *event_id != id && !torrent.info_hashes.contains(event_id))
            {
                continue;
            }
            if matches!(
                notice,
                SessionNotice::SettingsApplied { .. } | SessionNotice::StateApplied { .. }
            ) {
                self.unpersisted_updates.insert(id.clone());
            }
            let observation = self.observed.entry(id).or_insert_with(|| {
                self.command_clock = self
                    .command_clock
                    .checked_add(1)
                    .expect("registration revision exhausted");
                let mut row = TorrentObservation::pending(torrent);
                row.command_revision = self.command_clock;
                row
            });
            let before = observation.clone();
            observation.update(torrent, kind);
            self.dirty |= *observation != before;
        }
        self.dirty |= old_len != self.observed.len();
        if let SessionNotice::Removed { id } = &notice
            && self.pending_removals.remove(id)
        {
            if (!self.unpersisted_updates.contains(id) || self.persist_preserving_audit(id).is_ok())
                && self.backend.remove_torrent(id).is_ok()
            {
                self.torrents
                    .retain(|torrent| torrent.info_hashes.join(";") != *id);
                self.dirty = true;
            } else {
                self.pending_removals.insert(id.clone());
            }
        }
        if let Some(event) = acknowledgement
            && let Some(mut service) = self.agent.take()
        {
            service.event(&event, self);
            self.agent = Some(service);
        }
        notice
    }

    fn reconcile(&mut self) {
        let ids: std::collections::BTreeSet<_> = self
            .torrents
            .iter()
            .map(|torrent| torrent.info_hashes.join(";"))
            .collect();
        let old_len = self.observed.len();
        self.observed.retain(|id, _| ids.contains(id));
        self.unpersisted_updates.retain(|id| ids.contains(id));
        self.dirty |= old_len != self.observed.len();
        for torrent in &self.torrents {
            let id = torrent.info_hashes.join(";");
            if let std::collections::btree_map::Entry::Vacant(entry) = self.observed.entry(id) {
                self.command_clock = self
                    .command_clock
                    .checked_add(1)
                    .expect("registration revision exhausted");
                let mut observation = TorrentObservation::pending(torrent);
                observation.command_revision = self.command_clock;
                entry.insert(observation);
                self.dirty = true;
            }
        }
    }

    /// Publish once after a frontend drains events, including additions/removals.
    /// No history queues are allocated per reader and no work runs in a reader.
    pub fn publish(&mut self) {
        self.reconcile();
        if self.dirty {
            self.reader
                .publish(self.observed.values().cloned().collect());
            self.dirty = false;
        }
    }

    pub fn configure_agents(
        &mut self,
        directory: &std::path::Path,
        download_root: std::path::PathBuf,
    ) -> Result<(), wire::ErrorCode> {
        self.agent = Some(agent::AgentService::open(directory, download_root)?);
        Ok(())
    }

    pub fn agent_access(&mut self) -> Option<&mut agent::AgentService> {
        self.agent.as_mut()
    }

    /// Trusted frontend preferences for subsequent agent adds. Sandbox roots remain
    /// fixed for the lifetime of this configured service and cannot be changed by IPC.
    pub fn set_agent_settings(
        &mut self,
        settings: &crate::settings::ClientSettings,
    ) -> Result<(), wire::ErrorCode> {
        if !settings.validate().is_empty() {
            return Err(wire::ErrorCode::InvalidRequest);
        }
        let agent = self.agent.as_mut().ok_or(wire::ErrorCode::Disabled)?;
        agent.client_settings = settings.clone();
        Ok(())
    }

    pub fn handle_request(&mut self, request: wire::Request) -> wire::Response {
        let Some(mut service) = self.agent.take() else {
            return wire::Response::error(wire::ErrorCode::Disabled);
        };
        let response = service.request(self, request);
        self.agent = Some(service);
        response
    }

    /// Independent owner pump used by the host. Frontends consume bounded notices,
    /// not a second session receiver. Observations remain the recovery mechanism.
    pub fn tick(&mut self) {
        for _ in 0..256 {
            let Some(event) = self.backend.try_recv() else {
                break;
            };
            let notice = self.apply(event);
            if !matches!(notice, SessionNotice::None) {
                if self.notices.len() == 256 {
                    self.notices.pop_front();
                }
                self.notices.push_back(notice);
            }
        }
        let updates: Vec<_> = self.unpersisted_updates.iter().cloned().collect();
        for id in updates {
            let _ = self.persist_preserving_audit(&id);
        }
        if let Some(mut service) = self.agent.take() {
            service.tick(self);
            self.agent = Some(service);
        }
        self.publish();
        if self.last_checkpoint.elapsed() >= std::time::Duration::from_secs(5) {
            let _ = self.checkpoint();
            self.last_checkpoint = std::time::Instant::now();
        }
    }

    /// Flush measured counters without persisting UI drafts or fake engine states.
    pub fn checkpoint(&mut self) -> Result<(), String> {
        let result = self.backend.checkpoint_resumes(&self.torrents);
        if let Err(message) = &result {
            if self.notices.len() == 256 {
                self.notices.pop_front();
            }
            self.notices.push_back(SessionNotice::Error {
                id: String::new(),
                message: format!("Checkpoint failed: {message}"),
            });
        }
        result
    }

    /// Trusted local frontend only. Paths and filesystem-copy authority are NOT
    /// added to the agent protocol or arbitrary remote mutation schema.
    pub fn select_storage(&self, id: &str, directory: String, copy: bool) -> Result<(), String> {
        let torrent = self
            .torrents
            .iter()
            .find(|torrent| torrent.info_hashes.join(";") == id)
            .ok_or("Torrent unavailable")?;
        if torrent.state != crate::torrent::TorrentState::Paused {
            return Err("Pause the torrent before changing storage".into());
        }
        if self.agent.as_ref().is_some_and(|agent| agent.busy(id)) {
            return Err("Wait for the pending agent operation".into());
        }
        if !self.backend.persistence_available() {
            return Err("Persistent state unavailable".into());
        }
        self.backend
            .send(crate::session::SessionCommand::SelectStorage {
                id: id.into(),
                directory,
                copy,
            })
    }
    pub fn cancel_storage(&self, id: &str) -> Result<(), String> {
        self.backend
            .send(crate::session::SessionCommand::CancelStorage { id: id.into() })
    }
    pub fn set_file_priorities(&self, id: &str, high: Vec<usize>) -> Result<(), String> {
        if self.agent.as_ref().is_some_and(|agent| agent.busy(id)) {
            return Err("Wait for the pending agent operation".into());
        }
        self.backend
            .send(crate::session::SessionCommand::SetFilePriorities {
                id: id.into(),
                high,
            })
    }
    /// Caller must obtain an explicit local confirmation. Not exposed over IPC.
    pub fn remove_copy_originals(&self, id: &str) -> Result<(), String> {
        if self.agent.as_ref().is_some_and(|agent| agent.busy(id)) {
            return Err("Wait for the pending agent operation".into());
        }
        self.backend
            .send(crate::session::SessionCommand::CleanupStorage { id: id.into() })
    }
    pub fn move_storage(&self, id: &str, parent: String) -> Result<(), String> {
        if self.agent.as_ref().is_some_and(|agent| agent.busy(id)) {
            return Err("Wait for the pending agent operation".into());
        }
        self.backend
            .send(crate::session::SessionCommand::MoveStorage {
                id: id.into(),
                parent,
            })
    }

    /// Stop the owner before releasing its profile lock. Drain final real network
    /// totals before the last SQLite checkpoint, not just the last UI frame.
    pub fn shutdown(&mut self) -> Result<(), String> {
        if !self.backend.session_available() {
            return self.checkpoint();
        }
        self.backend
            .send(crate::session::SessionCommand::Shutdown)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            if let Some(event) = self.backend.try_recv() {
                let stopped = event == crate::session::SessionEvent::Stopped;
                self.apply(event);
                if stopped {
                    let mut persisted = Ok(());
                    for id in self.unpersisted_updates.iter().cloned().collect::<Vec<_>>() {
                        if let Err(error) = self.persist_preserving_audit(&id) {
                            persisted = Err(error);
                        }
                    }
                    let checkpoint = self.checkpoint();
                    self.backend.finish_shutdown();
                    return persisted.and(checkpoint);
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if std::time::Instant::now() >= deadline {
                return Err(
                    "Shutdown acknowledgement timed out; last periodic checkpoint retained".into(),
                );
            }
        }
    }

    pub fn has_confirmed_update(&self, id: &str) -> bool {
        self.unpersisted_updates.contains(id)
    }

    fn persist_preserving_audit(&mut self, id: &str) -> Result<(), String> {
        let audit = self
            .backend
            .load_torrents()?
            .into_iter()
            .find(|record| record.id == id)
            .and_then(|record| record.tracker_audit);
        self.persist_confirmed_update(id, audit)
    }

    pub fn add(
        &mut self,
        torrent: TorrentItem,
        settings: &crate::settings::ClientSettings,
        audit: Option<crate::state::StoredTrackerAuditProfile>,
    ) -> Result<(), String> {
        self.add_internal(torrent, settings, audit, None)
    }

    pub(crate) fn add_prepared(
        &mut self,
        torrent: TorrentItem,
        settings: &crate::settings::ClientSettings,
        prepared: crate::session::PreparedTorrent,
    ) -> Result<(), String> {
        self.add_internal(torrent, settings, None, Some(prepared))
    }

    fn add_internal(
        &mut self,
        torrent: TorrentItem,
        settings: &crate::settings::ClientSettings,
        audit: Option<crate::state::StoredTrackerAuditProfile>,
        prepared: Option<crate::session::PreparedTorrent>,
    ) -> Result<(), String> {
        let id = torrent.info_hashes.join(";");
        if id.is_empty()
            || self.torrents.iter().any(|item| {
                item.info_hashes
                    .iter()
                    .any(|hash| torrent.info_hashes.contains(hash))
            })
        {
            return Err("Torrent is already added or has no identity".into());
        }
        torrent.mode_config().validate()?;
        torrent.overrides.repair_policy.validate()?;
        crate::limits::validate_relative_paths(
            torrent.files.files().iter().map(|file| file.path.as_str()),
            &crate::limits::ResourceLimits::default(),
        )?;
        if !self.backend.persistence_available() {
            return Err("Persistent state store is unavailable".into());
        }
        let record = crate::client::torrent_to_record(&torrent, audit);
        self.backend.save_torrent(&record)?;
        let command = crate::client::add_torrent_command(&torrent, settings);
        let command = match prepared {
            Some(prepared) => crate::session::SessionCommand::PreparedAdd {
                command: Box::new(command),
                prepared,
            },
            None => command,
        };
        if let Err(error) = self.backend.send_checked(command) {
            let _ = self.backend.remove_torrent(&id);
            return Err(format!("Transfer engine rejected add: {error:?}"));
        }
        self.torrents.push(torrent);
        self.publish();
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<AcceptedCommand, CommandError> {
        let accepted = self.submit(id, TorrentCommand::Remove, None)?;
        self.pending_removals.insert(accepted.torrent_id.clone());
        Ok(accepted)
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.reader.close();
    }
}

#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod continuous_soak;
#[cfg(test)]
mod removal_tests;
#[cfg(test)]
pub(crate) mod soak_tests;
#[cfg(test)]
mod swarm_soak;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transfer_test;
