use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::Duration,
};

use eframe::egui;

mod file_location;
#[cfg(test)]
mod header_tests;

use crate::{
    blocklist::{self, Blocklist, BlocklistUpdate},
    client::{
        ClientBackend, SessionNotice, add_torrent_command, torrent_from_record, torrent_to_record,
    },
    control::{COMMAND_ACCEPTED, Controller, TorrentCommand},
    download_mode::DownloadMode,
    engine::EngineRuntime,
    file_selection::{FileSelectionModel, FileTreeRow},
    metainfo::{self, TorrentMetainfo},
    preferences::{self, PreferencesActions, PreferencesTab, PrivacyRuntime},
    session::SessionCommand,
    settings::ClientSettings,
    state::{StoredTrackerAuditProfile, StoredTrackerAuditStep, TorrentRecord},
    torrent::{
        EffectiveTorrentSettings, TorrentItem, TorrentOverrides, TorrentPriority, TorrentState,
    },
    tracker::{AnnounceEvent, AnnounceRequest, parse_info_hash_hex, validate_audit_target},
    tracker_behavior::BehaviorCatalog,
    ui_schema::{MainPage, TorrentDetailsTab as DetailsTab},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FooterAction {
    Reset,
    Cancel,
    Apply,
    Save,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrashScope {
    DownloadedFiles,
    TorrentFile,
    Everything,
}

impl TrashScope {
    fn label(self) -> &'static str {
        match self {
            Self::DownloadedFiles => "downloaded files",
            Self::TorrentFile => "the source .torrent file",
            Self::Everything => "the source .torrent and downloaded files",
        }
    }

    fn includes_data(self) -> bool {
        matches!(self, Self::DownloadedFiles | Self::Everything)
    }

    fn includes_source(self) -> bool {
        matches!(self, Self::TorrentFile | Self::Everything)
    }

    fn removes_torrent(self) -> bool {
        matches!(self, Self::TorrentFile | Self::Everything)
    }
}

#[derive(Clone, Debug)]
struct TrashRequest {
    torrent_id: String,
    torrent_name: String,
    scope: TrashScope,
    paths: Vec<PathBuf>,
}

use crate::live_history::AuditExchange as TrackerAuditExchange;

#[derive(Clone, Debug)]
struct TrackerAuditStep {
    delay_seconds: u64,
    uploaded: u64,
    downloaded: u64,
    left: u64,
    event: Option<AnnounceEvent>,
}

struct TrackerAuditRun {
    receiver: Receiver<TrackerAuditWorkerEvent>,
    cancel: Arc<AtomicBool>,
}

impl Drop for TrackerAuditRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum TrackerAuditWorkerEvent {
    Exchange(TrackerAuditExchange),
    ObservationStarted(u64),
    Finished,
    Cancelled,
}

struct TorrentTrackerAudit {
    tracker_url: String,
    allowed_host: String,
    port: u16,
    started_only: bool,
    observation_seconds: u64,
    steps: Vec<TrackerAuditStep>,
    exchanges: crate::live_history::LiveHistory<TrackerAuditExchange>,
    status: String,
    run: Option<TrackerAuditRun>,
}

impl TorrentTrackerAudit {
    fn new(tracker_url: String, total_size: u64, port: u16) -> Self {
        Self {
            tracker_url,
            allowed_host: String::new(),
            port,
            started_only: false,
            observation_seconds: 14_400,
            steps: vec![TrackerAuditStep {
                delay_seconds: 0,
                uploaded: 0,
                downloaded: 0,
                left: total_size,
                event: Some(AnnounceEvent::Started),
            }],
            exchanges: Default::default(),
            status: String::new(),
            run: None,
        }
    }

    fn from_stored(
        tracker_url: String,
        total_size: u64,
        fallback_port: u16,
        stored: Option<&StoredTrackerAuditProfile>,
    ) -> Self {
        let Some(stored) = stored else {
            return Self::new(tracker_url, total_size, fallback_port);
        };
        let mut steps: Vec<_> = stored
            .steps
            .iter()
            .map(|step| TrackerAuditStep {
                delay_seconds: step.delay_seconds,
                uploaded: step.uploaded,
                downloaded: step.downloaded,
                left: step.left,
                event: audit_event_from_storage(step.event.as_deref()),
            })
            .collect();
        if steps.is_empty() {
            steps.push(TrackerAuditStep {
                delay_seconds: 0,
                uploaded: 0,
                downloaded: 0,
                left: total_size,
                event: Some(AnnounceEvent::Started),
            });
        }
        Self {
            tracker_url,
            allowed_host: stored.allowed_host.clone(),
            port: stored.port,
            started_only: stored.started_only,
            observation_seconds: stored.observation_seconds,
            steps,
            exchanges: Default::default(),
            status: String::new(),
            run: None,
        }
    }

    fn to_stored(&self) -> StoredTrackerAuditProfile {
        StoredTrackerAuditProfile {
            allowed_host: self.allowed_host.clone(),
            port: self.port,
            started_only: self.started_only,
            observation_seconds: self.observation_seconds,
            steps: self
                .steps
                .iter()
                .map(|step| StoredTrackerAuditStep {
                    delay_seconds: step.delay_seconds,
                    uploaded: step.uploaded,
                    downloaded: step.downloaded,
                    left: step.left,
                    event: step.event.map(audit_event_storage).map(str::to_owned),
                })
                .collect(),
        }
    }
}

struct AddTrackerAudit {
    tracker_url: String,
    allowed_host: String,
    uploaded: u64,
    downloaded: u64,
    left: u64,
    port: u16,
    event: Option<AnnounceEvent>,
    started_only: bool,
    observation_seconds: u64,
    status: String,
    peers: Vec<String>,
    exchanges: crate::live_history::LiveHistory<TrackerAuditExchange>,
    pending: Option<Receiver<TrackerAuditExchange>>,
    preset_steps: Vec<TrackerAuditStep>,
}

impl AddTrackerAudit {
    fn new(tracker_url: String, total_size: u64, port: u16) -> Self {
        Self {
            tracker_url,
            allowed_host: String::new(),
            uploaded: 0,
            downloaded: 0,
            left: total_size,
            port,
            event: Some(AnnounceEvent::Started),
            started_only: false,
            observation_seconds: 14_400,
            status: String::new(),
            peers: Vec::new(),
            exchanges: Default::default(),
            pending: None,
            preset_steps: Vec::new(),
        }
    }

    fn to_profile(&self) -> TorrentTrackerAudit {
        let mut steps = self.preset_steps.clone();
        let first = TrackerAuditStep {
            delay_seconds: steps.first().map_or(0, |step| step.delay_seconds),
            uploaded: self.uploaded,
            downloaded: self.downloaded,
            left: self.left,
            event: self.event,
        };
        if steps.is_empty() {
            steps.push(first);
        } else {
            steps[0] = first;
        }
        TorrentTrackerAudit {
            tracker_url: self.tracker_url.clone(),
            allowed_host: self.allowed_host.clone(),
            port: self.port,
            started_only: self.started_only,
            observation_seconds: self.observation_seconds,
            steps,
            exchanges: self.exchanges.clone(),
            status: String::new(),
            run: None,
        }
    }

    fn apply_preset(&mut self, profile: &StoredTrackerAuditProfile) {
        let resolved = TorrentTrackerAudit::from_stored(
            self.tracker_url.clone(),
            self.left,
            self.port,
            Some(profile),
        );
        self.allowed_host = resolved.allowed_host;
        self.port = resolved.port;
        self.started_only = resolved.started_only;
        self.observation_seconds = resolved.observation_seconds;
        self.preset_steps = resolved.steps;
        if let Some(first) = self.preset_steps.first() {
            self.uploaded = first.uploaded;
            self.downloaded = first.downloaded;
            self.left = first.left;
            self.event = first.event;
        }
        self.status = format!(
            "Loaded external profile: {} step(s)",
            self.preset_steps.len()
        );
    }
}

struct AddTorrentDialog {
    repair_policy: crate::runtime_status::RepairPolicy,
    verify_after_download: bool,
    download_mode: DownloadMode,
    freeze_after_bytes: u64,
    source_path: PathBuf,
    name: String,
    is_private: bool,
    info_hash_v1_hex: Option<String>,
    info_hash_v2_hex: Option<String>,
    total_size: u64,
    files: FileSelectionModel,
    peer_id: String,
    download_directory: String,
    start_immediately: bool,
    tracker_audit: AddTrackerAudit,
    error: String,
}

impl AddTorrentDialog {
    fn new(
        source_path: PathBuf,
        metainfo: TorrentMetainfo,
        settings: &ClientSettings,
        download_directory: String,
    ) -> Self {
        let total_size = metainfo.total_size();
        let mut files = FileSelectionModel::from_metainfo_files(metainfo.files);
        if !settings.download.select_all_files {
            files.select_all(false);
        }
        let audit_tracker_url = metainfo.trackers.first().cloned().unwrap_or_default();
        Self {
            source_path,
            verify_after_download: false,
            repair_policy: Default::default(),
            download_mode: DownloadMode::Normal,
            freeze_after_bytes: crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES,
            peer_id: metainfo::generate_peer_id(&settings.bittorrent.peer_id_prefix),
            download_directory,
            start_immediately: settings.download.start_when_added,
            tracker_audit: AddTrackerAudit::new(
                audit_tracker_url,
                total_size,
                settings.connection.listen_port,
            ),
            name: metainfo.name,
            is_private: metainfo.is_private,
            info_hash_v1_hex: metainfo.info_hash_v1_hex,
            info_hash_v2_hex: metainfo.info_hash_v2_hex,
            total_size,
            files,
            error: String::new(),
        }
    }
}

pub struct PeerLumeApp {
    log_offset: usize,
    log_torrent: String,
    page: MainPage,
    details_tab: DetailsTab,
    preferences_tab: PreferencesTab,
    selected_torrent: usize,
    settings: ClientSettings,
    saved_settings: ClientSettings,
    settings_draft: ClientSettings,
    settings_status: String,
    validation_messages: Vec<String>,
    engine: EngineRuntime,
    blocklist: Blocklist,
    blocklist_status: String,
    blocklist_update: Option<Receiver<Result<BlocklistUpdate, String>>>,
    blocklist_probe: String,
    blocklist_probe_result: String,
    add_dialog: Option<AddTorrentDialog>,
    last_download_directory: Option<String>,
    pending_torrent_paths: VecDeque<PathBuf>,
    add_status: String,
    magnet_input: String,
    control: Controller,
    tracker_audits: HashMap<String, TorrentTrackerAudit>,
    behavior_catalog: Result<BehaviorCatalog, String>,
    trash_confirmation: Option<TrashRequest>,
    pending_trash: Option<TrashRequest>,
}

/// Synchronizes frontend transactions with the independent controller owner pump.
/// The transfer engine and canonical models are never cloned for an adapter.
pub struct HostedGui {
    host: crate::control::host::Host,
    owner: Arc<std::sync::Mutex<PeerLumeApp>>,
    access: crate::control::frontends::AccessPanel,
    _instance: crate::control::host::InstanceLock,
}

impl HostedGui {
    pub fn start() -> std::io::Result<Self> {
        let directory = crate::settings::data_directory();
        let instance = crate::control::host::InstanceLock::acquire(&directory)?;
        let mut app = PeerLumeApp::default();
        app.pending_torrent_paths
            .extend(crate::settings::open_torrent_arguments(
                &std::env::args().skip(1).collect::<Vec<_>>(),
            )?);
        app.open_next_queued_torrent();
        let download_root = PathBuf::from(&app.settings.download.download_directory);
        app.control
            .configure_agents(&directory, download_root)
            .map_err(|e| {
                std::io::Error::other(format!("Agent control initialization failed: {e:?}"))
            })?;
        let _ = app.control.set_agent_settings(&app.settings);
        let owner = Arc::new(std::sync::Mutex::new(app));
        let host =
            crate::control::host::Host::start(owner.clone(), |app| &mut app.control, directory)?;
        Ok(Self {
            host,
            owner,
            access: Default::default(),
            _instance: instance,
        })
    }
}

impl eframe::App for HostedGui {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let owner = Arc::clone(&self.owner);
        let mut app = owner.lock().unwrap_or_else(|e| e.into_inner());
        self.access.show(&mut app.control, ctx);
        if let Some(error) = *self.host.error.lock().unwrap_or_else(|e| e.into_inner()) {
            self.access.status = format!("IPC unavailable: {error:?}");
        }
        app.update(ctx, frame);
    }
}

impl Default for PeerLumeApp {
    fn default() -> Self {
        let settings = ClientSettings::load();
        let saved_settings = settings.clone();
        let settings_draft = settings.clone();
        let preferences_tab = PreferencesTab::from_index(settings.ui.last_preferences_tab);
        let blocklist = Blocklist::load_cache().unwrap_or_default();
        let blocklist_status = if blocklist.rule_count() == 0 {
            "No cached rules".into()
        } else {
            format!(
                "Loaded {} rules ({} merged ranges)",
                blocklist.rule_count(),
                blocklist.range_count()
            )
        };
        let engine = EngineRuntime::new(settings.clone());
        let auto_update =
            settings.privacy.blocklist_enabled && settings.privacy.blocklist_auto_update;
        let update_url = settings.privacy.blocklist_url.clone();
        let backend = ClientBackend::start(&settings);
        let (restored_records, restore_status) = backend.restore_for_frontend();
        let tracker_audits = restored_records
            .iter()
            .map(|record| {
                let total_size = record
                    .files
                    .iter()
                    .filter(|file| file.selected)
                    .map(|file| file.size_bytes)
                    .sum();
                (
                    record.id.clone(),
                    TorrentTrackerAudit::from_stored(
                        record.trackers.first().cloned().unwrap_or_default(),
                        total_size,
                        settings.connection.listen_port,
                        record.tracker_audit.as_ref(),
                    ),
                )
            })
            .collect();
        let torrents = restored_records
            .into_iter()
            .map(|record| torrent_from_record(record, &settings))
            .collect();

        let mut app = Self {
            page: MainPage::Torrents,
            details_tab: DetailsTab::Files,
            log_offset: 0,
            log_torrent: String::new(),
            preferences_tab,
            selected_torrent: 0,
            settings,
            saved_settings,
            settings_draft,
            settings_status: String::new(),
            validation_messages: Vec::new(),
            engine,
            blocklist,
            blocklist_status,
            blocklist_update: None,
            blocklist_probe: "127.0.0.1".into(),
            blocklist_probe_result: String::new(),
            add_dialog: None,
            last_download_directory: None,
            pending_torrent_paths: VecDeque::new(),
            add_status: restore_status.unwrap_or_default().into(),
            magnet_input: String::new(),
            control: Controller::new(backend, torrents),
            tracker_audits,
            behavior_catalog: BehaviorCatalog::load(),
            trash_confirmation: None,
            pending_trash: None,
        };
        if auto_update {
            app.start_blocklist_update(update_url);
        }
        for torrent in &app.control.torrents {
            let _ = app
                .control
                .backend
                .send(add_torrent_command(torrent, &app.settings));
        }
        app
    }
}

impl PeerLumeApp {
    fn poll_session_events(&mut self) {
        while let Some(notice) = self.control.poll_next() {
            match notice {
                SessionNotice::SettingsApplied { id } | SessionNotice::StateApplied { id } => {
                    if !self.control.has_confirmed_update(&id) {
                        continue;
                    }
                    let audit = self
                        .tracker_audits
                        .get(&id)
                        .map(TorrentTrackerAudit::to_stored);
                    if let Err(error) = self.control.persist_confirmed_update(&id, audit) {
                        self.add_status = error;
                    }
                }
                SessionNotice::Added { .. } => {
                    self.add_status = "Transfer engine started the torrent".into();
                }
                SessionNotice::Error { id, message } => {
                    if self
                        .pending_trash
                        .as_ref()
                        .is_some_and(|request| request.torrent_id == id)
                    {
                        self.pending_trash = None;
                    }
                    self.add_status = format!("Transfer error: {message}");
                }
                SessionNotice::Removed { id } => {
                    if !self
                        .control
                        .torrents
                        .iter()
                        .any(|torrent| torrent.info_hashes.join(";") == id)
                    {
                        self.tracker_audits.remove(&id);
                    }
                    if self
                        .pending_trash
                        .as_ref()
                        .is_some_and(|request| request.torrent_id == id)
                    {
                        self.complete_trash_request();
                    }
                }
                SessionNotice::Stopped => {
                    self.add_status = "Transfer engine stopped".into();
                }
                SessionNotice::None => {}
            }
        }
        self.control.publish();
    }

    fn complete_trash_request(&mut self) {
        let Some(request) = self.pending_trash.take() else {
            return;
        };
        if let Err(error) = trash::delete_all(&request.paths) {
            self.add_status = format!("Could not move items to the Recycle Bin: {error}");
            return;
        }

        if request.scope.removes_torrent() {
            if let Some(index) = self
                .control
                .torrents
                .iter()
                .position(|torrent| torrent.info_hashes.join(";") == request.torrent_id)
            {
                self.control.torrents.remove(index);
                self.tracker_audits.remove(&request.torrent_id);
                self.selected_torrent = self
                    .selected_torrent
                    .min(self.control.torrents.len().saturating_sub(1));
            }
            let _ = self.control.backend.remove_torrent(&request.torrent_id);
        } else if let Some(index) = self
            .control
            .torrents
            .iter()
            .position(|torrent| torrent.info_hashes.join(";") == request.torrent_id)
        {
            let command = {
                let torrent = &mut self.control.torrents[index];
                for file_index in 0..torrent.files.files().len() {
                    torrent.files.set_file_downloaded(file_index, 0);
                }
                torrent.downloaded_bytes = 0;
                torrent.uploaded_bytes = 0;
                torrent.download_rate = 0;
                torrent.upload_rate = 0;
                torrent.peers = 0;
                torrent.seeds = 0;
                torrent.state = TorrentState::Paused;
                add_torrent_command(torrent, &self.settings)
            };
            let _ = self.control.backend.send(command);
            if let Some(torrent) = self.control.torrents.get(index) {
                let audit = self.tracker_audits.get(&request.torrent_id);
                let _ = self
                    .control
                    .backend
                    .save_torrent(&torrent_record(torrent, audit));
            }
        }
        self.add_status = format!(
            "Moved {} for {} to the Recycle Bin",
            request.scope.label(),
            request.torrent_name
        );
    }

    fn prepare_trash_request(&mut self, id: &str, scope: TrashScope) {
        if self.pending_trash.is_some() {
            self.add_status = "Another Recycle Bin operation is still running".into();
            return;
        }
        let Some(torrent) = self
            .control
            .torrents
            .iter()
            .find(|torrent| torrent.info_hashes.join(";") == id)
        else {
            self.add_status = "Torrent no longer exists".into();
            return;
        };
        let paths = trash_paths(torrent, &self.settings, scope);
        if paths.is_empty() {
            self.add_status = format!("No existing {} were found", scope.label());
            return;
        }
        self.trash_confirmation = Some(TrashRequest {
            torrent_id: id.to_owned(),
            torrent_name: torrent.name.clone(),
            scope,
            paths,
        });
    }

    fn show_trash_confirmation(&mut self, ctx: &egui::Context) {
        let Some(request) = self.trash_confirmation.as_ref() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("trash_confirmation")).show(ctx, |ui| {
            ui.heading("Move items to Recycle Bin?");
            ui.label(format!(
                "{}: {}",
                request.torrent_name,
                request.scope.label()
            ));
            ui.label(format!(
                "{} existing item(s) will be moved.",
                request.paths.len()
            ));
            ui.separator();
            for path in request.paths.iter().take(8) {
                ui.label(path.display().to_string());
            }
            if request.paths.len() > 8 {
                ui.label(format!("… and {} more", request.paths.len() - 8));
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Move to Recycle Bin").clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.trash_confirmation = None;
            return;
        }
        if !confirm {
            return;
        }
        let request = self
            .trash_confirmation
            .take()
            .expect("trash confirmation exists");
        if !self.control.backend.session_available() {
            self.add_status = "Transfer engine is unavailable; nothing was deleted".into();
            return;
        }
        match self.control.backend.send(SessionCommand::Remove {
            id: request.torrent_id.clone(),
        }) {
            Ok(()) => {
                self.add_status = format!(
                    "Stopping {} before moving items to the Recycle Bin...",
                    request.torrent_name
                );
                self.pending_trash = Some(request);
            }
            Err(error) => {
                self.add_status = format!("Could not stop torrent; nothing was deleted: {error}");
            }
        }
    }

    fn poll_blocklist_update(&mut self) {
        let result = self
            .blocklist_update
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = result {
            self.blocklist_update = None;
            match result {
                Ok(update) => {
                    self.blocklist = update.blocklist;
                    self.blocklist_status = format!(
                        "Updated: {} rules, {} merged ranges, {} downloaded",
                        self.blocklist.rule_count(),
                        self.blocklist.range_count(),
                        format_size(update.downloaded_bytes as u64)
                    );
                }
                Err(error) => self.blocklist_status = format!("Update failed: {error}"),
            }
        }
    }

    fn start_blocklist_update(&mut self, url: String) {
        if self.blocklist_update.is_some() {
            return;
        }
        let url = url.trim().to_owned();
        if url.is_empty() {
            self.blocklist_status = "Blocklist URL is empty".into();
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.blocklist_update = Some(receiver);
        self.blocklist_status = "Downloading and validating blocklist...".into();
        std::thread::spawn(move || {
            let _ = sender.send(blocklist::download_and_cache(&url));
        });
    }

    fn show_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PeerLume");
                ui.separator();
                ui.selectable_value(
                    &mut self.page,
                    MainPage::Torrents,
                    MainPage::Torrents.label(),
                );
                ui.selectable_value(
                    &mut self.page,
                    MainPage::Preferences,
                    MainPage::Preferences.label(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let active = self
                        .control
                        .torrents
                        .iter()
                        .filter(|torrent| torrent.state == TorrentState::Downloading)
                        .count();
                    ui.label(format!(
                        "{active} active / {} total",
                        self.control.torrents.len()
                    ));
                });
            });
        });
    }

    fn show_torrents(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("+ Add torrent").clicked() {
                    self.open_torrent_picker();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.magnet_input)
                        .desired_width(220.0)
                        .hint_text("magnet:?xt=..."),
                );
                if ui.button("Add magnet").clicked() {
                    self.add_magnet();
                }
                if ui.button("Pause all").clicked() {
                    self.add_status = self.control.submit_batch(false);
                }
                if ui.button("Resume all").clicked() {
                    self.add_status = self.control.submit_batch(true);
                }
                if ui.button("Remove selected (keep data)").clicked() && !self.control.torrents.is_empty() {
                    let index = self.selected_torrent.min(self.control.torrents.len() - 1);
                    let id = self.control.torrents[index].info_hashes.join(";");
                    self.add_status = self.control.remove(&id).map(|_|"Removal queued; data will be kept".into()).unwrap_or_else(|error|error.to_string());
                }
                if !self.add_status.is_empty() {
                    ui.label(&self.add_status);
                }
            });
            ui.add_space(6.0);
            let mut directory_to_open = None;
            let mut trash_action = None;
            ui.scope(|ui| {
                ui.style_mut().spacing.scroll.floating = false;
                egui::ScrollArea::horizontal()
                    .id_salt("torrent_table_scroll")
                    .show(ui, |ui| {
                        egui::Grid::new("torrent_table")
                            .striped(true)
                            .min_col_width(72.0)
                            .show(ui, |ui| {
                                for title in [
                                    "Name", "Progress", "Status", "Down", "Up", "Peers", "ETA",
                                    "Ratio",
                                ] {
                                    ui.strong(title);
                                }
                                ui.end_row();
                                for (index, torrent) in self.control.torrents.iter().enumerate() {
                                    let name = if torrent.is_private {
                                        format!("[Private] {}", torrent.name)
                                    } else {
                                        torrent.name.clone()
                                    };
                                    let response = ui
                                        .add_sized(
                                            [420.0, 20.0],
                                            egui::Button::selectable(
                                                self.selected_torrent == index,
                                                name,
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&torrent.name);
                                    response.context_menu(|ui| {
                                        if ui.button("Open download folder").clicked() {
                                            let effective = EffectiveTorrentSettings::resolve(
                                                &self.settings,
                                                &torrent.overrides,
                                                torrent.is_private,
                                            );
                                            directory_to_open =
                                                Some(PathBuf::from(effective.download_directory));
                                            ui.close();
                                        }
                                        let source = PathBuf::from(&torrent.source);
                                        if source.is_file()
                                            && ui.button("Open .torrent location").clicked()
                                        {
                                            directory_to_open =
                                                source.parent().map(Path::to_path_buf);
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("Move downloaded files to Recycle Bin").clicked()
                                        {
                                            trash_action = Some((
                                                torrent.info_hashes.join(";"),
                                                TrashScope::DownloadedFiles,
                                            ));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                source.is_file(),
                                                egui::Button::new(
                                                    "Move source .torrent to Recycle Bin",
                                                ),
                                            )
                                            .clicked()
                                        {
                                            trash_action = Some((
                                                torrent.info_hashes.join(";"),
                                                TrashScope::TorrentFile,
                                            ));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                source.is_file(),
                                                egui::Button::new(
                                                    "Move torrent and downloaded files to Recycle Bin",
                                                ),
                                            )
                                            .clicked()
                                        {
                                            trash_action = Some((
                                                torrent.info_hashes.join(";"),
                                                TrashScope::Everything,
                                            ));
                                            ui.close();
                                        }
                                    });
                                    if response.clicked() {
                                        self.selected_torrent = index;
                                    }
                                    ui.add(
                                        egui::ProgressBar::new(torrent.display_progress())
                                            .desired_width(145.0)
                                            .text(torrent.progress_text())
                                            .fill(if torrent.completed_successfully() {
                                                egui::Color32::from_rgb(32, 135, 65)
                                            } else if matches!(torrent.completion, crate::completion::CompletionStatus::Failed { .. }) {
                                                egui::Color32::from_rgb(170, 45, 45)
                                            } else if torrent.state == TorrentState::Checking {
                                                egui::Color32::from_rgb(170, 125, 25)
                                            } else { ui.visuals().selection.bg_fill }),
                                    );
                                    ui.label(torrent.status_text());
                                    ui.label(format_rate(torrent.download_rate));
                                    ui.label(format_rate(torrent.upload_rate));
                                    ui.label(format!("{} / {}", torrent.peers, torrent.seeds));
                                    ui.label(
                                        torrent
                                            .eta_seconds()
                                            .map(format_duration)
                                            .unwrap_or_else(|| "--".into()),
                                    );
                                    ui.label(format!("{:.2}", torrent.ratio()));
                                    ui.end_row();
                                }
                            });
                    });
            });
            if let Some(path) = directory_to_open {
                match open_directory(&path) {
                    Ok(opened) => {
                        self.add_status = format!("Opened {}", opened.display());
                    }
                    Err(error) => self.add_status = format!("Could not open folder: {error}"),
                }
            }
            if let Some((id, scope)) = trash_action {
                self.prepare_trash_request(&id, scope);
            }
            ui.separator();
            self.show_selected_torrent(ui);
        });
    }

    fn open_torrent_picker(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("BitTorrent metainfo", &["torrent"])
            .pick_file()
        else {
            return;
        };
        self.load_torrent_path(path);
    }

    fn load_torrent_path(&mut self, path: PathBuf) {
        if !is_torrent_path(&path) {
            self.add_status = "Only files with the .torrent extension can be added".into();
            return;
        }
        match TorrentMetainfo::load(&path) {
            Ok(metainfo) => {
                let download_directory = self
                    .last_download_directory
                    .clone()
                    .unwrap_or_else(|| self.settings.download.download_directory.clone());
                self.add_dialog = Some(AddTorrentDialog::new(
                    path,
                    metainfo,
                    &self.settings,
                    download_directory,
                ));
                self.add_status.clear();
            }
            Err(error) => self.add_status = format!("Could not open torrent: {error}"),
        }
    }

    fn handle_dropped_files(&mut self, dropped_count: usize, paths: Vec<PathBuf>) {
        if dropped_count == 0 {
            return;
        }
        match torrent_drop_candidates(dropped_count, &paths) {
            Ok(paths) => {
                let added = paths.len();
                self.pending_torrent_paths.extend(paths);
                self.add_status = format!("Queued {added} torrent file(s) for preview");
                self.open_next_queued_torrent();
            }
            Err(error) => self.add_status = error.into(),
        }
    }

    fn open_next_queued_torrent(&mut self) {
        if self.add_dialog.is_some() {
            return;
        }
        while let Some(path) = self.pending_torrent_paths.pop_front() {
            self.load_torrent_path(path);
            if self.add_dialog.is_some() {
                break;
            }
        }
    }

    fn add_magnet(&mut self) {
        match crate::magnet::MagnetLink::parse(self.magnet_input.trim()) {
            Ok(magnet) => {
                let name = magnet
                    .display_name
                    .unwrap_or_else(|| "Magnet metadata".into());
                let id = magnet.exact_topics.join(";");
                let torrent = TorrentItem {
                    runtime: Default::default(),
                    completion: crate::completion::CompletionStatus::Pending,
                    name: name.clone(),
                    source: self.magnet_input.clone(),
                    info_hashes: magnet.exact_topics,
                    trackers: magnet.trackers,
                    peer_id: metainfo::generate_peer_id(&self.settings.bittorrent.peer_id_prefix),
                    files: FileSelectionModel::default(),
                    downloaded_bytes: 0,
                    uploaded_bytes: 0,
                    download_rate: 0,
                    upload_rate: 0,
                    peers: 0,
                    seeds: 0,
                    state: TorrentState::Paused,
                    is_private: false,
                    overrides: Default::default(),
                };
                if let Err(error) = self.control.add(torrent, &self.settings, None) {
                    self.add_status = error;
                    return;
                }
                self.selected_torrent = self.control.torrents.len() - 1;
                if let Some(torrent) = self.control.torrents.last() {
                    self.tracker_audits.insert(
                        id.clone(),
                        TorrentTrackerAudit::new(
                            torrent.trackers.first().cloned().unwrap_or_default(),
                            torrent.selected_size(),
                            self.settings.connection.listen_port,
                        ),
                    );
                }
                self.add_status = format!("Added {name}; waiting for metadata exchange");
                self.magnet_input.clear();
            }
            Err(error) => self.add_status = error,
        }
    }

    fn show_add_torrent_dialog(&mut self, ctx: &egui::Context) {
        let mut picked_download_directory = None;
        let Some(dialog) = self.add_dialog.as_mut() else {
            return;
        };
        poll_tracker_audit(dialog);
        let mut add = false;
        let mut cancel = false;
        let peer_prefix = self.settings.bittorrent.peer_id_prefix.clone();
        let modal_width = (ctx.content_rect().width() - 80.0).clamp(520.0, 1_000.0);
        let modal_height = (ctx.content_rect().height() - 100.0).max(280.0);

        let modal = egui::Modal::new(egui::Id::new("add_torrent_dialog"))
            .backdrop_color(egui::Color32::from_black_alpha(150))
            .show(ctx, |ui| {
                ui.set_width(modal_width);
                ui.heading("Review torrent before adding");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("add_torrent_dialog_scroll")
                    .max_height(modal_height)
                    .show(ui, |ui| {
                egui::Grid::new("add_torrent_summary")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Source");
                        ui.label(dialog.source_path.display().to_string());
                        ui.end_row();
                        ui.label("Name");
                        ui.strong(&dialog.name);
                        ui.end_row();
                        if let Some(hash) = &dialog.info_hash_v1_hex {
                            ui.label("Info hash v1");
                            ui.monospace(hash);
                            ui.end_row();
                        }
                        if let Some(hash) = &dialog.info_hash_v2_hex {
                            ui.label("Info hash v2");
                            ui.monospace(hash);
                            ui.end_row();
                        }
                        ui.label("Private");
                        ui.label(if dialog.is_private { "Yes" } else { "No" });
                        ui.end_row();
                        ui.label("Total size");
                        ui.label(format_size(dialog.total_size));
                        ui.end_row();
                    });

                ui.separator();
                ui.heading("Download mode");
                let modes = self.behavior_catalog.as_ref().map(|catalog| catalog.download_modes.as_slice())
                    .unwrap_or(&DownloadMode::ALL);
                let previous_mode = dialog.download_mode;
                egui::ComboBox::from_id_salt("add_download_mode")
                    .selected_text(dialog.download_mode.label())
                    .show_ui(ui, |ui| {
                        for mode in modes {
                            ui.selectable_value(&mut dialog.download_mode, *mode, mode.label());
                        }
                    });
                ui.label(dialog.download_mode.description());
                if dialog.download_mode == DownloadMode::FreezeAfter {
                    ui.horizontal(|ui| {
                        ui.label("Freeze after received bytes");
                        ui.add(egui::DragValue::new(&mut dialog.freeze_after_bytes).range(1..=i64::MAX as u64));
                    });
                }
                if dialog.download_mode != previous_mode && dialog.download_mode != DownloadMode::Normal {
                    if dialog.tracker_audit.started_only {
                        dialog.start_immediately = self.settings.download.start_when_added;
                    }
                    dialog.tracker_audit.started_only = false;
                }
                ui.horizontal(|ui| {
                    ui.label("Tracker URL");
                    ui.add(egui::TextEdit::singleline(&mut dialog.tracker_audit.tracker_url).desired_width(560.0));
                });
                show_tracker_account_identity(ui, &dialog.tracker_audit.tracker_url);
                if dialog.download_mode == DownloadMode::Normal {
                    ui.collapsing("Advanced tracker audit", |ui| {
                        show_tracker_audit(ui, dialog, &mut self.behavior_catalog);
                    });
                }

                ui.separator();
                ui.heading("Download location");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut dialog.download_directory)
                            .desired_width((ui.available_width() - 90.0).max(180.0)),
                    );
                    if ui.button("Browse...").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_directory(&dialog.download_directory)
                            .pick_folder()
                    {
                        let directory = path.display().to_string();
                        dialog.download_directory = directory.clone();
                        picked_download_directory = Some(directory);
                    }
                });
                ui.label("Files from this torrent will be written under this directory.");
                ui.checkbox(&mut dialog.verify_after_download, "Verify after download");
                ui.checkbox(&mut dialog.repair_policy.enabled, "Automatically repair failed integrity checks");
                if dialog.repair_policy.enabled {
                    dialog.verify_after_download = true;
                    ui.add(egui::DragValue::new(&mut dialog.repair_policy.max_attempts).range(1..=10).prefix("Maximum repair attempts: "));
                }
                ui.label("Optional full on-disk hash check before showing Done. Pieces are always checked during download.");

                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Files");
                    if ui.button("Select all").clicked() {
                        dialog.files.select_all(true);
                    }
                    if ui.button("Select none").clicked() {
                        dialog.files.select_all(false);
                    }
                    ui.label(format!(
                        "{} of {}, {} selected",
                        dialog.files.selected_count(),
                        dialog.files.files().len(),
                        format_size(dialog.files.selected_size())
                    ));
                });
                let mut selection_changed = false;
                egui::ScrollArea::vertical()
                    .id_salt("add_torrent_files")
                    .max_height(220.0)
                    .show_rows(ui, 22.0, dialog.files.row_count(), |ui, rows_range| {
                        let (rows, files) = dialog.files.rows_and_files_mut();
                        for row in &rows[rows_range] {
                            match row {
                                FileTreeRow::Group { name, file_count } => {
                                    ui.strong(format!("{name}/ ({file_count} files)"));
                                }
                                FileTreeRow::File {
                                    index,
                                    display_start,
                                } => {
                                    let file = &mut files[*index];
                                    ui.horizontal(|ui| {
                                        selection_changed |=
                                            ui.checkbox(&mut file.selected, "").changed();
                                        ui.label(&file.path[*display_start..]);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(format_size(file.size_bytes));
                                            },
                                        );
                                    });
                                }
                            }
                        }
                    });
                if selection_changed {
                    dialog.files.refresh_selection_totals();
                }

                ui.separator();
                ui.heading("Identity");
                ui.label(
                    "The info hash identifies the content and cannot be changed. The editable Peer ID below is used by tracker audit requests.",
                );
                ui.horizontal(|ui| {
                    ui.label("Audit Peer ID");
                    ui.add(
                        egui::TextEdit::singleline(&mut dialog.peer_id)
                            .desired_width(240.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if ui.button("Generate another").clicked() {
                        dialog.peer_id = metainfo::generate_peer_id(&peer_prefix);
                    }
                    ui.label(format!("{}/20 bytes", dialog.peer_id.len()));
                });
                ui.label(
                    "The normal transfer backend uses its own session-wide Peer ID; changing the audit ID does not change production peer connections.",
                );
                ui.label(
                    "The Tracker URL above is attached to this download.",
                );
                if dialog.tracker_audit.started_only {
                    dialog.start_immediately = false;
                }
                ui.add_enabled(
                    !dialog.tracker_audit.started_only,
                    egui::Checkbox::new(&mut dialog.start_immediately, "Start after adding"),
                );
                if dialog.tracker_audit.started_only {
                    ui.label(
                        "Normal transfer starts paused so the staging tracker receives only the single audit announce.",
                    );
                }
                if dialog.is_private {
                    ui.label("Private torrent: DHT, PEX and LSD will remain disabled.");
                }
                if !dialog.error.is_empty() {
                    ui.colored_label(egui::Color32::RED, &dialog.error);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Add torrent").clicked() {
                        add = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                    });
            });

        if let Some(directory) = picked_download_directory {
            self.last_download_directory = Some(directory);
        }

        if cancel || modal.should_close() {
            self.add_dialog = None;
            self.open_next_queued_torrent();
            return;
        }
        if !add {
            return;
        }

        let validation = {
            let dialog = self.add_dialog.as_ref().expect("dialog exists");
            validate_add_dialog(dialog)
        };
        let trackers = match validation {
            Ok(trackers) => trackers,
            Err(error) => {
                if let Some(dialog) = &mut self.add_dialog {
                    dialog.error = error;
                }
                return;
            }
        };
        let dialog = self.add_dialog.take().expect("dialog exists");
        self.last_download_directory = Some(dialog.download_directory.clone());
        let tracker_audit = dialog.tracker_audit.to_profile();
        let start_observation = tracker_audit.started_only;
        let name = dialog.name.clone();
        let state = if dialog.start_immediately {
            TorrentState::WaitingForEngine
        } else {
            TorrentState::Paused
        };
        let torrent = TorrentItem {
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
            name: name.clone(),
            source: dialog.source_path.display().to_string(),
            info_hashes: [
                dialog.info_hash_v1_hex.map(|hash| format!("v1: {hash}")),
                dialog.info_hash_v2_hex.map(|hash| format!("v2: {hash}")),
            ]
            .into_iter()
            .flatten()
            .collect(),
            trackers,
            peer_id: dialog.peer_id,
            files: dialog.files,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_rate: 0,
            upload_rate: 0,
            peers: 0,
            seeds: 0,
            state,
            is_private: dialog.is_private,
            overrides: TorrentOverrides {
                verify_after_download: dialog.verify_after_download,
                repair_policy: dialog.repair_policy,
                download_mode: dialog.download_mode,
                freeze_after_bytes: dialog.freeze_after_bytes,
                download_directory: Some(dialog.download_directory),
                ..Default::default()
            },
        };
        let id = torrent.info_hashes.join(";");
        if let Err(error) =
            self.control
                .add(torrent, &self.settings, Some(tracker_audit.to_stored()))
        {
            self.add_status = error;
            return;
        }
        self.tracker_audits.insert(id, tracker_audit);
        self.selected_torrent = self.control.torrents.len() - 1;
        if start_observation {
            let torrent = self
                .control
                .torrents
                .last()
                .expect("just added torrent exists");
            let id = torrent.info_hashes.join(";");
            if let Some(audit) = self.tracker_audits.get_mut(&id) {
                let full_size = torrent
                    .files
                    .files()
                    .iter()
                    .map(|file| file.size_bytes)
                    .sum();
                let step = TrackerAuditStep {
                    delay_seconds: 0,
                    uploaded: 0,
                    downloaded: 0,
                    left: full_size,
                    event: Some(AnnounceEvent::Started),
                };
                let observation_seconds = audit.observation_seconds;
                let _ = start_torrent_tracker_audit(
                    torrent,
                    audit,
                    vec![(0, step)],
                    observation_seconds,
                );
            }
            self.details_tab = DetailsTab::Trackers;
        } else {
            self.details_tab = DetailsTab::Files;
        }
        if self.add_status.is_empty() {
            self.add_status = if state == TorrentState::WaitingForEngine {
                format!("Added {name}; waiting for the full transfer engine")
            } else {
                format!("Added {name}")
            };
        }
        self.open_next_queued_torrent();
    }

    fn show_selected_torrent(&mut self, ui: &mut egui::Ui) {
        if self.control.torrents.is_empty() {
            ui.label("No torrents");
            return;
        }
        self.selected_torrent = self.selected_torrent.min(self.control.torrents.len() - 1);
        let mut commands = Vec::new();
        let torrent = &mut self.control.torrents[self.selected_torrent];
        let torrent_id = torrent.info_hashes.join(";");
        if let Some(command) =
            Self::show_torrent_header(ui, &torrent.name, torrent.state, &mut self.details_tab)
        {
            commands.push(command);
        }
        match self.details_tab {
            DetailsTab::Status => {
                let last = self.control.backend.journal().and_then(|j| {
                    torrent
                        .info_hashes
                        .iter()
                        .find_map(|h| j.latest(h.strip_prefix("v1: ")?))
                });
                show_runtime_status(ui, torrent, last.as_deref());
            }
            DetailsTab::Files => {
                let (selection, reveal) = Self::show_file_tree(ui, torrent);
                if let Some(indices) = selection {
                    commands.push(TorrentCommand::SetFiles(indices));
                }
                if let Some(index) = reveal {
                    self.add_status = match file_location::reveal(torrent, &self.settings, index) {
                        Ok(status) => status,
                        Err(error) => format!("Could not show file in folder: {error}"),
                    };
                }
            }
            DetailsTab::Info => Self::show_torrent_info(ui, torrent, &self.settings),
            DetailsTab::Settings => {
                let mut draft = torrent
                    .runtime
                    .mode_draft
                    .unwrap_or_else(|| torrent.mode_config());
                egui::ComboBox::from_id_salt("existing_download_mode")
                    .selected_text(draft.mode.label())
                    .show_ui(ui, |ui| {
                        for mode in crate::download_mode::DownloadMode::ALL {
                            ui.selectable_value(&mut draft.mode, mode, mode.label());
                        }
                    });
                if draft.mode == crate::download_mode::DownloadMode::FreezeAfter {
                    ui.add(
                        egui::DragValue::new(&mut draft.freeze_after_bytes)
                            .range(1..=u64::MAX)
                            .prefix("Freeze after bytes: "),
                    );
                }
                torrent.runtime.mode_draft = Some(draft);
                ui.label(torrent.runtime.mode.label());
                if ui
                    .add_enabled(
                        !matches!(
                            torrent.runtime.mode,
                            crate::runtime_status::ModeApplication::Applying { .. }
                        ),
                        egui::Button::new("Apply download mode"),
                    )
                    .clicked()
                {
                    commands.push(TorrentCommand::SetMode(draft));
                }
                ui.small("Applying briefly pauses and resumes active transfers. Already transmitted announces cannot be recalled.");
                let mut repair = torrent.overrides.repair_policy;
                let mut repair_changed = ui
                    .checkbox(
                        &mut repair.enabled,
                        "Automatically repair failed integrity checks",
                    )
                    .changed();
                repair_changed |= ui
                    .add(
                        egui::DragValue::new(&mut repair.max_attempts)
                            .range(1..=10)
                            .prefix("Maximum repair attempts: "),
                    )
                    .changed();
                if repair_changed {
                    commands.push(TorrentCommand::SetRepairPolicy(repair));
                }
                let before = torrent.overrides.verify_after_download;
                Self::show_torrent_settings(ui, torrent, &self.settings);
                if before != torrent.overrides.verify_after_download {
                    commands.push(TorrentCommand::SetVerifyAfterDownload(
                        torrent.overrides.verify_after_download,
                    ));
                    torrent.overrides.verify_after_download = before;
                }
            }
            DetailsTab::Peers => {
                ui.label(format!(
                    "Connected peers: {}; known seeds: {}",
                    torrent.peers, torrent.seeds
                ));
            }
            DetailsTab::Trackers => {
                let id = torrent.info_hashes.join(";");
                let audit = self.tracker_audits.entry(id.clone()).or_insert_with(|| {
                    TorrentTrackerAudit::new(
                        torrent.trackers.first().cloned().unwrap_or_default(),
                        torrent.selected_size(),
                        self.settings.connection.listen_port,
                    )
                });
                let changed =
                    show_torrent_tracker_audit(ui, torrent, audit, &mut self.behavior_catalog);
                if changed
                    && let Err(error) = self
                        .control
                        .backend
                        .save_torrent(&torrent_record(torrent, Some(audit)))
                {
                    audit.status = format!("Could not save tracker settings: {error}");
                }
            }
            DetailsTab::Pieces => {
                ui.label(format!(
                    "Verified payload: {} of {}",
                    format_size(torrent.files.selected_downloaded()),
                    format_size(torrent.files.selected_size())
                ));
            }
            DetailsTab::Log => {
                ui.horizontal(|ui| {
                    ui.label("Real download announces (not the standalone audit)");
                    if ui.button("Recheck files").clicked() {
                        commands.push(TorrentCommand::Recheck);
                    }
                });
                if let Some(journal) = self.control.backend.journal() {
                    if let Some(path) = journal.path() {
                        ui.label(format!("JSONL: {}", path.display()));
                    }
                    if let Some(error) = journal.error() {
                        ui.colored_label(egui::Color32::RED, error);
                    }
                    let hash = crate::announce_journal::torrent_hash(&torrent.info_hashes);
                    ui.horizontal(|ui| {
                        for (label, filter, enabled) in [
                            ("Export torrent JSONL", hash, hash.is_some()),
                            ("Export session JSONL", None, true),
                        ] {
                            if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("JSON Lines", &["jsonl"])
                                    .set_file_name("announce-export.jsonl")
                                    .save_file()
                            {
                                self.add_status = match journal.export(&path, filter) {
                                    Ok(count) => {
                                        format!("Exported {count} announces to {}", path.display())
                                    }
                                    Err(e) => e,
                                };
                            }
                        }
                    });
                    if self.log_torrent != hash.unwrap_or_default() {
                        self.log_torrent = hash.unwrap_or_default().into();
                        self.log_offset = 0;
                    }
                    let page = journal.page(
                        Some(hash.unwrap_or_default()),
                        self.log_offset,
                        crate::announce_journal::PAGE_SIZE,
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.log_offset > 0, egui::Button::new("Newer"))
                            .clicked()
                        {
                            self.log_offset = self
                                .log_offset
                                .saturating_sub(crate::announce_journal::PAGE_SIZE);
                        }
                        if ui
                            .add_enabled(
                                self.log_offset + crate::announce_journal::PAGE_SIZE < page.total,
                                egui::Button::new("Older"),
                            )
                            .clicked()
                        {
                            self.log_offset += crate::announce_journal::PAGE_SIZE;
                        }
                    });
                    if self.log_offset >= page.total {
                        self.log_offset = page.total.saturating_sub(1)
                            / crate::announce_journal::PAGE_SIZE
                            * crate::announce_journal::PAGE_SIZE;
                    }
                    let page = journal.page(
                        Some(hash.unwrap_or_default()),
                        self.log_offset,
                        crate::announce_journal::PAGE_SIZE,
                    );
                    ui.label(format!("{} live entries; {} / {} retained bytes; {} oversized entries on disk only. Export reads full disk history.", page.total, page.retained_bytes, crate::announce_journal::LIVE_BYTE_BUDGET, page.skipped_oversize));
                    egui::ScrollArea::vertical()
                        .id_salt("real_announce_log")
                        .show(ui, |ui| {
                            for entry in &page.entries {
                                ui.collapsing(entry.summary(), |ui| {
                                    ui.monospace(format!("info_hash={} peer_id={} port={} transport={} full_size={} selected={:?} request_transmitted={:?}", entry.announce.info_hash, entry.announce.peer_id_hex, entry.announce.port, entry.announce.transport, entry.announce.full_size, entry.announce.selected_bytes, entry.announce.request_transmitted));
                                    ui.label(format!(
                                        "Mode: {} · event: {:?} · reply: {} · HTTP {:?}",
                                        entry.announce.mode,
                                        entry.announce.event,
                                        entry.announce.outcome,
                                        entry.announce.reply.http_status
                                    ));
                                    ui.label(format!(
                                        "Interval {:?} s · min {:?} s · seeds {:?} · leechers {:?}",
                                        entry.announce.reply.interval,
                                        entry.announce.reply.min_interval,
                                        entry.announce.reply.seeders,
                                        entry.announce.reply.leechers
                                    ));
                                    if let Some(value) = &entry.announce.reply.warning {
                                        ui.label(format!("Warning: {value}"));
                                    }
                                    if let Some(value) = &entry.announce.reply.failure {
                                        ui.label(format!("Failure: {value}"));
                                    }
                                    egui::ScrollArea::vertical()
                                        .id_salt(("announce_peers", entry.sequence))
                                        .max_height(180.0)
                                        .show_rows(
                                            ui,
                                            20.0,
                                            entry.announce.reply.peers.len(),
                                            |ui, rows| {
                                                for peer in &entry.announce.reply.peers[rows] {
                                                    ui.monospace(peer.to_string());
                                                }
                                            },
                                        );
                                });
                            }
                        });
                } else {
                    ui.label("Transfer session is unavailable");
                }
            }
        }
        if let Some(status) =
            Self::submit_torrent_commands(&mut self.control, &torrent_id, commands)
        {
            self.add_status = status;
        }
    }

    fn show_torrent_header(
        ui: &mut egui::Ui,
        name: &str,
        state: TorrentState,
        details_tab: &mut DetailsTab,
    ) -> Option<TorrentCommand> {
        let mut command = None;
        // Keep these as separate rows even inside a horizontal parent layout.
        ui.vertical(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(name).heading()).wrap());
            ui.horizontal_wrapped(|ui| {
                let (label, action) = if state == TorrentState::Paused {
                    ("Resume", TorrentCommand::Resume)
                } else {
                    ("Pause", TorrentCommand::Pause)
                };
                if ui.button(label).clicked() {
                    command = Some(action);
                }
                ui.separator();
                for tab in DetailsTab::ALL {
                    ui.selectable_value(details_tab, tab, tab.label());
                }
            });
        });
        command
    }

    fn submit_torrent_commands(
        control: &mut Controller,
        id: &str,
        commands: Vec<TorrentCommand>,
    ) -> Option<String> {
        let mut first_error = None;
        let mut accepted = 0;
        let mut rejected = 0;
        for command in commands {
            match control.submit(id, command, None) {
                Ok(_) => accepted += 1,
                Err(error) => {
                    rejected += 1;
                    first_error.get_or_insert_with(|| error.to_string());
                }
            }
        }
        match first_error {
            Some(error) if accepted > 0 => Some(format!(
                "Commands queued: {accepted}; rejected: {rejected}. {error}"
            )),
            Some(error) => Some(error),
            None if accepted > 0 => Some(COMMAND_ACCEPTED.into()),
            None => None,
        }
    }

    fn show_file_tree(
        ui: &mut egui::Ui,
        torrent: &TorrentItem,
    ) -> (Option<Vec<usize>>, Option<usize>) {
        let mut select_all = None;
        let mut changes = std::collections::BTreeMap::new();
        let mut reveal = None;
        ui.horizontal(|ui| {
            if ui.small_button("Select all").clicked() {
                select_all = Some(true);
            }
            if ui.small_button("Select none").clicked() {
                select_all = Some(false);
            }
            ui.label(format!(
                "{} of {} files selected ({}), {} downloaded",
                torrent.files.selected_count(),
                torrent.files.files().len(),
                format_size(torrent.files.selected_size()),
                format_size(torrent.files.selected_downloaded())
            ));
        });
        egui::ScrollArea::vertical()
            .id_salt("selected_torrent_files_scroll")
            .max_height(270.0)
            .show_rows(ui, 22.0, torrent.files.row_count(), |ui, rows_range| {
                let rows = torrent.files.rows();
                let files = torrent.files.files();
                for row in &rows[rows_range] {
                    match row {
                        FileTreeRow::Group { name, file_count } => {
                            ui.strong(format!("{name}/ ({file_count} files)"));
                        }
                        FileTreeRow::File {
                            index,
                            display_start,
                        } => {
                            let file = &files[*index];
                            let progress = if file.size_bytes == 0 {
                                0.0
                            } else {
                                file.downloaded_bytes as f32 / file.size_bytes as f32
                            };
                            ui.horizontal(|ui| {
                                let mut selected = select_all.unwrap_or(file.selected);
                                if ui.checkbox(&mut selected, "").changed() {
                                    changes.insert(*index, selected);
                                }
                                let response = ui.add_sized(
                                    [(ui.available_width() - 220.0).max(80.0), 20.0],
                                    egui::Label::new(&file.path[*display_start..])
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                );
                                if file_location::context_menu(&response) {
                                    reveal = Some(*index);
                                }
                                ui.add(
                                    egui::ProgressBar::new(progress)
                                        .desired_width(80.0)
                                        .show_percentage(),
                                );
                                ui.label(format_size(file.size_bytes));
                            });
                        }
                    }
                }
            });
        (
            torrent.files.selection_after_edit(select_all, &changes),
            reveal,
        )
    }

    fn show_torrent_info(ui: &mut egui::Ui, torrent: &TorrentItem, global: &ClientSettings) {
        show_tracker_account_identity(
            ui,
            torrent
                .trackers
                .first()
                .map(String::as_str)
                .unwrap_or_default(),
        );
        let effective =
            EffectiveTorrentSettings::resolve(global, &torrent.overrides, torrent.is_private);
        egui::Grid::new("torrent_info")
            .striped(true)
            .show(ui, |ui| {
                for (label, value) in [
                    ("Source", torrent.source.clone()),
                    ("Info hash", torrent.info_hashes.join("; ")),
                    ("Audit Peer ID", torrent.peer_id.clone()),
                    (
                        "Download mode",
                        torrent.overrides.download_mode.label().into(),
                    ),
                    (
                        "Freeze threshold (bytes)",
                        (if torrent.overrides.freeze_after_bytes == 0 {
                            crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES
                        } else {
                            torrent.overrides.freeze_after_bytes
                        })
                        .to_string(),
                    ),
                    ("Selected size", format_size(torrent.selected_size())),
                    ("Downloaded", format_size(torrent.downloaded_bytes)),
                    ("Uploaded", format_size(torrent.uploaded_bytes)),
                    (
                        "Peers / seeds",
                        format!("{} / {}", torrent.peers, torrent.seeds),
                    ),
                    (
                        "Private torrent",
                        if torrent.is_private {
                            "Yes".into()
                        } else {
                            "No".into()
                        },
                    ),
                    ("Download directory", effective.download_directory),
                    (
                        "Discovery",
                        if torrent.is_private {
                            "Tracker only; DHT, PEX and LSD disabled".into()
                        } else {
                            format!(
                                "DHT {}, PEX {}, LSD {}",
                                effective.dht, effective.pex, effective.lsd
                            )
                        },
                    ),
                    (
                        "Tracker traffic",
                        if effective.tracker_enabled {
                            torrent.overrides.download_mode.description()
                        } else {
                            "Disabled"
                        }
                        .into(),
                    ),
                ] {
                    ui.label(label);
                    ui.label(value);
                    ui.end_row();
                }
            });
        ui.collapsing(format!("Trackers ({})", torrent.trackers.len()), |ui| {
            if global.privacy.redact_tracker_credentials {
                ui.label("Tracker URLs are hidden because credential redaction is enabled.");
            } else if torrent.trackers.is_empty() {
                ui.label("No trackers");
            } else {
                for tracker in &torrent.trackers {
                    ui.monospace(tracker);
                }
            }
        });
    }

    fn show_torrent_settings(
        ui: &mut egui::Ui,
        torrent: &mut TorrentItem,
        global: &ClientSettings,
    ) {
        let overrides = &mut torrent.overrides;
        ui.add_enabled_ui(!overrides.repair_policy.enabled, |ui| {
            ui.checkbox(
                &mut overrides.verify_after_download,
                "Verify after download",
            );
        });
        ui.label(
            "Full on-disk hash check before Done; does not replace normal piece verification.",
        );
        ui.label(
            "Overrides apply only to this torrent. Unchecked values inherit global preferences.",
        );
        option_string(
            ui,
            "Download directory",
            &mut overrides.download_directory,
            &global.download.download_directory,
        );
        ui.horizontal(|ui| {
            ui.label("Sequential download");
            egui::ComboBox::from_id_salt("torrent_sequential")
                .selected_text(match overrides.sequential_download {
                    None => "Use global",
                    Some(true) => "Enabled",
                    Some(false) => "Disabled",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut overrides.sequential_download, None, "Use global");
                    ui.selectable_value(&mut overrides.sequential_download, Some(true), "Enabled");
                    ui.selectable_value(
                        &mut overrides.sequential_download,
                        Some(false),
                        "Disabled",
                    );
                });
        });
        ui.horizontal(|ui| {
            ui.label("Priority");
            ui.radio_value(&mut overrides.priority, TorrentPriority::Low, "Low");
            ui.radio_value(&mut overrides.priority, TorrentPriority::Normal, "Normal");
            ui.radio_value(&mut overrides.priority, TorrentPriority::High, "High");
        });
        option_u64(
            ui,
            "Download limit KiB/s",
            &mut overrides.download_limit_kib,
            global.speed.download_limit_kib,
        );
        option_u64(
            ui,
            "Upload limit KiB/s",
            &mut overrides.upload_limit_kib,
            global.speed.upload_limit_kib,
        );
        option_usize(
            ui,
            "Maximum peers",
            &mut overrides.max_peers,
            global.connection.max_connections_per_torrent,
        );
        option_f64(
            ui,
            "Stop ratio",
            &mut overrides.ratio_limit,
            global.seeding.ratio_limit,
        );
        option_u64(
            ui,
            "Seeding time minutes",
            &mut overrides.seeding_time_limit_minutes,
            global.seeding.time_limit_minutes,
        );
        ui.separator();
        let effective = EffectiveTorrentSettings::resolve(global, overrides, torrent.is_private);
        ui.label(format!(
            "Effective: directory {}, down {} KiB/s, up {} KiB/s, peers {}, strategy {:?}, tracker {}",
            effective.download_directory,
            effective.download_limit_kib,
            effective.upload_limit_kib,
            effective.max_peers,
            effective.piece_strategy,
            if effective.tracker_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ));
        if torrent.is_private {
            ui.label("Private flag enforced: DHT, PEX and LSD are disabled.");
        }
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        let mut actions = PreferencesActions::default();
        let mut footer_action = None;

        egui::SidePanel::left("preferences_navigation")
            .resizable(false)
            .exact_width(170.0)
            .show(ctx, |ui| {
                ui.heading("Preferences");
                ui.separator();
                for tab in PreferencesTab::ALL {
                    if ui
                        .selectable_label(self.preferences_tab == tab, tab.label())
                        .clicked()
                    {
                        self.preferences_tab = tab;
                        self.settings_draft.ui.last_preferences_tab = tab.index();
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if self.settings != self.saved_settings {
                        ui.label("Runtime settings are not saved to disk");
                    }
                });
            });

        egui::TopBottomPanel::bottom("preferences_footer").show(ctx, |ui| {
            let dirty = self.settings_draft != self.settings;
            ui.horizontal(|ui| {
                if ui.button("Reset defaults").clicked() {
                    footer_action = Some(FooterAction::Reset);
                }
                if ui
                    .add_enabled(dirty, egui::Button::new("Cancel changes"))
                    .clicked()
                {
                    footer_action = Some(FooterAction::Cancel);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let needs_save = dirty || self.settings != self.saved_settings;
                    if ui
                        .add_enabled(needs_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        footer_action = Some(FooterAction::Save);
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Apply")).clicked() {
                        footer_action = Some(FooterAction::Apply);
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut runtime = PrivacyRuntime {
                blocklist_status: &self.blocklist_status,
                blocklist_rules: self.blocklist.rule_count(),
                blocklist_ranges: self.blocklist.range_count(),
                blocklist_updating: self.blocklist_update.is_some(),
                probe: &mut self.blocklist_probe,
                probe_result: &self.blocklist_probe_result,
            };
            actions = preferences::show_tab(
                ui,
                self.preferences_tab,
                &mut self.settings_draft,
                &mut runtime,
            );
            if !self.validation_messages.is_empty() {
                ui.separator();
                ui.colored_label(egui::Color32::RED, "Settings contain errors:");
                for message in &self.validation_messages {
                    ui.label(format!("- {message}"));
                }
            }
            if !self.settings_status.is_empty() {
                ui.separator();
                ui.label(&self.settings_status);
            }
        });

        if actions.update_blocklist {
            self.start_blocklist_update(self.settings_draft.privacy.blocklist_url.clone());
        }
        if actions.check_blocklist_ip {
            self.blocklist_probe_result = match self.blocklist_probe.parse::<IpAddr>() {
                Ok(ip) if self.blocklist.is_blocked(ip) => "Blocked".into(),
                Ok(_) => "Allowed".into(),
                Err(_) => "Invalid IP address".into(),
            };
        }
        if let Some(action) = footer_action {
            self.handle_footer_action(action);
        }
    }

    fn handle_footer_action(&mut self, action: FooterAction) {
        match action {
            FooterAction::Reset => {
                self.settings_draft = ClientSettings::default();
                self.settings_draft.ui.last_preferences_tab = self.preferences_tab.index();
                self.validation_messages.clear();
                self.settings_status = "Defaults loaded into the editor".into();
            }
            FooterAction::Cancel => {
                self.settings_draft = self.settings.clone();
                self.validation_messages.clear();
                self.settings_status = "Unapplied changes discarded".into();
            }
            FooterAction::Apply => self.apply_draft(false),
            FooterAction::Save => self.apply_draft(true),
        }
    }

    fn apply_draft(&mut self, persist: bool) {
        let issues = self.settings_draft.validate();
        self.validation_messages = issues
            .iter()
            .map(|issue| format!("{}: {}", issue.field, issue.message))
            .collect();
        if !self.validation_messages.is_empty() {
            self.settings_status = "Settings were not applied".into();
            return;
        }
        let diff = self.engine.apply_settings(&self.settings_draft);
        self.settings = self.settings_draft.clone();
        let _ = self.control.set_agent_settings(&self.settings);
        if persist {
            match self.settings.save() {
                Ok(()) => {
                    self.saved_settings = self.settings.clone();
                    self.settings_status = format!("Preferences saved; {}", diff.summary());
                }
                Err(error) => self.settings_status = format!("Applied but could not save: {error}"),
            }
        } else {
            self.settings_status = format!("Preferences applied; {}", diff.summary());
        }
    }
}

impl eframe::App for PeerLumeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (dropped_count, dropped_paths) = ctx.input(|input| {
            (
                input.raw.dropped_files.len(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect(),
            )
        });
        self.handle_dropped_files(dropped_count, dropped_paths);
        self.poll_session_events();
        self.poll_blocklist_update();
        self.show_header(ctx);
        match self.page {
            MainPage::Torrents => self.show_torrents(ctx),
            MainPage::Preferences => self.show_settings(ctx),
        }
        self.show_add_torrent_dialog(ctx);
        self.show_trash_confirmation(ctx);
        if self.add_dialog.is_none() {
            show_file_drop_overlay(ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

fn is_torrent_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
}

fn trash_paths(
    torrent: &TorrentItem,
    settings: &ClientSettings,
    scope: TrashScope,
) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    if scope.includes_data() {
        let effective =
            EffectiveTorrentSettings::resolve(settings, &torrent.overrides, torrent.is_private);
        let root = absolute_path(Path::new(&effective.download_directory));
        if let Ok(canonical_root) = root.canonicalize() {
            let multifile = torrent.files.files().len() > 1;
            for file in torrent.files.files() {
                let file_path = PathBuf::from(&file.path);
                let relative = if multifile {
                    PathBuf::from(&torrent.name).join(file_path)
                } else {
                    file_path
                };
                if !is_safe_relative_path(&relative) {
                    continue;
                }
                let candidate = root.join(relative);
                let Ok(canonical_candidate) = candidate.canonicalize() else {
                    continue;
                };
                if canonical_candidate.starts_with(&canonical_root)
                    && (candidate.is_file() || candidate.is_symlink())
                {
                    paths.insert(candidate);
                }
            }
        }
    }
    if scope.includes_source() {
        let source = absolute_path(Path::new(&torrent.source));
        if is_torrent_path(&source) && (source.is_file() || source.is_symlink()) {
            paths.insert(source);
        }
    }
    paths.into_iter().collect()
}

fn is_safe_relative_path(path: &Path) -> bool {
    let mut has_component = false;
    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return false;
        }
        has_component = true;
    }
    has_component
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn open_directory(configured_path: &Path) -> Result<PathBuf, String> {
    let path = if configured_path.is_absolute() {
        configured_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(configured_path)
    };
    fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    Command::new("explorer.exe")
        .arg(&canonical)
        .spawn()
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    Command::new("open")
        .arg(&canonical)
        .spawn()
        .map_err(|error| error.to_string())?;
    #[cfg(all(unix, not(target_os = "macos")))]
    Command::new("xdg-open")
        .arg(&canonical)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(canonical)
}

fn torrent_drop_candidates(
    dropped_count: usize,
    paths: &[PathBuf],
) -> Result<Vec<PathBuf>, &'static str> {
    if dropped_count == 0 || dropped_count != paths.len() {
        return Err("The dropped item is not a local file");
    }
    if paths.iter().any(|path| !is_torrent_path(path)) {
        return Err("Only files with the .torrent extension can be added");
    }
    Ok(paths.to_vec())
}

fn show_file_drop_overlay(ctx: &egui::Context) {
    let (hovered_count, accepts_torrent) = ctx.input(|input| {
        let files = &input.raw.hovered_files;
        (
            files.len(),
            !files.is_empty()
                && files
                    .iter()
                    .all(|file| file.path.as_deref().is_some_and(is_torrent_path)),
        )
    });
    if hovered_count == 0 {
        return;
    }

    let (message, color) = if accepts_torrent {
        (
            "Drop .torrent files to queue their previews",
            egui::Color32::from_rgba_unmultiplied(30, 110, 70, 225),
        )
    } else {
        (
            "Only local .torrent files are supported",
            egui::Color32::from_rgba_unmultiplied(130, 55, 45, 225),
        )
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("torrent_file_drop_overlay"),
    ));
    let rect = ctx.content_rect();
    painter.rect_filled(rect, 0.0, color);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(24.0),
        egui::Color32::WHITE,
    );
}

fn show_behavior_picker(
    ui: &mut egui::Ui,
    id: &str,
    catalog: &mut Result<BehaviorCatalog, String>,
    full_size: u64,
    selected_size: u64,
    port: u16,
) -> Option<StoredTrackerAuditProfile> {
    let mut selected = None;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text("Load behavior profile")
            .show_ui(ui, |ui| {
                if let Ok(catalog) = catalog {
                    for profile in &catalog.profiles {
                        if ui.selectable_label(false, &profile.label).clicked() {
                            selected =
                                Some(catalog.resolve(&profile.id, full_size, selected_size, port));
                        }
                    }
                }
            });
        if ui.button("Reload profiles").clicked() {
            *catalog = BehaviorCatalog::load();
        }
    });
    if let Err(error) = catalog {
        ui.colored_label(egui::Color32::RED, format!("Behavior profiles: {error}"));
    }
    match selected {
        Some(Ok(profile)) => Some(profile),
        Some(Err(error)) => {
            ui.colored_label(egui::Color32::RED, error);
            None
        }
        None => None,
    }
}

fn show_tracker_account_identity(ui: &mut egui::Ui, tracker_url: &str) {
    let fields = crate::identity::tracker_identity_fields(tracker_url);
    // Transient visibility only. Changing the URL hides the newly selected key.
    let id = ui.make_persistent_id(("tracker_account_identity", tracker_url));
    let mut reveal = ui.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.strong("Tracker account / user identity");
            if ui
                .add_enabled(
                    !fields.is_empty(),
                    egui::Button::new(if reveal { "Hide" } else { "Show" }),
                )
                .clicked()
            {
                reveal = !reveal;
            }
        });
        if fields.is_empty() {
            ui.label("No recognized account ID/key in this Tracker URL.");
        } else {
            egui::Grid::new(id.with("fields"))
                .num_columns(2)
                .show(ui, |ui| {
                    for field in &fields {
                        ui.label(field.label());
                        ui.monospace(field.display_value(reveal));
                        ui.end_row();
                    }
                });
        }
        ui.small(crate::identity::TRACKER_IDENTITY_HINT);
        ui.small("Read-only, derived from Tracker URL. Edit that URL to change the attached key.");
    });
    ui.data_mut(|data| data.insert_temp(id, reveal));
}

fn show_tracker_audit(
    ui: &mut egui::Ui,
    dialog: &mut AddTorrentDialog,
    catalog: &mut Result<BehaviorCatalog, String>,
) {
    let mut send = false;
    let audit = &mut dialog.tracker_audit;
    ui.heading("Tracker audit");
    ui.add_enabled_ui(audit.pending.is_none(), |ui| {
        if let Some(profile) = show_behavior_picker(
            ui,
            "add_behavior_profile",
            catalog,
            dialog.total_size,
            dialog.files.selected_size(),
            audit.port,
        ) {
            audit.apply_preset(&profile);
        }
    });
    if audit.preset_steps.len() > 1 {
        ui.label(format!("{} external scenario steps will be attached. Run them from the Trackers tab after adding.", audit.preset_steps.len()));
    }
    ui.label(
        "Send one controlled announce with independent counters and inspect the returned peer list.",
    );
    ui.label(
        "These values apply only to the audit request and are never used by the normal transfer session.",
    );
    ui.horizontal(|ui| {
        ui.label("Allowed staging host");
        ui.add(egui::TextEdit::singleline(&mut audit.allowed_host).desired_width(280.0));
        ui.label("Leave empty for localhost/loopback.");
    });
    ui.add_enabled(
        audit.pending.is_none(),
        egui::Checkbox::new(
            &mut audit.started_only,
            "Started-only observation after adding (staging audit only)",
        ),
    );
    if audit.started_only {
        ui.horizontal(|ui| {
            ui.label("Observation duration");
            ui.add(
                egui::DragValue::new(&mut audit.observation_seconds)
                    .range(60..=604_800)
                    .suffix(" s"),
            );
            ui.label(format!("({})", format_duration(audit.observation_seconds)));
        });
        ui.label(
            "The audit sends one started announce with uploaded=0, downloaded=0 and left=full size, then waits without further announces or completed.",
        );
        ui.label("The single announce will be sent automatically after Add torrent.");
    }
    ui.add_enabled_ui(!audit.started_only, |ui| {
        egui::Grid::new("add_torrent_tracker_audit_counters")
            .num_columns(4)
            .show(ui, |ui| {
                ui.label("Uploaded");
                ui.add(egui::DragValue::new(&mut audit.uploaded).range(0..=u64::MAX));
                ui.label("Downloaded");
                ui.add(egui::DragValue::new(&mut audit.downloaded).range(0..=u64::MAX));
                ui.end_row();
                ui.label("Left");
                ui.add(egui::DragValue::new(&mut audit.left).range(0..=u64::MAX));
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut audit.port).range(1..=u16::MAX));
                ui.end_row();
            });
    });
    if !audit.started_only {
        ui.horizontal(|ui| {
            ui.label("Event");
            egui::ComboBox::from_id_salt("add_torrent_tracker_audit_event")
                .selected_text(audit_event_label(audit.event))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut audit.event, None, "None");
                    ui.selectable_value(&mut audit.event, Some(AnnounceEvent::Started), "Started");
                    ui.selectable_value(
                        &mut audit.event,
                        Some(AnnounceEvent::Completed),
                        "Completed",
                    );
                    ui.selectable_value(&mut audit.event, Some(AnnounceEvent::Stopped), "Stopped");
                });
            if ui
                .add_enabled(
                    audit.pending.is_none(),
                    egui::Button::new("Send audit announce"),
                )
                .clicked()
            {
                send = true;
            }
            if audit.pending.is_some() {
                ui.spinner();
            }
        });
    }
    if !audit.status.is_empty() {
        ui.label(&audit.status);
    }
    if !audit.peers.is_empty() {
        ui.collapsing(format!("Returned peers ({})", audit.peers.len()), |ui| {
            for peer in &audit.peers {
                ui.monospace(peer);
            }
        });
    }
    if send {
        start_tracker_audit(dialog);
    }
}

fn start_tracker_audit(dialog: &mut AddTorrentDialog) {
    let preparation = (|| {
        let info_hash = dialog
            .info_hash_v1_hex
            .as_deref()
            .ok_or_else(|| "Tracker audit currently requires a v1 or hybrid torrent".to_owned())
            .and_then(parse_info_hash_hex)?;
        metainfo::validate_peer_id(&dialog.peer_id)?;
        let peer_id: [u8; 20] = dialog
            .peer_id
            .as_bytes()
            .try_into()
            .map_err(|_| "Peer ID must contain exactly 20 ASCII bytes".to_owned())?;
        let tracker_url = dialog.tracker_audit.tracker_url.trim().to_owned();
        if tracker_url.is_empty() {
            return Err("Choose a tracker URL for the audit request".into());
        }
        let allowed_host = dialog.tracker_audit.allowed_host.trim();
        let allow_hosts = (!allowed_host.is_empty()).then(|| vec![allowed_host.to_owned()]);
        let allow_hosts = allow_hosts.unwrap_or_default();
        validate_audit_target(&tracker_url, &allow_hosts)?;
        let request = AnnounceRequest {
            info_hash,
            peer_id,
            port: dialog.tracker_audit.port,
            uploaded: if dialog.tracker_audit.started_only {
                0
            } else {
                dialog.tracker_audit.uploaded
            },
            downloaded: if dialog.tracker_audit.started_only {
                0
            } else {
                dialog.tracker_audit.downloaded
            },
            left: if dialog.tracker_audit.started_only {
                dialog.total_size
            } else {
                dialog.tracker_audit.left
            },
            event: if dialog.tracker_audit.started_only {
                Some(AnnounceEvent::Started)
            } else {
                dialog.tracker_audit.event
            },
        };
        Ok((tracker_url, allow_hosts, request))
    })();
    let (tracker_url, allow_hosts, request) = match preparation {
        Ok(prepared) => prepared,
        Err(error) => {
            dialog.tracker_audit.status = error;
            return;
        }
    };

    let (sender, receiver) = mpsc::channel();
    dialog.tracker_audit.status = "Sending controlled announce...".into();
    dialog.tracker_audit.peers.clear();
    dialog.tracker_audit.pending = Some(receiver);
    std::thread::spawn(move || {
        let exchange = execute_tracker_audit(&tracker_url, &allow_hosts, request, 0);
        let _ = sender.send(exchange);
    });
}

fn poll_tracker_audit(dialog: &mut AddTorrentDialog) {
    let completed =
        dialog
            .tracker_audit
            .pending
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(exchange) => Some(Ok(exchange)),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Tracker audit worker stopped unexpectedly".to_owned()))
                }
            });
    let Some(result) = completed else {
        return;
    };
    dialog.tracker_audit.pending = None;
    match result {
        Ok(exchange) => {
            if let Some(error) = &exchange.error {
                dialog.tracker_audit.status = format!("Audit announce failed: {error}");
            } else {
                dialog.tracker_audit.status = format!(
                    "Tracker responded: interval {} seconds, {} peers",
                    exchange.interval_seconds.unwrap_or_default(),
                    exchange.peers.len()
                );
            }
            dialog.tracker_audit.peers.clone_from(&exchange.peers);
            dialog.tracker_audit.exchanges.push(exchange);
        }
        Err(error) => {
            dialog.tracker_audit.status = format!("Audit announce failed: {error}");
            dialog.tracker_audit.peers.clear();
        }
    }
}

fn audit_event_label(event: Option<AnnounceEvent>) -> &'static str {
    match event {
        None => "None",
        Some(AnnounceEvent::Started) => "Started",
        Some(AnnounceEvent::Completed) => "Completed",
        Some(AnnounceEvent::Stopped) => "Stopped",
    }
}

fn audit_event_storage(event: AnnounceEvent) -> &'static str {
    match event {
        AnnounceEvent::Started => "started",
        AnnounceEvent::Completed => "completed",
        AnnounceEvent::Stopped => "stopped",
    }
}

fn audit_event_from_storage(event: Option<&str>) -> Option<AnnounceEvent> {
    match event {
        Some("started") => Some(AnnounceEvent::Started),
        Some("completed") => Some(AnnounceEvent::Completed),
        Some("stopped") => Some(AnnounceEvent::Stopped),
        _ => None,
    }
}

enum TrackerAuditStepAction {
    Send(usize),
    MoveUp(usize),
    MoveDown(usize),
    Remove(usize),
}

fn show_torrent_tracker_audit(
    ui: &mut egui::Ui,
    torrent: &mut TorrentItem,
    audit: &mut TorrentTrackerAudit,
    catalog: &mut Result<BehaviorCatalog, String>,
) -> bool {
    poll_torrent_tracker_audit(audit);
    let mut profile_changed = false;
    ui.heading("Per-torrent tracker audit");
    ui.add_enabled_ui(audit.run.is_none(), |ui| {
        let full_size = torrent
            .files
            .files()
            .iter()
            .map(|file| file.size_bytes)
            .sum();
        if let Some(profile) = show_behavior_picker(
            ui,
            "torrent_behavior_profile",
            catalog,
            full_size,
            torrent.selected_size(),
            audit.port,
        ) {
            let exchanges = std::mem::take(&mut audit.exchanges);
            *audit = TorrentTrackerAudit::from_stored(
                audit.tracker_url.clone(),
                full_size,
                audit.port,
                Some(&profile),
            );
            audit.exchanges = exchanges;
            audit.status = "External profile applied; saved as a per-torrent snapshot".into();
            profile_changed = true;
        }
    });
    ui.label(
        "The scenario below is independent from normal tracker reporting and keeps a separate exchange log.",
    );
    ui.horizontal(|ui| {
        ui.label("Tracker URL");
        if ui
            .add(egui::TextEdit::singleline(&mut audit.tracker_url).desired_width(560.0))
            .changed()
        {
            torrent.trackers.clear();
            let tracker_url = audit.tracker_url.trim();
            if !tracker_url.is_empty() {
                torrent.trackers.push(tracker_url.to_owned());
            }
            profile_changed = true;
        }
    });
    show_tracker_account_identity(ui, &audit.tracker_url);
    ui.label("Tracker URL changes are saved securely and applied to the transfer after restart.");
    ui.horizontal(|ui| {
        ui.label("Allowed staging host");
        profile_changed |= ui
            .add(egui::TextEdit::singleline(&mut audit.allowed_host).desired_width(280.0))
            .changed();
        ui.label("Leave empty for localhost/loopback.");
    });
    ui.horizontal(|ui| {
        ui.label("Announced port");
        profile_changed |= ui
            .add(egui::DragValue::new(&mut audit.port).range(1..=u16::MAX))
            .changed();
    });
    profile_changed |= ui
        .checkbox(
            &mut audit.started_only,
            "Started-only observation (staging audit only)",
        )
        .changed();
    if audit.started_only {
        ui.horizontal(|ui| {
            ui.label("Observation duration");
            profile_changed |= ui
                .add(
                    egui::DragValue::new(&mut audit.observation_seconds)
                        .range(60..=604_800)
                        .suffix(" s"),
                )
                .changed();
            ui.label(format!("({})", format_duration(audit.observation_seconds)));
        });
        ui.label(
            "One started announce is sent with uploaded=0, downloaded=0 and left=full selected size. No further announce, completed, or stopped is sent during the observation window.",
        );
    }

    ui.add_space(6.0);
    ui.strong("Scenario steps");
    ui.label("Delay is measured from the previous step. Every step performs one announce.");
    let mut action = None;
    ui.add_enabled_ui(!audit.started_only, |ui| {
        egui::Grid::new("torrent_tracker_audit_steps")
            .striped(true)
            .show(ui, |ui| {
                for heading in [
                    "#",
                    "Delay s",
                    "Uploaded",
                    "Downloaded",
                    "Left",
                    "Event",
                    "Actions",
                ] {
                    ui.strong(heading);
                }
                ui.end_row();
                let step_count = audit.steps.len();
                for (index, step) in audit.steps.iter_mut().enumerate() {
                    ui.label((index + 1).to_string());
                    profile_changed |= ui
                        .add(egui::DragValue::new(&mut step.delay_seconds).range(0..=86_400))
                        .changed();
                    profile_changed |= ui
                        .add(egui::DragValue::new(&mut step.uploaded).range(0..=u64::MAX))
                        .changed();
                    profile_changed |= ui
                        .add(egui::DragValue::new(&mut step.downloaded).range(0..=u64::MAX))
                        .changed();
                    profile_changed |= ui
                        .add(egui::DragValue::new(&mut step.left).range(0..=u64::MAX))
                        .changed();
                    egui::ComboBox::from_id_salt(("torrent_tracker_audit_event", index))
                        .selected_text(audit_event_label(step.event))
                        .show_ui(ui, |ui| {
                            profile_changed |=
                                ui.selectable_value(&mut step.event, None, "None").changed();
                            profile_changed |= ui
                                .selectable_value(
                                    &mut step.event,
                                    Some(AnnounceEvent::Started),
                                    "Started",
                                )
                                .changed();
                            profile_changed |= ui
                                .selectable_value(
                                    &mut step.event,
                                    Some(AnnounceEvent::Completed),
                                    "Completed",
                                )
                                .changed();
                            profile_changed |= ui
                                .selectable_value(
                                    &mut step.event,
                                    Some(AnnounceEvent::Stopped),
                                    "Stopped",
                                )
                                .changed();
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(audit.run.is_none(), egui::Button::new("Send"))
                            .clicked()
                        {
                            action = Some(TrackerAuditStepAction::Send(index));
                        }
                        if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                            action = Some(TrackerAuditStepAction::MoveUp(index));
                        }
                        if ui
                            .add_enabled(index + 1 < step_count, egui::Button::new("↓"))
                            .clicked()
                        {
                            action = Some(TrackerAuditStepAction::MoveDown(index));
                        }
                        if ui
                            .add_enabled(step_count > 1, egui::Button::new("Remove"))
                            .clicked()
                        {
                            action = Some(TrackerAuditStepAction::Remove(index));
                        }
                    });
                    ui.end_row();
                }
            });
    });

    let mut run_all = false;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                audit.run.is_none() && !audit.started_only,
                egui::Button::new("Add step"),
            )
            .clicked()
        {
            let previous = audit.steps.last().cloned().unwrap_or(TrackerAuditStep {
                delay_seconds: 0,
                uploaded: 0,
                downloaded: 0,
                left: torrent.selected_size(),
                event: None,
            });
            audit.steps.push(TrackerAuditStep {
                event: None,
                ..previous
            });
            profile_changed = true;
        }
        if ui
            .add_enabled(
                audit.run.is_none() && (audit.started_only || !audit.steps.is_empty()),
                egui::Button::new(if audit.started_only {
                    "Start observation"
                } else {
                    "Run scenario"
                }),
            )
            .clicked()
        {
            run_all = true;
        }
        if ui
            .add_enabled(audit.run.is_some(), egui::Button::new("Stop"))
            .clicked()
            && let Some(run) = &audit.run
        {
            run.cancel.store(true, Ordering::Relaxed);
            audit.status = "Stopping scenario...".into();
        }
        if ui.button("Clear exchange log").clicked() {
            audit.exchanges.clear();
        }
        if audit.run.is_some() {
            ui.spinner();
        }
    });

    match action {
        Some(TrackerAuditStepAction::Send(index)) => {
            let mut step = audit.steps[index].clone();
            step.delay_seconds = 0;
            let _ = start_torrent_tracker_audit(torrent, audit, vec![(index, step)], 0);
        }
        Some(TrackerAuditStepAction::MoveUp(index)) => {
            audit.steps.swap(index, index - 1);
            profile_changed = true;
        }
        Some(TrackerAuditStepAction::MoveDown(index)) => {
            audit.steps.swap(index, index + 1);
            profile_changed = true;
        }
        Some(TrackerAuditStepAction::Remove(index)) => {
            audit.steps.remove(index);
            profile_changed = true;
        }
        None => {}
    }
    if run_all {
        let (steps, observation_seconds) = if audit.started_only {
            let full_size = torrent
                .files
                .files()
                .iter()
                .map(|file| file.size_bytes)
                .sum();
            (
                vec![(
                    0,
                    TrackerAuditStep {
                        delay_seconds: 0,
                        uploaded: 0,
                        downloaded: 0,
                        left: full_size,
                        event: Some(AnnounceEvent::Started),
                    },
                )],
                audit.observation_seconds,
            )
        } else {
            (audit.steps.iter().cloned().enumerate().collect(), 0)
        };
        let _ = start_torrent_tracker_audit(torrent, audit, steps, observation_seconds);
    }
    if !audit.status.is_empty() {
        ui.label(&audit.status);
    }

    ui.separator();
    ui.strong(format!("Tracker exchange log ({})", audit.exchanges.len()));
    ui.label(audit.exchanges.status());
    if audit.exchanges.is_empty() {
        ui.label("No audit announces have been sent for this torrent.");
    }
    for exchange in audit.exchanges.iter().rev() {
        let title = format!(
            "{} · step {} · {} · up {} / down {} / left {}",
            exchange.timestamp_unix_ms,
            exchange.step_index + 1,
            audit_event_label(exchange.event),
            exchange.uploaded,
            exchange.downloaded,
            exchange.left
        );
        ui.collapsing(title, |ui| {
            ui.label(format!("Sent to host: {}", exchange.tracker_host));
            ui.monospace(format!("info_hash={}", exchange.info_hash_hex));
            ui.monospace(format!("peer_id={}", exchange.peer_id));
            ui.monospace(format!(
                "port={} uploaded={} downloaded={} left={} event={}",
                exchange.port,
                exchange.uploaded,
                exchange.downloaded,
                exchange.left,
                audit_event_label(exchange.event)
            ));
            if let Some(error) = &exchange.error {
                ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
            } else {
                ui.label(format!(
                    "Received interval: {} seconds",
                    exchange.interval_seconds.unwrap_or_default()
                ));
                ui.label(format!("Received peers: {}", exchange.peers.len()));
                egui::ScrollArea::vertical()
                    .id_salt((
                        "audit_peers",
                        exchange.timestamp_unix_ms,
                        exchange.step_index,
                    ))
                    .max_height(180.0)
                    .show_rows(ui, 20.0, exchange.peers.len(), |ui, rows| {
                        for peer in &exchange.peers[rows] {
                            ui.monospace(peer);
                        }
                    });
            }
        });
    }
    profile_changed
}

fn start_torrent_tracker_audit(
    torrent: &TorrentItem,
    audit: &mut TorrentTrackerAudit,
    steps: Vec<(usize, TrackerAuditStep)>,
    observation_seconds: u64,
) -> Result<(), String> {
    if audit.run.is_some() {
        return Err("An audit scenario is already running".into());
    }
    let info_hash = torrent
        .info_hashes
        .iter()
        .find_map(|hash| hash.strip_prefix("v1: "))
        .ok_or_else(|| "Tracker audit currently requires a v1 or hybrid torrent".to_owned())
        .and_then(parse_info_hash_hex);
    let info_hash = match info_hash {
        Ok(hash) => hash,
        Err(error) => {
            audit.status = error.clone();
            return Err(error);
        }
    };
    if let Err(error) = metainfo::validate_peer_id(&torrent.peer_id) {
        audit.status = error.clone();
        return Err(error);
    }
    let peer_id: [u8; 20] = torrent
        .peer_id
        .as_bytes()
        .try_into()
        .map_err(|_| "Peer ID must contain exactly 20 ASCII bytes".to_owned())?;
    let tracker_url = audit.tracker_url.trim().to_owned();
    let allowed_host = audit.allowed_host.trim();
    let allow_hosts = if allowed_host.is_empty() {
        Vec::new()
    } else {
        vec![allowed_host.to_owned()]
    };
    if let Err(error) = validate_audit_target(&tracker_url, &allow_hosts) {
        audit.status = error.clone();
        return Err(error);
    }
    let port = audit.port;
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (sender, receiver) = mpsc::sync_channel(4);
    audit.status = if observation_seconds == 0 {
        format!("Running {} audit step(s)...", steps.len())
    } else {
        format!(
            "Starting {} observation with one announce...",
            format_duration(observation_seconds)
        )
    };
    audit.run = Some(TrackerAuditRun { receiver, cancel });
    std::thread::spawn(move || {
        for (step_index, step) in steps {
            if !wait_for_audit_delay(step.delay_seconds, &worker_cancel) {
                let _ = sender.send(TrackerAuditWorkerEvent::Cancelled);
                return;
            }
            let request = AnnounceRequest {
                info_hash,
                peer_id,
                port,
                uploaded: step.uploaded,
                downloaded: step.downloaded,
                left: step.left,
                event: step.event,
            };
            let exchange = execute_tracker_audit(&tracker_url, &allow_hosts, request, step_index);
            if sender
                .send(TrackerAuditWorkerEvent::Exchange(exchange))
                .is_err()
            {
                return;
            }
        }
        if observation_seconds > 0 {
            if sender
                .send(TrackerAuditWorkerEvent::ObservationStarted(
                    observation_seconds,
                ))
                .is_err()
            {
                return;
            }
            if !wait_for_audit_delay(observation_seconds, &worker_cancel) {
                let _ = sender.send(TrackerAuditWorkerEvent::Cancelled);
                return;
            }
        }
        let _ = sender.send(TrackerAuditWorkerEvent::Finished);
    });
    Ok(())
}

fn poll_torrent_tracker_audit(audit: &mut TorrentTrackerAudit) {
    let mut finished = None;
    if let Some(run) = &audit.run {
        loop {
            match run.receiver.try_recv() {
                Ok(TrackerAuditWorkerEvent::Exchange(exchange)) => {
                    audit.exchanges.push(exchange);
                    audit.status =
                        format!("Completed {} audit step(s)", audit.exchanges.recorded());
                }
                Ok(TrackerAuditWorkerEvent::ObservationStarted(seconds)) => {
                    audit.status = format!(
                        "Observing for {}; no further announces will be sent",
                        format_duration(seconds)
                    );
                }
                Ok(TrackerAuditWorkerEvent::Finished) => {
                    finished = Some(if audit.started_only {
                        "Started-only observation completed".to_owned()
                    } else {
                        "Scenario completed".to_owned()
                    });
                    break;
                }
                Ok(TrackerAuditWorkerEvent::Cancelled) => {
                    finished = Some("Scenario cancelled".to_owned());
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = Some("Audit worker stopped unexpectedly".to_owned());
                    break;
                }
            }
        }
    }
    if let Some(status) = finished {
        audit.run = None;
        audit.status = status;
    }
}

fn wait_for_audit_delay(seconds: u64, cancel: &AtomicBool) -> bool {
    crate::tracker_behavior::wait_for_delay(seconds, cancel)
}

fn execute_tracker_audit(
    tracker_url: &str,
    allow_hosts: &[String],
    request: AnnounceRequest,
    step_index: usize,
) -> TrackerAuditExchange {
    TrackerAuditExchange::execute(tracker_url, allow_hosts, request, step_index)
}

fn validate_add_dialog(dialog: &AddTorrentDialog) -> Result<Vec<String>, String> {
    dialog.repair_policy.validate()?;
    dialog
        .download_mode
        .validate_threshold(dialog.freeze_after_bytes)?;
    dialog
        .download_mode
        .validate_metadata(dialog.info_hash_v1_hex.is_some(), dialog.total_size)?;
    if dialog.files.selected_count() == 0 {
        return Err("Select at least one file".into());
    }
    if dialog.download_directory.trim().is_empty() {
        return Err("Choose a download directory".into());
    }
    if Path::new(dialog.download_directory.trim()).is_file() {
        return Err("Download location points to a file, not a directory".into());
    }
    metainfo::validate_peer_id(&dialog.peer_id)?;
    if dialog.tracker_audit.started_only {
        if dialog.info_hash_v1_hex.is_none() {
            return Err(
                "Started-only observation currently requires a v1 or hybrid torrent".into(),
            );
        }
        let allowed_host = dialog.tracker_audit.allowed_host.trim();
        let allow_hosts = if allowed_host.is_empty() {
            Vec::new()
        } else {
            vec![allowed_host.to_owned()]
        };
        validate_audit_target(dialog.tracker_audit.tracker_url.trim(), &allow_hosts)?;
    }

    let tracker = dialog.tracker_audit.tracker_url.trim();
    let trackers = if tracker.is_empty() {
        Vec::new()
    } else {
        if !["http://", "https://", "udp://"]
            .iter()
            .any(|prefix| tracker.starts_with(prefix))
        {
            return Err(format!("Unsupported tracker URL: {tracker}"));
        }
        vec![tracker.to_owned()]
    };
    if dialog.is_private && dialog.start_immediately && trackers.is_empty() {
        return Err("A private torrent needs a tracker URL before it can start".into());
    }
    Ok(trackers)
}

fn torrent_record(
    torrent: &TorrentItem,
    tracker_audit: Option<&TorrentTrackerAudit>,
) -> TorrentRecord {
    torrent_to_record(torrent, tracker_audit.map(TorrentTrackerAudit::to_stored))
}

fn option_string(ui: &mut egui::Ui, label: &str, value: &mut Option<String>, default: &str) {
    let mut enabled = value.is_some();
    let mut edited = value.clone().unwrap_or_else(|| default.to_owned());
    ui.horizontal(|ui| {
        ui.checkbox(&mut enabled, label);
        ui.add_enabled_ui(enabled, |ui| {
            ui.text_edit_singleline(&mut edited);
        });
    });
    *value = enabled.then_some(edited);
}

fn option_u64(ui: &mut egui::Ui, label: &str, value: &mut Option<u64>, default: u64) {
    let mut enabled = value.is_some();
    let mut edited = value.unwrap_or(default);
    ui.horizontal(|ui| {
        ui.checkbox(&mut enabled, label);
        ui.add_enabled_ui(enabled, |ui| {
            ui.add(egui::DragValue::new(&mut edited).range(0..=10_000_000));
        });
    });
    *value = enabled.then_some(edited);
}

fn option_usize(ui: &mut egui::Ui, label: &str, value: &mut Option<usize>, default: usize) {
    let mut enabled = value.is_some();
    let mut edited = value.unwrap_or(default);
    ui.horizontal(|ui| {
        ui.checkbox(&mut enabled, label);
        ui.add_enabled_ui(enabled, |ui| {
            ui.add(egui::DragValue::new(&mut edited).range(1..=100_000));
        });
    });
    *value = enabled.then_some(edited);
}

fn option_f64(ui: &mut egui::Ui, label: &str, value: &mut Option<f64>, default: f64) {
    let mut enabled = value.is_some();
    let mut edited = value.unwrap_or(default);
    ui.horizontal(|ui| {
        ui.checkbox(&mut enabled, label);
        ui.add_enabled_ui(enabled, |ui| {
            ui.add(
                egui::DragValue::new(&mut edited)
                    .range(0.01..=1000.0)
                    .speed(0.1),
            );
        });
    });
    *value = enabled.then_some(edited);
}

fn format_rate(bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        "--".into()
    } else {
        format!("{}/s", format_size(bytes_per_second))
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

fn show_runtime_status(
    ui: &mut egui::Ui,
    torrent: &TorrentItem,
    last: Option<&crate::announce_journal::JournalEntry>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("shared_runtime_status")
            .striped(true)
            .show(ui, |ui| {
                for (label, value) in crate::runtime_status::status_card(torrent, last) {
                    ui.label(label);
                    ui.label(value);
                    ui.end_row();
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_context_menu_requests_only_navigation_not_a_selection_or_transfer_change() {
        fn frame(
            context: &egui::Context,
            torrent: &TorrentItem,
            events: Vec<egui::Event>,
        ) -> (egui::FullOutput, (Option<Vec<usize>>, Option<usize>)) {
            let mut action = (None, None);
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        action = PeerLumeApp::show_file_tree(ui, torrent);
                    });
                },
            );
            (output, action)
        }
        fn text_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Missing rendered text: {label}"))
        }
        fn pointer(
            pos: egui::Pos2,
            button: egui::PointerButton,
            pressed: bool,
        ) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }
        let context = egui::Context::default();
        let mut torrent = crate::client::tests::torrent();
        torrent.files =
            FileSelectionModel::from_files([("another.bin".into(), 100), ("file.bin".into(), 100)]);
        let state = torrent.state;
        frame(&context, &torrent, vec![]);
        let (output, action) = frame(&context, &torrent, vec![]);
        assert_eq!(action, (None, None));
        let file = text_center(&output, "file.bin");
        frame(
            &context,
            &torrent,
            pointer(file, egui::PointerButton::Secondary, true),
        );
        frame(
            &context,
            &torrent,
            pointer(file, egui::PointerButton::Secondary, false),
        );
        let (output, action) = frame(&context, &torrent, vec![]);
        assert_eq!(action, (None, None));
        let menu = text_center(&output, "Show in folder");
        frame(
            &context,
            &torrent,
            pointer(menu, egui::PointerButton::Primary, true),
        );
        let (_, action) = frame(
            &context,
            &torrent,
            pointer(menu, egui::PointerButton::Primary, false),
        );
        assert_eq!(action, (None, Some(1)));
        assert!(torrent.files.files()[0].selected);
        assert!(torrent.files.files()[1].selected);
        assert_eq!(torrent.downloaded_bytes, 0);
        assert_eq!(torrent.state, state);
    }

    #[test]
    #[ignore = "installed frontend workflow; Reliability CI supplies the copied adapter"]
    fn reliability_installed_frontend_workflow_egui() {
        crate::control::test_support::installed_frontend_workflow(
            |control, command| {
                let id = control.torrents[0].info_hashes.join(";");
                assert_eq!(
                    PeerLumeApp::submit_torrent_commands(control, &id, vec![command]).unwrap(),
                    COMMAND_ACCEPTED
                );
            },
            |control| {
                let torrent = &control.torrents[0];
                let context = egui::Context::default();
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1600.0, 1000.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show_runtime_status(ui, torrent, None);
                        });
                    },
                );
                assert!(!output.shapes.is_empty());
            },
        );
    }

    #[test]
    fn egui_dispatch_distinguishes_acceptance_from_application_and_reports_rejection() {
        let mut control = Controller::new(
            ClientBackend::offline(),
            vec![crate::client::tests::torrent()],
        );
        assert_eq!(
            PeerLumeApp::submit_torrent_commands(&mut control, "v1: abc", vec![]),
            None
        );
        let before = control.torrents[0].state;
        let rejected = PeerLumeApp::submit_torrent_commands(
            &mut control,
            "v1: abc",
            vec![TorrentCommand::Pause],
        )
        .unwrap();
        assert!(rejected.contains("unavailable"));
        assert_eq!(control.torrents[0].state, before);
        let mut fixture = crate::control::test_support::LiveFixture::new();
        let accepted = PeerLumeApp::submit_torrent_commands(
            &mut fixture.controller,
            &fixture.id,
            vec![TorrentCommand::Resume],
        )
        .unwrap();
        assert_eq!(accepted, COMMAND_ACCEPTED);
        assert_eq!(fixture.controller.torrents[0].state, TorrentState::Paused);
        fixture.wait_for(|fixture| fixture.controller.torrents[0].state == TorrentState::Seeding);
        let partial = PeerLumeApp::submit_torrent_commands(
            &mut fixture.controller,
            &fixture.id,
            vec![
                TorrentCommand::SetRepairPolicy(crate::runtime_status::RepairPolicy {
                    enabled: true,
                    max_attempts: 0,
                }),
                TorrentCommand::Pause,
            ],
        )
        .unwrap();
        assert!(partial.contains("queued: 1; rejected: 1"));
        assert!(partial.contains("Repair attempts"));
        fixture.wait_for(|fixture| fixture.controller.torrents[0].state == TorrentState::Paused);
    }

    #[test]
    fn egui_status_renders_the_shared_unconfirmed_and_active_policy_model() {
        use crate::runtime_status::{ModeApplication, ModeConfig};
        let mut torrent = crate::client::tests::torrent();
        for application in [
            ModeApplication::AwaitingEngine,
            ModeApplication::Active {
                config: ModeConfig::default(),
                tracker_enabled: true,
            },
        ] {
            torrent.runtime.mode = application;
            let expected = crate::runtime_status::status_card(&torrent, None);
            let context = egui::Context::default();
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 1000.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_runtime_status(ui, &torrent, None);
                    });
                },
            );
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(t) => Some(t.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            for (label, value) in expected {
                assert!(text.contains(&label), "missing label: {label}");
                assert!(text.contains(&value), "missing value: {value}");
            }
        }
    }

    #[test]
    fn tracker_account_ui_masks_by_default_reveals_explicitly_and_resets_on_url_change() {
        fn render(url: &str, previously_revealed_url: Option<&str>) -> String {
            let context = egui::Context::default();
            let output = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(previous) = previously_revealed_url {
                        let id = ui.make_persistent_id(("tracker_account_identity", previous));
                        ui.data_mut(|data| data.insert_temp(id, true));
                    }
                    show_tracker_account_identity(ui, url);
                });
            });
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        let url = "https://tracker.test/announce?uk=fixture-account-key&uid=42";
        let hidden = render(url, None);
        assert!(hidden.contains("Tracker account / user identity"));
        assert!(hidden.contains("(uk)"));
        assert!(hidden.contains("(uid)"));
        assert!(hidden.contains("[hidden]"));
        assert!(!hidden.contains("fixture-account-key"));
        assert!(render(url, Some(url)).contains("fixture-account-key"));
        let changed = render(
            "https://tracker.test/announce?uk=new-fixture-key",
            Some(url),
        );
        assert!(!changed.contains("new-fixture-key"));
    }

    #[test]
    fn add_dialog_defaults_to_normal_and_validates_all_download_modes() {
        let metainfo = TorrentMetainfo {
            name: "fixture".into(),
            files: vec![crate::metainfo::MetainfoFile {
                path: "file.bin".into(),
                size_bytes: 100,
            }],
            trackers: vec!["http://127.0.0.1/announce".into()],
            is_private: true,
            info_hash_v1_hex: Some("0123456789abcdef0123456789abcdef01234567".into()),
            info_hash_v2_hex: None,
        };
        let mut dialog = AddTorrentDialog::new(
            "test.torrent".into(),
            metainfo,
            &ClientSettings::default(),
            "downloads".into(),
        );
        assert_eq!(dialog.download_mode, DownloadMode::Normal);
        assert!(dialog.start_immediately);
        assert!(!dialog.tracker_audit.started_only);
        for mode in DownloadMode::ALL {
            dialog.download_mode = mode;
            assert!(validate_add_dialog(&dialog).is_ok());
        }
        dialog.info_hash_v1_hex = None;
        assert!(validate_add_dialog(&dialog).is_err());
    }

    #[test]
    fn add_dialog_preserves_every_external_step_and_first_step_edits() {
        let catalog =
            BehaviorCatalog::from_toml(include_str!("../config/tracker-behaviors.example.toml"))
                .unwrap();
        let profile = catalog.resolve("periodic-test", 1000, 250, 1234).unwrap();
        let mut audit = AddTrackerAudit::new("http://localhost/announce".into(), 1000, 1234);
        audit.apply_preset(&profile);
        audit.uploaded = 42;
        let saved = audit.to_profile().to_stored();
        assert_eq!(saved.steps.len(), 4);
        assert_eq!(saved.steps[0].uploaded, 42);
        assert_eq!(saved.steps[1..], profile.steps[1..]);
        assert_eq!(audit.tracker_url, "http://localhost/announce");
    }

    #[test]
    fn tracker_audit_defaults_are_safe_and_match_the_torrent() {
        let audit =
            AddTrackerAudit::new("http://127.0.0.1:8080/announce".into(), 1_048_576, 51_413);

        assert_eq!(audit.uploaded, 0);
        assert_eq!(audit.downloaded, 0);
        assert_eq!(audit.left, 1_048_576);
        assert_eq!(audit.port, 51_413);
        assert_eq!(audit.event, Some(AnnounceEvent::Started));
        assert!(audit.allowed_host.is_empty());
        assert!(!audit.started_only);
        assert_eq!(audit.observation_seconds, 14_400);
        assert!(audit.pending.is_none());
    }

    #[test]
    fn tracker_audit_profile_round_trips_without_url_or_exchange_log() {
        let mut audit = TorrentTrackerAudit::new(
            "https://tracker.test/private-passkey/announce".into(),
            1_000,
            51_413,
        );
        audit.allowed_host = "tracker.test".into();
        audit.started_only = true;
        audit.observation_seconds = 21_600;
        audit.steps.push(TrackerAuditStep {
            delay_seconds: 30,
            uploaded: 40,
            downloaded: 60,
            left: 940,
            event: Some(AnnounceEvent::Completed),
        });

        let stored = audit.to_stored();
        let restored = TorrentTrackerAudit::from_stored(String::new(), 1_000, 1, Some(&stored));

        assert!(restored.tracker_url.is_empty());
        assert!(restored.exchanges.is_empty());
        assert_eq!(restored.allowed_host, "tracker.test");
        assert_eq!(restored.port, 51_413);
        assert!(restored.started_only);
        assert_eq!(restored.observation_seconds, 21_600);
        assert_eq!(restored.steps.len(), 2);
        assert_eq!(restored.steps[1].delay_seconds, 30);
        assert_eq!(restored.steps[1].event, Some(AnnounceEvent::Completed));
    }

    #[test]
    fn audit_log_info_hash_uses_lowercase_hex() {
        assert_eq!(
            crate::live_history::hex_bytes(&[0x00, 0xab, 0xff]),
            "00abff"
        );
    }

    #[test]
    fn dropped_torrent_extension_is_case_insensitive() {
        let path = PathBuf::from(r"C:\downloads\Example.TORRENT");

        assert_eq!(
            torrent_drop_candidates(1, std::slice::from_ref(&path)),
            Ok(vec![path])
        );
    }

    #[test]
    fn dropped_non_torrent_file_is_rejected() {
        let paths = [PathBuf::from(r"C:\downloads\notes.txt")];

        assert_eq!(
            torrent_drop_candidates(1, &paths),
            Err("Only files with the .torrent extension can be added")
        );
    }

    #[test]
    fn multiple_dropped_torrent_files_are_queued_in_order() {
        let paths = [
            PathBuf::from(r"C:\downloads\one.torrent"),
            PathBuf::from(r"C:\downloads\two.torrent"),
        ];

        assert_eq!(torrent_drop_candidates(2, &paths), Ok(paths.to_vec()));
    }

    #[test]
    fn non_local_drop_is_rejected() {
        assert_eq!(
            torrent_drop_candidates(1, &[]),
            Err("The dropped item is not a local file")
        );
    }

    #[test]
    fn recycle_bin_paths_reject_parent_and_absolute_components() {
        assert!(is_safe_relative_path(Path::new("Movie/video.mkv")));
        assert!(!is_safe_relative_path(Path::new("../outside.mkv")));
        assert!(!is_safe_relative_path(Path::new(r"C:\outside.mkv")));
        assert!(!is_safe_relative_path(Path::new("")));
    }

    #[test]
    fn recycle_bin_data_paths_are_existing_and_contained_by_download_root() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("peerlume-trash-paths-{nonce}"));
        let movie = root.join("Movie");
        fs::create_dir_all(&movie).unwrap();
        let expected = movie.join("video.mkv");
        fs::write(&expected, b"data").unwrap();
        let outside = root.parent().unwrap().join("outside.mkv");
        fs::write(&outside, b"keep").unwrap();

        let mut settings = ClientSettings::default();
        settings.download.download_directory = root.display().to_string();
        let torrent = TorrentItem {
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
            name: "Movie".into(),
            source: root.join("Movie.torrent").display().to_string(),
            info_hashes: vec!["v1: test".into()],
            trackers: Vec::new(),
            peer_id: "-PL0001-123456789012".into(),
            files: FileSelectionModel::from_files([
                ("video.mkv".into(), 4),
                ("../outside.mkv".into(), 4),
            ]),
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_rate: 0,
            upload_rate: 0,
            peers: 0,
            seeds: 0,
            state: TorrentState::Paused,
            is_private: false,
            overrides: TorrentOverrides::default(),
        };

        assert_eq!(
            trash_paths(&torrent, &settings, TrashScope::DownloadedFiles),
            vec![expected]
        );
        assert!(outside.exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(outside).unwrap();
    }
}
