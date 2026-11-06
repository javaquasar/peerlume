use super::{PROTOCOL_VERSION, transport, wire::*};
use crate::{
    client::SessionNotice,
    control::{AcceptedCommand, CommandError, TorrentCommand},
    settings::ClientSettings,
    state::{StoredFile, StoredTrackerAuditProfile},
    torrent::TorrentItem,
};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting(String),
    StopRequested,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct ConnectionView {
    pub status: ConnectionStatus,
    pub owner_instance: Option<String>,
    pub snapshot_serial: u64,
}
struct Inner {
    directory: PathBuf,
    client_id: String,
    credentials: Mutex<(String, String)>,
    settings: Mutex<ClientSettings>,
    shared: Mutex<Shared>,
    jobs: mpsc::SyncSender<Job>,
    stop: AtomicBool,
    wake: Mutex<Option<Arc<tokio::sync::Notify>>>,
    ui_wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    journal: crate::announce_journal::AnnounceJournal,
    queued: AtomicUsize,
    inflight: AtomicBool,
    next_job: AtomicU64,
}
static CLIENTS: OnceLock<Mutex<Vec<Weak<Inner>>>> = OnceLock::new();
static ACTION_ERRORS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

pub fn finish_pending_actions(timeout: Duration) -> Result<(), String> {
    let clients: Vec<_> = CLIENTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    let deadline = Instant::now() + timeout;
    while clients.iter().any(|client| {
        client.queued.load(Ordering::Acquire) > 0 || client.inflight.load(Ordering::Acquire)
    }) {
        if Instant::now() >= deadline {
            return Err("Some native actions were not confirmed by the background owner before frontend exit timed out. The owner was not stopped; reconnect and inspect current state before retrying.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(error) = ACTION_ERRORS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .pop_back()
    {
        Err(format!(
            "A native action failed: {error}. The background owner was not stopped."
        ))
    } else {
        Ok(())
    }
}
struct Shared {
    connection: ConnectionStatus,
    stopping_owner: Option<String>,
    receipt_overflow: u64,
    snapshot_serial: u64,
    delivered_serial: u64,
    latest: Option<Snapshot>,
    revisions: HashMap<String, u64>,
    notices: VecDeque<SessionNotice>,
    receipts: VecDeque<Receipt>,
    interest: Option<(String, crate::session::DetailInterest)>,
    access: AccessView,
    counts: HashMap<String, usize>,
    visible: Option<String>,
    journal_active: bool,
    tokens: VecDeque<String>,
    audits: HashMap<String, (u64, Option<StoredTrackerAuditProfile>)>,
    audit_updates: VecDeque<(String, Option<StoredTrackerAuditProfile>)>,
    delivered_instance: Option<String>,
    delivered_file_revisions: HashMap<String, u64>,
}
struct Job {
    sequence: u64,
    operation: &'static str,
    command: Option<TorrentCommand>,
    completed_by_reply: bool,
    action: Action,
    label: &'static str,
    id: String,
    keep_alive: Arc<Inner>,
    owner_instance: String,
}
#[derive(Default)]
pub struct Receipt {
    pub job_id: u64,
    pub owner_instance: String,
    pub queued: bool,
    pub ambiguous_error: bool,
    pub snapshot_after: u64,
    pub accepted: Option<AcceptedCommand>,
    pub ui_saved: Option<crate::settings::UiSettings>,
    pub operation: &'static str,
    pub command: Option<TorrentCommand>,
    pub completed_by_reply: bool,
    pub label: &'static str,
    pub message: String,
    pub settings: Option<ClientSettings>,
    pub trash_plan: Option<TrashPlan>,
    pub id: String,
    pub audit_saved: Option<(String, Option<StoredTrackerAuditProfile>)>,
    pub trackers_saved: Option<Vec<String>>,
    pub error: Option<String>,
}

impl Client {
    pub fn connect_or_launch(directory: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let directory = directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if let Ok(client) = Self::connect(&directory) {
            return Ok(client);
        }
        let lock = match crate::control::host::InstanceLock::acquire(&directory) {
            Ok(lock) => lock,
            Err(_) => {
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if let Ok(client) = Self::connect(&directory) {
                        return Ok(client);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                return Err("Another or legacy PeerLume owns this profile but does not expose a compatible native endpoint. Close it normally or use its existing interface; no replacement engine was started.".into());
            }
        };
        if !directory.join("settings.toml").exists() && !directory.join("state.sqlite3").exists() {
            let mut settings = ClientSettings::default();
            settings.download.download_directory =
                directory.join("downloads").to_string_lossy().into_owned();
            settings.download.incomplete_directory = directory
                .join("downloads/incomplete")
                .to_string_lossy()
                .into_owned();
            settings
                .save_to(&directory.join("settings.toml"))
                .map_err(|error| error.to_string())?;
        }
        drop(lock);
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let daemon = executable.with_file_name(if cfg!(windows) {
            "peerlume-daemon.exe"
        } else {
            "peerlume-daemon"
        });
        if !daemon.is_file() {
            return Err("peerlume-daemon is missing beside the frontend; build/install the complete application package".into());
        }
        let mut command = Command::new(daemon);
        command
            .arg("--profile")
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe extern "C" {
                fn setsid() -> i32;
            }
            // SAFETY: only the async-signal-safe setsid syscall runs between fork
            // and exec; stdin/stdout/stderr are detached before the child starts.
            unsafe {
                command.pre_exec(|| {
                    if setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        let mut child = spawn_owner(&mut command)
            .map_err(|error| format!("Could not start background owner: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut last = "Background owner is starting".to_owned();
        while Instant::now() < deadline {
            match Self::connect(&directory) {
                Ok(client) => return Ok(client),
                Err(error) => last = error,
            }
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                // A concurrent frontend may have won InstanceLock. Connect once
                // more, but never kill or replace that winning owner.
                std::thread::sleep(Duration::from_millis(100));
                return Self::connect(&directory).map_err(|_|format!("Background owner could not start. Check absolute download paths, configuration and credential availability. {last}"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "Background startup timed out; no second engine was started. {last}"
        ))
    }

    pub fn connect(directory: &Path) -> Result<Self, String> {
        let credentials = transport::read_credentials(directory)?;
        let client_id = crate::control::access::random_id().map_err(|error| error.to_string())?;
        let request = Request {
            protocol: PROTOCOL_VERSION,
            request_id: crate::control::access::random_id().map_err(|error| error.to_string())?,
            client_id: client_id.clone(),
            token: credentials.1.clone(),
            instance_id: Some(credentials.0.clone()),
            action: Action::Hello,
        };
        let response = transport::call_blocking(directory.to_path_buf(), request)
            .map_err(|error| error.to_string())?;
        let Reply::Hello { settings, .. } = response.result.map_err(|error| error.to_string())?
        else {
            return Err("Incompatible native handshake".into());
        };
        let (jobs, receiver) = mpsc::sync_channel(64);
        let inner = Arc::new(Inner {
            directory: directory.to_path_buf(),
            client_id,
            credentials: Mutex::new(credentials),
            settings: Mutex::new(*settings),
            shared: Mutex::new(Shared {
                connection: ConnectionStatus::Connecting,
                stopping_owner: None,
                receipt_overflow: 0,
                snapshot_serial: 0,
                delivered_serial: 0,
                latest: None,
                revisions: HashMap::new(),
                notices: VecDeque::new(),
                receipts: VecDeque::new(),
                interest: None,
                access: Default::default(),
                counts: HashMap::new(),
                visible: None,
                journal_active: false,
                tokens: VecDeque::new(),
                audits: HashMap::new(),
                audit_updates: VecDeque::new(),
                delivered_instance: None,
                delivered_file_revisions: HashMap::new(),
            }),
            jobs,
            stop: AtomicBool::new(false),
            wake: Mutex::new(None),
            ui_wake: Mutex::new(None),
            journal: crate::announce_journal::AnnounceJournal::frontend_view(),
            queued: AtomicUsize::new(0),
            inflight: AtomicBool::new(false),
            next_job: AtomicU64::new(1),
        });
        {
            let mut clients = CLIENTS
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            clients.retain(|client| client.strong_count() > 0);
            clients.push(Arc::downgrade(&inner));
        }
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("peerlume-native-client".into())
            .spawn(move || worker(weak, receiver))
            .map_err(|error| error.to_string())?;
        let client = Self { inner };
        let _ = client.access_action(AccessAction::View);
        Ok(client)
    }
    pub fn settings(&self) -> ClientSettings {
        self.inner
            .settings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    pub fn connection(&self) -> ConnectionView {
        let current = self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
            .clone();
        let shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let status = if shared.connection == ConnectionStatus::Connected
            && shared.delivered_instance.as_deref() != Some(current.as_str())
        {
            ConnectionStatus::Connecting
        } else {
            shared.connection.clone()
        };
        ConnectionView {
            status,
            owner_instance: shared.delivered_instance.clone(),
            snapshot_serial: shared.delivered_serial,
        }
    }
    pub fn take_receipt_overflow(&self) -> u64 {
        let mut shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut shared.receipt_overflow)
    }
    pub fn sync(&self) -> Option<Snapshot> {
        let mut shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshot = shared.latest.take();
        if let Some(snapshot) = &snapshot {
            shared.delivered_serial = shared.snapshot_serial;
            shared.delivered_instance = Some(snapshot.instance_id.clone());
            shared.delivered_file_revisions = snapshot
                .torrents
                .iter()
                .map(|row| (row.item.info_hashes.join(";"), row.file_revision))
                .collect();
        }
        snapshot
    }
    pub fn poll_notice(&self) -> Option<SessionNotice> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .notices
            .pop_front()
    }
    pub fn take_receipts(&self) -> Vec<Receipt> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .receipts
            .drain(..)
            .collect()
    }
    pub fn journal(&self) -> &crate::announce_journal::AnnounceJournal {
        &self.inner.journal
    }
    pub fn set_event_wake(&self, wake: Arc<tokio::sync::Notify>) {
        *self
            .inner
            .wake
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(wake);
    }
    pub fn set_ui_waker(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .inner
            .ui_wake
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(wake);
    }
    pub fn set_visible_torrent(&self, id: Option<String>) {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .visible = id;
    }
    pub fn set_journal_active(&self, active: bool) {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .journal_active = active;
    }
    pub fn audit_ready(&self, id: &str) -> bool {
        let shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        shared
            .audits
            .get(id)
            .is_some_and(|(revision, _)| shared.revisions.get(id) == Some(revision))
    }
    pub fn audit_profile(&self, id: &str) -> Option<StoredTrackerAuditProfile> {
        let shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        shared
            .audits
            .get(id)
            .filter(|(revision, _)| shared.revisions.get(id) == Some(revision))
            .and_then(|(_, profile)| profile.clone())
    }
    pub fn cached_revision(&self, id: &str) -> Option<u64> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revisions
            .get(id)
            .copied()
    }
    pub fn take_access_token(&self) -> Option<String> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .tokens
            .pop_front()
    }
    pub fn take_audits(&self) -> Vec<(String, Option<StoredTrackerAuditProfile>)> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .audit_updates
            .drain(..)
            .collect()
    }
    pub fn set_detail_interest(&self, interest: Option<(String, crate::session::DetailInterest)>) {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .interest = interest;
    }
    pub fn configure_agents(&self, download_root: &Path) -> Result<(), String> {
        self.call(Action::ConfigureAgents {
            download_root: download_root.to_path_buf(),
        })
        .map(|_| ())
    }
    pub fn set_agent_settings(&self, settings: &ClientSettings) -> Result<(), String> {
        self.queue(
            Action::AgentSettings(settings.clone()),
            "Agent defaults",
            String::new(),
        )
    }
    pub fn access_view(&self) -> AccessView {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .access
            .clone()
    }
    pub fn access_action(&self, action: AccessAction) -> Result<(), String> {
        self.queue(Action::Access(action), "Agent access", String::new())
    }
    pub fn save_settings(&self, settings: &ClientSettings) -> Result<(), String> {
        self.queue(
            Action::SaveSettings(settings.clone()),
            "Preferences",
            String::new(),
        )
    }
    pub fn save_view(&self, ui: &crate::settings::UiSettings) -> Result<(), String> {
        self.queue(Action::SaveView(ui.clone()), "View", String::new())
    }
    pub fn apply_settings(&self, settings: &ClientSettings) -> Result<(), String> {
        self.queue(
            Action::ApplySettings(settings.clone()),
            "Apply preferences",
            String::new(),
        )
    }
    pub fn export(&self, id: Option<String>, destination: PathBuf) -> Result<(), String> {
        self.queue(
            Action::Export { id, destination },
            "Journal export",
            String::new(),
        )
    }
    pub fn stop_owner(&self) -> Result<(), String> {
        self.call(Action::Stop).map(|_| ())
    }
    /// GUI shutdown is tied to the owner shown in its confirmation. The CLI's
    /// immediate authenticated Stop intentionally remains a separate entry point.
    pub fn enqueue_stop_owner(&self, displayed_owner: &str) -> Result<u64, String> {
        let view = self.connection();
        if view.status != ConnectionStatus::Connected
            || view.owner_instance.as_deref() != Some(displayed_owner)
        {
            return Err("Background owner changed or is not connected; review Stop again".into());
        }
        self.queue_for_instance(
            Action::Stop,
            "Stop background client",
            String::new(),
            Some(displayed_owner),
        )
    }
    pub fn submit(
        &self,
        id: &str,
        command: &TorrentCommand,
        expected: Option<u64>,
    ) -> Result<AcceptedCommand, CommandError> {
        let action = self
            .command_action(id, command, expected)
            .map_err(|_| CommandError::InvalidFileSelection)?;
        match self.call_raw(action).map_err(|error| match error {
            Error::Command(error) => error,
            _ => CommandError::EngineUnavailable,
        })? {
            Reply::Accepted(accepted) => Ok(accepted),
            _ => Err(CommandError::EngineUnavailable),
        }
    }
    pub fn enqueue_submit(
        &self,
        id: &str,
        command: TorrentCommand,
        expected: Option<u64>,
    ) -> Result<(), String> {
        self.queue(
            self.command_action(id, &command, expected)?,
            "Torrent command",
            id.into(),
        )
    }
    fn command_action(
        &self,
        id: &str,
        command: &TorrentCommand,
        revision: Option<u64>,
    ) -> Result<Action, String> {
        let revision = match revision {
            Some(revision) => revision,
            None => self.revision(id)?,
        };
        if let TorrentCommand::SetFiles(indices) = command {
            let count = self.current_count(id)?;
            Ok(Action::Selection {
                id: id.into(),
                file_revision: self.delivered_file_revision(id)?,
                files: PackedIndices::new(count, indices).map_err(|error| error.to_string())?,
                revision,
            })
        } else {
            Ok(Action::Mutate {
                id: id.into(),
                command: command.clone(),
                revision: Some(revision),
            })
        }
    }
    fn delivered_file_revision(&self, id: &str) -> Result<u64, String> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .delivered_file_revisions
            .get(id)
            .copied()
            .filter(|revision| *revision != 0 && *revision != u64::MAX)
            .ok_or_else(|| {
                "File selection snapshot has not reached this frontend; wait for refresh".into()
            })
    }
    fn current_count(&self, id: &str) -> Result<usize, String> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .counts
            .get(id)
            .copied()
            .ok_or_else(|| "File summary is not ready".into())
    }
    pub fn add(
        &self,
        item: &TorrentItem,
        _settings: &ClientSettings,
        audit: Option<&StoredTrackerAuditProfile>,
    ) -> Result<(), String> {
        let selected = item
            .files
            .selected_indices()
            .ok_or("Complete selection is not loaded")?;
        let request = NewTorrent {
            source: item.source.clone(),
            hashes: item.info_hashes.clone(),
            trackers: item.trackers.clone(),
            peer_id: item.peer_id.clone(),
            overrides: item.overrides.clone(),
            selected: PackedIndices::new(item.files.file_count(), &selected)
                .map_err(|error| error.to_string())?,
            high: PackedIndices::new(item.files.file_count(), &item.files.high_priority())
                .map_err(|error| error.to_string())?,
            start: item.state == crate::torrent::TorrentState::WaitingForEngine,
            audit: audit.cloned(),
        };
        self.queue(
            Action::Add(request),
            "Torrent registration",
            item.info_hashes.join(";"),
        )
    }
    pub fn retry_start(&self, id: &str, _settings: &ClientSettings) -> Result<(), String> {
        self.queue(
            Action::Retry {
                id: id.into(),
                revision: self.revision(id)?,
            },
            "Retry start",
            id.into(),
        )
    }
    pub fn storage(&self, id: &str, action: StorageAction) -> Result<(), String> {
        let revision = self.revision(id)?;
        let action = if let StorageAction::Priorities { high } = action {
            Action::Priorities {
                id: id.into(),
                file_revision: self.delivered_file_revision(id)?,
                files: PackedIndices::new(self.current_count(id)?, &high)
                    .map_err(|error| error.to_string())?,
                revision,
            }
        } else {
            Action::Storage {
                id: id.into(),
                action,
                revision,
            }
        };
        self.queue(action, "Storage command", id.into())
    }
    pub fn save_editable(
        &self,
        id: &str,
        trackers: &[String],
        audit: Option<&StoredTrackerAuditProfile>,
    ) -> Result<(), String> {
        self.queue(
            Action::SaveEditable {
                id: id.into(),
                revision: self.revision(id)?,
                trackers: trackers.to_vec(),
                audit: audit.cloned(),
            },
            "Tracker settings",
            id.into(),
        )
    }
    pub fn save_audit(
        &self,
        id: &str,
        audit: Option<&StoredTrackerAuditProfile>,
    ) -> Result<(), String> {
        self.queue(
            Action::SaveAudit {
                id: id.into(),
                revision: self.revision(id)?,
                audit: audit.cloned(),
            },
            "Tracker audit",
            id.into(),
        )
    }
    pub fn prepare_trash(&self, id: &str, scope: TrashScope) -> Result<(), String> {
        self.queue(
            Action::PrepareTrash {
                id: id.into(),
                revision: self.revision(id)?,
                scope,
            },
            "Prepare Recycle Bin operation",
            id.into(),
        )
    }
    pub fn commit_trash(&self, plan: &TrashPlan) -> Result<(), String> {
        self.queue(
            Action::Trash {
                plan_id: plan.plan_id.clone(),
                digest: plan.digest.clone(),
            },
            "Recycle Bin operation",
            plan.id.clone(),
        )
    }
    fn revision(&self, id: &str) -> Result<u64, String> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revisions
            .get(id)
            .copied()
            .ok_or_else(|| "Owner snapshot is not ready; refresh first".into())
    }
    fn queue(&self, action: Action, label: &'static str, id: String) -> Result<(), String> {
        self.queue_for_instance(action, label, id, None).map(|_| ())
    }
    fn queue_for_instance(
        &self,
        action: Action,
        label: &'static str,
        id: String,
        expected_owner: Option<&str>,
    ) -> Result<u64, String> {
        let owner_instance = self
            .inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
            .clone();
        if expected_owner.is_some_and(|expected| expected != owner_instance) {
            return Err("Background owner changed; the request was not queued".into());
        }
        if !self.inner.snapshot_allows(&action, &owner_instance) {
            return Err("Wait for an authoritative snapshot of the current background owner; the action was not queued".into());
        }
        let quiet = matches!(&action, Action::Access(AccessAction::View));
        let mut shared = self
            .inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !quiet && shared.connection != ConnectionStatus::Connected {
            return Err("Background client is not connected; wait for its current state before changing downloads".into());
        }
        let sequence = self
            .inner
            .next_job
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .map_err(|_| "Native operation ID space exhausted".to_owned())?;
        let operation = operation_name(&action, label);
        let command = if let Action::Mutate { command, .. } = &action {
            Some(command.clone())
        } else {
            None
        };
        let completed_by_reply = matches!(
            &action,
            Action::SaveView(_)
                | Action::SaveSettings(_)
                | Action::SaveEditable { .. }
                | Action::SaveAudit { .. }
                | Action::Export { .. }
                | Action::Trash { .. }
        );
        self.inner.queued.fetch_add(1, Ordering::Release);
        if self
            .inner
            .jobs
            .try_send(Job {
                sequence,
                operation,
                command: command.clone(),
                completed_by_reply,
                action,
                label,
                id: id.clone(),
                keep_alive: self.inner.clone(),
                owner_instance: owner_instance.clone(),
            })
            .is_err()
        {
            self.inner.queued.fetch_sub(1, Ordering::AcqRel);
            Err("Native request queue is full or disconnected".into())
        } else {
            if !quiet {
                shared.push_receipt(Receipt {
                    job_id: sequence,
                    owner_instance,
                    queued: true,
                    label,
                    operation,
                    command,
                    id,
                    completed_by_reply,
                    message: "Request queued; awaiting background client".into(),
                    ..Default::default()
                });
            }
            drop(shared);
            self.inner.notify();
            Ok(sequence)
        }
    }
    fn call(&self, action: Action) -> Result<Reply, String> {
        self.call_raw(action).map_err(|error| error.to_string())
    }
    fn call_raw(&self, action: Action) -> Result<Reply, Error> {
        let request = self.inner.request(action)?;
        transport::call_blocking(self.inner.directory.clone(), request)?.result
    }
}

impl Shared {
    fn push_receipt(&mut self, receipt: Receipt) {
        if self.receipts.len() == 256 {
            self.receipts.pop_front();
            self.receipt_overflow = self.receipt_overflow.saturating_add(1);
        }
        self.receipts.push_back(receipt);
    }
}

fn operation_name(action: &Action, fallback: &'static str) -> &'static str {
    match action {
        Action::Mutate { command, .. } => match command {
            TorrentCommand::Pause => "Pause",
            TorrentCommand::Resume => "Resume",
            TorrentCommand::Recheck => "Integrity check",
            TorrentCommand::RecheckAndRepair => "Check and repair",
            TorrentCommand::SetMode(_) => "Change download mode",
            TorrentCommand::SetFiles(_) => "Select files",
            TorrentCommand::Remove => "Remove torrent; keep data",
            TorrentCommand::SetRepairPolicy(_) => "Change repair policy",
            TorrentCommand::SetVerifyAfterDownload(_) => "Change verification policy",
            TorrentCommand::SetTransferSettings(_) => "Change transfer settings",
        },
        Action::Selection { .. } => "Select files",
        Action::Priorities { .. } => "Change file priorities",
        Action::Storage { action, .. } => match action {
            StorageAction::Select { copy: true, .. } => "Copy data",
            StorageAction::Select { copy: false, .. } => "Use existing data",
            StorageAction::Cancel => "Cancel copy",
            StorageAction::Priorities { .. } => "Change file priorities",
            StorageAction::Cleanup => "Remove verified duplicate originals",
            StorageAction::Move { .. } => "Move data directory",
        },
        _ => fallback,
    }
}

#[cfg(windows)]
fn spawn_owner(command: &mut Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        SetHandleInformation,
    };
    static SPAWN: Mutex<()> = Mutex::new(());
    let _serialize = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
    struct Restore(Vec<(HANDLE, u32)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for &(handle, flags) in &self.0 {
                // SAFETY: these borrowed standard handles stay owned by this process;
                // restore only the inheritance bit changed before spawning.
                unsafe {
                    SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
                }
            }
        }
    }
    let mut restore = Restore(Vec::new());
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if handle.is_null()
            || handle == INVALID_HANDLE_VALUE
            || restore.0.iter().any(|(saved, _)| *saved == handle)
        {
            continue;
        }
        let mut flags = 0;
        // SAFETY: GetHandleInformation only queries the borrowed standard handle.
        if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if flags & HANDLE_FLAG_INHERIT != 0 {
            // SAFETY: temporarily clear only inheritance; RAII restores on every
            // spawn/error path. The child receives explicit null stdio instead.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            restore.0.push((handle, flags));
        }
    }
    command.spawn()
}
#[cfg(not(windows))]
fn spawn_owner(command: &mut Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}
impl Inner {
    fn connection_state(&self, requested: ConnectionStatus) {
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let status = if shared.stopping_owner.is_some() {
            if shared.connection == ConnectionStatus::Stopped {
                ConnectionStatus::Stopped
            } else {
                ConnectionStatus::StopRequested
            }
        } else {
            requested
        };
        let changed = shared.connection != status;
        shared.connection = status;
        drop(shared);
        if changed {
            self.notify();
        }
    }

    fn inspect_requested_stop(&self) {
        let stopping = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopping_owner
            .is_some();
        if !stopping {
            return;
        }
        // This runs on the native worker, never inside a GUI frame. As in the
        // Stop CLI, lock release proves termination, not every durability result.
        if let Ok(lock) = crate::control::host::InstanceLock::acquire(&self.directory) {
            drop(lock);
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            shared.connection = ConnectionStatus::Stopped;
            shared.interest = None;
            shared.visible = None;
            drop(shared);
            self.notify();
        }
    }
    fn snapshot_allows(&self, action: &Action, instance: &str) -> bool {
        if matches!(
            action,
            Action::Hello
                | Action::Snapshot { .. }
                | Action::Files { .. }
                | Action::Interest { .. }
                | Action::ConfigureAgents { .. }
                | Action::Notices { .. }
                | Action::TrackerAudit { .. }
                | Action::Journal { .. }
                | Action::Access(AccessAction::View)
                | Action::Stop
        ) {
            return true;
        }
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .delivered_instance
            .as_deref()
            == Some(instance)
    }
    fn job_is_current(&self, job: &Job) -> bool {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
            == job.owner_instance
    }
    fn refresh_credentials(&self) -> bool {
        let Ok(credentials) = transport::read_credentials(&self.directory) else {
            return false;
        };
        let mut current = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *current == credentials {
            return false;
        }
        *current = credentials;
        drop(current);
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        shared.latest = None;
        shared.revisions.clear();
        shared.counts.clear();
        shared.audits.clear();
        shared.audit_updates.clear();
        shared.connection = ConnectionStatus::Connecting;
        shared.stopping_owner = None;
        drop(shared);
        let _ = self
            .journal
            .replace_frontend_entries(Vec::new(), None, None);
        true
    }
    fn selected_audit_current(
        &self,
        visible: Option<&str>,
        audit_key: Option<&(String, u64)>,
    ) -> bool {
        visible.is_none_or(|id| {
            audit_key.is_some_and(|(loaded, revision)| {
                loaded == id
                    && self
                        .shared
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .revisions
                        .get(id)
                        == Some(revision)
            })
        })
    }
    fn apply_access(&self, mut view: AccessView) {
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(token) = view.token.take() {
            if shared.tokens.len() == 8 {
                shared.tokens.pop_front();
            }
            shared.tokens.push_back(token);
        }
        shared.access = view;
    }
    fn request(&self, action: Action) -> Result<Request, Error> {
        let credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.snapshot_allows(&action, &credentials.0) {
            return Err(Error::Stale);
        }
        Ok(Request {
            protocol: PROTOCOL_VERSION,
            request_id: crate::control::access::random_id().map_err(|_| Error::Busy)?,
            client_id: self.client_id.clone(),
            token: credentials.1.clone(),
            instance_id: Some(credentials.0.clone()),
            action,
        })
    }
    async fn call(&self, action: Action) -> Result<Reply, Error> {
        transport::call(&self.directory, &self.request(action)?)
            .await?
            .result
    }
    fn notify(&self) {
        if let Some(wake) = &*self.wake.lock().unwrap_or_else(|error| error.into_inner()) {
            wake.notify_one();
        }
        let wake = self
            .ui_wake
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    fn error(&self, id: String, message: String) {
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if shared.notices.len() == 256 {
            shared.notices.pop_front();
        }
        shared
            .notices
            .push_back(SessionNotice::Error { id, message });
        drop(shared);
        self.notify();
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn worker(weak: Weak<Inner>, receiver: mpsc::Receiver<Job>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let mut next = Instant::now();
    let mut disconnected = false;
    let mut loaded: Option<(String, u64, u64, usize, u64)> = None;
    let mut failed_detail: Option<(String, u64, u64, usize, u64)> = None;
    let mut notice_cursor = 0;
    let mut owner_instance = String::new();
    let mut audit_key: Option<(String, u64)> = None;
    let mut journal_updated = Instant::now() - Duration::from_secs(2);
    let mut last_revision = None;
    let mut last_view = None;
    let mut was_live = false;
    loop {
        let timeout = next.saturating_duration_since(Instant::now());
        let job = match receiver.recv_timeout(timeout) {
            Ok(job) => Some(job),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => break,
        };
        let Some(inner) = job
            .as_ref()
            .map(|job| job.keep_alive.clone())
            .or_else(|| weak.upgrade())
        else {
            break;
        };
        if inner.stop.load(Ordering::Acquire) {
            break;
        }
        if let Some(job) = job {
            let receipt_context = Receipt {
                job_id: job.sequence,
                owner_instance: job.owner_instance.clone(),
                operation: job.operation,
                command: job.command.clone(),
                ..Default::default()
            };
            let requests_stop = matches!(&job.action, Action::Stop);
            inner.inflight.store(true, Ordering::Release);
            inner.queued.fetch_sub(1, Ordering::AcqRel);
            let quiet = matches!(&job.action, Action::Access(AccessAction::View));
            let audit_saved = match &job.action {
                Action::SaveAudit { id, audit, .. } | Action::SaveEditable { id, audit, .. } => {
                    Some((id.clone(), audit.clone()))
                }
                _ => None,
            };
            let trackers_saved = if let Action::SaveEditable { trackers, .. } = &job.action {
                Some(trackers.clone())
            } else {
                None
            };
            let settings = if let Action::SaveSettings(settings) | Action::ApplySettings(settings) =
                &job.action
            {
                Some(settings.clone())
            } else {
                None
            };
            let view = if let Action::SaveView(view) = &job.action {
                Some(view.clone())
            } else {
                None
            };
            let ui_saved = view.clone();
            let response = if inner.job_is_current(&job) {
                runtime.block_on(inner.call(job.action))
            } else {
                Err(Error::Stale)
            };
            match response {
                Ok(reply) => {
                    if let Some(settings) = &settings {
                        *inner
                            .settings
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = settings.clone();
                    }
                    if let Some(view) = view {
                        inner
                            .settings
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .ui = view;
                    }
                    let mut trash_plan = None;
                    let mut accepted = None;
                    let message = match reply {
                        Reply::Done(message) => message,
                        Reply::TrashPlan(plan) => {
                            trash_plan = Some(plan);
                            "Review the exact Recycle Bin plan before confirming".into()
                        }
                        Reply::Access(view) => {
                            inner.apply_access(view);
                            "Agent access updated".into()
                        }
                        Reply::Accepted(value) => {
                            accepted = Some(value);
                            "Owner accepted the command; awaiting engine confirmation and persistence".into()
                        }
                        _ => "Owner response received".into(),
                    };
                    let mut shared = inner
                        .shared
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if !quiet {
                        let snapshot_after = shared.snapshot_serial.saturating_add(1);
                        if requests_stop {
                            shared.stopping_owner = Some(job.owner_instance.clone());
                            shared.connection = ConnectionStatus::StopRequested;
                        }
                        shared.push_receipt(Receipt {
                            label: job.label,
                            message,
                            settings,
                            trash_plan,
                            id: job.id.clone(),
                            audit_saved,
                            trackers_saved,
                            error: None,
                            accepted,
                            ui_saved,
                            completed_by_reply: job.completed_by_reply,
                            snapshot_after,
                            ..receipt_context
                        });
                    } else {
                        shared.push_receipt(Receipt {
                            label: "Agent view",
                            message: String::new(),
                            settings: None,
                            trash_plan: None,
                            id: String::new(),
                            audit_saved: None,
                            trackers_saved: None,
                            error: None,
                            ..receipt_context
                        });
                    }
                    drop(shared);
                    inner.notify();
                }
                Err(error) => {
                    let message = format!("{}: {error}", job.label);
                    let mut errors = ACTION_ERRORS
                        .get_or_init(Default::default)
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if errors.len() == 64 {
                        errors.pop_front();
                    }
                    errors.push_back(message.clone());
                    drop(errors);
                    let mut shared = inner
                        .shared
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    shared.push_receipt(Receipt {
                        label: job.label,
                        message: message.clone(),
                        settings: None,
                        trash_plan: None,
                        id: job.id.clone(),
                        audit_saved: None,
                        trackers_saved: None,
                        error: Some(message.clone()),
                        ambiguous_error: matches!(error, Error::Transport(_)),
                        ..receipt_context
                    });
                    drop(shared);
                    inner.error(job.id, message);
                }
            }
            inner.inflight.store(false, Ordering::Release);
            next = Instant::now();
        }
        if Instant::now() < next {
            continue;
        }
        let interest = inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .interest
            .clone();
        let visible = inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .visible
            .clone();
        let instance = inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
            .clone();
        if owner_instance != instance {
            owner_instance = instance;
            notice_cursor = 0;
            loaded = None;
            failed_detail = None;
            audit_key = None;
            last_revision = None;
            inner
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .audits
                .clear();
        }
        if let Ok(Reply::Notices {
            cursor,
            resync,
            entries,
        }) = runtime.block_on(inner.call(Action::Notices {
            after: notice_cursor,
            limit: 128,
        })) {
            notice_cursor = cursor;
            if resync {
                inner.error(String::new(),"Owner diagnostic history exceeded the bounded live window; current state was resynchronized".into());
            }
            if !entries.is_empty() {
                let mut shared = inner
                    .shared
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                for entry in entries {
                    if shared.notices.len() == 256 {
                        shared.notices.pop_front();
                    }
                    shared.notices.push_back(entry);
                }
                drop(shared);
                inner.notify();
            }
        }
        if let Some(id) = visible.as_ref()
            && inner
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .journal_active
            && journal_updated.elapsed() >= Duration::from_secs(1)
        {
            if let Err(error) = runtime.block_on(fetch_journal(&inner, id)) {
                inner.error(id.clone(), format!("Journal view: {error}"));
            }
            journal_updated = Instant::now();
        }
        let view = (visible.clone(), interest.clone());
        let audit_ready = inner.selected_audit_current(visible.as_deref(), audit_key.as_ref());
        let unchanged = if last_view.as_ref() == Some(&view)
            && audit_ready
            && (interest.as_ref().is_none_or(|(_, detail)| !detail.files)
                || loaded.is_some()
                || failed_detail.is_some())
        {
            last_revision
        } else {
            None
        };
        let result = runtime.block_on(async {
            inner
                .call(Action::Interest {
                    id: interest.as_ref().map(|(id, _)| id.clone()),
                    files: interest.as_ref().is_some_and(|(_, detail)| detail.files),
                    peers: interest.as_ref().is_some_and(|(_, detail)| detail.peers),
                })
                .await?;
            fetch_snapshot(&inner, visible.clone(), unchanged).await
        });
        match result {
            Ok(None) => {
                disconnected = false;
                inner.connection_state(ConnectionStatus::Connected);
                next = Instant::now()
                    + if was_live {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_secs(1)
                    };
            }
            Ok(Some(mut snapshot)) => {
                inner.connection_state(ConnectionStatus::Connected);
                last_revision = Some(snapshot.revision);
                last_view = Some(view);
                if owner_instance != snapshot.instance_id {
                    owner_instance = snapshot.instance_id.clone();
                    notice_cursor = 0;
                    loaded = None;
                    failed_detail = None;
                    audit_key = None;
                    inner
                        .shared
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .audits
                        .clear();
                }
                snapshot.detail_torrent = interest
                    .as_ref()
                    .filter(|(_, detail)| detail.files)
                    .map(|(id, _)| id.clone());
                disconnected = false;
                if let Some((id, detail)) = &interest
                    && detail.files
                {
                    let geometry = snapshot
                        .torrents
                        .iter()
                        .find(|row| row.item.info_hashes.join(";") == *id)
                        .map(|row| {
                            (
                                id.clone(),
                                row.command_revision,
                                row.file_revision,
                                row.item.files.file_count(),
                                row.item.files.total_size(),
                            )
                        });
                    if geometry != failed_detail {
                        failed_detail = None;
                    }
                    match if failed_detail.is_some() {
                        Ok(None)
                    } else {
                        runtime.block_on(fetch_files(
                            &inner,
                            id,
                            snapshot
                                .torrents
                                .iter()
                                .find(|row| row.item.info_hashes.join(";") == *id)
                                .map(|row| {
                                    (
                                        row.command_revision,
                                        row.file_revision,
                                        row.item.files.summary(),
                                    )
                                }),
                            loaded.as_ref(),
                        ))
                    } {
                        Ok(Some((files, updates, revision))) => {
                            if let Some(row) = snapshot
                                .torrents
                                .iter_mut()
                                .find(|row| row.item.info_hashes.join(";") == *id)
                            {
                                if let Some(files) = files {
                                    row.item.files = files;
                                }
                                row.file_updates = updates;
                                loaded = Some((
                                    id.clone(),
                                    revision,
                                    row.file_revision,
                                    row.item.files.file_count(),
                                    row.item.files.total_size(),
                                ));
                            }
                        }
                        Ok(None) => {}
                        Err(Error::Stale) => loaded = None,
                        Err(Error::Busy) => {}
                        Err(Error::TooLarge) => {
                            failed_detail = geometry;
                            inner.error(id.clone(),"File details exceed the 64 MiB path budget; close Files and reopen to retry".into());
                        }
                        Err(error) => inner.error(id.clone(), format!("File details: {error}")),
                    }
                } else {
                    loaded = None;
                    failed_detail = None;
                }
                if let Some(id) = visible.as_ref() {
                    let revision = snapshot
                        .torrents
                        .iter()
                        .find(|row| row.item.info_hashes.join(";") == *id)
                        .map(|row| row.command_revision);
                    if let Some(revision) = revision {
                        if audit_key.as_ref() != Some(&(id.clone(), revision))
                            && let Ok(Reply::TrackerAudit { id, audit }) = runtime
                                .block_on(inner.call(Action::TrackerAudit { id: id.clone() }))
                        {
                            let mut shared = inner
                                .shared
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            shared.audits.insert(id.clone(), (revision, audit.clone()));
                            if shared.audit_updates.len() == 64 {
                                shared.audit_updates.pop_front();
                            }
                            shared.audit_updates.push_back((id.clone(), audit));
                            audit_key = Some((id, revision));
                        }
                        if inner
                            .shared
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .journal_active
                            && journal_updated.elapsed() >= Duration::from_secs(1)
                        {
                            if let Err(error) = runtime.block_on(fetch_journal(&inner, id)) {
                                inner.error(id.clone(), format!("Journal view: {error}"));
                            }
                            journal_updated = Instant::now();
                        }
                    }
                }
                let live = snapshot.torrents.iter().any(|row| {
                    let item = &row.item;
                    item.download_rate > 0
                        || item.upload_rate > 0
                        || item.runtime.command.pending_action().is_some()
                        || matches!(
                            item.state,
                            crate::torrent::TorrentState::Downloading
                                | crate::torrent::TorrentState::Checking
                                | crate::torrent::TorrentState::WaitingForEngine
                        )
                });
                was_live = live;
                let mut shared = inner
                    .shared
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                shared.revisions = snapshot
                    .torrents
                    .iter()
                    .map(|row| (row.item.info_hashes.join(";"), row.command_revision))
                    .collect();
                shared.counts = snapshot
                    .torrents
                    .iter()
                    .map(|row| (row.item.info_hashes.join(";"), row.item.files.file_count()))
                    .collect();
                retain_pending_details(shared.latest.take(), &mut snapshot);
                shared.snapshot_serial = shared.snapshot_serial.saturating_add(1);
                shared.latest = Some(snapshot);
                drop(shared);
                inner.notify();
                next = Instant::now()
                    + if live {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_secs(1)
                    };
            }
            Err(Error::Stale) => {
                inner.connection_state(ConnectionStatus::Connecting);
                if inner.refresh_credentials() {
                    loaded = None;
                    failed_detail = None;
                    audit_key = None;
                    notice_cursor = 0;
                    owner_instance.clear();
                    last_view = None;
                    if let Ok(Reply::Hello { settings, .. }) =
                        runtime.block_on(inner.call(Action::Hello))
                    {
                        *inner
                            .settings
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = *settings;
                    }
                }
                last_revision = None;
                next = Instant::now() + Duration::from_millis(50);
            }
            Err(Error::Busy) => {
                last_revision = None;
                next = Instant::now() + Duration::from_millis(50);
            }
            Err(error) => {
                inner.connection_state(ConnectionStatus::Reconnecting(error.to_string()));
                inner.inspect_requested_stop();
                last_revision = None;
                if !disconnected {
                    inner.error(
                        String::new(),
                        format!(
                            "Background owner disconnected: {error}; transfers may still be running"
                        ),
                    );
                    disconnected = true;
                }
                if inner.refresh_credentials() {
                    loaded = None;
                    failed_detail = None;
                    audit_key = None;
                    notice_cursor = 0;
                    owner_instance.clear();
                    last_view = None;
                }
                next = Instant::now() + Duration::from_secs(1);
            }
        }
    }
}

async fn fetch_journal(inner: &Inner, id: &str) -> Result<(), Error> {
    let mut entries = Vec::new();
    let mut offset = 0;
    let mut limit = 32;
    let mut retained = 0usize;
    let (path, journal_error) = loop {
        if inner.queued.load(Ordering::Acquire) > 0 {
            return Ok(());
        }
        {
            let shared = inner
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !shared.journal_active || shared.visible.as_deref() != Some(id) {
                return Ok(());
            }
        }
        match inner
            .call(Action::Journal {
                id: Some(id.into()),
                offset,
                limit,
            })
            .await
        {
            Ok(Reply::Journal {
                entries: page,
                total,
                path,
                error,
            }) => {
                if total > crate::announce_journal::LIVE_CAPACITY {
                    return Err(Error::TooLarge);
                }
                let count = page.len();
                for entry in page {
                    retained = retained.saturating_add(entry.retained_bytes());
                    if retained > crate::announce_journal::LIVE_BYTE_BUDGET {
                        return Err(Error::TooLarge);
                    }
                    entries.push(entry);
                }
                offset = entries.len();
                if offset >= total || count == 0 {
                    break (path, error);
                }
            }
            Err(Error::TooLarge) if limit > 1 => {
                limit = (limit / 2).max(1);
            }
            Err(error) => return Err(error),
            _ => return Err(Error::Incompatible),
        }
    };
    entries.sort_by_key(|entry| entry.sequence);
    entries.dedup_by_key(|entry| entry.sequence);
    inner
        .journal
        .replace_frontend_entries(entries, path, journal_error)
        .map_err(Error::Failed)
}

fn retain_pending_details(previous: Option<Snapshot>, snapshot: &mut Snapshot) {
    let Some(mut previous) = previous else {
        return;
    };
    if previous.instance_id != snapshot.instance_id {
        return;
    }
    previous.notices.append(&mut snapshot.notices);
    if previous.notices.len() > 256 {
        previous.notices.drain(..previous.notices.len() - 256);
    }
    snapshot.notices = previous.notices;
    let Some(id) = snapshot.detail_torrent.as_ref() else {
        return;
    };
    let Some(old) = previous
        .torrents
        .into_iter()
        .find(|row| row.item.info_hashes.join(";") == *id && row.item.files.details_loaded())
    else {
        return;
    };
    let Some(row) = snapshot.torrents.iter_mut().find(|row| {
        row.item.info_hashes.join(";") == *id
            && row.command_revision == old.command_revision
            && row.file_revision == old.file_revision
            && !row.item.files.details_loaded()
    }) else {
        return;
    };
    let summary = row.item.files.summary();
    let mut files = old.item.files;
    if let Some(update) = &row.file_updates
        && files.apply_persistence_snapshot(update).is_err()
    {
        return;
    }
    if files.metadata_revision() == row.file_revision
        && files.apply_summary(summary).is_ok()
        && files.adopt_metadata_revision(row.file_revision).is_ok()
    {
        row.item.files = files;
        row.file_updates = None;
    }
}

async fn fetch_snapshot(
    inner: &Inner,
    detail: Option<String>,
    unchanged: Option<u64>,
) -> Result<Option<Snapshot>, Error> {
    let mut offset = 0;
    let mut epoch = None;
    let mut total = None;
    let mut revision = 0;
    let mut torrents = Vec::new();
    let mut observations = Vec::new();
    let mut lease = Instant::now();
    let mut limit = if unchanged.is_some() { 1 } else { 128 };
    loop {
        if inner.queued.load(Ordering::Acquire) > 0 {
            return Err(Error::Busy);
        }
        if inner
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .visible
            != detail
        {
            return Err(Error::Busy);
        }
        if lease.elapsed() >= Duration::from_secs(1) {
            renew_interest(inner).await?;
            lease = Instant::now();
        }
        let response = inner
            .call(Action::Snapshot {
                offset,
                limit,
                revision: epoch,
                detail: detail.clone(),
            })
            .await;
        let page = match response {
            Ok(Reply::Snapshot(page)) => page,
            Err(Error::TooLarge) if limit > 1 => {
                limit = (limit / 2).max(1);
                continue;
            }
            Err(error) => return Err(error),
            _ => return Err(Error::Incompatible),
        };
        if page.total > 100_000 || page.next_offset.is_some_and(|next| next <= offset) {
            return Err(Error::TooLarge);
        }
        if total.is_some_and(|total| total != page.total)
            || epoch.is_some_and(|epoch| epoch != page.epoch)
            || page
                .torrents
                .iter()
                .any(|row| row.file_revision == 0 || row.file_revision == u64::MAX)
            || page.observations.len() != page.torrents.len()
            || page
                .next_offset
                .is_some_and(|next| next != offset + page.torrents.len() || next >= page.total)
            || page
                .observations
                .iter()
                .zip(&page.torrents)
                .any(|(observed, item)| observed.id != item.info_hashes.join(";"))
        {
            return Err(Error::Stale);
        }
        total = Some(page.total);
        if offset == 0 && unchanged == Some(page.revision) {
            return Ok(None);
        }
        if offset == 0 && unchanged.is_some() {
            limit = 128;
        }
        epoch = Some(page.epoch);
        // Later telemetry may already have changed a row read on an earlier
        // page. Keep the first covered revision so the next probe converges.
        if offset == 0 {
            revision = page.revision;
        }
        torrents.extend(
            page.torrents
                .into_iter()
                .map(WireTorrent::into_native)
                .collect::<Result<Vec<_>, _>>()?,
        );
        observations.extend(page.observations);
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    if torrents.len() != total.unwrap_or(0) {
        return Err(Error::Stale);
    }
    Ok(Some(Snapshot {
        instance_id: inner
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .0
            .clone(),
        revision,
        torrents,
        observations,
        notices: Vec::new(),
        detail_torrent: detail,
    }))
}

async fn renew_interest(inner: &Inner) -> Result<(), Error> {
    let interest = inner
        .shared
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .interest
        .clone();
    inner
        .call(Action::Interest {
            id: interest.as_ref().map(|(id, _)| id.clone()),
            files: interest.as_ref().is_some_and(|(_, detail)| detail.files),
            peers: interest.as_ref().is_some_and(|(_, detail)| detail.peers),
        })
        .await
        .map(|_| ())
}

async fn fetch_files(
    inner: &Inner,
    id: &str,
    revision: Option<(u64, u64, crate::file_selection::FileSummary)>,
    loaded: Option<&(String, u64, u64, usize, u64)>,
) -> Result<
    Option<(
        Option<crate::file_selection::FileSelectionModel>,
        Option<crate::file_selection::CompactFileUpdate>,
        u64,
    )>,
    Error,
> {
    let Some((revision, file_revision, summary)) = revision else {
        return Ok(None);
    };
    if file_revision == 0 || file_revision == u64::MAX {
        return Err(Error::Stale);
    }
    let paths = loaded.is_none_or(|(old, rev, generation, count, size)| {
        old != id
            || *rev != revision
            || *generation != file_revision
            || *count != summary.file_count
            || *size != summary.total_size
    });
    let mut rows: Vec<StoredFile> = Vec::new();
    let mut offset = 0;
    let mut path_bytes = 0usize;
    let mut lease = Instant::now();
    let mut limit = if paths { 128 } else { 8192 };
    loop {
        if lease.elapsed() >= Duration::from_secs(1) {
            renew_interest(inner).await?;
            lease = Instant::now();
        }
        if inner.queued.load(Ordering::Acquire) > 0
            || !inner
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .interest
                .as_ref()
                .is_some_and(|(current, interest)| current == id && interest.files)
        {
            return Err(Error::Busy);
        }
        let response = inner
            .call(Action::Files {
                id: id.into(),
                offset,
                limit,
                revision,
                file_revision,
                paths,
            })
            .await;
        let page = match response {
            Ok(Reply::Files(page)) => page,
            Err(Error::TooLarge) if limit > 1 => {
                limit = (limit / 2).max(1);
                continue;
            }
            Err(error) => return Err(error),
            _ => return Err(Error::Incompatible),
        };
        if inner.queued.load(Ordering::Acquire) > 0
            || !inner
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .interest
                .as_ref()
                .is_some_and(|(current, interest)| current == id && interest.files)
        {
            return Err(Error::Busy);
        }
        validate_file_page(
            &page,
            id,
            revision,
            file_revision,
            summary.file_count,
            offset,
            limit,
        )?;
        path_bytes =
            path_bytes.saturating_add(page.files.iter().map(|file| file.path.len()).sum::<usize>());
        if path_bytes > 64 * 1024 * 1024 {
            return Err(Error::TooLarge);
        }
        rows.extend(page.files);
        match page.next_offset {
            Some(next) if next > offset => offset = next,
            Some(_) => return Err(Error::Stale),
            None => break,
        }
    }
    if rows.len() != summary.file_count {
        return Err(Error::Stale);
    }
    if paths {
        let high: Vec<_> = rows
            .iter()
            .filter_map(|file| (file.priority > 0).then_some(file.index as usize))
            .collect();
        let mut model = crate::file_selection::FileSelectionModel::from_snapshot(
            rows.into_iter().map(|file| {
                (
                    file.path,
                    file.size_bytes,
                    file.downloaded_bytes,
                    file.selected,
                )
            }),
        );
        model.set_high_priority(&high);
        model.apply_summary(summary).map_err(Error::Failed)?;
        model
            .adopt_metadata_revision(file_revision)
            .map_err(Error::Failed)?;
        Ok(Some((Some(model), None, revision)))
    } else {
        let mut update = crate::file_selection::CompactFileUpdate {
            file_count: rows.len(),
            selected_bits: vec![0; rows.len().div_ceil(8)],
            priority_bits: vec![0; rows.len().div_ceil(8)],
            downloaded: Vec::with_capacity(rows.len()),
        };
        for (index, file) in rows.into_iter().enumerate() {
            if file.selected {
                update.selected_bits[index / 8] |= 1 << (index % 8);
            }
            if file.priority > 0 {
                update.priority_bits[index / 8] |= 1 << (index % 8);
            }
            update.downloaded.push(file.downloaded_bytes);
        }
        Ok(Some((None, Some(update), revision)))
    }
}

fn validate_file_page(
    page: &FilePage,
    id: &str,
    revision: u64,
    file_revision: u64,
    total: usize,
    offset: usize,
    limit: usize,
) -> Result<(), Error> {
    let end = offset.checked_add(page.files.len()).ok_or(Error::Stale)?;
    if page.id != id
        || page.revision != revision
        || file_revision == 0
        || file_revision == u64::MAX
        || page.file_revision != file_revision
        || page.total != total
        || total > crate::limits::ResourceLimits::default().max_files
        || end > total
        || page.files.len() > limit
        || page
            .files
            .iter()
            .enumerate()
            .any(|(index, row)| row.index as usize != offset + index)
        || page.next_offset != (end < total).then_some(end)
        || (page.files.is_empty() && offset < total)
    {
        return Err(Error::Stale);
    }
    Ok(())
}

#[cfg(test)]
mod ui04_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_selection::{CompactFileUpdate, FileSelectionModel};
    pub(super) fn snapshot(instance: &str, full: bool, files_interest: bool) -> Snapshot {
        let mut item = crate::client::tests::torrent();
        item.files =
            FileSelectionModel::from_files([("one.bin".into(), 100), ("two.bin".into(), 200)]);
        if !full {
            item.files = FileSelectionModel::from_summary(item.files.summary());
        }
        if full {
            item.files.set_high_priority(&[1]);
        }
        item.files.adopt_metadata_revision(11).unwrap();
        let id = item.info_hashes.join(";");
        Snapshot {
            instance_id: instance.into(),
            revision: 3,
            torrents: vec![NativeTorrent {
                item,
                command_revision: 4,
                file_revision: 11,
                durable: true,
                file_updates: None,
            }],
            observations: vec![],
            notices: vec![],
            detail_torrent: files_interest.then_some(id),
        }
    }
    #[test]
    fn slow_frontend_keeps_pending_hydration_and_newest_progress_until_consumed() {
        let mut first = snapshot("owner", true, true);
        first
            .notices
            .push(SessionNotice::Added { id: "first".into() });
        let mut second = snapshot("owner", false, true);
        second.torrents[0].file_updates = Some(CompactFileUpdate {
            file_count: 2,
            selected_bits: vec![3],
            priority_bits: vec![2],
            downloaded: vec![20, 30],
        });
        let mut summary = second.torrents[0].item.files.summary();
        summary.selected_downloaded = 50;
        second.torrents[0]
            .item
            .files
            .apply_summary(summary)
            .unwrap();
        retain_pending_details(Some(first), &mut second);
        assert!(second.torrents[0].item.files.details_loaded());
        assert_eq!(second.torrents[0].item.files.files()[1].path, "two.bin");
        assert_eq!(
            second.torrents[0].item.files.files()[1].downloaded_bytes,
            30
        );
        assert!(second.torrents[0].item.files.is_high_priority(1));
        assert_eq!(second.notices.len(), 1);
        let mut third = snapshot("owner", false, true);
        retain_pending_details(Some(second), &mut third);
        assert!(third.torrents[0].item.files.details_loaded());
        assert_eq!(third.notices.len(), 1);
    }
    #[test]
    fn closing_files_or_restarting_owner_drops_pending_paths() {
        for (owner, interest) in [("owner", false), ("new-owner", true)] {
            let first = snapshot("owner", true, true);
            let mut next = snapshot(owner, false, interest);
            retain_pending_details(Some(first), &mut next);
            assert!(!next.torrents[0].item.files.details_loaded());
        }
        let first = snapshot("owner", true, true);
        let mut next = snapshot("owner", false, true);
        next.torrents[0].item.files =
            FileSelectionModel::from_summary(crate::file_selection::FileSummary {
                file_count: 3,
                total_size: 300,
                ..Default::default()
            });
        retain_pending_details(Some(first), &mut next);
        assert!(!next.torrents[0].item.files.details_loaded());
    }
    pub(super) fn disconnected_client() -> (Client, mpsc::Receiver<Job>) {
        let (jobs, receiver) = mpsc::sync_channel(64);
        let inner = Arc::new(Inner {
            directory: PathBuf::from("unused"),
            client_id: "test".into(),
            credentials: Mutex::new(("owner".into(), "a".repeat(64))),
            settings: Mutex::new(ClientSettings::default()),
            shared: Mutex::new(Shared {
                connection: ConnectionStatus::Connected,
                stopping_owner: None,
                receipt_overflow: 0,
                snapshot_serial: 0,
                delivered_serial: 0,
                latest: None,
                revisions: HashMap::from([("id".into(), 7)]),
                notices: VecDeque::new(),
                receipts: VecDeque::new(),
                interest: None,
                access: Default::default(),
                counts: HashMap::from([("id".into(), 3)]),
                visible: None,
                journal_active: false,
                tokens: VecDeque::new(),
                audits: HashMap::new(),
                audit_updates: VecDeque::new(),
                delivered_instance: Some("owner".into()),
                delivered_file_revisions: HashMap::from([("id".into(), 77)]),
            }),
            jobs,
            stop: AtomicBool::new(false),
            wake: Mutex::new(None),
            ui_wake: Mutex::new(None),
            journal: crate::announce_journal::AnnounceJournal::frontend_view(),
            queued: AtomicUsize::new(0),
            inflight: AtomicBool::new(false),
            next_job: AtomicU64::new(1),
        });
        (Client { inner }, receiver)
    }
    #[test]
    fn ui_selection_uses_cached_count_and_bounded_queue_without_waiting_for_rpc() {
        let (client, receiver) = disconnected_client();
        client
            .enqueue_submit("id", TorrentCommand::SetFiles(vec![0, 2]), Some(7))
            .unwrap();
        let job = receiver.try_recv().unwrap();
        let Action::Selection {
            files, revision, ..
        } = job.action
        else {
            panic!("wrong request")
        };
        assert_eq!(files.indices().unwrap(), vec![0, 2]);
        assert_eq!(revision, 7);
        let queued = client.take_receipts();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].queued);
        assert_eq!(queued[0].job_id, job.sequence);
        assert_eq!(queued[0].owner_instance, "owner");
        for _ in 0..64 {
            client
                .enqueue_submit("id", TorrentCommand::Pause, Some(7))
                .unwrap();
        }
        assert!(
            client
                .enqueue_submit("id", TorrentCommand::Pause, Some(7))
                .is_err()
        );
    }
    #[test]
    fn ordinary_access_refresh_cannot_erase_one_time_grant_token() {
        let (client, _receiver) = disconnected_client();
        client.inner.apply_access(AccessView {
            token: Some("one-time".into()),
            ..Default::default()
        });
        client.inner.apply_access(AccessView {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(client.take_access_token().as_deref(), Some("one-time"));
        assert!(client.take_access_token().is_none());
        assert!(client.access_view().enabled);
    }
    #[test]
    fn old_delivered_view_cannot_mutate_new_auth_even_when_row_revision_coincides() {
        let (client, _receiver) = disconnected_client();
        client.inner.credentials.lock().unwrap().0 = "replacement-owner".into();
        assert_eq!(client.cached_revision("id"), Some(7));
        assert!(
            client
                .enqueue_submit("id", TorrentCommand::Pause, Some(7))
                .is_err()
        );
        assert!(matches!(
            client.inner.request(Action::Mutate {
                id: "id".into(),
                command: TorrentCommand::Pause,
                revision: Some(7)
            }),
            Err(Error::Stale)
        ));
        client.inner.shared.lock().unwrap().latest =
            Some(snapshot("replacement-owner", false, false));
        assert!(client.sync().is_some());
        client
            .enqueue_submit("id", TorrentCommand::Pause, Some(7))
            .unwrap();
    }
    #[test]
    fn failed_audit_fetch_forces_retry_and_old_revision_cannot_unlock_tracker_editing() {
        let (client, _receiver) = disconnected_client();
        assert!(
            !client.inner.selected_audit_current(Some("id"), None),
            "A failed first fetch must prevent unchanged-snapshot short circuit"
        );
        assert!(
            client
                .inner
                .selected_audit_current(Some("id"), Some(&("id".into(), 7)))
        );
        {
            let mut shared = client.inner.shared.lock().unwrap();
            shared.audits.insert("id".into(), (7, None));
        }
        assert!(client.audit_ready("id"));
        {
            client
                .inner
                .shared
                .lock()
                .unwrap()
                .revisions
                .insert("id".into(), 8);
        }
        assert!(!client.audit_ready("id"));
        assert!(
            !client
                .inner
                .selected_audit_current(Some("id"), Some(&("id".into(), 7)))
        );
        {
            client
                .inner
                .shared
                .lock()
                .unwrap()
                .audits
                .insert("id".into(), (8, None));
        }
        assert!(client.audit_ready("id"));
    }
    #[test]
    fn paused_owner_retries_failed_audit_rpc_without_an_unrelated_state_change() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = ClientSettings::default();
        settings.download.download_directory = directory.path().display().to_string();
        let mut item = crate::client::tests::torrent();
        item.state = crate::torrent::TorrentState::Paused;
        let id = item.info_hashes.join(";");
        let record = crate::client::torrent_to_record(&item, None);
        let mut owner =
            crate::control::Controller::new(crate::client::ClientBackend::offline(), vec![item]);
        owner
            .configure_agents(directory.path(), directory.path().to_owned())
            .unwrap();
        let owner = Arc::new(Mutex::new(owner));
        let server =
            super::super::Server::start(owner.clone(), directory.path().to_owned(), settings)
                .unwrap();
        let client = Client::connect(directory.path()).unwrap();
        client.set_visible_torrent(Some(id.clone()));
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut snapshots = 0;
        loop {
            if client.sync().is_some() {
                snapshots += 1;
                if snapshots >= 2 {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!client.audit_ready(&id));
        let revision = owner.lock().unwrap().frontend_revision();
        let mut store = crate::state::StateStore::in_memory_with_secrets(Arc::new(
            crate::secrets::MemorySecrets::default(),
        ))
        .unwrap();
        store.save_torrent(&record).unwrap();
        owner.lock().unwrap().backend = crate::client::ClientBackend::with_state_store(store);
        while !client.audit_ready(&id) {
            assert!(
                Instant::now() < deadline,
                "Paused owner never retried the failed audit-only RPC"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(owner.lock().unwrap().frontend_revision(), revision);
        drop(client);
        drop(server);
    }
    #[test]
    fn existing_frontend_authenticates_restarted_owner_and_rejects_old_queued_actions() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = ClientSettings::default();
        settings.download.download_directory = directory.path().display().to_string();
        let make_owner = |name: &str| {
            let mut item = crate::client::tests::torrent();
            item.state = crate::torrent::TorrentState::Paused;
            item.name = name.into();
            let mut store = crate::state::StateStore::in_memory_with_secrets(Arc::new(
                crate::secrets::MemorySecrets::default(),
            ))
            .unwrap();
            store
                .save_torrent(&crate::client::torrent_to_record(&item, None))
                .unwrap();
            let mut owner = crate::control::Controller::new(
                crate::client::ClientBackend::with_state_store(store),
                vec![item],
            );
            owner
                .configure_agents(directory.path(), directory.path().to_owned())
                .unwrap();
            Arc::new(Mutex::new(owner))
        };
        let owner = make_owner("original");
        let server =
            super::super::Server::start(owner, directory.path().to_owned(), settings.clone())
                .unwrap();
        let client = Client::connect(directory.path()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let first = loop {
            if let Some(snapshot) = client.sync() {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(server);
        let restarted = super::super::Server::start(
            make_owner("restarted"),
            directory.path().to_owned(),
            settings,
        )
        .unwrap();
        let latest = loop {
            if let Some(snapshot) = client.sync()
                && snapshot.instance_id != first.instance_id
            {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "Existing client remained fenced to old native credentials"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(latest.torrents[0].item.name, "restarted");
        drop(client);
        drop(restarted);

        let (client, receiver) = disconnected_client();
        client
            .enqueue_submit("id", TorrentCommand::Pause, Some(7))
            .unwrap();
        let old = receiver.try_recv().unwrap();
        assert!(client.inner.job_is_current(&old));
        client.inner.credentials.lock().unwrap().0 = "replacement-owner".into();
        assert!(
            !client.inner.job_is_current(&old),
            "Queued actions must never replay into a replacement owner"
        );
    }
    #[test]
    fn closed_or_changed_files_interest_cancels_before_any_endpoint_request() {
        let (client, _receiver) = disconnected_client();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(fetch_files(
            &client.inner,
            "id",
            Some((
                7,
                77,
                crate::file_selection::FileSummary {
                    file_count: 3,
                    ..Default::default()
                },
            )),
            None,
        ));
        assert!(matches!(result, Err(Error::Busy)));
    }
}
