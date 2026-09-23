use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use eframe::egui;

use crate::{
    blocklist::{self, Blocklist, BlocklistUpdate},
    engine::EngineRuntime,
    file_selection::{FileSelectionModel, FileTreeRow},
    metainfo::{self, TorrentMetainfo},
    preferences::{self, PreferencesActions, PreferencesTab, PrivacyRuntime},
    session::{SessionCommand, SessionEvent, SessionRuntime},
    settings::{ClientSettings, TrackerReportingMode, data_directory},
    state::{StateStore, StoredFile, TorrentRecord},
    torrent::{
        EffectiveTorrentSettings, TorrentItem, TorrentOverrides, TorrentPriority, TorrentState,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainPage {
    Torrents,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailsTab {
    Files,
    Info,
    Settings,
    Peers,
    Trackers,
    Pieces,
    Log,
}

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

struct AddTorrentDialog {
    source_path: PathBuf,
    name: String,
    is_private: bool,
    info_hash_v1_hex: Option<String>,
    info_hash_v2_hex: Option<String>,
    total_size: u64,
    files: FileSelectionModel,
    trackers_text: String,
    peer_id: String,
    download_directory: String,
    start_immediately: bool,
    error: String,
}

impl AddTorrentDialog {
    fn new(source_path: PathBuf, metainfo: TorrentMetainfo, settings: &ClientSettings) -> Self {
        let total_size = metainfo.total_size();
        let mut files = FileSelectionModel::from_metainfo_files(metainfo.files);
        if !settings.download.select_all_files {
            files.select_all(false);
        }
        Self {
            source_path,
            trackers_text: metainfo.trackers.join("\n"),
            peer_id: metainfo::generate_peer_id(&settings.bittorrent.peer_id_prefix),
            download_directory: settings.download.download_directory.clone(),
            start_immediately: settings.download.start_when_added,
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
    page: MainPage,
    details_tab: DetailsTab,
    preferences_tab: PreferencesTab,
    torrents: Vec<TorrentItem>,
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
    pending_torrent_paths: VecDeque<PathBuf>,
    add_status: String,
    magnet_input: String,
    state_store: Option<StateStore>,
    session_runtime: Option<SessionRuntime>,
    trash_confirmation: Option<TrashRequest>,
    pending_trash: Option<TrashRequest>,
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
        let state_store = StateStore::open(&data_directory().join("state.sqlite3")).ok();
        let session_runtime = SessionRuntime::start(
            256,
            PathBuf::from(&settings.download.download_directory),
            settings.privacy.tracker_reporting == TrackerReportingMode::Disabled,
        )
        .ok();
        let restored_torrents: Vec<_> = state_store
            .as_ref()
            .and_then(|store| store.load_all_torrents().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|record| torrent_from_record(record, &settings))
            .collect();
        let torrents = restored_torrents;

        let mut app = Self {
            page: MainPage::Torrents,
            details_tab: DetailsTab::Files,
            preferences_tab,
            torrents,
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
            pending_torrent_paths: VecDeque::new(),
            add_status: String::new(),
            magnet_input: String::new(),
            state_store,
            session_runtime,
            trash_confirmation: None,
            pending_trash: None,
        };
        if auto_update {
            app.start_blocklist_update(update_url);
        }
        if let Some(runtime) = &app.session_runtime {
            for torrent in &app.torrents {
                let _ = runtime.try_send(session_add_command(torrent, &app.settings));
            }
        }
        app
    }
}

impl PeerLumeApp {
    fn poll_session_events(&mut self) {
        loop {
            let event = self
                .session_runtime
                .as_ref()
                .and_then(SessionRuntime::try_recv);
            let Some(event) = event else { break };
            match event {
                SessionEvent::Added { id } => {
                    if let Some(torrent) = self.find_torrent_mut(&id)
                        && torrent.state == TorrentState::WaitingForEngine
                    {
                        torrent.state = TorrentState::Downloading;
                    }
                    self.add_status = "Transfer engine started the torrent".into();
                }
                SessionEvent::StateChanged { id, state } => {
                    if let Some(torrent) = self.find_torrent_mut(&id) {
                        torrent.state = if state == "paused" {
                            TorrentState::Paused
                        } else {
                            TorrentState::Downloading
                        };
                    }
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
                    if let Some(torrent) = self.find_torrent_mut(&id) {
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
                        torrent.state = if finished {
                            TorrentState::Seeding
                        } else if paused {
                            TorrentState::Paused
                        } else {
                            TorrentState::Downloading
                        };
                    }
                }
                SessionEvent::Error { id, message } => {
                    if self
                        .pending_trash
                        .as_ref()
                        .is_some_and(|request| request.torrent_id == id)
                    {
                        self.pending_trash = None;
                    }
                    if !id.is_empty()
                        && let Some(torrent) = self.find_torrent_mut(&id)
                    {
                        torrent.state = TorrentState::Error;
                        torrent.download_rate = 0;
                        torrent.upload_rate = 0;
                    }
                    self.add_status = format!("Transfer error: {message}");
                }
                SessionEvent::Removed { id } => {
                    if self
                        .pending_trash
                        .as_ref()
                        .is_some_and(|request| request.torrent_id == id)
                    {
                        self.complete_trash_request();
                    }
                }
                SessionEvent::Stopped => {
                    self.add_status = "Transfer engine stopped".into();
                }
            }
        }
    }

    fn find_torrent_mut(&mut self, id: &str) -> Option<&mut TorrentItem> {
        self.torrents.iter_mut().find(|torrent| {
            torrent.info_hashes.join(";") == id || torrent.info_hashes.contains(&id.to_owned())
        })
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
                .torrents
                .iter()
                .position(|torrent| torrent.info_hashes.join(";") == request.torrent_id)
            {
                self.torrents.remove(index);
                self.selected_torrent = self
                    .selected_torrent
                    .min(self.torrents.len().saturating_sub(1));
            }
            if let Some(store) = &self.state_store {
                let _ = store.remove_torrent(&request.torrent_id);
            }
        } else if let Some(index) = self
            .torrents
            .iter()
            .position(|torrent| torrent.info_hashes.join(";") == request.torrent_id)
        {
            let command = {
                let torrent = &mut self.torrents[index];
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
                session_add_command(torrent, &self.settings)
            };
            if let Some(runtime) = &self.session_runtime {
                let _ = runtime.try_send(command);
            }
            if let (Some(store), Some(torrent)) = (&mut self.state_store, self.torrents.get(index))
            {
                let _ = store.save_torrent(&torrent_record(torrent));
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
        let Some(runtime) = &self.session_runtime else {
            self.add_status = "Transfer engine is unavailable; nothing was deleted".into();
            return;
        };
        match runtime.try_send(SessionCommand::Remove {
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
                ui.selectable_value(&mut self.page, MainPage::Torrents, "Torrents");
                ui.selectable_value(&mut self.page, MainPage::Settings, "Preferences");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let active = self
                        .torrents
                        .iter()
                        .filter(|torrent| torrent.state == TorrentState::Downloading)
                        .count();
                    ui.label(format!("{active} active / {} total", self.torrents.len()));
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
                    for torrent in &mut self.torrents {
                        if matches!(
                            torrent.state,
                            TorrentState::Downloading | TorrentState::WaitingForEngine
                        ) {
                            torrent.state = TorrentState::Paused;
                            if let Some(runtime) = &self.session_runtime {
                                let _ = runtime.try_send(SessionCommand::Pause {
                                    id: torrent.info_hashes.join(";"),
                                });
                            }
                        }
                    }
                }
                if ui.button("Resume all").clicked() {
                    for torrent in &mut self.torrents {
                        if torrent.state == TorrentState::Paused {
                            torrent.state = TorrentState::WaitingForEngine;
                            torrent.download_rate = 0;
                            torrent.upload_rate = 0;
                            if let Some(runtime) = &self.session_runtime {
                                let _ = runtime.try_send(SessionCommand::Resume {
                                    id: torrent.info_hashes.join(";"),
                                });
                            }
                        }
                    }
                }
                if ui.button("Remove selected (keep data)").clicked() && !self.torrents.is_empty() {
                    let index = self.selected_torrent.min(self.torrents.len() - 1);
                    let removed = self.torrents.remove(index);
                    let id = removed.info_hashes.join(";");
                    if let Some(store) = &self.state_store {
                        let _ = store.remove_torrent(&id);
                    }
                    self.selected_torrent = self
                        .selected_torrent
                        .min(self.torrents.len().saturating_sub(1));
                    self.add_status = format!("Removed {}; downloaded data was kept", removed.name);
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
                                for (index, torrent) in self.torrents.iter().enumerate() {
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
                                        egui::ProgressBar::new(torrent.progress())
                                            .desired_width(120.0)
                                            .show_percentage(),
                                    );
                                    ui.label(torrent.state.label());
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
                self.add_dialog = Some(AddTorrentDialog::new(path, metainfo, &self.settings));
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
                self.torrents.push(TorrentItem {
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
                });
                self.selected_torrent = self.torrents.len() - 1;
                self.add_status = format!("Added {name}; waiting for metadata exchange");
                if let (Some(runtime), Some(torrent)) =
                    (&self.session_runtime, self.torrents.last())
                {
                    let mut command = session_add_command(torrent, &self.settings);
                    if let SessionCommand::AddTorrent { id: command_id, .. } = &mut command {
                        *command_id = id;
                    }
                    let _ = runtime.try_send(command);
                }
                self.magnet_input.clear();
            }
            Err(error) => self.add_status = error,
        }
    }

    fn show_add_torrent_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.add_dialog.as_mut() else {
            return;
        };
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
                        dialog.download_directory = path.display().to_string();
                    }
                });
                ui.label("Files from this torrent will be written under this directory.");

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
                ui.heading("Identity and trackers");
                ui.label(
                    "The info hash identifies the content and cannot be changed. Peer ID belongs to this client session.",
                );
                ui.horizontal(|ui| {
                    ui.label("Peer ID");
                    ui.monospace(&dialog.peer_id);
                    if ui.button("Generate another").clicked() {
                        dialog.peer_id = metainfo::generate_peer_id(&peer_prefix);
                    }
                });
                ui.text_edit_singleline(&mut dialog.peer_id);
                ui.label(
                    "Tracker URLs may contain another user's passkey. Review, replace, or remove those URLs before adding.",
                );
                ui.add(
                    egui::TextEdit::multiline(&mut dialog.trackers_text)
                        .desired_rows(4)
                        .hint_text("One tracker URL per line"),
                );
                ui.checkbox(&mut dialog.start_immediately, "Start after adding");
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
        let name = dialog.name.clone();
        let state = if dialog.start_immediately {
            TorrentState::WaitingForEngine
        } else {
            TorrentState::Paused
        };
        self.torrents.push(TorrentItem {
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
                download_directory: Some(dialog.download_directory),
                ..Default::default()
            },
        });
        self.selected_torrent = self.torrents.len() - 1;
        if let Some(torrent) = self.torrents.last() {
            if let Some(store) = &mut self.state_store
                && let Err(error) = store.save_torrent(&torrent_record(torrent))
            {
                self.add_status = format!("Added {name}; state save failed: {error}");
            }
            if let Some(runtime) = &self.session_runtime {
                let _ = runtime.try_send(session_add_command(torrent, &self.settings));
            }
        }
        self.details_tab = DetailsTab::Files;
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
        if self.torrents.is_empty() {
            ui.label("No torrents");
            return;
        }
        self.selected_torrent = self.selected_torrent.min(self.torrents.len() - 1);
        let torrent = &mut self.torrents[self.selected_torrent];
        ui.horizontal(|ui| {
            ui.heading(&torrent.name);
            ui.separator();
            if torrent.state == TorrentState::Paused {
                if ui.button("Resume").clicked() {
                    torrent.state = TorrentState::WaitingForEngine;
                    torrent.download_rate = 0;
                    torrent.upload_rate = 0;
                    if let Some(runtime) = &self.session_runtime {
                        let _ = runtime.try_send(SessionCommand::Resume {
                            id: torrent.info_hashes.join(";"),
                        });
                    }
                }
            } else if ui.button("Pause").clicked() {
                torrent.state = TorrentState::Paused;
                torrent.download_rate = 0;
                torrent.upload_rate = 0;
                if let Some(runtime) = &self.session_runtime {
                    let _ = runtime.try_send(SessionCommand::Pause {
                        id: torrent.info_hashes.join(";"),
                    });
                }
            }
            ui.selectable_value(&mut self.details_tab, DetailsTab::Files, "Files");
            ui.selectable_value(&mut self.details_tab, DetailsTab::Info, "Info");
            ui.selectable_value(
                &mut self.details_tab,
                DetailsTab::Settings,
                "Torrent settings",
            );
            ui.selectable_value(&mut self.details_tab, DetailsTab::Peers, "Peers");
            ui.selectable_value(&mut self.details_tab, DetailsTab::Trackers, "Trackers");
            ui.selectable_value(&mut self.details_tab, DetailsTab::Pieces, "Pieces");
            ui.selectable_value(&mut self.details_tab, DetailsTab::Log, "Log");
        });
        match self.details_tab {
            DetailsTab::Files => Self::show_file_tree(ui, torrent),
            DetailsTab::Info => Self::show_torrent_info(ui, torrent, &self.settings),
            DetailsTab::Settings => Self::show_torrent_settings(ui, torrent, &self.settings),
            DetailsTab::Peers => {
                ui.label(format!(
                    "Connected peers: {}; known seeds: {}",
                    torrent.peers, torrent.seeds
                ));
            }
            DetailsTab::Trackers => {
                for tracker in &torrent.trackers {
                    ui.monospace(crate::identity::redact_tracker_url(tracker));
                }
                if torrent.trackers.is_empty() {
                    ui.label("No trackers");
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
                ui.label("Session events will appear here as the network engine connects.");
            }
        }
    }

    fn show_file_tree(ui: &mut egui::Ui, torrent: &mut TorrentItem) {
        let previous_size = torrent.selected_size();
        ui.horizontal(|ui| {
            if ui.small_button("Select all").clicked() {
                torrent.files.select_all(true);
            }
            if ui.small_button("Select none").clicked() {
                torrent.files.select_all(false);
            }
            ui.label(format!(
                "{} of {} files selected ({}), {} downloaded",
                torrent.files.selected_count(),
                torrent.files.files().len(),
                format_size(torrent.files.selected_size()),
                format_size(torrent.files.selected_downloaded())
            ));
        });
        let mut selection_changed = false;
        egui::ScrollArea::vertical()
            .id_salt("selected_torrent_files_scroll")
            .max_height(270.0)
            .show_rows(ui, 22.0, torrent.files.row_count(), |ui, rows_range| {
                let (rows, files) = torrent.files.rows_and_files_mut();
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
                            let progress = if file.size_bytes == 0 {
                                0.0
                            } else {
                                file.downloaded_bytes as f32 / file.size_bytes as f32
                            };
                            ui.horizontal(|ui| {
                                selection_changed |= ui.checkbox(&mut file.selected, "").changed();
                                ui.add_sized(
                                    [(ui.available_width() - 220.0).max(80.0), 20.0],
                                    egui::Label::new(&file.path[*display_start..]).truncate(),
                                );
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
        if selection_changed {
            torrent.files.refresh_selection_totals();
        }
        let selected_size = torrent.selected_size();
        if selected_size != previous_size {
            torrent.downloaded_bytes = torrent.files.selected_downloaded();
        }
    }

    fn show_torrent_info(ui: &mut egui::Ui, torrent: &TorrentItem, global: &ClientSettings) {
        let effective =
            EffectiveTorrentSettings::resolve(global, &torrent.overrides, torrent.is_private);
        egui::Grid::new("torrent_info")
            .striped(true)
            .show(ui, |ui| {
                for (label, value) in [
                    ("Source", torrent.source.clone()),
                    ("Info hash", torrent.info_hashes.join("; ")),
                    ("Peer ID", torrent.peer_id.clone()),
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
                            "Enabled with accurate counters"
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
            MainPage::Settings => self.show_settings(ctx),
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

fn validate_add_dialog(dialog: &AddTorrentDialog) -> Result<Vec<String>, String> {
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

    let mut trackers = Vec::new();
    for line in dialog.trackers_text.lines() {
        let tracker = line.trim();
        if tracker.is_empty() {
            continue;
        }
        if !["http://", "https://", "udp://"]
            .iter()
            .any(|prefix| tracker.starts_with(prefix))
        {
            return Err(format!("Unsupported tracker URL: {tracker}"));
        }
        if !trackers.iter().any(|existing| existing == tracker) {
            trackers.push(tracker.to_owned());
        }
    }
    if dialog.is_private && dialog.start_immediately && trackers.is_empty() {
        return Err("A private torrent needs a tracker URL before it can start".into());
    }
    Ok(trackers)
}

fn session_add_command(torrent: &TorrentItem, settings: &ClientSettings) -> SessionCommand {
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
        id: torrent.info_hashes.join(";"),
        source: torrent.source.clone(),
        output_folder: effective.download_directory,
        only_files,
        trackers: torrent.trackers.clone(),
        paused: matches!(torrent.state, TorrentState::Paused | TorrentState::Error),
        peer_limit: (effective.max_peers > 0).then_some(effective.max_peers),
    }
}

fn torrent_record(torrent: &TorrentItem) -> TorrentRecord {
    let hash = |prefix: &str| {
        torrent
            .info_hashes
            .iter()
            .find_map(|value| value.strip_prefix(prefix).map(str::to_owned))
    };
    TorrentRecord {
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
    }
}

fn torrent_from_record(record: TorrentRecord, settings: &ClientSettings) -> TorrentItem {
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
            "error" => TorrentState::Error,
            _ => TorrentState::Paused,
        },
        is_private: record.is_private,
        overrides: TorrentOverrides {
            download_directory: record.download_directory,
            ..Default::default()
        },
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
