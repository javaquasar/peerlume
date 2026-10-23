//! Ratatui frontend backed by the same client model and component schema as egui.

use std::{
    collections::{HashMap, VecDeque},
    io::{self, Stdout},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{
        Block, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table, Tabs,
        Wrap,
    },
};

use crate::{
    client::{
        ClientBackend, SessionNotice, add_torrent_command, apply_session_event,
        torrent_from_record, torrent_to_record,
    },
    download_mode::DownloadMode,
    file_selection::FileSelectionModel,
    magnet::MagnetLink,
    metainfo::{self, TorrentMetainfo},
    session::SessionCommand,
    settings::ClientSettings,
    state::{StoredTrackerAuditProfile, StoredTrackerAuditStep},
    torrent::{EffectiveTorrentSettings, TorrentItem, TorrentOverrides, TorrentState},
    tracker::{
        AnnounceEvent, AnnounceRequest, announce_for_audit, compact_ipv4_peers, parse_info_hash_hex,
    },
    tracker_behavior::BehaviorCatalog,
    ui_schema::{COMPONENTS, MainPage, PreferencesTab, TorrentDetailsTab},
};

pub const SUPPORTED_COMPONENT_COUNT: usize = COMPONENTS.len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    TorrentPath,
    DownloadMode,
    FreezeAfterBytes,
    ExportJournal,
    ExportSessionJournal,
    Magnet,
    DownloadDirectory,
    TrackerUrl,
    BehaviorProfile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputPrompt {
    kind: PromptKind,
    title: &'static str,
    value: String,
}

pub struct TuiApp {
    settings: ClientSettings,
    backend: ClientBackend,
    torrents: Vec<TorrentItem>,
    tracker_audits: HashMap<String, StoredTrackerAuditProfile>,
    page: MainPage,
    details_tab: TorrentDetailsTab,
    preferences_tab: PreferencesTab,
    selected_torrent: usize,
    selected_file: usize,
    input: Option<InputPrompt>,
    status: String,
    log: VecDeque<String>,
    audit_result: Option<Receiver<String>>,
    audit_cancel: Option<Arc<AtomicBool>>,
    behavior_catalog: Result<BehaviorCatalog, String>,
    running: bool,
    last_download_directory: Option<String>,
    pending_add: Option<(PathBuf, TorrentMetainfo)>,
    show_tracker_identity: bool,
}

impl TuiApp {
    pub fn load() -> Self {
        let settings = ClientSettings::load();
        let backend = ClientBackend::start(&settings);
        let records = backend.load_torrents().unwrap_or_default();
        let tracker_audits = records
            .iter()
            .filter_map(|record| {
                record
                    .tracker_audit
                    .clone()
                    .map(|audit| (record.id.clone(), audit))
            })
            .collect();
        let torrents: Vec<_> = records
            .into_iter()
            .map(|record| torrent_from_record(record, &settings))
            .collect();
        for torrent in &torrents {
            let _ = backend.send(add_torrent_command(torrent, &settings));
        }
        Self::from_parts(settings, backend, torrents, tracker_audits)
    }

    fn from_parts(
        settings: ClientSettings,
        backend: ClientBackend,
        torrents: Vec<TorrentItem>,
        tracker_audits: HashMap<String, StoredTrackerAuditProfile>,
    ) -> Self {
        let preferences_tab = PreferencesTab::from_index(settings.ui.last_preferences_tab);
        Self {
            settings,
            backend,
            torrents,
            tracker_audits,
            page: MainPage::Torrents,
            details_tab: TorrentDetailsTab::Files,
            preferences_tab,
            selected_torrent: 0,
            selected_file: 0,
            input: None,
            status: "Ready".into(),
            log: VecDeque::new(),
            audit_result: None,
            audit_cancel: None,
            behavior_catalog: Err("Press b in Trackers to load external profiles".into()),
            running: true,
            last_download_directory: None,
            pending_add: None,
            show_tracker_identity: false,
        }
    }

    fn poll(&mut self) {
        while let Some(event) = self.backend.try_recv() {
            match apply_session_event(&mut self.torrents, event) {
                SessionNotice::Added { id } => self.push_log(format!("started {id}")),
                SessionNotice::Error { id, message } => {
                    self.status = format!("Transfer error: {message}");
                    self.push_log(format!("error {id}: {message}"));
                }
                SessionNotice::Removed { id } => self.push_log(format!("removed {id}")),
                SessionNotice::Stopped => self.push_log("transfer engine stopped"),
                SessionNotice::None => {}
            }
        }
        let audit_message = self
            .audit_result
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(message) = audit_message {
            self.status = message.clone();
            self.push_log(message);
            self.audit_result = None;
            self.audit_cancel = None;
        }
        self.clamp_selection();
    }

    fn push_log(&mut self, message: impl Into<String>) {
        if self.log.len() == 200 {
            self.log.pop_front();
        }
        self.log.push_back(message.into());
    }

    fn clamp_selection(&mut self) {
        self.selected_torrent = self
            .selected_torrent
            .min(self.torrents.len().saturating_sub(1));
        let file_count = self
            .torrents
            .get(self.selected_torrent)
            .map_or(0, |torrent| torrent.files.files().len());
        self.selected_file = self.selected_file.min(file_count.saturating_sub(1));
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::F(2)
            && self.page == MainPage::Torrents
            && (matches!(
                self.details_tab,
                TorrentDetailsTab::Info | TorrentDetailsTab::Trackers
            ) || self.input.as_ref().is_some_and(|p| {
                matches!(
                    p.kind,
                    PromptKind::DownloadMode | PromptKind::FreezeAfterBytes
                )
            }))
        {
            self.show_tracker_identity = !self.show_tracker_identity;
            return;
        }
        if self.input.is_some() {
            self.handle_prompt_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.running = false;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('1') => self.page = MainPage::Torrents,
            KeyCode::Char('2') => self.page = MainPage::Preferences,
            KeyCode::Char('a') if self.page == MainPage::Torrents => {
                self.open_prompt(PromptKind::TorrentPath, "Add .torrent path", String::new());
            }
            KeyCode::Char('m') if self.page == MainPage::Torrents => {
                self.open_prompt(PromptKind::Magnet, "Add magnet URI", String::new());
            }
            KeyCode::Char('p') if self.page == MainPage::Torrents => self.pause_selected(),
            KeyCode::Char('r') if self.page == MainPage::Torrents => self.resume_selected(),
            KeyCode::Char('P') if self.page == MainPage::Torrents => self.pause_all(),
            KeyCode::Char('R') if self.page == MainPage::Torrents => self.resume_all(),
            KeyCode::Char('d') if self.page == MainPage::Torrents => self.remove_selected(),
            KeyCode::Char('v') if self.page == MainPage::Torrents => {
                if let Some(torrent) = self.selected() {
                    self.status = match self.backend.send(SessionCommand::Recheck {
                        id: torrent.info_hashes.join(";"),
                    }) {
                        Ok(()) => {
                            "Rechecking files (existing payload is not a new download)".into()
                        }
                        Err(e) => e,
                    };
                }
            }
            KeyCode::Char('e') if self.details_tab == TorrentDetailsTab::Log => {
                self.open_prompt(
                    PromptKind::ExportJournal,
                    "Export selected torrent JSONL (new file path; no overwrite)",
                    "announce-export.jsonl".into(),
                );
            }
            KeyCode::Char('E') if self.details_tab == TorrentDetailsTab::Log => {
                self.open_prompt(
                    PromptKind::ExportSessionJournal,
                    "Export session JSONL (new file path; no overwrite)",
                    "session-export.jsonl".into(),
                );
            }
            KeyCode::Char('u')
                if self.page == MainPage::Torrents
                    && self.details_tab == TorrentDetailsTab::Trackers =>
            {
                let value = self
                    .selected()
                    .and_then(|torrent| torrent.trackers.first())
                    .cloned()
                    .unwrap_or_default();
                self.open_prompt(PromptKind::TrackerUrl, "Tracker URL", value);
            }
            KeyCode::Char('A')
                if self.page == MainPage::Torrents
                    && self.details_tab == TorrentDetailsTab::Trackers =>
            {
                self.start_tracker_audit();
            }
            KeyCode::Char('b')
                if self.page == MainPage::Torrents
                    && self.details_tab == TorrentDetailsTab::Trackers =>
            {
                self.behavior_catalog = BehaviorCatalog::load();
                match &self.behavior_catalog {
                    Ok(catalog) => {
                        self.status = catalog
                            .profiles
                            .iter()
                            .map(|profile| format!("{}: {}", profile.id, profile.label))
                            .collect::<Vec<_>>()
                            .join(" | ");
                        self.open_prompt(
                            PromptKind::BehaviorProfile,
                            "Behavior profile ID (Enter applies, Esc cancels)",
                            String::new(),
                        );
                    }
                    Err(error) => self.status = format!("Behavior profiles: {error}"),
                }
            }
            KeyCode::Char('x') if self.details_tab == TorrentDetailsTab::Trackers => {
                if let Some(cancel) = &self.audit_cancel {
                    cancel.store(true, Ordering::Relaxed);
                    self.status = "Cancelling tracker scenario...".into();
                }
            }
            KeyCode::Char('o')
                if self.page == MainPage::Torrents
                    && self.details_tab == TorrentDetailsTab::Settings =>
            {
                let value = self
                    .selected()
                    .and_then(|torrent| torrent.overrides.download_directory.clone())
                    .unwrap_or_else(|| self.settings.download.download_directory.clone());
                self.open_prompt(PromptKind::DownloadDirectory, "Download directory", value);
            }
            KeyCode::Char(' ')
                if self.page == MainPage::Torrents
                    && self.details_tab == TorrentDetailsTab::Files =>
            {
                self.toggle_selected_file()
            }
            KeyCode::Char(' ') if self.page == MainPage::Preferences => {
                self.toggle_primary_preference();
            }
            KeyCode::Char('s') if self.page == MainPage::Preferences => self.save_preferences(),
            KeyCode::Char('D') if self.page == MainPage::Preferences => {
                self.settings = ClientSettings::default();
                self.status = "Defaults loaded; press s to save".into();
            }
            KeyCode::Tab if self.page == MainPage::Torrents => {
                self.details_tab = self.details_tab.next();
                self.selected_file = 0;
            }
            KeyCode::BackTab if self.page == MainPage::Torrents => {
                self.details_tab = self.details_tab.previous();
                self.selected_file = 0;
            }
            KeyCode::Right if self.page == MainPage::Preferences => {
                self.preferences_tab = self.preferences_tab.next();
            }
            KeyCode::Left if self.page == MainPage::Preferences => {
                self.preferences_tab = self.preferences_tab.previous();
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            _ => {}
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, title: &'static str, value: String) {
        self.show_tracker_identity = false;
        self.input = Some(InputPrompt { kind, title, value });
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                self.pending_add = None;
            }
            KeyCode::Enter => {
                let prompt = self.input.take().expect("prompt exists");
                self.submit_prompt(prompt);
            }
            KeyCode::Backspace => {
                self.input.as_mut().expect("prompt exists").value.pop();
            }
            KeyCode::Char(character) => {
                let prompt = self.input.as_mut().expect("prompt exists");
                if prompt.kind == PromptKind::DownloadMode {
                    prompt.value.clear();
                }
                prompt.value.push(character);
            }
            _ => {}
        }
    }

    fn submit_prompt(&mut self, prompt: InputPrompt) {
        let retry = prompt.clone();
        let result = match prompt.kind {
            PromptKind::TorrentPath => self.prepare_torrent_add(PathBuf::from(prompt.value)),
            PromptKind::DownloadMode => self.confirm_torrent_mode(prompt.value.trim()),
            PromptKind::FreezeAfterBytes => self.confirm_freeze_threshold(prompt.value.trim()),
            PromptKind::ExportJournal => self.export_journal(Path::new(&prompt.value), false),
            PromptKind::ExportSessionJournal => self.export_journal(Path::new(&prompt.value), true),
            PromptKind::Magnet => self.add_magnet(&prompt.value),
            PromptKind::DownloadDirectory => self.set_download_directory(prompt.value),
            PromptKind::TrackerUrl => self.set_tracker_url(prompt.value),
            PromptKind::BehaviorProfile => self.apply_behavior_profile(prompt.value.trim()),
        };
        match result {
            Ok(message) => {
                self.status = message.clone();
                self.push_log(message);
            }
            Err(error) => {
                self.status = error;
                if self.pending_add.is_some()
                    && matches!(
                        retry.kind,
                        PromptKind::DownloadMode | PromptKind::FreezeAfterBytes
                    )
                {
                    self.input = Some(retry);
                }
            }
        }
    }

    fn move_selection(&mut self, direction: isize) {
        if self.page == MainPage::Torrents && self.details_tab == TorrentDetailsTab::Log {
            self.selected_file = self
                .selected_file
                .saturating_add_signed(direction)
                .min(u16::MAX as usize);
            return;
        }
        if self.page == MainPage::Torrents
            && self.details_tab == TorrentDetailsTab::Files
            && let Some(torrent) = self.selected()
        {
            self.selected_file =
                move_index(self.selected_file, torrent.files.files().len(), direction);
            return;
        }
        self.selected_torrent = move_index(self.selected_torrent, self.torrents.len(), direction);
        self.selected_file = 0;
        self.show_tracker_identity = false;
    }

    fn selected(&self) -> Option<&TorrentItem> {
        self.torrents.get(self.selected_torrent)
    }

    fn selected_mut(&mut self) -> Option<&mut TorrentItem> {
        self.torrents.get_mut(self.selected_torrent)
    }

    fn pause_selected(&mut self) {
        let Some(torrent) = self.selected_mut() else {
            return;
        };
        torrent.state = TorrentState::Paused;
        torrent.download_rate = 0;
        torrent.upload_rate = 0;
        let id = torrent.info_hashes.join(";");
        let _ = self.backend.send(SessionCommand::Pause { id });
    }

    fn resume_selected(&mut self) {
        let Some(torrent) = self.selected_mut() else {
            return;
        };
        torrent.state = TorrentState::WaitingForEngine;
        let id = torrent.info_hashes.join(";");
        let _ = self.backend.send(SessionCommand::Resume { id });
    }

    fn pause_all(&mut self) {
        for torrent in &mut self.torrents {
            torrent.state = TorrentState::Paused;
            torrent.download_rate = 0;
            torrent.upload_rate = 0;
            let _ = self.backend.send(SessionCommand::Pause {
                id: torrent.info_hashes.join(";"),
            });
        }
        self.status = "All torrents paused".into();
    }

    fn resume_all(&mut self) {
        for torrent in &mut self.torrents {
            torrent.state = TorrentState::WaitingForEngine;
            let _ = self.backend.send(SessionCommand::Resume {
                id: torrent.info_hashes.join(";"),
            });
        }
        self.status = "All torrents resumed".into();
    }

    fn remove_selected(&mut self) {
        if self.torrents.is_empty() {
            return;
        }
        let torrent = self.torrents.remove(self.selected_torrent);
        let id = torrent.info_hashes.join(";");
        let _ = self.backend.send(SessionCommand::Remove { id: id.clone() });
        let _ = self.backend.remove_torrent(&id);
        self.tracker_audits.remove(&id);
        self.status = format!("Removed {}; data kept", torrent.name);
        self.clamp_selection();
    }

    fn toggle_selected_file(&mut self) {
        let index = self.selected_file;
        let Some(torrent) = self.selected_mut() else {
            return;
        };
        let Some(file) = torrent.files.files().get(index) else {
            return;
        };
        torrent.files.set_selected(index, !file.selected);
        let command = crate::client::file_selection_command(torrent);
        let _ = self.backend.send(command);
        self.persist_selected();
    }

    fn persist_selected(&mut self) {
        let Some(torrent) = self.selected() else {
            return;
        };
        let id = torrent.info_hashes.join(";");
        let audit = self.tracker_audits.get(&id).cloned();
        let record = torrent_to_record(torrent, audit);
        if let Err(error) = self.backend.save_torrent(&record) {
            self.status = format!("Could not save torrent: {error}");
        }
    }

    fn prepare_torrent_add(&mut self, path: PathBuf) -> Result<String, String> {
        if path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            != Some("torrent".into())
        {
            return Err("Path must point to a .torrent file".into());
        }
        let metainfo = TorrentMetainfo::load(&path)?;
        self.pending_add = Some((path, metainfo));
        self.behavior_catalog = BehaviorCatalog::load();
        let modes = self
            .behavior_catalog
            .as_ref()
            .map(|catalog| catalog.download_modes.as_slice())
            .unwrap_or(&DownloadMode::ALL);
        let choices = modes
            .iter()
            .enumerate()
            .map(|(index, mode)| format!("{}: {}", index + 1, mode.label()))
            .collect::<Vec<_>>()
            .join(" | ");
        self.open_prompt(
            PromptKind::DownloadMode,
            "Download mode · Enter confirms · Esc cancels",
            "1".into(),
        );
        Ok(choices)
    }

    fn confirm_torrent_mode(&mut self, value: &str) -> Result<String, String> {
        let modes = self
            .behavior_catalog
            .as_ref()
            .map(|catalog| catalog.download_modes.as_slice())
            .unwrap_or(&DownloadMode::ALL);
        let mode = value
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| modes.get(index))
            .copied()
            .ok_or("Select a displayed download mode number")?;
        if mode == DownloadMode::FreezeAfter {
            self.open_prompt(
                PromptKind::FreezeAfterBytes,
                "Freeze after received bytes",
                crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES.to_string(),
            );
            return Ok("Choose the network-received byte threshold before adding".into());
        }
        let (path, metainfo) = self
            .pending_add
            .take()
            .ok_or("No torrent pending addition")?;
        self.add_torrent(path, metainfo, mode)
    }

    fn confirm_freeze_threshold(&mut self, value: &str) -> Result<String, String> {
        let threshold = value.parse::<u64>().map_err(|_| "Enter a byte count")?;
        DownloadMode::FreezeAfter.validate_threshold(threshold)?;
        let (path, meta) = self
            .pending_add
            .take()
            .ok_or("No torrent pending addition")?;
        self.add_torrent_with_threshold(path, meta, DownloadMode::FreezeAfter, threshold)
    }

    fn export_journal(&self, path: &Path, all: bool) -> Result<String, String> {
        let hash = if all {
            None
        } else {
            Some(
                crate::announce_journal::torrent_hash(
                    &self.selected().ok_or("Select a torrent")?.info_hashes,
                )
                .ok_or("Torrent has no v1 hash")?,
            )
        };
        let journal = self
            .backend
            .journal()
            .ok_or("Transfer session is unavailable")?;
        journal
            .export(path, hash)
            .map(|count| format!("Exported {count} announces to {}", path.display()))
    }

    fn add_torrent(
        &mut self,
        path: PathBuf,
        metainfo: TorrentMetainfo,
        download_mode: DownloadMode,
    ) -> Result<String, String> {
        self.add_torrent_with_threshold(
            path,
            metainfo,
            download_mode,
            crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES,
        )
    }

    fn add_torrent_with_threshold(
        &mut self,
        path: PathBuf,
        metainfo: TorrentMetainfo,
        download_mode: DownloadMode,
        freeze_after_bytes: u64,
    ) -> Result<String, String> {
        let total_size = metainfo.total_size();
        download_mode.validate_metadata(metainfo.info_hash_v1_hex.is_some(), total_size)?;
        let mut files = FileSelectionModel::from_metainfo_files(metainfo.files);
        if !self.settings.download.select_all_files {
            files.select_all(false);
        }
        let state = if self.settings.download.start_when_added {
            TorrentState::WaitingForEngine
        } else {
            TorrentState::Paused
        };
        let download_directory = self
            .last_download_directory
            .clone()
            .unwrap_or_else(|| self.settings.download.download_directory.clone());
        let torrent = TorrentItem {
            name: metainfo.name,
            source: path.display().to_string(),
            info_hashes: [
                metainfo.info_hash_v1_hex.map(|hash| format!("v1: {hash}")),
                metainfo.info_hash_v2_hex.map(|hash| format!("v2: {hash}")),
            ]
            .into_iter()
            .flatten()
            .collect(),
            trackers: metainfo.trackers,
            peer_id: metainfo::generate_peer_id(&self.settings.bittorrent.peer_id_prefix),
            files,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_rate: 0,
            upload_rate: 0,
            peers: 0,
            seeds: 0,
            state,
            is_private: metainfo.is_private,
            overrides: TorrentOverrides {
                download_mode,
                freeze_after_bytes,
                download_directory: Some(download_directory),
                ..Default::default()
            },
        };
        let id = torrent.info_hashes.join(";");
        if self
            .torrents
            .iter()
            .any(|existing| existing.info_hashes.join(";") == id)
        {
            return Err("Torrent is already added".into());
        }
        let audit = default_audit(total_size, self.settings.connection.listen_port);
        self.backend
            .save_torrent(&torrent_to_record(&torrent, Some(audit.clone())))?;
        self.backend
            .send(add_torrent_command(&torrent, &self.settings))?;
        let name = torrent.name.clone();
        self.tracker_audits.insert(id, audit);
        self.torrents.push(torrent);
        self.selected_torrent = self.torrents.len() - 1;
        Ok(format!("Added {name}"))
    }

    fn add_magnet(&mut self, uri: &str) -> Result<String, String> {
        let magnet = MagnetLink::parse(uri.trim())?;
        let name = magnet
            .display_name
            .unwrap_or_else(|| "Magnet metadata".into());
        let torrent = TorrentItem {
            name: name.clone(),
            source: uri.trim().to_owned(),
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
            overrides: TorrentOverrides::default(),
        };
        let id = torrent.info_hashes.join(";");
        self.backend
            .save_torrent(&torrent_to_record(&torrent, None))?;
        let mut command = add_torrent_command(&torrent, &self.settings);
        if let SessionCommand::AddTorrent { id: command_id, .. } = &mut command {
            *command_id = id;
        }
        self.backend.send(command)?;
        self.torrents.push(torrent);
        self.selected_torrent = self.torrents.len() - 1;
        Ok(format!("Added {name}"))
    }

    fn set_download_directory(&mut self, directory: String) -> Result<String, String> {
        if directory.trim().is_empty() {
            return Err("Download directory cannot be empty".into());
        }
        if Path::new(directory.trim()).is_file() {
            return Err("Download directory points to a file".into());
        }
        let directory = directory.trim().to_owned();
        let Some(torrent) = self.selected_mut() else {
            return Err("No torrent selected".into());
        };
        torrent.overrides.download_directory = Some(directory.clone());
        self.last_download_directory = Some(directory.clone());
        self.persist_selected();
        Ok(format!("Download directory set to {directory}"))
    }

    fn set_tracker_url(&mut self, tracker_url: String) -> Result<String, String> {
        self.show_tracker_identity = false;
        let tracker_url = tracker_url.trim().to_owned();
        if !tracker_url.is_empty()
            && !["http://", "https://", "udp://"]
                .iter()
                .any(|prefix| tracker_url.starts_with(prefix))
        {
            return Err("Tracker URL must use http, https, or udp".into());
        }
        let Some(torrent) = self.selected_mut() else {
            return Err("No torrent selected".into());
        };
        torrent.trackers = (!tracker_url.is_empty())
            .then_some(tracker_url)
            .into_iter()
            .collect();
        self.persist_selected();
        Ok("Tracker URL updated".into())
    }

    fn start_tracker_audit(&mut self) {
        if self.audit_result.is_some() {
            self.status = "Tracker audit already running".into();
            return;
        }
        let Some(torrent) = self.selected() else {
            return;
        };
        let id = torrent.info_hashes.join(";");
        let profile = self.tracker_audits.get(&id).cloned().unwrap_or_else(|| {
            default_audit(
                torrent.selected_size(),
                self.settings.connection.listen_port,
            )
        });
        let Some(tracker_url) = torrent.trackers.first().cloned() else {
            self.status = "No tracker URL configured".into();
            return;
        };
        let Some(hash) = torrent
            .info_hashes
            .iter()
            .find_map(|hash| hash.strip_prefix("v1: "))
        else {
            self.status = "Tracker audit requires a v1 or hybrid torrent".into();
            return;
        };
        let info_hash = match parse_info_hash_hex(hash) {
            Ok(hash) => hash,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if let Err(error) = metainfo::validate_peer_id(&torrent.peer_id) {
            self.status = error;
            return;
        }
        let peer_id: [u8; 20] = match torrent.peer_id.as_bytes().try_into() {
            Ok(peer_id) => peer_id,
            Err(_) => {
                self.status = "Peer ID must contain exactly 20 bytes".into();
                return;
            }
        };
        let steps = if profile.started_only {
            let full_size = torrent
                .files
                .files()
                .iter()
                .map(|file| file.size_bytes)
                .sum();
            vec![StoredTrackerAuditStep {
                delay_seconds: 0,
                uploaded: 0,
                downloaded: 0,
                left: full_size,
                event: Some("started".into()),
            }]
        } else {
            profile.steps.clone()
        };
        if steps.is_empty() {
            self.status = "Tracker scenario has no steps".into();
            return;
        }
        let allow_hosts = (!profile.allowed_host.trim().is_empty())
            .then(|| profile.allowed_host.trim().to_owned())
            .into_iter()
            .collect::<Vec<_>>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.audit_cancel = Some(cancel);
        let (sender, receiver) = mpsc::channel();
        self.audit_result = Some(receiver);
        self.status = format!(
            "Running {} tracker scenario step(s); x cancels",
            steps.len()
        );
        std::thread::spawn(move || {
            let mut messages = Vec::new();
            for (index, step) in steps.iter().enumerate() {
                if !crate::tracker_behavior::wait_for_delay(step.delay_seconds, &worker_cancel) {
                    let _ = sender.send("Tracker scenario cancelled".into());
                    return;
                }
                let event = match step.event.as_deref() {
                    Some("started") => Some(AnnounceEvent::Started),
                    Some("completed") => Some(AnnounceEvent::Completed),
                    Some("stopped") => Some(AnnounceEvent::Stopped),
                    _ => None,
                };
                let request = AnnounceRequest {
                    info_hash,
                    peer_id,
                    port: profile.port,
                    uploaded: step.uploaded,
                    downloaded: step.downloaded,
                    left: step.left,
                    event,
                };
                let message = announce_for_audit(&tracker_url, &request, &allow_hosts)
                    .and_then(|response| {
                        compact_ipv4_peers(&response.compact_peers).map(|peers| {
                            format!(
                                "step {}: up={} down={} left={} event={} -> interval={}s peers={}",
                                index + 1,
                                step.uploaded,
                                step.downloaded,
                                step.left,
                                step.event.as_deref().unwrap_or("none"),
                                response.interval_seconds,
                                peers
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        })
                    })
                    .unwrap_or_else(|error| format!("step {} failed: {error}", index + 1));
                messages.push(message);
            }
            if profile.started_only
                && !crate::tracker_behavior::wait_for_delay(
                    profile.observation_seconds,
                    &worker_cancel,
                )
            {
                messages.push("Observation cancelled".into());
            }
            let _ = sender.send(messages.join("\n"));
        });
    }

    fn apply_behavior_profile(&mut self, id: &str) -> Result<String, String> {
        if self.audit_result.is_some() {
            return Err("Stop the running scenario before applying another profile".into());
        }
        let catalog = self.behavior_catalog.as_ref().map_err(Clone::clone)?;
        let torrent = self.selected().ok_or("No torrent selected")?;
        let full_size = torrent
            .files
            .files()
            .iter()
            .map(|file| file.size_bytes)
            .sum();
        let profile = catalog.resolve(
            id,
            full_size,
            torrent.selected_size(),
            self.settings.connection.listen_port,
        )?;
        let torrent_id = torrent.info_hashes.join(";");
        let record = torrent_to_record(torrent, Some(profile.clone()));
        self.backend.save_torrent(&record)?;
        self.tracker_audits.insert(torrent_id, profile);
        Ok(format!(
            "Applied tracker profile '{id}'; A runs the saved scenario"
        ))
    }

    fn toggle_primary_preference(&mut self) {
        match self.preferences_tab {
            PreferencesTab::Downloads => {
                self.settings.download.start_when_added = !self.settings.download.start_when_added
            }
            PreferencesTab::Queue => self.settings.queue.enabled = !self.settings.queue.enabled,
            PreferencesTab::Speed => {
                self.settings.speed.alternate_enabled = !self.settings.speed.alternate_enabled
            }
            PreferencesTab::Connection => {
                self.settings.connection.port_forwarding = !self.settings.connection.port_forwarding
            }
            PreferencesTab::BitTorrent => {
                self.settings.bittorrent.dht = !self.settings.bittorrent.dht
            }
            PreferencesTab::Seeding => {
                self.settings.seeding.ratio_limit_enabled =
                    !self.settings.seeding.ratio_limit_enabled
            }
            PreferencesTab::Privacy => {
                self.settings.privacy.blocklist_enabled = !self.settings.privacy.blocklist_enabled
            }
            PreferencesTab::Storage => {
                self.settings.storage.check_free_space = !self.settings.storage.check_free_space
            }
            PreferencesTab::Automation => {
                self.settings.automation.watch_directory_enabled =
                    !self.settings.automation.watch_directory_enabled
            }
        }
        self.status = "Preference changed; press s to validate and save".into();
    }

    fn save_preferences(&mut self) {
        let issues = self.settings.validate();
        if issues.is_empty() {
            self.settings.ui.last_preferences_tab = self.preferences_tab.index();
            self.status = match self.settings.save() {
                Ok(()) => "Preferences saved; restart may be required".into(),
                Err(error) => format!("Could not save preferences: {error}"),
            };
        } else {
            self.status = issues
                .iter()
                .map(|issue| format!("{}: {}", issue.field, issue.message))
                .collect::<Vec<_>>()
                .join("; ");
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
                Constraint::Length(2),
            ])
            .split(frame.area());
        let selected_page = MainPage::ALL
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or_default();
        frame.render_widget(
            Tabs::new(MainPage::ALL.map(|page| Line::from(page.label())))
                .select(selected_page)
                .block(
                    Block::default()
                        .title(" PeerLume TUI ")
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            areas[0],
        );
        match self.page {
            MainPage::Torrents => self.render_torrents(frame, areas[1]),
            MainPage::Preferences => self.render_preferences(frame, areas[1]),
        }
        frame.render_widget(
            Paragraph::new(self.status.as_str())
                .block(Block::default().title(" Status ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            areas[2],
        );
        let help = if self.page == MainPage::Torrents {
            "1/2 pages · ↑↓ select · Tab details · a add · m magnet · p/r pause/resume · P/R all · d remove · q quit"
        } else {
            "1/2 pages · ←→ section · Space toggle highlighted option · s save · D defaults · q quit"
        };
        frame.render_widget(Paragraph::new(help).alignment(Alignment::Center), areas[3]);
        if let Some(prompt) = &self.input {
            let mode_prompt = prompt.kind == PromptKind::DownloadMode;
            let mut lines = vec![Line::from(prompt.value.as_str())];
            if mode_prompt {
                let modes = self
                    .behavior_catalog
                    .as_ref()
                    .map(|catalog| catalog.download_modes.as_slice())
                    .unwrap_or(&DownloadMode::ALL);
                lines.extend(
                    modes.iter().enumerate().map(|(index, mode)| {
                        Line::from(format!("{}: {}", index + 1, mode.label()))
                    }),
                );
            }
            if matches!(
                prompt.kind,
                PromptKind::DownloadMode | PromptKind::FreezeAfterBytes
            ) && let Some((_, metadata)) = &self.pending_add
            {
                let tracker = metadata
                    .trackers
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default();
                lines.push(Line::from(format!(
                    "Tracker: {}",
                    crate::identity::redact_tracker_url(tracker)
                )));
                lines.extend(tracker_identity_lines(tracker, self.show_tracker_identity));
            }
            let popup = centered_rect(80, (lines.len() + 3).clamp(5, 20) as u16, frame.area());
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().title(prompt.title).borders(Borders::ALL))
                    .wrap(Wrap { trim: false }),
                popup,
            );
            let cursor_x = popup.x + 1 + prompt.value.chars().count() as u16;
            frame.set_cursor_position((cursor_x.min(popup.right().saturating_sub(2)), popup.y + 1));
        }
    }

    fn render_torrents(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(43), Constraint::Percentage(57)])
            .split(area);
        let rows = self.torrents.iter().map(|torrent| {
            let percent = if torrent.selected_size() == 0 {
                0
            } else {
                torrent.downloaded_bytes.saturating_mul(100) / torrent.selected_size()
            };
            Row::new(vec![
                Cell::from(torrent.name.clone()),
                Cell::from(format!("{percent}%")),
                Cell::from(torrent.state.label()),
                Cell::from(format_size(torrent.download_rate)),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(48),
                Constraint::Length(6),
                Constraint::Length(12),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(["Name", "%", "State", "Down/s"]).style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().title(" Torrents ").borders(Borders::ALL))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
        let mut state = ratatui::widgets::TableState::default();
        if !self.torrents.is_empty() {
            state.select(Some(self.selected_torrent));
        }
        frame.render_stateful_widget(table, columns[0], &mut state);
        self.render_details(frame, columns[1]);
    }

    fn render_details(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(4)])
            .split(area);
        let selected = TorrentDetailsTab::ALL
            .iter()
            .position(|tab| *tab == self.details_tab)
            .unwrap_or_default();
        frame.render_widget(
            Tabs::new(TorrentDetailsTab::ALL.map(|tab| Line::from(tab.label())))
                .select(selected)
                .block(Block::default().borders(Borders::ALL))
                .highlight_style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let Some(torrent) = self.selected() else {
            frame.render_widget(
                Paragraph::new("No torrents. Press a to add a .torrent or m for a magnet.")
                    .block(Block::default().borders(Borders::ALL)),
                areas[1],
            );
            return;
        };
        match self.details_tab {
            TorrentDetailsTab::Files => {
                let items = torrent.files.files().iter().map(|file| {
                    ListItem::new(format!(
                        "[{}] {}  {} / {}",
                        if file.selected { 'x' } else { ' ' },
                        file.path,
                        format_size(file.downloaded_bytes),
                        format_size(file.size_bytes)
                    ))
                });
                let mut state = ListState::default();
                if !torrent.files.files().is_empty() {
                    state.select(Some(self.selected_file));
                }
                frame.render_stateful_widget(
                    List::new(items)
                        .block(
                            Block::default()
                                .title(" Files · Space toggles ")
                                .borders(Borders::ALL),
                        )
                        .highlight_style(Style::default().bg(Color::DarkGray))
                        .highlight_symbol("▶ "),
                    areas[1],
                    &mut state,
                );
            }
            TorrentDetailsTab::Info => {
                let mut lines = vec![
                    Line::from(format!("Name: {}", torrent.name)),
                    Line::from(format!("Private: {}", torrent.is_private)),
                    Line::from(format!("Source: {}", torrent.source)),
                    Line::from(format!("Info hashes: {}", torrent.info_hashes.join(", "))),
                    Line::from(format!("Audit Peer ID: {}", torrent.peer_id)),
                    Line::from(format!(
                        "Download mode: {}",
                        torrent.overrides.download_mode.label()
                    )),
                    Line::from(format!(
                        "Freeze threshold: {} bytes",
                        if torrent.overrides.freeze_after_bytes == 0 {
                            crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES
                        } else {
                            torrent.overrides.freeze_after_bytes
                        }
                    )),
                    Line::from(format!(
                        "Selected: {}",
                        format_size(torrent.selected_size())
                    )),
                    Line::from(format!(
                        "Downloaded: {}",
                        format_size(torrent.downloaded_bytes)
                    )),
                    Line::from(format!("Uploaded: {}", format_size(torrent.uploaded_bytes))),
                ];
                let tracker = torrent
                    .trackers
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default();
                lines.splice(
                    5..5,
                    tracker_identity_lines(tracker, self.show_tracker_identity),
                );
                frame.render_widget(
                    Paragraph::new(lines)
                        .block(Block::default().title(" Info ").borders(Borders::ALL))
                        .wrap(Wrap { trim: false }),
                    areas[1],
                );
            }
            TorrentDetailsTab::Settings => {
                let effective = EffectiveTorrentSettings::resolve(
                    &self.settings,
                    &torrent.overrides,
                    torrent.is_private,
                );
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(format!(
                            "Download directory: {}",
                            effective.download_directory
                        )),
                        Line::from(format!(
                            "Download limit: {} KiB/s",
                            effective.download_limit_kib
                        )),
                        Line::from(format!(
                            "Upload limit: {} KiB/s",
                            effective.upload_limit_kib
                        )),
                        Line::from(format!("Max peers: {}", effective.max_peers)),
                        Line::from(format!("Sequential: {}", effective.sequential_download)),
                        Line::from("Press o to override the download directory."),
                    ])
                    .block(
                        Block::default()
                            .title(" Torrent settings ")
                            .borders(Borders::ALL),
                    ),
                    areas[1],
                );
            }
            TorrentDetailsTab::Peers => frame.render_widget(
                Paragraph::new(format!(
                    "Connected peers: {}\nKnown seeds: {}",
                    torrent.peers, torrent.seeds
                ))
                .block(Block::default().title(" Peers ").borders(Borders::ALL)),
                areas[1],
            ),
            TorrentDetailsTab::Trackers => {
                let id = torrent.info_hashes.join(";");
                let profile = self.tracker_audits.get(&id);
                let mut lines = vec![
                    Line::from(format!(
                        "Tracker URL: {}",
                        torrent
                            .trackers
                            .first()
                            .map(|url| {
                                if self.settings.privacy.redact_tracker_credentials
                                    && !self.show_tracker_identity
                                {
                                    crate::identity::redact_tracker_url(url)
                                } else {
                                    url.clone()
                                }
                            })
                            .unwrap_or_else(|| "none".into())
                    )),
                    Line::from(
                        "u edits URL · b loads behavior profile · A runs scenario · x cancels",
                    ),
                ];
                lines.splice(
                    1..1,
                    tracker_identity_lines(
                        torrent
                            .trackers
                            .first()
                            .map(String::as_str)
                            .unwrap_or_default(),
                        self.show_tracker_identity,
                    ),
                );
                if let Some(profile) = profile {
                    lines.extend([
                        Line::from(format!("Allowed staging host: {}", profile.allowed_host)),
                        Line::from(format!("Announce port: {}", profile.port)),
                        Line::from(format!("Started-only: {}", profile.started_only)),
                        Line::from(format!("Scenario steps: {}", profile.steps.len())),
                    ]);
                    for (index, step) in profile.steps.iter().enumerate() {
                        lines.push(Line::from(format!(
                            "{}. +{}s up={} down={} left={} event={}",
                            index + 1,
                            step.delay_seconds,
                            step.uploaded,
                            step.downloaded,
                            step.left,
                            step.event.as_deref().unwrap_or("none")
                        )));
                    }
                }
                frame.render_widget(
                    Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title(" Tracker audit ")
                                .borders(Borders::ALL),
                        )
                        .wrap(Wrap { trim: false }),
                    areas[1],
                );
            }
            TorrentDetailsTab::Pieces => {
                let total = torrent.selected_size();
                let ratio = if total == 0 {
                    0.0
                } else {
                    torrent.downloaded_bytes.min(total) as f64 / total as f64
                };
                frame.render_widget(
                    Gauge::default()
                        .block(
                            Block::default()
                                .title(" Verified payload ")
                                .borders(Borders::ALL),
                        )
                        .ratio(ratio)
                        .label(format!(
                            "{} / {}",
                            format_size(torrent.downloaded_bytes),
                            format_size(total)
                        )),
                    areas[1],
                );
            }
            TorrentDetailsTab::Log => {
                let mut lines = Vec::new();
                if let Some(journal) = self.backend.journal() {
                    if let Some(path) = journal.path() {
                        lines.push(Line::from(format!("JSONL: {}", path.display())));
                    }
                    if let Some(error) = journal.error() {
                        lines.push(Line::from(error));
                    }
                    let hash = crate::announce_journal::torrent_hash(&torrent.info_hashes);
                    let entries = hash
                        .map(|hash| journal.entries(Some(hash)))
                        .unwrap_or_default();
                    for entry in entries.iter().rev() {
                        lines.push(Line::from(entry.summary()));
                        lines.push(Line::from(format!("info_hash={} transport={} full_size={} selected_bytes={:?} request_transmitted={:?}", entry.announce.info_hash, entry.announce.transport, entry.announce.full_size, entry.announce.selected_bytes, entry.announce.request_transmitted)));
                        lines.push(Line::from(format!("mode={} peer_id={} port={} response: HTTP={:?}, interval={:?}, min={:?}, seeders={:?}, leechers={:?}, warning={:?}, failure={:?}", entry.announce.mode, entry.announce.peer_id_hex, entry.announce.port, entry.announce.reply.http_status, entry.announce.reply.interval, entry.announce.reply.min_interval, entry.announce.reply.seeders, entry.announce.reply.leechers, entry.announce.reply.warning, entry.announce.reply.failure)));
                        for peer in &entry.announce.reply.peers {
                            lines.push(Line::from(format!("  peer {peer}")));
                        }
                    }
                }
                if lines.is_empty() {
                    lines.push(Line::from(
                        "No real announces yet. Standalone audit exchanges are separate.",
                    ));
                }
                frame.render_widget(
                    Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title(" Real announces · e/E export torrent/session · v recheck · ↑↓ scroll ")
                                .borders(Borders::ALL),
                        )
                        .wrap(Wrap { trim: false })
                        .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0)),
                    areas[1],
                );
            }
        }
    }

    fn render_preferences(&self, frame: &mut Frame<'_>, area: Rect) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        frame.render_widget(
            Tabs::new(PreferencesTab::ALL.map(|tab| Line::from(tab.label())))
                .select(self.preferences_tab.index())
                .block(Block::default().borders(Borders::ALL))
                .highlight_style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let body = match self.preferences_tab {
            PreferencesTab::Downloads => format!("{:#?}", self.settings.download),
            PreferencesTab::Queue => format!("{:#?}", self.settings.queue),
            PreferencesTab::Speed => format!("{:#?}", self.settings.speed),
            PreferencesTab::Connection => format!("{:#?}", self.settings.connection),
            PreferencesTab::BitTorrent => format!("{:#?}", self.settings.bittorrent),
            PreferencesTab::Seeding => format!("{:#?}", self.settings.seeding),
            PreferencesTab::Privacy => format!("{:#?}", self.settings.privacy),
            PreferencesTab::Storage => format!("{:#?}", self.settings.storage),
            PreferencesTab::Automation => format!("{:#?}", self.settings.automation),
        };
        frame.render_widget(
            Paragraph::new(body)
                .block(
                    Block::default()
                        .title(" Settings · Space toggles primary option · s validates/saves ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false }),
            areas[1],
        );
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        if let Some(cancel) = &self.audit_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub fn run() -> io::Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let mut app = TuiApp::load();
    while app.running {
        app.poll();
        terminal.terminal.draw(|frame| app.render(frame))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            app.handle_key(key);
        }
    }
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn default_audit(total_size: u64, port: u16) -> StoredTrackerAuditProfile {
    StoredTrackerAuditProfile {
        allowed_host: String::new(),
        port,
        started_only: false,
        observation_seconds: 14_400,
        steps: vec![StoredTrackerAuditStep {
            delay_seconds: 0,
            uploaded: 0,
            downloaded: 0,
            left: total_size,
            event: Some("started".into()),
        }],
    }
}

fn move_index(current: usize, len: usize, direction: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + direction).clamp(0, len.saturating_sub(1) as isize) as usize
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(height.min(area.height)),
            Constraint::Percentage(50),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn tracker_identity_lines(tracker: &str, reveal: bool) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(
        "Tracker account / user identity (F2 show/hide; not Peer ID):",
    )];
    lines.extend(
        crate::identity::tracker_identity_summary(tracker, reveal)
            .into_iter()
            .map(Line::from),
    );
    lines
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn test_torrent() -> TorrentItem {
        TorrentItem {
            name: "fixture".into(),
            source: "fixture.torrent".into(),
            info_hashes: vec!["v1: 0123456789abcdef0123456789abcdef01234567".into()],
            trackers: vec!["http://127.0.0.1:8080/announce".into()],
            peer_id: "-PL0001-123456789012".into(),
            files: FileSelectionModel::from_files([("video.mkv".into(), 1024)]),
            downloaded_bytes: 256,
            uploaded_bytes: 10,
            download_rate: 50,
            upload_rate: 5,
            peers: 3,
            seeds: 1,
            state: TorrentState::Downloading,
            is_private: true,
            overrides: TorrentOverrides::default(),
        }
    }

    fn app() -> TuiApp {
        TuiApp::from_parts(
            ClientSettings::default(),
            ClientBackend::offline(),
            vec![test_torrent()],
            HashMap::new(),
        )
    }

    #[test]
    fn tracker_account_is_visible_in_info_and_trackers_but_the_key_requires_f2() {
        let mut app = app();
        app.settings.privacy.redact_tracker_credentials = true;
        app.torrents[0].trackers = vec!["https://tracker.test/ann?uk=fixture-key".into()];
        let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
        for tab in [TorrentDetailsTab::Info, TorrentDetailsTab::Trackers] {
            app.details_tab = tab;
            app.show_tracker_identity = false;
            terminal.draw(|frame| app.render(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Account/authentication key (uk)"));
            assert!(text.contains("[hidden]"));
            assert!(!text.contains("fixture-key"));
            app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
            terminal.draw(|frame| app.render(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("fixture-key"));
        }
        app.move_selection(1);
        assert!(!app.show_tracker_identity);
    }

    #[test]
    fn pending_add_shows_account_key_before_download_mode_confirmation() {
        let mut app = app();
        app.pending_add = Some((
            PathBuf::from("pending.torrent"),
            TorrentMetainfo {
                name: "pending".into(),
                files: vec![],
                trackers: vec!["https://tracker.test/ann?uk=pending-key".into()],
                is_private: false,
                info_hash_v1_hex: Some("01".repeat(20)),
                info_hash_v2_hex: None,
            },
        ));
        app.open_prompt(PromptKind::DownloadMode, "Download mode", "1".into());
        let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Account/authentication key (uk)"));
        assert!(!text.contains("pending-key"));
        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("pending-key"));
        assert_eq!(app.input.as_ref().unwrap().value, "1");
    }

    #[test]
    fn tui_declares_every_shared_component() {
        assert_eq!(SUPPORTED_COMPONENT_COUNT, COMPONENTS.len());
    }

    #[test]
    fn external_profile_application_uses_shared_resolver_and_keeps_url() {
        let mut app = app();
        app.behavior_catalog =
            BehaviorCatalog::from_toml(include_str!("../config/tracker-behaviors.example.toml"));
        let original_trackers = app.torrents[0].trackers.clone();
        app.apply_behavior_profile("periodic-test").unwrap();
        let id = app.torrents[0].info_hashes.join(";");
        assert_eq!(app.tracker_audits[&id].steps.len(), 4);
        assert_eq!(app.tracker_audits[&id].steps[0].left, 1024);
        assert_eq!(app.torrents[0].trackers, original_trackers);
        let previous = app.tracker_audits[&id].clone();
        assert!(app.apply_behavior_profile("typo").is_err());
        assert_eq!(app.tracker_audits[&id], previous);
    }

    #[test]
    fn external_combination_sends_all_steps_in_order_to_local_tracker() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            time::Instant,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut requests = Vec::new();
            while requests.len() < 4 && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(line);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                }
                let body = b"d8:intervali60e5:peers0:e";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
            requests
        });

        let mut app = app();
        app.torrents[0].trackers = vec![format!("http://{address}/announce")];
        app.behavior_catalog = BehaviorCatalog::from_toml(
            &include_str!("../config/tracker-behaviors.example.toml")
                .replace("delay_seconds = 1800", "delay_seconds = 0"),
        );
        app.apply_behavior_profile("periodic-test").unwrap();
        app.start_tracker_audit();
        let result = app
            .audit_result
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(8))
            .unwrap();
        assert!(!result.contains("failed"), "{result}");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].contains("event=started"));
        assert!(!requests[1].contains("event="));
        assert!(!requests[2].contains("event="));
        assert!(requests[3].contains("event=stopped"));
        assert!(requests.iter().all(|request| request.contains("left=1024")));
    }

    #[test]
    fn scenario_cancellation_interrupts_delay_without_sending_request() {
        let mut app = app();
        let id = app.torrents[0].info_hashes.join(";");
        let mut profile = default_audit(1024, 1234);
        profile.steps[0].delay_seconds = 3600;
        app.tracker_audits.insert(id, profile);
        app.start_tracker_audit();
        app.audit_cancel
            .as_ref()
            .unwrap()
            .store(true, Ordering::Relaxed);
        let result = app
            .audit_result
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(result, "Tracker scenario cancelled");
    }

    #[test]
    fn selection_is_bounded() {
        assert_eq!(move_index(0, 0, 1), 0);
        assert_eq!(move_index(0, 2, -1), 0);
        assert_eq!(move_index(0, 2, 1), 1);
        assert_eq!(move_index(1, 2, 1), 1);
    }

    #[test]
    fn prompt_editing_and_cancel_are_deterministic() {
        let mut app = app();
        app.open_prompt(PromptKind::Magnet, "Magnet", "abc".into());
        app.handle_prompt_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value, "abcd");
        app.handle_prompt_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value, "abc");
        app.handle_prompt_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.input.is_none());
    }

    #[test]
    fn download_mode_prompt_replaces_default_and_shows_all_choices() {
        let mut app = app();
        app.open_prompt(PromptKind::DownloadMode, "Download mode", "1".into());
        app.handle_prompt_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value, "2");
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        app.handle_prompt_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.pending_add.is_none());
        assert!(app.input.is_none());
    }

    #[test]
    fn freeze_threshold_is_requested_before_addition_and_invalid_values_preserve_pending_torrent() {
        let mut app = app();
        app.behavior_catalog =
            BehaviorCatalog::from_toml(include_str!("../config/tracker-behaviors.example.toml"));
        app.pending_add = Some((
            PathBuf::from("test.torrent"),
            TorrentMetainfo {
                name: "pending".into(),
                files: vec![metainfo::MetainfoFile {
                    path: "pending".into(),
                    size_bytes: 100,
                }],
                trackers: vec![],
                is_private: false,
                info_hash_v1_hex: Some("01".repeat(20)),
                info_hash_v2_hex: None,
            },
        ));
        let count = app.torrents.len();
        app.confirm_torrent_mode("5").unwrap();
        assert_eq!(
            app.input.as_ref().unwrap().kind,
            PromptKind::FreezeAfterBytes
        );
        assert_eq!(app.torrents.len(), count);
        assert!(app.confirm_freeze_threshold("0").is_err());
        assert!(app.confirm_freeze_threshold("NaN").is_err());
        assert!(app.pending_add.is_some());
    }

    #[test]
    fn log_export_keys_and_scrolling_are_separate_from_file_selection() {
        let mut app = app();
        app.details_tab = TorrentDetailsTab::Log;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_file, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().kind, PromptKind::ExportJournal);
        app.input = None;
        app.handle_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT));
        assert_eq!(
            app.input.as_ref().unwrap().kind,
            PromptKind::ExportSessionJournal
        );
        assert!(
            app.export_journal(Path::new("unused.jsonl"), false)
                .is_err()
        );
    }

    #[test]
    fn file_toggle_updates_shared_torrent_model() {
        let mut app = app();
        assert!(app.torrents[0].files.files()[0].selected);
        app.toggle_selected_file();
        assert!(!app.torrents[0].files.files()[0].selected);
    }

    #[test]
    fn all_pages_and_detail_tabs_render() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = app();
        for tab in TorrentDetailsTab::ALL {
            app.page = MainPage::Torrents;
            app.details_tab = tab;
            terminal.draw(|frame| app.render(frame)).unwrap();
        }
        for tab in PreferencesTab::ALL {
            app.page = MainPage::Preferences;
            app.preferences_tab = tab;
            terminal.draw(|frame| app.render(frame)).unwrap();
        }
    }

    #[test]
    fn empty_torrent_page_renders() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = TuiApp::from_parts(
            ClientSettings::default(),
            ClientBackend::offline(),
            Vec::new(),
            HashMap::new(),
        );
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn keyboard_navigation_uses_shared_schema_order() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.details_tab, TorrentDetailsTab::Info);
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.details_tab, TorrentDetailsTab::Files);
        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(app.page, MainPage::Preferences);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.preferences_tab, PreferencesTab::Queue);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.preferences_tab, PreferencesTab::Downloads);
    }

    #[test]
    fn pause_resume_and_remove_actions_update_model_offline() {
        let mut app = app();
        app.pause_selected();
        assert_eq!(app.torrents[0].state, TorrentState::Paused);
        app.resume_selected();
        assert_eq!(app.torrents[0].state, TorrentState::WaitingForEngine);
        app.pause_all();
        assert_eq!(app.torrents[0].state, TorrentState::Paused);
        app.resume_all();
        assert_eq!(app.torrents[0].state, TorrentState::WaitingForEngine);
        app.remove_selected();
        assert!(app.torrents.is_empty());
    }

    #[test]
    fn torrent_paths_and_tracker_urls_are_validated_and_persisted_in_model() {
        let mut app = app();
        assert!(app.set_download_directory(String::new()).is_err());
        assert!(app.set_download_directory("downloads/other".into()).is_ok());
        assert_eq!(
            app.torrents[0].overrides.download_directory.as_deref(),
            Some("downloads/other")
        );
        assert!(app.set_tracker_url("ftp://invalid".into()).is_err());
        assert!(
            app.set_tracker_url("http://127.0.0.1:9000/announce".into())
                .is_ok()
        );
        assert_eq!(app.torrents[0].trackers, ["http://127.0.0.1:9000/announce"]);
    }

    #[test]
    fn every_preference_section_has_a_tui_mutation() {
        let mut app = app();
        let original = app.settings.clone();
        for tab in PreferencesTab::ALL {
            app.preferences_tab = tab;
            app.toggle_primary_preference();
        }
        assert_ne!(app.settings.download, original.download);
        assert_ne!(app.settings.queue, original.queue);
        assert_ne!(app.settings.speed, original.speed);
        assert_ne!(app.settings.connection, original.connection);
        assert_ne!(app.settings.bittorrent, original.bittorrent);
        assert_ne!(app.settings.seeding, original.seeding);
        assert_ne!(app.settings.privacy, original.privacy);
        assert_ne!(app.settings.storage, original.storage);
        assert_ne!(app.settings.automation, original.automation);
    }

    #[test]
    fn audit_completion_is_polled_into_status_and_log() {
        let mut app = app();
        let (sender, receiver) = mpsc::channel();
        app.audit_result = Some(receiver);
        sender.send("audit complete".into()).unwrap();
        app.poll();
        assert_eq!(app.status, "audit complete");
        assert_eq!(app.log.back().map(String::as_str), Some("audit complete"));
        assert!(app.audit_result.is_none());
    }

    #[test]
    fn invalid_prompt_submission_reports_error_without_panicking() {
        let mut app = app();
        app.open_prompt(PromptKind::Magnet, "Magnet", "not-a-magnet".into());
        app.handle_prompt_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.input.is_none());
        assert!(!app.status.is_empty());
    }

    #[test]
    fn formatting_helpers_cover_boundaries() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        let area = centered_rect(80, 5, Rect::new(0, 0, 100, 20));
        assert_eq!(area.width, 80);
        assert_eq!(area.height, 5);
    }

    #[test]
    fn default_audit_uses_real_size_and_safe_started_counters() {
        let audit = default_audit(4096, 51413);
        assert_eq!(audit.steps.len(), 1);
        assert_eq!(audit.steps[0].uploaded, 0);
        assert_eq!(audit.steps[0].downloaded, 0);
        assert_eq!(audit.steps[0].left, 4096);
        assert_eq!(audit.steps[0].event.as_deref(), Some("started"));
    }
}
