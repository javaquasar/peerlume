//! Internal application controller. This is not a supported standalone library API.
//!
//! The daemon controller is the only consumer of transfer events and the only
//! durable writer. Native frontend mirrors import acknowledged owner snapshots
//! and retain presentation state. Read handles cannot send commands, consume
//! session events, or own an engine. Native IPC and restricted external adapters
//! are separate authorized boundaries around that owner. The host pumps events
//! independently of frontend frames and sleeps until work or a deadline arrives.

pub mod access;
pub mod adapters;
pub mod agent;
mod commands;
mod diagnostics;
mod export;
mod filesystem;
pub mod frontend;
#[cfg(any(feature = "gui", feature = "tui"))]
pub(crate) mod frontends;
pub mod host;
mod observation;
pub mod operations;
mod presentation;
mod priority_restore;
mod restore;
pub mod wire;
pub use commands::{AcceptedCommand, COMMAND_ACCEPTED, CommandError, TorrentCommand};

use observation::UpdateKind;
pub use observation::{
    MAX_PAGE_SIZE, Observation, ObservedCompletion, ObservedModeState, ObservedState,
    PROTOCOL_VERSION, ReadHandle, ReadPage, ReadPageError, TorrentObservation,
};

use std::collections::{BTreeMap, HashMap};

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
    unpersisted_updates: BTreeMap<String, crate::state::PersistenceIntent>,
    torrent_indices: HashMap<String, usize>,
    canonical_ids: Vec<String>,
    structural_dirty: bool,
    owner_wake: std::sync::Arc<tokio::sync::Notify>,
    diagnostic_wake: std::sync::Arc<tokio::sync::Notify>,
    diagnostic_tail: diagnostics::Tail,
    pub(crate) frontend_busy: std::collections::HashSet<String>,
    native_instance_id: Option<String>,
    native_durability: HashMap<String, (u64, bool)>,
    native_operations: Option<frontend::operations::SharedHistory>,
    pub(crate) agent: Option<agent::AgentService>,
    pending_removals: std::collections::BTreeSet<String>,
    pending_restore: std::collections::BTreeSet<String>,
    pending_priority_restore: std::collections::BTreeSet<String>,
    admitted_priority_restore: std::collections::BTreeSet<String>,
    detached_removals: std::collections::BTreeSet<String>,
    detachment_acks: HashMap<String, u64>,
    notices: std::collections::VecDeque<SessionNotice>,
    last_checkpoint: std::time::Instant,
    frontend_revision: u64,
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
            torrent_indices: Default::default(),
            canonical_ids: Default::default(),
            structural_dirty: true,
            owner_wake: Default::default(),
            diagnostic_wake: Default::default(),
            diagnostic_tail: Default::default(),
            frontend_busy: Default::default(),
            native_instance_id: None,
            native_durability: Default::default(),
            native_operations: None,
            agent: None,
            pending_removals: Default::default(),
            pending_restore: Default::default(),
            pending_priority_restore: Default::default(),
            admitted_priority_restore: Default::default(),
            detached_removals: Default::default(),
            detachment_acks: Default::default(),
            notices: Default::default(),
            last_checkpoint: std::time::Instant::now(),
            frontend_revision: 0,
        };
        controller
            .backend
            .set_event_wake(controller.owner_wake.clone());
        controller.publish();
        controller
    }

    /// A read-only, independently polled observer. Dropping it cannot stop transfers.
    pub fn read_handle(&self) -> ReadHandle {
        self.reader.clone()
    }

    /// Local presentation wake-up token, independent of protocol/CAS revisions.
    pub fn frontend_revision(&self) -> u64 {
        self.frontend_revision
    }

    pub fn set_detail_interest(&self, interest: Option<(String, crate::session::DetailInterest)>) {
        self.backend.set_detail_interest(interest);
    }

    /// Owner-side pump; headless in-project hosts may drive it without a frontend.
    /// Handle each notice, then call publish after draining the current batch.
    /// Observers use ReadHandle and never consume this stream.
    pub fn poll_next(&mut self) -> Option<SessionNotice> {
        if self.backend.attached_client().is_some() {
            self.import_native_snapshot();
            let notice = self.notices.pop_front().or_else(|| {
                self.backend
                    .attached_client()
                    .and_then(|client| client.poll_notice())
            });
            if notice.is_some() {
                self.frontend_revision = self.frontend_revision.wrapping_add(1);
            }
            return notice;
        }
        if let Some(notice) = self.notices.pop_front() {
            return Some(notice);
        }
        let event = self.backend.try_recv()?;
        Some(self.apply(event))
    }

    fn apply(&mut self, event: SessionEvent) -> SessionNotice {
        self.reconcile();
        let acknowledgement = if self.agent.is_some()
            && !matches!(
                event,
                SessionEvent::Stats { .. }
                    | SessionEvent::FileSnapshot { .. }
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
            | SessionEvent::FileSnapshot { id, .. }
            | SessionEvent::Error { id, .. }
            | SessionEvent::Removed { id } => Some(id.clone()),
            SessionEvent::VerifyAfterDownload { id, .. }
            | SessionEvent::TransferSettingsApplied { id, .. }
            | SessionEvent::FilesApplied { id, .. } => Some(id.clone()),
            SessionEvent::Stopped => None,
        };
        let index = event_id.as_deref().and_then(|id| self.torrent_index(id));
        let operation_feedback = if self.native_operations.is_some() {
            index.and_then(|index| {
                frontend::operations::Feedback::from_event(
                    &event,
                    self.torrents[index].runtime.command.pending_action(),
                )
            })
        } else {
            None
        };
        let quiet_telemetry = matches!(&event, SessionEvent::NetworkTotals { .. })
            || matches!(&event, SessionEvent::Stats { file_progress, .. } if file_progress.is_empty());
        let before = if quiet_telemetry {
            index.map(|index| presentation::TelemetryStamp::capture(&self.torrents[index]))
        } else {
            None
        };
        let removed = matches!(&event, SessionEvent::Removed { .. });
        if self.observe_priority_restore(&event, index) {
            // An old priority ACK cannot replace newer canonical desired bits.
            return SessionNotice::None;
        }
        let retry_acknowledged = matches!(&event, SessionEvent::Added { .. })
            && index.is_some_and(|index| {
                self.torrents[index].runtime.command.pending_action()
                    == Some(crate::runtime_status::TransferAction::Start)
            });
        let persistence_intent = match &event {
            SessionEvent::FilesApplied { .. } => crate::state::PersistenceIntent::SELECTION,
            SessionEvent::FilePrioritiesApplied { .. } => {
                crate::state::PersistenceIntent::PRIORITIES
            }
            SessionEvent::Storage { .. } => crate::state::PersistenceIntent::STORAGE,
            _ => crate::state::PersistenceIntent::SCALAR,
        };
        // The pure reducer sees only the addressed canonical item. Global stop
        // is intentionally the sole all-torrent event.
        let notice = match index {
            Some(index) => {
                apply_session_event(std::slice::from_mut(&mut self.torrents[index]), event)
            }
            None if event_id.is_none() => apply_session_event(&mut self.torrents, event),
            None => apply_session_event(&mut [], event),
        };
        if !quiet_telemetry
            || before
                != index.map(|index| presentation::TelemetryStamp::capture(&self.torrents[index]))
        {
            self.frontend_revision = self.frontend_revision.wrapping_add(1);
        }
        if removed && let Some(index) = index {
            self.detachment_acks
                .insert(self.canonical_ids[index].clone(), self.frontend_revision);
        }
        let old_len = self.observed.len();
        let affected = match index {
            Some(index) => index..index + 1,
            None if event_id.is_none() => 0..self.torrents.len(),
            None => 0..0,
        };
        for index in affected {
            let torrent = &self.torrents[index];
            let id = self.canonical_ids[index].clone();
            if matches!(
                notice,
                SessionNotice::SettingsApplied { .. } | SessionNotice::StateApplied { .. }
            ) || retry_acknowledged
            {
                self.unpersisted_updates
                    .entry(id.clone())
                    .and_modify(|intent| intent.merge(persistence_intent))
                    .or_insert(persistence_intent);
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
        if let (Some(feedback), Some(index), Some(history)) =
            (operation_feedback, index, &self.native_operations)
        {
            let id = &self.canonical_ids[index];
            if let Some(row) = self.observed.get(id) {
                history
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .observe(id, row.command_revision, feedback);
            }
        }
        if let SessionNotice::Removed { id } = &notice
            && self.pending_removals.contains(id)
        {
            self.detached_removals.insert(id.clone());
            if let Err(error) = self.finish_detached_removal(id)
                && let Some(index) = self.torrent_index(id)
            {
                self.torrents[index].runtime.record_error(&format!(
                    "Transfer detached; durable removal failed: {error}. Retained for retry"
                ));
            }
        }
        if let Some(event) = acknowledgement
            && let Some(mut service) = self.agent.take()
        {
            service.event(&event, self);
            self.agent = Some(service);
        }
        let notice = match notice {
            SessionNotice::Added { id } if retry_acknowledged => SessionNotice::StateApplied { id },
            other => other,
        };
        self.record_native_notice(&notice);
        notice
    }

    fn reconcile(&mut self) {
        if !self.structural_dirty && self.canonical_ids.len() == self.torrents.len() {
            return;
        }
        self.torrent_indices.clear();
        let previous_ids = std::mem::take(&mut self.canonical_ids);
        for (index, torrent) in self.torrents.iter().enumerate() {
            let id = torrent.info_hashes.join(";");
            self.torrent_indices.insert(id.clone(), index);
            for alias in &torrent.info_hashes {
                self.torrent_indices.insert(alias.clone(), index);
            }
            self.canonical_ids.push(id);
        }
        if !previous_ids.is_empty() && previous_ids != self.canonical_ids {
            self.command_clock = self
                .command_clock
                .checked_add(1)
                .expect("structural revision exhausted");
            self.dirty = true;
        }
        // Observation/persistence keys are canonical IDs, never aliases. An
        // old canonical key can become an alias after metadata replacement.
        let ids: std::collections::HashSet<_> =
            self.canonical_ids.iter().map(String::as_str).collect();
        let old_len = self.observed.len();
        self.observed.retain(|id, _| ids.contains(id.as_str()));
        self.unpersisted_updates
            .retain(|id, _| ids.contains(id.as_str()));
        self.pending_restore.retain(|id| ids.contains(id.as_str()));
        self.pending_priority_restore
            .retain(|id| ids.contains(id.as_str()));
        self.admitted_priority_restore
            .retain(|id| ids.contains(id.as_str()));
        self.detachment_acks
            .retain(|id, _| ids.contains(id.as_str()));
        self.dirty |= old_len != self.observed.len();
        for (torrent, id) in self.torrents.iter().zip(&self.canonical_ids) {
            let id = id.clone();
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
        self.structural_dirty = false;
    }

    /// Call before publishing changed identities/aliases or a reordered mirror
    /// at unchanged length. Additions/removals are detected by collection length.
    pub(crate) fn invalidate_torrent_index(&mut self) {
        self.structural_dirty = true;
    }

    pub(crate) fn torrent_index(&mut self, id: &str) -> Option<usize> {
        self.reconcile();
        if let Some(&index) = self.torrent_indices.get(id)
            && self.torrents.get(index).is_some_and(|torrent| {
                torrent.info_hashes.iter().any(|hash| hash == id)
                    || torrent.info_hashes.join(";") == id
            })
        {
            return Some(index);
        }
        // A native metadata replacement can change aliases without changing
        // collection length. Repair the index once at that structural boundary.
        self.structural_dirty = true;
        self.reconcile();
        self.torrent_indices.get(id).copied()
    }

    pub(crate) fn owner_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.owner_wake.clone()
    }

    pub(crate) fn is_busy(&self, id: &str) -> bool {
        self.frontend_busy.contains(id)
            || self.pending_removals.contains(id)
            || self.pending_restore.contains(id)
            || self.agent.as_ref().is_some_and(|agent| agent.busy(id))
    }

    pub(crate) fn next_wake_delay(&self) -> std::time::Duration {
        let remaining =
            std::time::Duration::from_secs(5).saturating_sub(self.last_checkpoint.elapsed());
        let remaining = if self.unpersisted_updates.is_empty()
            && self.detached_removals.is_empty()
            && self.pending_priority_restore.is_empty()
        {
            remaining
        } else {
            remaining.min(std::time::Duration::from_millis(100))
        };
        self.agent
            .as_ref()
            .map_or(remaining, |agent| agent.next_wake_delay(remaining))
    }

    /// Publish once after a frontend drains events, including additions/removals.
    /// No history queues are allocated per reader and no work runs in a reader.
    pub fn publish(&mut self) {
        self.reconcile();
        if self.dirty {
            self.frontend_revision = self.frontend_revision.wrapping_add(1);
            self.reader
                .publish(self.observed.values().cloned().collect());
            self.dirty = false;
            self.owner_wake.notify_one();
        }
    }

    pub fn configure_agents(
        &mut self,
        directory: &std::path::Path,
        download_root: std::path::PathBuf,
    ) -> Result<(), wire::ErrorCode> {
        if let Some(client) = self.backend.attached_client() {
            return client
                .configure_agents(&download_root)
                .map_err(|_| wire::ErrorCode::PersistenceUnavailable);
        }
        self.agent = Some(agent::AgentService::open(directory, download_root)?);
        self.owner_wake.notify_one();
        Ok(())
    }

    pub fn agent_access(&mut self) -> Option<&mut agent::AgentService> {
        self.owner_wake.notify_one();
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
        if let Some(client) = self.backend.attached_client() {
            return client
                .set_agent_settings(settings)
                .map_err(|_| wire::ErrorCode::PersistenceUnavailable);
        }
        let agent = self.agent.as_mut().ok_or(wire::ErrorCode::Disabled)?;
        agent.client_settings = settings.clone();
        self.owner_wake.notify_one();
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
        if self.backend.attached_client().is_some() {
            self.import_native_snapshot();
            return;
        }
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
        let updates: Vec<_> = self.unpersisted_updates.keys().cloned().collect();
        for id in updates {
            let _ = self.persist_preserving_audit(&id);
        }
        self.retry_priority_restores();
        for id in self
            .detached_removals
            .iter()
            .take(16)
            .cloned()
            .collect::<Vec<_>>()
        {
            let _ = self.finish_detached_removal(&id);
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
        if self.backend.attached_client().is_some() {
            // The owning daemon checkpoints independently. A native mirror must
            // never persist its drafts, stale counters or deferred file model.
            return Ok(());
        }
        let result = self.backend.checkpoint_resumes(&self.torrents);
        if let Err(message) = &result {
            let notice = SessionNotice::Error {
                id: String::new(),
                message: format!("Checkpoint failed: {message}"),
            };
            if self.record_native_notice(&notice) {
                if self.notices.len() == 256 {
                    self.notices.pop_front();
                }
                self.notices.push_back(notice);
            }
        }
        result
    }

    /// Trusted local frontend only. Paths and filesystem-copy authority are NOT
    /// added to the agent protocol or arbitrary remote mutation schema.
    pub fn select_storage(&self, id: &str, directory: String, copy: bool) -> Result<(), String> {
        if let Some(client) = self.backend.attached_client() {
            return client.storage(id, frontend::StorageAction::Select { directory, copy });
        }
        let torrent = self
            .torrents
            .iter()
            .find(|torrent| torrent.info_hashes.join(";") == id)
            .ok_or("Torrent unavailable")?;
        if torrent.state != crate::torrent::TorrentState::Paused {
            return Err("Pause the torrent before changing storage".into());
        }
        if self.is_busy(id) {
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
        if let Some(client) = self.backend.attached_client() {
            return client.storage(id, frontend::StorageAction::Cancel);
        }
        self.backend
            .send(crate::session::SessionCommand::CancelStorage { id: id.into() })
    }
    pub fn set_file_priorities(&self, id: &str, high: Vec<usize>) -> Result<(), String> {
        if let Some(client) = self.backend.attached_client() {
            return client.storage(id, frontend::StorageAction::Priorities { high });
        }
        if self.is_busy(id) {
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
        if let Some(client) = self.backend.attached_client() {
            return client.storage(id, frontend::StorageAction::Cleanup);
        }
        if self.is_busy(id) {
            return Err("Wait for the pending agent operation".into());
        }
        self.backend
            .send(crate::session::SessionCommand::CleanupStorage { id: id.into() })
    }
    pub fn move_storage(&self, id: &str, parent: String) -> Result<(), String> {
        if let Some(client) = self.backend.attached_client() {
            return client.storage(id, frontend::StorageAction::Move { parent });
        }
        if self.is_busy(id) {
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
        if self.backend.attached_client().is_some() {
            return Ok(());
        }
        if !self.backend.session_available() {
            let checkpoint = self.checkpoint();
            let journal = self
                .backend
                .journal()
                .map_or(Ok(()), |journal| journal.finish());
            return checkpoint.and(journal);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            match self
                .backend
                .send_checked(crate::session::SessionCommand::Shutdown)
            {
                Ok(()) => break,
                Err(crate::session::SessionSendError::Full) => {
                    // A startup/restore burst can fill both bounded queues.
                    // Release event backpressure while waiting to admit the
                    // shutdown barrier; never drop the acknowledged state.
                    for _ in 0..256 {
                        let Some(event) = self.backend.try_recv() else {
                            break;
                        };
                        self.apply(event);
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err("Shutdown admission timed out; owner state retained".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => return Err(format!("Shutdown could not be admitted: {error:?}")),
            }
        }
        loop {
            if let Some(event) = self.backend.try_recv() {
                let stopped = event == crate::session::SessionEvent::Stopped;
                self.apply(event);
                if stopped {
                    let mut persisted = Ok(());
                    for id in self.unpersisted_updates.keys().cloned().collect::<Vec<_>>() {
                        if let Err(error) = self.persist_preserving_audit(&id) {
                            persisted = Err(error);
                        }
                    }
                    let checkpoint = self.checkpoint();
                    // Journal observers/readers may retain clones beyond the
                    // runtime. Seal and durably flush this owner explicitly
                    // before its engine/profile lock can be released.
                    let journal = self
                        .backend
                        .journal()
                        .map_or(Ok(()), |journal| journal.finish());
                    self.backend.finish_shutdown();
                    return persisted.and(checkpoint).and(journal);
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
        self.unpersisted_updates.contains_key(id)
    }

    fn persist_preserving_audit(&mut self, id: &str) -> Result<(), String> {
        // ACK writes preserve tracker/audit columns in place; no file tree,
        // tracker secret or unrelated torrent is loaded on this path.
        let result = self.persist_confirmed_update(id, None);
        if result.is_ok() {
            self.record_native_operation_persistence(id, false);
        }
        if let Err(message) = &result {
            let notice = SessionNotice::Error {
                id: id.to_owned(),
                message: format!("Could not persist acknowledged update: {message}"),
            };
            if self.record_native_notice(&notice) {
                if self.notices.len() == 256 {
                    self.notices.pop_front();
                }
                self.notices.push_back(notice);
            }
        }
        result
    }

    fn finish_detached_removal(&mut self, id: &str) -> Result<(), String> {
        if !self.detached_removals.contains(id) {
            return Err("Engine detachment is not acknowledged".into());
        }
        if self.unpersisted_updates.contains_key(id) {
            self.persist_preserving_audit(id)?;
        }
        if let Err(error) = self.backend.remove_torrent(id) {
            self.record_native_notice(&SessionNotice::Error {
                id: id.into(),
                message: format!(
                    "Transfer detached; durable removal failed: {error}. Retained for retry"
                ),
            });
            return Err(error);
        }
        self.record_native_operation_persistence(id, true);
        self.torrents
            .retain(|torrent| torrent.info_hashes.join(";") != id);
        self.pending_removals.remove(id);
        self.detached_removals.remove(id);
        self.structural_dirty = true;
        self.dirty = true;
        Ok(())
    }

    fn record_native_operation_persistence(&self, id: &str, removal: bool) {
        if let (Some(history), Some(row)) = (&self.native_operations, self.observed.get(id)) {
            history
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .persisted(id, row.command_revision, removal);
        }
    }

    fn import_native_snapshot(&mut self) {
        let Some(snapshot) = self
            .backend
            .attached_client()
            .and_then(|client| client.sync())
        else {
            return;
        };
        self.apply_native_snapshot(snapshot);
    }

    fn apply_native_snapshot(&mut self, snapshot: frontend::Snapshot) {
        let same_owner = self.native_instance_id.as_ref() == Some(&snapshot.instance_id);
        if !same_owner {
            self.native_durability.clear();
        }
        let native_durability = snapshot
            .torrents
            .iter()
            .map(|row| {
                (
                    row.item.info_hashes.join(";"),
                    (row.command_revision, row.durable),
                )
            })
            .collect();
        self.native_instance_id = Some(snapshot.instance_id);
        let mut previous: HashMap<_, _> = std::mem::take(&mut self.torrents)
            .into_iter()
            .map(|item| (item.info_hashes.join(";"), item))
            .collect();
        self.torrents = snapshot
            .torrents
            .into_iter()
            .map(|mut row| {
                let id = row.item.info_hashes.join(";");
                if same_owner
                    && snapshot.detail_torrent.as_deref() == Some(id.as_str())
                    && self
                        .observed
                        .get(&id)
                        .is_some_and(|old| old.command_revision == row.command_revision)
                    && !row.item.files.details_loaded()
                    && let Some(mut old) = previous.remove(&id)
                    && old.files.details_loaded()
                {
                    Self::preserve_native_file_details(&mut row, &mut old);
                }
                row.item
            })
            .collect();
        self.observed = snapshot
            .observations
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        self.native_durability = native_durability;
        self.unpersisted_updates.clear();
        self.pending_removals.clear();
        self.pending_restore.clear();
        self.pending_priority_restore.clear();
        self.admitted_priority_restore.clear();
        self.detached_removals.clear();
        self.invalidate_torrent_index();
        // Publish the owner's acknowledged projection directly. Re-deriving it
        // from presentation drafts would manufacture command acknowledgements.
        self.reader
            .publish(self.observed.values().cloned().collect());
        self.dirty = false;
        self.frontend_revision = self.frontend_revision.wrapping_add(1);
        for notice in snapshot.notices {
            if self.notices.len() == 256 {
                self.notices.pop_front();
            }
            self.notices.push_back(notice);
        }
    }

    /// Durability is an explicit owner snapshot fact, never inferred from a
    /// frontend mirror's lack of local SQLite work. Revisions must still match.
    pub fn native_command_evidence(&self, id: &str) -> Option<(u64, bool)> {
        let instance = self.native_instance_id.as_ref()?;
        if let Some(client) = self.backend.attached_client() {
            let connection = client.connection();
            if connection.status != frontend::ConnectionStatus::Connected
                || connection.owner_instance.as_ref() != Some(instance)
            {
                return None;
            }
        }
        let canonical = self
            .observed
            .get_key_value(id)
            .map(|(id, _)| id)
            .or_else(|| {
                self.torrent_indices
                    .get(id)
                    .and_then(|&index| self.canonical_ids.get(index))
            })?;
        let observed = self.observed.get(canonical)?;
        self.native_durability
            .get(canonical)
            .copied()
            .filter(|(revision, _)| *revision == observed.command_revision)
    }

    fn preserve_native_file_details(row: &mut frontend::NativeTorrent, old: &mut TorrentItem) {
        // Command acceptance and engine metadata ACKs are independent. Equal
        // selected counts or command revisions cannot authorize stale bitmaps.
        if old.files.metadata_revision() != row.file_revision {
            return;
        }
        let summary = row.item.files.summary();
        let updates_ok = row.file_updates.as_ref().is_none_or(|update| {
            old.files.apply_persistence_snapshot(update).is_ok()
                && old.files.metadata_revision() == row.file_revision
        });
        if updates_ok
            && old.files.apply_summary(summary).is_ok()
            && old.files.adopt_metadata_revision(row.file_revision).is_ok()
        {
            std::mem::swap(&mut row.item.files, &mut old.files);
        }
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
        if let Some(client) = self.backend.attached_client() {
            return client.add(&torrent, settings, audit.as_ref());
        }
        if torrent.files.selected_indices().is_none() {
            return Err("Complete authoritative file selection is unavailable".into());
        }
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
        let has_priorities = !torrent.files.high_priority().is_empty();
        self.torrents.push(torrent);
        if has_priorities {
            self.pending_restore.insert(id);
        }
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
mod feedback_tests;
#[cfg(test)]
mod native_file_tests;
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
