use super::super::{
    AcceptedCommand, CommandError, TorrentCommand, TorrentObservation,
    access::{ClientGrant, Grants},
    operations::Operation,
};
use crate::{
    client::SessionNotice,
    completion::CompletionStatus,
    diagnostics::{PeerSnapshot, RateSample},
    file_selection::{CompactFileUpdate, FileSelectionModel, FileSummary},
    runtime_status::{
        ModeApplication, RepairStatus, RuntimeStatus, TransferAction, TransferFeedback,
    },
    settings::ClientSettings,
    settings::speed_policy::{SpeedPolicy, SpeedView},
    state::{StoredFile, StoredTrackerAuditProfile},
    storage::relocation::StorageStatus,
    torrent::{TorrentItem, TorrentOverrides, TorrentState},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageAction {
    Select { directory: String, copy: bool },
    Cancel,
    Priorities { high: Vec<usize> },
    Cleanup,
    Move { parent: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrashScope {
    DownloadedFiles,
    TorrentFile,
    Everything,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashPlan {
    pub id: String,
    pub plan_id: String,
    pub digest: String,
    pub root: String,
    pub source: String,
    pub count: usize,
    pub preview: Vec<String>,
    pub revision: u64,
    pub scope: TrashScope,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AccessAction {
    View,
    Enable(bool),
    Grant(Grants),
    Revoke(String),
    RevokeAll,
    Approve { operation: String, allow: bool },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccessView {
    pub enabled: bool,
    pub clients: Vec<ClientGrant>,
    pub approvals: Vec<(Operation, String)>,
    pub import_root: PathBuf,
    pub download_root: PathBuf,
    pub export_root: PathBuf,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewTorrent {
    pub source: String,
    pub hashes: Vec<String>,
    pub trackers: Vec<String>,
    pub peer_id: String,
    pub overrides: TorrentOverrides,
    pub selected: PackedIndices,
    pub high: PackedIndices,
    pub start: bool,
    pub audit: Option<StoredTrackerAuditProfile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackedIndices {
    pub count: usize,
    pub bits: Vec<u8>,
}
impl PackedIndices {
    pub fn new(count: usize, indices: &[usize]) -> Result<Self, Error> {
        if count > crate::limits::ResourceLimits::default().max_files {
            return Err(Error::TooLarge);
        }
        let mut bits = vec![0; count.div_ceil(8)];
        for &index in indices {
            if index >= count {
                return Err(Error::Failed("File index is out of range".into()));
            }
            bits[index / 8] |= 1 << (index % 8);
        }
        Ok(Self { count, bits })
    }
    pub fn indices(&self) -> Result<Vec<usize>, Error> {
        if self.count > crate::limits::ResourceLimits::default().max_files
            || self.bits.len() != self.count.div_ceil(8)
        {
            return Err(Error::TooLarge);
        }
        Ok((0..self.count)
            .filter(|index| self.bits[index / 8] & (1 << (index % 8)) != 0)
            .collect())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    Hello,
    Snapshot {
        offset: usize,
        limit: usize,
        revision: Option<u64>,
        detail: Option<String>,
    },
    Files {
        id: String,
        offset: usize,
        limit: usize,
        revision: u64,
        file_revision: u64,
        paths: bool,
    },
    Mutate {
        id: String,
        command: TorrentCommand,
        revision: Option<u64>,
    },
    Selection {
        id: String,
        files: PackedIndices,
        revision: u64,
        file_revision: u64,
    },
    Priorities {
        id: String,
        files: PackedIndices,
        revision: u64,
        file_revision: u64,
    },
    Add(NewTorrent),
    Retry {
        id: String,
        revision: u64,
    },
    Storage {
        id: String,
        action: StorageAction,
        revision: u64,
    },
    Interest {
        id: Option<String>,
        files: bool,
        peers: bool,
    },
    ConfigureAgents {
        download_root: PathBuf,
    },
    AgentSettings(ClientSettings),
    Access(AccessAction),
    SaveSettings(ClientSettings),
    SaveView(crate::settings::UiSettings),
    ApplySettings(ClientSettings),
    SpeedState,
    SetSpeedPolicy(SpeedPolicy),
    SaveEditable {
        id: String,
        revision: u64,
        trackers: Vec<String>,
        audit: Option<StoredTrackerAuditProfile>,
    },
    TrackerAudit {
        id: String,
    },
    SaveAudit {
        id: String,
        revision: u64,
        audit: Option<StoredTrackerAuditProfile>,
    },
    Journal {
        id: Option<String>,
        offset: usize,
        limit: usize,
    },
    Notices {
        after: u64,
        limit: usize,
    },
    Operations {
        before: Option<u64>,
        limit: usize,
    },
    Export {
        id: Option<String>,
        destination: PathBuf,
    },
    PrepareTrash {
        id: String,
        revision: u64,
        scope: TrashScope,
    },
    Trash {
        plan_id: String,
        digest: String,
    },
    Stop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub request_id: String,
    pub client_id: String,
    pub token: String,
    pub instance_id: Option<String>,
    pub action: Action,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Hello {
        settings: Box<ClientSettings>,
        instance_id: String,
    },
    Snapshot(Page),
    SpeedState(SpeedView),
    Files(FilePage),
    Accepted(AcceptedCommand),
    Access(AccessView),
    TrackerAudit {
        id: String,
        audit: Option<StoredTrackerAuditProfile>,
    },
    TrashPlan(TrashPlan),
    Notices {
        cursor: u64,
        resync: bool,
        entries: Vec<SessionNotice>,
    },
    Operations(NativeOperationPage),
    Journal {
        entries: Vec<crate::announce_journal::JournalEntry>,
        total: usize,
        path: Option<PathBuf>,
        error: Option<String>,
    },
    Done(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Error {
    Unauthorized,
    Incompatible,
    Stale,
    TooLarge,
    Busy,
    Command(CommandError),
    Failed(String),
    /// The request may have reached the owner without a verifiable reply.
    Transport(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("Native frontend authentication failed"),
            Self::Incompatible => {
                f.write_str("The background owner uses an incompatible native protocol")
            }
            Self::Stale => {
                f.write_str("Background owner or torrent changed; refresh before retrying")
            }
            Self::TooLarge => f.write_str("Native frontend frame exceeds its bounded limit"),
            Self::Busy => f.write_str("Background owner is busy; retry after refreshing"),
            Self::Command(error) => error.fmt(f),
            Self::Failed(message) | Self::Transport(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub request_id: String,
    pub instance_id: String,
    pub operation_id: Option<String>,
    pub result: Result<Reply, Error>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOperationState {
    Queued,
    Accepted,
    Running,
    EffectConfirmed,
    Succeeded,
    Failed,
    NeedsReview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperationProgress {
    pub bytes: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperation {
    pub id: String,
    pub sequence: u64,
    pub owner_instance: String,
    pub request_id: String,
    pub target: Option<String>,
    pub command: String,
    pub revision: Option<u64>,
    pub state: NativeOperationState,
    pub progress: Option<NativeOperationProgress>,
    pub message: Option<String>,
    pub created_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperationPage {
    pub entries: Vec<NativeOperation>,
    pub next_before: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub revision: u64,
    pub epoch: u64,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub torrents: Vec<WireTorrent>,
    pub observations: Vec<TorrentObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilePage {
    pub id: String,
    pub revision: u64,
    pub file_revision: u64,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub files: Vec<StoredFile>,
}

pub struct NativeTorrent {
    pub item: TorrentItem,
    pub command_revision: u64,
    pub file_revision: u64,
    pub durable: bool,
    pub file_updates: Option<CompactFileUpdate>,
}

pub struct Snapshot {
    pub instance_id: String,
    pub revision: u64,
    pub torrents: Vec<NativeTorrent>,
    pub observations: Vec<TorrentObservation>,
    pub notices: Vec<SessionNotice>,
    pub detail_torrent: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireTorrent {
    pub name: String,
    pub source: String,
    pub info_hashes: Vec<String>,
    pub trackers: Vec<String>,
    pub peer_id: String,
    pub is_private: bool,
    pub overrides: TorrentOverrides,
    pub summary: FileSummary,
    pub state: TorrentState,
    pub completion: CompletionStatus,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_rate: u64,
    pub upload_rate: u64,
    pub peers: usize,
    pub seeds: usize,
    pub runtime: RuntimeWire,
    pub command_revision: u64,
    pub file_revision: u64,
    pub durable: bool,
}

impl WireTorrent {
    pub fn capture(item: &TorrentItem, revision: u64, durable: bool, detail: bool) -> Self {
        Self {
            name: item.name.clone(),
            source: item.source.clone(),
            info_hashes: item.info_hashes.clone(),
            trackers: item.trackers.clone(),
            peer_id: item.peer_id.clone(),
            is_private: item.is_private,
            overrides: item.overrides.clone(),
            summary: item.files.summary(),
            state: item.state,
            completion: item.completion.clone(),
            downloaded_bytes: item.downloaded_bytes,
            uploaded_bytes: item.uploaded_bytes,
            download_rate: item.download_rate,
            upload_rate: item.upload_rate,
            peers: item.peers,
            seeds: item.seeds,
            runtime: RuntimeWire::capture(&item.runtime, detail),
            command_revision: revision,
            file_revision: item.files.metadata_revision(),
            durable,
        }
    }
    pub fn into_native(self) -> Result<NativeTorrent, Error> {
        let mut files = FileSelectionModel::from_summary(self.summary);
        files
            .adopt_metadata_revision(self.file_revision)
            .map_err(|_| Error::Stale)?;
        Ok(NativeTorrent {
            command_revision: self.command_revision,
            file_revision: self.file_revision,
            durable: self.durable,
            file_updates: None,
            item: TorrentItem {
                name: self.name,
                source: self.source,
                info_hashes: self.info_hashes,
                trackers: self.trackers,
                peer_id: self.peer_id,
                is_private: self.is_private,
                overrides: self.overrides,
                files,
                state: self.state,
                completion: self.completion,
                downloaded_bytes: self.downloaded_bytes,
                uploaded_bytes: self.uploaded_bytes,
                download_rate: self.download_rate,
                upload_rate: self.upload_rate,
                peers: self.peers,
                seeds: self.seeds,
                runtime: self.runtime.into_runtime(),
            },
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FeedbackWire {
    Idle,
    Pending {
        action: TransferAction,
        elapsed_ms: u64,
    },
    Confirmed(TransferAction),
    Failed {
        action: TransferAction,
        message: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeWire {
    pub registered: bool,
    pub last_error: Option<String>,
    pub command: FeedbackWire,
    pub storage: StorageStatus,
    pub mode: ModeApplication,
    pub mode_draft: Option<crate::runtime_status::ModeConfig>,
    pub repair: RepairStatus,
    pub network_downloaded: u64,
    pub network_uploaded: u64,
    pub seeded_seconds: u64,
    pub history: VecDeque<RateSample>,
    pub peers: Vec<PeerSnapshot>,
    pub queue_waiting: bool,
}
impl RuntimeWire {
    fn capture(status: &RuntimeStatus, detail: bool) -> Self {
        let command = match &status.command {
            TransferFeedback::Idle => FeedbackWire::Idle,
            TransferFeedback::Pending { action, since } => FeedbackWire::Pending {
                action: *action,
                elapsed_ms: since.elapsed().as_millis().min(u64::MAX as u128) as u64,
            },
            TransferFeedback::Confirmed { action } => FeedbackWire::Confirmed(*action),
            TransferFeedback::Failed { action, message } => FeedbackWire::Failed {
                action: *action,
                message: message.clone(),
            },
        };
        Self {
            registered: status.registered,
            last_error: status.last_error.clone(),
            command,
            storage: status.storage.clone(),
            mode: status.mode.clone(),
            mode_draft: status.mode_draft,
            repair: status.repair.clone(),
            network_downloaded: status.network_downloaded,
            network_uploaded: status.network_uploaded,
            seeded_seconds: status.seeded_seconds,
            history: if detail {
                status.diagnostics.history.clone()
            } else {
                VecDeque::new()
            },
            peers: if detail {
                status.diagnostics.peers.clone()
            } else {
                Vec::new()
            },
            queue_waiting: status.diagnostics.queue_waiting,
        }
    }
    fn into_runtime(self) -> RuntimeStatus {
        let command = match self.command {
            FeedbackWire::Idle => TransferFeedback::Idle,
            FeedbackWire::Pending { action, elapsed_ms } => TransferFeedback::Pending {
                action,
                since: Instant::now()
                    .checked_sub(Duration::from_millis(elapsed_ms))
                    .unwrap_or_else(Instant::now),
            },
            FeedbackWire::Confirmed(action) => TransferFeedback::Confirmed { action },
            FeedbackWire::Failed { action, message } => {
                TransferFeedback::Failed { action, message }
            }
        };
        let mut status = RuntimeStatus {
            registered: self.registered,
            last_error: self.last_error,
            command,
            storage: self.storage,
            mode: self.mode,
            mode_draft: self.mode_draft,
            repair: self.repair,
            network_downloaded: self.network_downloaded,
            network_uploaded: self.network_uploaded,
            seeded_seconds: self.seeded_seconds,
            ..Default::default()
        };
        status.diagnostics.history = self.history;
        status.diagnostics.peers = self.peers;
        status.diagnostics.queue_waiting = self.queue_waiting;
        status
    }
}
