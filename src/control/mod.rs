//! Internal application controller. This is not a supported standalone library API.
//!
//! The controller is the only consumer of transfer events. Frontends retain their
//! presentation state and (temporarily) their existing local command/persistence
//! paths. Read handles cannot send commands, consume session events, or own an
//! engine. Local IPC and mutation authorization are separate future slices.

mod commands;
mod observation;
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
        let event = self.backend.try_recv()?;
        Some(self.apply(event))
    }

    fn apply(&mut self, event: SessionEvent) -> SessionNotice {
        let kind = UpdateKind::from_event(&event);
        // Build the public projection only from engine events. Optimistic UI
        // mode drafts, restored counters and local errors are not acknowledgements.
        let event_id = match &event {
            SessionEvent::Mode { id, .. }
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
            | SessionEvent::FilesApplied { id, .. } => Some(id.clone()),
            SessionEvent::Stopped => None,
        };
        let notice = apply_session_event(&mut self.torrents, event);
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
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.reader.close();
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
