//! Bounded, latest-value observation without event stealing or secret-bearing text.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::{
    completion::CompletionStatus, download_mode::DownloadMode, runtime_status::ModeApplication,
    session::SessionEvent, torrent::TorrentItem,
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_PAGE_SIZE: usize = 128;

#[derive(Clone, Copy)]
pub(super) enum UpdateKind {
    State,
    Stats,
    Error,
    Checking,
    VerificationFailed,
    Verification,
    Network,
    Mode,
    Removed,
    Stopped,
    Other,
    Selection,
}

impl UpdateKind {
    pub(super) fn from_event(event: &SessionEvent) -> Self {
        match event {
            // Added acknowledges ownership of a handle, not its transfer state.
            SessionEvent::Added { .. } => Self::Other,
            SessionEvent::StateChanged { .. } => Self::State,
            SessionEvent::Stats { .. } => Self::Stats,
            SessionEvent::Error { .. } => Self::Error,
            SessionEvent::Verification {
                status: CompletionStatus::Checking { .. },
                ..
            } => Self::Checking,
            SessionEvent::Verification {
                status: CompletionStatus::Failed { .. },
                ..
            } => Self::VerificationFailed,
            SessionEvent::Verification { .. } => Self::Verification,
            SessionEvent::NetworkTotals { .. } => Self::Network,
            SessionEvent::Mode { .. } => Self::Mode,
            SessionEvent::Removed { .. } => Self::Removed,
            SessionEvent::Stopped => Self::Stopped,
            SessionEvent::RepairPolicy { .. } | SessionEvent::Repair { .. } => Self::Other,
            SessionEvent::VerifyAfterDownload { .. } => Self::Other,
            SessionEvent::FilesApplied { .. } => Self::Selection,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedState {
    #[default]
    AwaitingEngine,
    Downloading,
    Paused,
    Seeding,
    Checking,
    VerificationFailed,
    Error,
    Removed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedCompletion {
    #[default]
    Pending,
    Checking,
    Verified,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedModeState {
    #[default]
    AwaitingEngine,
    Applying,
    Active,
    Failed,
}

/// Deliberately excludes torrent names, paths, URLs, peer addresses and free-form
/// engine errors. Those require a separate redaction/authorization policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentObservation {
    pub id: String,
    pub command_revision: u64,
    pub state: ObservedState,
    pub completion: ObservedCompletion,
    pub checked_bytes: u64,
    pub check_total_bytes: u64,
    /// Latest engine file progress projected onto the current local selection.
    /// This is not an acknowledgement that a file-selection command applied.
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub network_downloaded_bytes: u64,
    pub network_uploaded_bytes: u64,
    pub connected_peers: usize,
    pub mode_state: ObservedModeState,
    pub active_mode: Option<DownloadMode>,
    pub active_freeze_after_bytes: Option<u64>,
    pub tracker_enabled: Option<bool>,
}

impl TorrentObservation {
    pub(super) fn pending(torrent: &TorrentItem) -> Self {
        Self {
            id: torrent.info_hashes.join(";"),
            command_revision: 0,
            state: ObservedState::AwaitingEngine,
            completion: ObservedCompletion::Pending,
            checked_bytes: 0,
            check_total_bytes: 0,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            network_downloaded_bytes: 0,
            network_uploaded_bytes: 0,
            connected_peers: 0,
            mode_state: ObservedModeState::AwaitingEngine,
            active_mode: None,
            active_freeze_after_bytes: None,
            tracker_enabled: None,
        }
    }

    pub(super) fn update(&mut self, torrent: &TorrentItem, event: UpdateKind) {
        // Project the canonical reducer's result; never independently implement
        // completion, selected-file accounting, mode application or repair logic.
        // The canonical reducer ignores stale stats during/after failed checks.
        // Do not let those samples publish unrelated optimistic UI edits either.
        if matches!(event, UpdateKind::Stats)
            && matches!(
                torrent.completion,
                CompletionStatus::Checking { .. } | CompletionStatus::Failed { .. }
            )
        {
            return;
        }
        match event {
            UpdateKind::State | UpdateKind::Stats | UpdateKind::Error => {
                self.state = match torrent.state {
                    crate::torrent::TorrentState::WaitingForEngine => ObservedState::AwaitingEngine,
                    crate::torrent::TorrentState::Downloading => ObservedState::Downloading,
                    crate::torrent::TorrentState::Paused => ObservedState::Paused,
                    crate::torrent::TorrentState::Seeding => ObservedState::Seeding,
                    crate::torrent::TorrentState::Checking => ObservedState::Checking,
                    crate::torrent::TorrentState::VerificationFailed => {
                        ObservedState::VerificationFailed
                    }
                    crate::torrent::TorrentState::Error => ObservedState::Error,
                };
            }
            UpdateKind::Checking => {
                self.state = ObservedState::Checking;
            }
            UpdateKind::VerificationFailed => {
                self.state = ObservedState::VerificationFailed;
            }
            UpdateKind::Removed => self.state = ObservedState::Removed,
            UpdateKind::Stopped => self.state = ObservedState::Stopped,
            _ => {}
        }
        if matches!(
            event,
            UpdateKind::Stats
                | UpdateKind::Checking
                | UpdateKind::VerificationFailed
                | UpdateKind::Verification
                | UpdateKind::State
                | UpdateKind::Error
                | UpdateKind::Selection
        ) {
            self.completion = match &torrent.completion {
                CompletionStatus::Pending => ObservedCompletion::Pending,
                CompletionStatus::Checking {
                    checked_bytes,
                    total_bytes,
                } => {
                    self.checked_bytes = *checked_bytes;
                    self.check_total_bytes = *total_bytes;
                    ObservedCompletion::Checking
                }
                CompletionStatus::Verified => ObservedCompletion::Verified,
                CompletionStatus::Failed { .. } => ObservedCompletion::Failed,
            };
        }
        if matches!(event, UpdateKind::Stats | UpdateKind::Selection) {
            self.downloaded_bytes = torrent.downloaded_bytes;
            self.uploaded_bytes = torrent.uploaded_bytes;
            self.connected_peers = torrent.peers;
        }
        if matches!(event, UpdateKind::Network) {
            self.network_uploaded_bytes = torrent.runtime.network_uploaded;
            self.network_downloaded_bytes = torrent.runtime.network_downloaded;
        }
        if matches!(event, UpdateKind::Mode) {
            self.active_mode = torrent.runtime.mode.active().map(|config| config.mode);
            self.active_freeze_after_bytes = torrent
                .runtime
                .mode
                .active()
                .map(|config| config.freeze_after_bytes);
            if self.active_mode.is_none() {
                self.tracker_enabled = None;
            }
            self.mode_state = match &torrent.runtime.mode {
                ModeApplication::AwaitingEngine => {
                    self.tracker_enabled = None;
                    ObservedModeState::AwaitingEngine
                }
                ModeApplication::Applying { .. } => ObservedModeState::Applying,
                ModeApplication::Active {
                    tracker_enabled, ..
                } => {
                    self.tracker_enabled = Some(*tracker_enabled);
                    ObservedModeState::Active
                }
                ModeApplication::Failed { .. } => ObservedModeState::Failed,
            };
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub revision: u64,
    pub controller_available: bool,
    pub torrents: Vec<TorrentObservation>,
}

/// One retained latest snapshot for all readers. Clones do not create a session,
/// background task or per-client queue. A reader holds no lock while using a page.
#[derive(Clone, Debug)]
pub struct ReadHandle(Arc<RwLock<Arc<Observation>>>);

impl Default for ReadHandle {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(Arc::new(Observation {
            revision: 0,
            controller_available: true,
            torrents: Vec::new(),
        }))))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPage {
    pub protocol_version: u32,
    pub revision: u64,
    /// False means the owning controller was dropped; remaining fields are stale.
    pub controller_available: bool,
    /// True means intermediate publications are unavailable: refetch page zero.
    pub resync_required: bool,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub torrents: Vec<TorrentObservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadPageError {
    InvalidLimit,
    InvalidOffset,
    RevisionConflict,
}

impl ReadHandle {
    /// expected_revision pins subsequent pages to the first page's version.
    /// after_revision is the reader's own cursor, not shared consumer state.
    pub fn page(
        &self,
        offset: usize,
        limit: usize,
        expected_revision: Option<u64>,
        after_revision: Option<u64>,
    ) -> Result<ReadPage, ReadPageError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(ReadPageError::InvalidLimit);
        }
        let snapshot = Arc::clone(&self.0.read().unwrap_or_else(|error| error.into_inner()));
        if expected_revision.is_some_and(|expected| expected != snapshot.revision)
            || after_revision.is_some_and(|after| after > snapshot.revision)
        {
            return Err(ReadPageError::RevisionConflict);
        }
        if offset > snapshot.torrents.len() {
            return Err(ReadPageError::InvalidOffset);
        }
        let end = offset.saturating_add(limit).min(snapshot.torrents.len());
        Ok(ReadPage {
            protocol_version: PROTOCOL_VERSION,
            revision: snapshot.revision,
            controller_available: snapshot.controller_available,
            resync_required: after_revision
                .is_some_and(|after| snapshot.revision.saturating_sub(after) > 1),
            total: snapshot.torrents.len(),
            next_offset: (end < snapshot.torrents.len()).then_some(end),
            torrents: snapshot.torrents[offset..end].to_vec(),
        })
    }

    pub(super) fn publish(&self, torrents: Vec<TorrentObservation>) {
        let mut snapshot = self.0.write().unwrap_or_else(|error| error.into_inner());
        if snapshot.torrents == torrents {
            return;
        }
        let revision = snapshot
            .revision
            .checked_add(1)
            .expect("observation revision exhausted");
        *snapshot = Arc::new(Observation {
            revision,
            controller_available: true,
            torrents,
        });
    }

    pub(super) fn close(&self) {
        let mut snapshot = self.0.write().unwrap_or_else(|error| error.into_inner());
        let current = Arc::make_mut(&mut snapshot);
        current.revision = current
            .revision
            .checked_add(1)
            .expect("observation revision exhausted");
        current.controller_available = false;
    }
}
