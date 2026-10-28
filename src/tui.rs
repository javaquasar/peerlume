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
        ClientBackend, SessionNotice, add_torrent_command, torrent_from_record, torrent_to_record,
    },
    control::{COMMAND_ACCEPTED, Controller, TorrentCommand},
    download_mode::DownloadMode,
    file_selection::FileSelectionModel,
    magnet::MagnetLink,
    metainfo::{self, TorrentMetainfo},
    settings::ClientSettings,
    state::{StoredTrackerAuditProfile, StoredTrackerAuditStep},
    torrent::{EffectiveTorrentSettings, TorrentItem, TorrentOverrides, TorrentState},
    torrent_filter::StatusFilter,
    tracker::{AnnounceEvent, AnnounceRequest, parse_info_hash_hex},
    tracker_behavior::BehaviorCatalog,
    ui_schema::{COMPONENTS, MainPage, PreferencesTab, TorrentDetailsTab},
};

pub const SUPPORTED_COMPONENT_COUNT: usize = COMPONENTS.len();
mod interaction;
#[cfg(test)]
mod parity_tests;
mod preferences_editor;
mod prompt;
use interaction::{Focus, PendingBatch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    Transfer,
    Preference,
    Search,
    Command,
    ExistingMode,
    RepairLimit,
    TorrentPath,
    DownloadMode,
    FreezeAfterBytes,
    UploadRatioRange,
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
    cursor: usize,
}

pub struct TuiApp {
    focus: Focus,
    split_percent: u16,
    search: String,
    marked: std::collections::BTreeSet<String>,
    help: bool,
    confirmation: Option<PendingBatch>,
    preferences_draft: Option<ClientSettings>,
    selected_preference: usize,
    editing_preference: Option<(PreferencesTab, String)>,
    journal_page: usize,
    settings: ClientSettings,
    control: Controller,
    tracker_audits: HashMap<String, StoredTrackerAuditProfile>,
    page: MainPage,
    details_tab: TorrentDetailsTab,
    preferences_tab: PreferencesTab,
    selected_torrent: usize,
    status_filter: StatusFilter,
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
    pending_verify_after_download: bool,
    pending_repair: crate::runtime_status::RepairPolicy,
    agent_panel: crate::control::frontends::AccessPanel,
}

impl TuiApp {
    pub fn load() -> Self {
        let loaded = ClientSettings::load_checked();
        let settings = loaded.as_ref().cloned().unwrap_or_default();
        let backend = match loaded {
            Ok(_) => ClientBackend::start(&settings),
            Err(error) => {
                ClientBackend::blocked(format!("Cannot read settings; engine not started: {error}"))
            }
        };
        let (records, restore_status) = backend.restore_for_frontend();
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
        let mut app = Self::from_parts(settings, backend, torrents, tracker_audits);
        if let Some(status) = restore_status {
            app.status = status;
        }
        app
    }

    fn from_parts(
        settings: ClientSettings,
        backend: ClientBackend,
        torrents: Vec<TorrentItem>,
        tracker_audits: HashMap<String, StoredTrackerAuditProfile>,
    ) -> Self {
        let preferences_tab = PreferencesTab::from_index(settings.ui.last_preferences_tab);
        Self {
            focus: Focus::Torrents,
            split_percent: if settings.ui.torrent_pane_percent == 0 {
                43
            } else {
                settings.ui.torrent_pane_percent.clamp(20, 80)
            },
            search: settings.ui.search.clone(),
            marked: Default::default(),
            help: false,
            confirmation: None,
            preferences_draft: None,
            selected_preference: 0,
            editing_preference: None,
            settings,
            control: Controller::new(backend, torrents),
            tracker_audits,
            page: MainPage::Torrents,
            details_tab: TorrentDetailsTab::Files,
            preferences_tab,
            selected_torrent: 0,
            status_filter: StatusFilter::default(),
            journal_page: 0,
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
            pending_verify_after_download: false,
            pending_repair: Default::default(),
            agent_panel: Default::default(),
        }
    }

    fn poll(&mut self) {
        while let Some(notice) = self.control.poll_next() {
            match notice {
                SessionNotice::SettingsApplied { id } | SessionNotice::StateApplied { id } => {
                    if !self.control.has_confirmed_update(&id) {
                        continue;
                    }
                    if let Err(error) = self
                        .control
                        .persist_confirmed_update(&id, self.tracker_audits.get(&id).cloned())
                    {
                        self.status = error;
                    }
                }
                SessionNotice::Added { id } => self.push_log(format!("started {id}")),
                SessionNotice::Error { id, message } => {
                    self.status = format!("Transfer error: {message}");
                    self.push_log(format!("error {id}: {message}"));
                }
                SessionNotice::Removed { id } => {
                    self.tracker_audits.remove(&id);
                    self.push_log(format!("removed {id}"));
                }
                SessionNotice::Stopped => self.push_log("transfer engine stopped"),
                SessionNotice::None => {}
            }
        }
        self.control.publish();
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
        let visible = self.visible_indices();
        if !visible.contains(&self.selected_torrent) {
            self.selected_torrent = visible.first().copied().unwrap_or(0);
            self.selected_file = 0;
            self.show_tracker_identity = false;
        }
        self.marked.retain(|id| {
            self.control
                .torrents
                .iter()
                .any(|torrent| torrent.info_hashes.join(";") == *id)
        });
        let file_count = self
            .control
            .torrents
            .get(self.selected_torrent)
            .map_or(0, |torrent| torrent.files.files().len());
        if self.details_tab == TorrentDetailsTab::Files {
            self.selected_file = self.selected_file.min(file_count.saturating_sub(1));
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Press
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.handle_interaction(key);
            return;
        }
        if key.kind == KeyEventKind::Press && self.agent_panel.key(&mut self.control, key.code) {
            return;
        }
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::F(4)
            && self.pending_add.is_some()
        {
            self.pending_repair.enabled = !self.pending_repair.enabled;
            if self.pending_repair.enabled {
                self.pending_verify_after_download = true;
            }
            return;
        }
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.handle_interaction(key) {
            return;
        }
        if key.code == KeyCode::F(3) && self.pending_add.is_some() {
            self.pending_verify_after_download =
                self.pending_repair.enabled || !self.pending_verify_after_download;
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
                    PromptKind::DownloadMode
                        | PromptKind::FreezeAfterBytes
                        | PromptKind::UploadRatioRange
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
            KeyCode::Char('f') if self.page == MainPage::Torrents => {
                self.status_filter = self.status_filter.next();
                self.clamp_selection();
                self.retain_visible_marks();
                self.selected_file = 0;
                self.show_tracker_identity = false;
            }
            KeyCode::Char('m') if self.page == MainPage::Torrents => {
                self.open_prompt(PromptKind::Magnet, "Add magnet URI", String::new());
            }
            KeyCode::Char('M') if self.page == MainPage::Torrents => {
                self.open_prompt(PromptKind::ExistingMode, "Mode: 1 normal, 2 no progress, 3 upload [min max], 4 hide downloaded, 5 freeze [bytes]", "1".into());
            }
            KeyCode::Char('H') if self.page == MainPage::Torrents => {
                if let Some(torrent) = self.control.torrents.get(self.selected_torrent) {
                    let mut policy = torrent.overrides.repair_policy;
                    policy.enabled = !policy.enabled;
                    self.status = self
                        .submit_selected(TorrentCommand::SetRepairPolicy(policy))
                        .unwrap_or_else(|error| error);
                }
            }
            KeyCode::Char('L') if self.page == MainPage::Torrents => {
                self.open_prompt(
                    PromptKind::RepairLimit,
                    "Maximum automatic repair attempts (1-10)",
                    "3".into(),
                );
            }
            KeyCode::Char('p') if self.page == MainPage::Torrents => self.pause_selected(),
            KeyCode::Char('r') if self.page == MainPage::Torrents => self.resume_selected(),
            KeyCode::Char('P') if self.page == MainPage::Torrents => self.pause_all(),
            KeyCode::Char('R') if self.page == MainPage::Torrents => self.resume_all(),
            KeyCode::Char('d') if self.page == MainPage::Torrents => {
                self.request_batch(interaction::BatchAction::Remove)
            }
            KeyCode::Char('v') if self.page == MainPage::Torrents => {
                self.status = self
                    .submit_selected(TorrentCommand::Recheck)
                    .unwrap_or_else(|error| error);
            }
            KeyCode::Char('V') if self.page == MainPage::Torrents => {
                if let Some(torrent) = self.selected() {
                    let enabled = torrent.overrides.repair_policy.enabled
                        || !torrent.overrides.verify_after_download;
                    self.status = self
                        .submit_selected(TorrentCommand::SetVerifyAfterDownload(enabled))
                        .unwrap_or_else(|error| error);
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
                    && self.focus == Focus::Details
                    && self.details_tab == TorrentDetailsTab::Files =>
            {
                self.toggle_selected_file()
            }
            KeyCode::Char(' ') if self.page == MainPage::Preferences => {
                self.toggle_preference();
            }
            KeyCode::Char('s') if self.page == MainPage::Preferences => self.save_preferences(),
            KeyCode::Char('D') if self.page == MainPage::Preferences => {
                self.preferences_draft = Some(ClientSettings::default());
                self.status = "Defaults loaded; press s to save".into();
            }
            KeyCode::Tab if self.page == MainPage::Torrents => {
                self.focus = self.focus.next();
            }
            KeyCode::BackTab if self.page == MainPage::Torrents => {
                self.focus = self.focus.next();
            }
            KeyCode::Right if self.page == MainPage::Preferences => {
                self.preferences_tab = self.preferences_tab.next();
                self.selected_preference = 0;
            }
            KeyCode::Left if self.page == MainPage::Preferences => {
                self.preferences_tab = self.preferences_tab.previous();
                self.selected_preference = 0;
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageDown if self.details_tab == TorrentDetailsTab::Log => {
                self.journal_page = self.journal_page.saturating_add(1);
                self.selected_file = 0;
            }
            KeyCode::PageUp if self.details_tab == TorrentDetailsTab::Log => {
                self.journal_page = self.journal_page.saturating_sub(1);
                self.selected_file = 0;
            }
            KeyCode::Up => self.move_selection(-1),
            _ => {}
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, title: &'static str, value: String) {
        self.show_tracker_identity = false;
        let cursor = value.len();
        self.input = Some(InputPrompt {
            kind,
            title,
            value,
            cursor,
        });
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
            _ => {
                self.input.as_mut().expect("prompt exists").edit(key);
            }
        }
    }

    fn submit_prompt(&mut self, prompt: InputPrompt) {
        let retry = prompt.clone();
        let result = match prompt.kind {
            PromptKind::Transfer => self.change_transfer(&prompt.value),
            PromptKind::Preference => self.commit_preference(&prompt.value),
            PromptKind::Search => {
                self.search = prompt.value;
                self.clamp_selection();
                self.retain_visible_marks();
                Ok(format!("Search: {}", self.search))
            }
            PromptKind::Command => self.run_command(&prompt.value),
            PromptKind::ExistingMode => self.change_existing_mode(&prompt.value),
            PromptKind::RepairLimit => (|| {
                let torrent = self
                    .control
                    .torrents
                    .get(self.selected_torrent)
                    .ok_or("No selected torrent")?;
                let mut policy = torrent.overrides.repair_policy;
                policy.max_attempts = prompt
                    .value
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a number from 1 to 10")?;
                policy.validate()?;
                self.submit_selected(TorrentCommand::SetRepairPolicy(policy))
            })(),
            PromptKind::TorrentPath => self.prepare_torrent_add(PathBuf::from(prompt.value)),
            PromptKind::DownloadMode => self.confirm_torrent_mode(prompt.value.trim()),
            PromptKind::FreezeAfterBytes => self.confirm_freeze_threshold(prompt.value.trim()),
            PromptKind::UploadRatioRange => self.confirm_upload_ratio(prompt.value.trim()),
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
                        PromptKind::DownloadMode
                            | PromptKind::FreezeAfterBytes
                            | PromptKind::UploadRatioRange
                    )
                {
                    self.input = Some(retry);
                }
            }
        }
    }

    fn move_selection(&mut self, direction: isize) {
        if self.page == MainPage::Preferences {
            let count = preferences_editor::fields(
                self.preferences_draft.as_ref().unwrap_or(&self.settings),
                self.preferences_tab,
            )
            .len();
            self.selected_preference = move_index(self.selected_preference, count, direction);
            return;
        }
        if self.page == MainPage::Torrents
            && self.focus == Focus::Details
            && matches!(
                self.details_tab,
                TorrentDetailsTab::Log | TorrentDetailsTab::Status
            )
        {
            self.selected_file = self.selected_file.saturating_add_signed(direction).min(
                if self.details_tab == TorrentDetailsTab::Log {
                    usize::MAX
                } else {
                    u16::MAX as usize
                },
            );
            return;
        }
        if self.page == MainPage::Torrents
            && self.focus == Focus::Details
            && self.details_tab == TorrentDetailsTab::Files
            && let Some(torrent) = self.selected()
        {
            self.selected_file =
                move_index(self.selected_file, torrent.files.files().len(), direction);
            return;
        }
        if self.page == MainPage::Torrents && self.focus == Focus::Details {
            self.selected_file = self
                .selected_file
                .saturating_add_signed(direction)
                .min(u16::MAX as usize);
            return;
        }
        let visible = self.visible_indices();
        let position = visible
            .iter()
            .position(|index| *index == self.selected_torrent)
            .unwrap_or(0);
        self.selected_torrent = visible
            .get(move_index(position, visible.len(), direction))
            .copied()
            .unwrap_or(0);
        self.selected_file = 0;
        self.show_tracker_identity = false;
    }

    fn selected(&self) -> Option<&TorrentItem> {
        let visible = self.visible_indices().contains(&self.selected_torrent);
        self.control
            .torrents
            .get(self.selected_torrent)
            .filter(|_| visible)
    }

    fn selected_mut(&mut self) -> Option<&mut TorrentItem> {
        let visible = self.visible_indices().contains(&self.selected_torrent);
        self.control
            .torrents
            .get_mut(self.selected_torrent)
            .filter(|_| visible)
    }

    fn submit_selected(&mut self, command: TorrentCommand) -> Result<String, String> {
        let id = self
            .selected()
            .ok_or("No selected torrent")?
            .info_hashes
            .join(";");
        self.control
            .submit(&id, command, None)
            .map_err(|error| error.to_string())?;
        Ok(COMMAND_ACCEPTED.into())
    }

    fn pause_selected(&mut self) {
        self.execute_batch(interaction::BatchAction::Pause, None);
    }

    fn resume_selected(&mut self) {
        self.execute_batch(interaction::BatchAction::Resume, None);
    }

    fn pause_all(&mut self) {
        self.status = self.control.submit_batch(false);
    }

    fn resume_all(&mut self) {
        self.status = self.control.submit_batch(true);
    }

    #[cfg(test)]
    fn remove_selected(&mut self) {
        let Some(torrent) = self.selected() else {
            return;
        };
        let id = torrent.info_hashes.join(";");
        self.status = self
            .control
            .remove(&id)
            .map(|_| "Removal queued; data will be kept".into())
            .unwrap_or_else(|e| e.to_string());
        self.clamp_selection();
    }

    fn toggle_selected_file(&mut self) {
        let index = self.selected_file;
        let Some(torrent) = self.selected() else {
            return;
        };
        let Some(_) = torrent.files.files().get(index) else {
            return;
        };
        let indices = torrent
            .files
            .files()
            .iter()
            .enumerate()
            .filter_map(|(file_index, file)| {
                (if file_index == index {
                    !file.selected
                } else {
                    file.selected
                })
                .then_some(file_index)
            })
            .collect();
        self.status = self
            .submit_selected(TorrentCommand::SetFiles(indices))
            .unwrap_or_else(|error| error);
    }

    fn persist_selected(&mut self) {
        let Some(torrent) = self.selected() else {
            return;
        };
        let id = torrent.info_hashes.join(";");
        let audit = self.tracker_audits.get(&id).cloned();
        let record = torrent_to_record(torrent, audit);
        if let Err(error) = self.control.backend.save_torrent(&record) {
            self.status = format!("Could not save torrent: {error}");
        }
    }

    fn change_existing_mode(&mut self, input: &str) -> Result<String, String> {
        let mut parts = input.split_whitespace();
        let index: usize = parts
            .next()
            .ok_or("Enter a mode number")?
            .parse()
            .map_err(|_| "Enter a mode number from 1 to 5")?;
        let mode = *DownloadMode::ALL
            .get(index.wrapping_sub(1))
            .ok_or("Enter a mode number from 1 to 5")?;
        let torrent = self
            .control
            .torrents
            .get(self.selected_torrent)
            .ok_or("No selected torrent")?;
        let mut config = torrent.mode_config();
        config.mode = mode;
        if mode == DownloadMode::UploadRatio15 {
            if let Some(minimum) = parts.next() {
                let maximum = parts
                    .next()
                    .ok_or("Enter both minimum and maximum upload multipliers")?;
                config.upload_ratio = crate::download_mode::UploadRatioRange {
                    min_milli: crate::download_mode::UploadRatioRange::parse_multiplier(minimum)?,
                    max_milli: crate::download_mode::UploadRatioRange::parse_multiplier(maximum)?,
                };
            }
        } else if let Some(value) = parts.next() {
            config.freeze_after_bytes = value
                .parse()
                .map_err(|_| "Freeze threshold must be a positive byte count")?;
        }
        if parts.next().is_some() {
            return Err(
                "Expected a mode number, optional multiplier minimum/maximum or freeze threshold"
                    .into(),
            );
        }
        config.validate()?;
        self.submit_selected(TorrentCommand::SetMode(config))
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
        self.pending_verify_after_download = false;
        self.pending_repair = Default::default();
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
        if mode == DownloadMode::UploadRatio15 {
            self.open_prompt(
                PromptKind::UploadRatioRange,
                "Upload multiplier minimum maximum",
                "1.5 2.5".into(),
            );
            return Ok("Choose the upload multiplier range before adding".into());
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

    fn confirm_upload_ratio(&mut self, value: &str) -> Result<String, String> {
        let parts: Vec<_> = value.split_whitespace().collect();
        if parts.len() != 2 {
            return Err("Enter the minimum and maximum upload multipliers".into());
        }
        let upload_ratio = crate::download_mode::UploadRatioRange {
            min_milli: crate::download_mode::UploadRatioRange::parse_multiplier(parts[0])?,
            max_milli: crate::download_mode::UploadRatioRange::parse_multiplier(parts[1])?,
        };
        upload_ratio.validate()?;
        let (path, meta) = self
            .pending_add
            .take()
            .ok_or("No torrent pending addition")?;
        self.add_torrent_with_config(
            path,
            meta,
            crate::runtime_status::ModeConfig {
                mode: DownloadMode::UploadRatio15,
                upload_ratio,
                ..Default::default()
            },
        )
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
            .control
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
        self.add_torrent_with_config(
            path,
            metainfo,
            crate::runtime_status::ModeConfig {
                mode: download_mode,
                freeze_after_bytes,
                ..Default::default()
            },
        )
    }

    fn add_torrent_with_config(
        &mut self,
        path: PathBuf,
        metainfo: TorrentMetainfo,
        config: crate::runtime_status::ModeConfig,
    ) -> Result<String, String> {
        config.validate()?;
        let download_mode = config.mode;
        let freeze_after_bytes = config.freeze_after_bytes;
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
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
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
                verify_after_download: self.pending_verify_after_download,
                repair_policy: self.pending_repair,
                download_mode,
                freeze_after_bytes,
                upload_ratio: config.upload_ratio,
                download_directory: Some(download_directory),
                ..Default::default()
            },
        };
        let id = torrent.info_hashes.join(";");
        if self
            .control
            .torrents
            .iter()
            .any(|existing| existing.info_hashes.join(";") == id)
        {
            return Err("Torrent is already added".into());
        }
        let audit = default_audit(total_size, self.settings.connection.listen_port);
        let name = torrent.name.clone();
        self.control
            .add(torrent, &self.settings, Some(audit.clone()))?;
        self.tracker_audits.insert(id, audit);
        self.selected_torrent = self.control.torrents.len() - 1;
        Ok(format!("Added {name}"))
    }

    fn add_magnet(&mut self, uri: &str) -> Result<String, String> {
        let magnet = MagnetLink::parse(uri.trim())?;
        let name = magnet
            .display_name
            .unwrap_or_else(|| "Magnet metadata".into());
        let torrent = TorrentItem {
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
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
        self.control.add(torrent, &self.settings, None)?;
        self.selected_torrent = self.control.torrents.len() - 1;
        Ok(format!("Added {name}"))
    }

    fn set_download_directory(&mut self, directory: String) -> Result<String, String> {
        if directory.trim().is_empty() {
            return Err("Download directory cannot be empty".into());
        }
        if Path::new(directory.trim()).is_file() {
            return Err("Download directory points to a file".into());
        }
        Err("Moving existing payload is not supported. Change Downloads preferences for new torrents; no data or settings changed.".into())
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
            let mut messages = crate::live_history::LiveHistory::default();
            for (index, step) in steps.iter().enumerate() {
                if !crate::tracker_behavior::wait_for_delay(step.delay_seconds, &worker_cancel) {
                    let text = if messages.is_empty() && messages.recorded() == 0 {
                        "Tracker scenario cancelled".into()
                    } else {
                        format!("Tracker scenario cancelled · {}", messages.status())
                    };
                    let _ = sender.send(text);
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
                messages.push(crate::live_history::AuditExchange::execute(
                    &tracker_url,
                    &allow_hosts,
                    request,
                    index,
                ));
            }
            if profile.started_only
                && !crate::tracker_behavior::wait_for_delay(
                    profile.observation_seconds,
                    &worker_cancel,
                )
            {
                let _ = sender.send(format!("Observation cancelled · {}", messages.status()));
                return;
            }
            let summary = messages
                .iter()
                .rev()
                .take(8)
                .map(|entry| entry.summary())
                .collect::<Vec<_>>()
                .join("\n");
            let result = format!("{}\n{summary}", messages.status());
            messages.clear();
            let _ = sender.send(result);
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
        self.control.backend.save_torrent(&record)?;
        self.tracker_audits.insert(torrent_id, profile);
        Ok(format!(
            "Applied tracker profile '{id}'; A runs the saved scenario"
        ))
    }

    fn save_preferences(&mut self) {
        self.save_preferences_draft();
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
            "Tab focus · ←→ detail tabs · ↑↓ move · Space mark/file · / search · : commands · F1 help · q quit"
        } else {
            "←→ section · ↑↓ field · Enter edit · Space toggle · s save · Esc discard · F1 help"
        };
        frame.render_widget(Paragraph::new(help).alignment(Alignment::Center), areas[3]);
        if let Some(prompt) = &self.input {
            let mode_prompt = prompt.kind == PromptKind::DownloadMode;
            let sensitive = prompt.kind == PromptKind::Preference
                && self
                    .editing_preference
                    .as_ref()
                    .is_some_and(|(_, key)| preferences_editor::secret(key));
            let (display, cursor_column) = prompt.window(
                sensitive,
                (usize::from(frame.area().width) * 80 / 100).saturating_sub(3),
            );
            let mut lines = vec![Line::from(display)];
            if mode_prompt {
                lines.push(Line::from(format!(
                    "Verify after download: {} · F3 toggles",
                    self.pending_verify_after_download
                )));
                lines.push(Line::from(format!(
                    "Automatic repair: {} · F4 toggles · limit {}",
                    self.pending_repair.enabled, self.pending_repair.max_attempts
                )));
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
                PromptKind::DownloadMode
                    | PromptKind::FreezeAfterBytes
                    | PromptKind::UploadRatioRange
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
            if popup.width > 2 && popup.height > 2 {
                let cursor_x = popup
                    .x
                    .saturating_add(1)
                    .saturating_add(cursor_column.min(u16::MAX as usize) as u16);
                frame.set_cursor_position((
                    cursor_x.min(popup.right().saturating_sub(2)),
                    popup.y + 1,
                ));
            }
        }
        self.render_interaction(frame);
    }

    fn render_torrents(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(if area.width >= 120 {
                Direction::Horizontal
            } else {
                Direction::Vertical
            })
            .constraints([
                Constraint::Percentage(self.split_percent),
                Constraint::Percentage(100 - self.split_percent),
            ])
            .split(area);
        self.clamp_selection();
        let visible = self.visible_indices();
        let selected = visible
            .iter()
            .position(|index| *index == self.selected_torrent);
        let height = columns[0].height.saturating_sub(3).max(1) as usize;
        let start = selected.unwrap_or(0).saturating_sub(height - 1);
        let width = columns[0].width.saturating_sub(4);
        let optional = [
            ("State", 22_u16),
            ("Down/s", 10),
            ("Up/s", 10),
            ("Peers", 9),
            ("ETA", 12),
            ("Ratio", 7),
        ];
        let mut headers = vec!["Name", "%"];
        let mut widths = vec![
            Constraint::Min(12),
            Constraint::Length(if width < 45 { 9 } else { 20 }),
        ];
        let mut remaining = width.saturating_sub(if width < 45 { 23 } else { 34 });
        for (name, size) in optional {
            if remaining < size + 1 {
                break;
            }
            headers.push(name);
            widths.push(Constraint::Length(size));
            remaining -= size + 1;
        }
        let rows = visible.iter().skip(start).take(height).map(|index| {
            let torrent = &self.control.torrents[*index];
            let progress_style = if torrent.completed_successfully() {
                Style::default().fg(Color::Black).bg(Color::Green)
            } else if torrent.state == TorrentState::Checking {
                Style::default().fg(Color::Yellow)
            } else if torrent.state == TorrentState::VerificationFailed {
                Style::default().fg(Color::White).bg(Color::Red)
            } else {
                Style::default()
            };
            let mut cells = vec![
                Cell::from(format!(
                    "{} {}",
                    if self.marked.contains(&torrent.info_hashes.join(";")) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    torrent.name
                )),
                Cell::from(torrent.progress_text()).style(progress_style),
                Cell::from(torrent.status_text()),
                Cell::from(format_size(torrent.download_rate)),
                Cell::from(format_size(torrent.upload_rate)),
                Cell::from(format!("{} / {}", torrent.peers, torrent.seeds)),
                Cell::from(
                    torrent
                        .eta_seconds()
                        .map(|seconds| format!("{}m", seconds / 60))
                        .unwrap_or_else(|| "--".into()),
                ),
                Cell::from(format!("{:.2}", torrent.ratio())),
            ];
            cells.truncate(headers.len());
            Row::new(cells)
        });
        let table = Table::new(rows, widths)
            .header(Row::new(headers.clone()).style(Style::default().fg(Color::Yellow)))
            .block(
                Block::default()
                    .title(format!(
                        " Torrents · {} ({}/{}) · marked {} ",
                        self.status_filter.label(),
                        visible.len(),
                        self.control.torrents.len(),
                        self.marked.len()
                    ))
                    .border_style(Style::default().fg(if self.focus == Focus::Torrents {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    }))
                    .borders(Borders::ALL),
            )
            .row_highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        let mut state = ratatui::widgets::TableState::default();
        state.select(selected.map(|position| position - start));
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
                .block(
                    Block::default()
                        .title(" Details · Tab focuses · arrows change tab ")
                        .border_style(Style::default().fg(if self.focus == Focus::Details {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        }))
                        .borders(Borders::ALL),
                )
                .highlight_style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let visible = self.visible_indices().contains(&self.selected_torrent);
        let Some(torrent) = self
            .control
            .torrents
            .get(self.selected_torrent)
            .filter(|_| visible)
        else {
            frame.render_widget(
                Paragraph::new(if self.control.torrents.is_empty() {
                    "No torrents. Press a to add a .torrent or m for a magnet."
                } else {
                    "No torrents match this status. Press f to change the filter."
                })
                .block(Block::default().borders(Borders::ALL)),
                areas[1],
            );
            return;
        };
        match self.details_tab {
            TorrentDetailsTab::Status => {
                let last = self.control.backend.journal().and_then(|j| {
                    torrent
                        .info_hashes
                        .iter()
                        .find_map(|h| j.latest(h.strip_prefix("v1: ")?))
                });
                let lines: Vec<_> = crate::runtime_status::status_card(torrent, last.as_deref())
                    .into_iter()
                    .map(|(key, value)| Line::from(format!("{key}: {value}")))
                    .collect();
                frame.render_widget(
                    Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title(" Runtime status · up/down scroll ")
                                .borders(Borders::ALL),
                        )
                        .wrap(Wrap { trim: false })
                        .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0)),
                    areas[1],
                );
            }
            TorrentDetailsTab::Files => {
                let height = areas[1].height.saturating_sub(2).max(1) as usize;
                let start = self.selected_file.saturating_sub(height - 1);
                let items = torrent
                    .files
                    .files()
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(height)
                    .map(|(index, file)| {
                        ListItem::new(format!(
                            "[{}] {}{}  {} / {}",
                            if file.selected { 'x' } else { ' ' },
                            if torrent.files.is_high_priority(index) {
                                "[high] "
                            } else {
                                ""
                            },
                            file.path,
                            format_size(file.downloaded_bytes),
                            format_size(file.size_bytes)
                        ))
                    });
                let mut state = ListState::default();
                if !torrent.files.files().is_empty() {
                    state.select(Some(self.selected_file - start));
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
                    Line::from(format!(
                        "State: {} · {}",
                        torrent.status_text(),
                        torrent.progress_text()
                    )),
                    Line::from(format!("Integrity: {:?}", torrent.completion)),
                    Line::from(format!("Private: {}", torrent.is_private)),
                    Line::from(format!("Source: {}", torrent.source)),
                    Line::from(format!("Info hashes: {}", torrent.info_hashes.join(", "))),
                    Line::from(format!("Audit Peer ID: {}", torrent.peer_id)),
                    Line::from(format!("Download mode: {}", torrent.mode_config().label())),
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
                        .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0))
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
                        Line::from(format!("Priority: {:?} (download queue admission)", torrent.overrides.priority)),
                        Line::from(format!("Seed ratio: {:?}; minutes: {:?}", effective.ratio_limit, effective.seeding_time_limit_minutes)),
                        Line::from("F7 edits transfer controls; :set FIELD VALUE edits one field."),
                        Line::from("Changing peer capacity rechecks only this torrent, in the same engine."),
                        Line::from(format!(
                            "Verify after download: {} · V toggles",
                            torrent.overrides.verify_after_download
                        )),
                        Line::from("Payload relocation is not supported; Downloads preferences set new-torrent defaults."),
                        Line::from(format!(
                            "Saved mode: {} · M changes",
                            torrent.mode_config().label()
                        )),
                        Line::from(format!(
                            "Policy status: {} · see Status",
                            torrent.runtime.mode.label()
                        )),
                        Line::from(format!(
                            "Auto repair: {} · H toggles · limit {} · L edits",
                            torrent.overrides.repair_policy.enabled,
                            torrent.overrides.repair_policy.max_attempts
                        )),
                    ])
                    .block(
                        Block::default()
                            .title(" Torrent settings ")
                            .borders(Borders::ALL),
                    )
                    .wrap(Wrap { trim: false })
                    .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0)),
                    areas[1],
                );
            }
            TorrentDetailsTab::Peers => frame.render_widget(
                Paragraph::new(
                    std::iter::once(Line::from(format!(
                        "Connected: {} · showing at most 128 live peers",
                        torrent.peers
                    )))
                    .chain(torrent.runtime.diagnostics.peers.iter().map(|peer| {
                        Line::from(format!(
                            "{} {} {} down={} up={} errors={}",
                            peer.address,
                            peer.state,
                            peer.client,
                            peer.downloaded,
                            peer.uploaded,
                            peer.errors
                        ))
                    }))
                    .collect::<Vec<_>>(),
                )
                .block(Block::default().title(" Peers ").borders(Borders::ALL))
                .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0)),
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
                        .scroll((self.selected_file.min(u16::MAX as usize) as u16, 0))
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
                                .title(if torrent.state == TorrentState::Checking {
                                    " Integrity check "
                                } else {
                                    " Verified payload "
                                })
                                .borders(Borders::ALL),
                        )
                        .ratio(
                            torrent
                                .completion
                                .checking_progress()
                                .map(f64::from)
                                .unwrap_or(ratio),
                        )
                        .label(format!(
                            "{} · {} / {}",
                            torrent.progress_text(),
                            format_size(torrent.downloaded_bytes),
                            format_size(total)
                        )),
                    areas[1],
                );
            }
            TorrentDetailsTab::Log => {
                let mut lines = Vec::new();
                if let Some(journal) = self.control.backend.journal() {
                    if let Some(path) = journal.path() {
                        lines.push(Line::from(format!("JSONL: {}", path.display())));
                    }
                    if let Some(error) = journal.error() {
                        lines.push(Line::from(error));
                    }
                    let hash = crate::announce_journal::torrent_hash(&torrent.info_hashes);
                    let page = journal.page(
                        Some(hash.unwrap_or_default()),
                        self.journal_page
                            .saturating_mul(crate::announce_journal::PAGE_SIZE),
                        crate::announce_journal::PAGE_SIZE,
                    );
                    let last_page =
                        page.total.saturating_sub(1) / crate::announce_journal::PAGE_SIZE;
                    self.journal_page = self.journal_page.min(last_page);
                    let page = journal.page(
                        Some(hash.unwrap_or_default()),
                        self.journal_page * crate::announce_journal::PAGE_SIZE,
                        crate::announce_journal::PAGE_SIZE,
                    );
                    lines.push(Line::from(format!("Page {}/{} · {} live entries · {} / {} retained bytes · {} oversized on disk only", self.journal_page + 1, last_page + 1, page.total, page.retained_bytes, crate::announce_journal::LIVE_BYTE_BUDGET, page.skipped_oversize)));
                    let available = areas[1].height.saturating_sub(2 + lines.len() as u16) as usize;
                    lines.extend(journal_visible_lines(
                        &page.entries,
                        self.selected_file,
                        available,
                    ));
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
                                .title(" Real announces · e/E export · v recheck · PgUp/PgDn page · up/down lines ")
                                .borders(Borders::ALL),
                        )
                        .wrap(Wrap { trim: false }),
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
        let fields = preferences_editor::fields(
            self.preferences_draft.as_ref().unwrap_or(&self.settings),
            self.preferences_tab,
        );
        let items = fields.iter().map(|(key, value)| {
            ListItem::new(format!(
                "{key}: {} [{}]",
                preferences_editor::display(key, value),
                preferences_editor::application(self.preferences_tab, key)
            ))
        });
        let mut state = ListState::default();
        state.select(Some(
            self.selected_preference.min(fields.len().saturating_sub(1)),
        ));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    Block::default()
                        .title(if self.preferences_draft.is_some() {
                            " Preferences draft · s saves; active engine unchanged "
                        } else {
                            " Saved preferences · Enter edits selected field "
                        })
                        .borders(Borders::ALL),
                )
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("▶ "),
            areas[1],
            &mut state,
        );
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        if let Some(cancel) = &self.audit_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Err(error) = self.control.shutdown() {
            eprintln!("PeerLume shutdown: {error}");
        }
    }
}

pub fn run() -> io::Result<()> {
    crate::settings::configure_profile_from_args()?;
    let directory = crate::settings::data_directory();
    let _instance = crate::control::host::InstanceLock::acquire(&directory)?;
    let mut terminal = TerminalGuard::enter()?;
    let mut app = TuiApp::load();
    let download_root = PathBuf::from(&app.settings.download.download_directory);
    app.control
        .configure_agents(&directory, download_root)
        .map_err(|e| io::Error::other(format!("Agent initialization failed: {e:?}")))?;
    let _ = app.control.set_agent_settings(&app.settings);
    let owner = Arc::new(std::sync::Mutex::new(app));
    let _host =
        crate::control::host::Host::start(owner.clone(), |app| &mut app.control, directory)?;
    loop {
        {
            let mut app = owner.lock().unwrap_or_else(|e| e.into_inner());
            if !app.running {
                break;
            }
            app.poll();
            terminal.terminal.draw(|frame| {
                if app.agent_panel.open {
                    app.agent_panel.render(&app.control, frame);
                } else {
                    app.render(frame);
                }
            })?;
        }
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            owner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .handle_key(key);
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

/// Format only the viewport, including when a tracker returns thousands of peers.
fn journal_visible_lines(
    entries: &[Arc<crate::announce_journal::JournalEntry>],
    mut offset: usize,
    limit: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(limit);
    for entry in entries {
        let length = 3 + entry.announce.reply.peers.len();
        if offset >= length {
            offset -= length;
            continue;
        }
        for index in offset..length {
            if lines.len() == limit {
                return lines;
            }
            let a = &entry.announce;
            let text = match index {
                0 => entry.summary().to_owned(),
                1 => format!(
                    "info_hash={} transport={} full_size={} selected_bytes={:?} request_transmitted={:?}",
                    a.info_hash, a.transport, a.full_size, a.selected_bytes, a.request_transmitted
                ),
                2 => format!(
                    "mode={} peer_id={} port={} HTTP={:?}, interval={:?}, min={:?}, seeds={:?}, leechers={:?}, warning={:?}, failure={:?}",
                    a.mode,
                    a.peer_id_hex,
                    a.port,
                    a.reply.http_status,
                    a.reply.interval,
                    a.reply.min_interval,
                    a.reply.seeders,
                    a.reply.leechers,
                    a.reply.warning,
                    a.reply.failure
                ),
                _ => format!("  peer {}", a.reply.peers[index - 3]),
            };
            lines.push(Line::from(text));
        }
        offset = 0;
    }
    lines
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

    #[test]
    #[ignore = "installed frontend workflow; Reliability CI supplies the copied adapter"]
    fn reliability_installed_frontend_workflow_ratatui() {
        crate::control::test_support::installed_frontend_workflow(
            |control, command| {
                let mut app = app();
                std::mem::swap(&mut app.control, control);
                match command {
                    TorrentCommand::Resume => app.resume_selected(),
                    TorrentCommand::Pause => app.pause_selected(),
                    _ => panic!("unexpected command"),
                }
                assert_eq!(app.status, COMMAND_ACCEPTED);
                std::mem::swap(&mut app.control, control);
            },
            |control| {
                let mut app = app();
                std::mem::swap(&mut app.control, control);
                app.details_tab = TorrentDetailsTab::Status;
                let mut terminal = Terminal::new(TestBackend::new(240, 70)).unwrap();
                terminal.draw(|f| app.render(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("Status"));
                std::mem::swap(&mut app.control, control);
            },
        );
    }

    pub(super) fn wait_for_tui(app: &mut TuiApp, predicate: impl Fn(&TuiApp) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if predicate(app) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "TUI did not confirm the command: {}",
                app.status
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn live_tui_pause_resume_and_files_use_engine_confirmations() {
        let crate::control::test_support::LiveFixture {
            controller,
            root: _root,
            ..
        } = crate::control::test_support::LiveFixture::new();
        let mut app = TuiApp::from_parts(
            ClientSettings::default(),
            ClientBackend::offline(),
            vec![],
            HashMap::new(),
        );
        app.control = controller;
        app.resume_selected();
        assert_eq!(app.status, COMMAND_ACCEPTED);
        assert_eq!(app.control.torrents[0].state, TorrentState::Paused);
        wait_for_tui(&mut app, |app| {
            app.control.torrents[0].state == TorrentState::Seeding
        });
        assert_eq!(
            app.control.backend.load_torrents().unwrap()[0].state,
            "downloading"
        );
        app.pause_selected();
        assert_eq!(app.control.torrents[0].state, TorrentState::Seeding);
        assert_eq!(
            app.control.backend.load_torrents().unwrap()[0].state,
            "downloading"
        );
        wait_for_tui(&mut app, |app| {
            app.control.torrents[0].state == TorrentState::Paused
        });
        assert_eq!(
            app.control.backend.load_torrents().unwrap()[0].state,
            "paused"
        );
        app.toggle_selected_file();
        assert!(app.control.torrents[0].files.files()[0].selected);
        wait_for_tui(&mut app, |app| {
            !app.control.torrents[0].files.files()[0].selected
        });
        assert!(!app.control.backend.load_torrents().unwrap()[0].files[0].selected);
    }

    #[test]
    fn live_tui_verification_toggle_persists_the_engine_acknowledgement() {
        let crate::control::test_support::LiveFixture {
            controller,
            root: _root,
            ..
        } = crate::control::test_support::LiveFixture::new();
        let mut app = TuiApp::from_parts(
            ClientSettings::default(),
            ClientBackend::offline(),
            vec![],
            HashMap::new(),
        );
        app.control = controller;
        app.handle_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE));
        assert_eq!(app.status, COMMAND_ACCEPTED);
        assert!(!app.control.torrents[0].overrides.verify_after_download);
        assert!(!app.control.backend.load_torrents().unwrap()[0].verify_after_download);
        wait_for_tui(&mut app, |app| {
            app.control.torrents[0].overrides.verify_after_download
        });
        assert!(app.control.backend.load_torrents().unwrap()[0].verify_after_download);
    }

    #[test]
    fn journal_formats_only_the_viewport_of_a_large_peer_reply() {
        let journal = crate::announce_journal::AnnounceJournal::memory();
        let mut observation = crate::announce_journal::tests::observation("a");
        observation.reply.peers = (0..100_000)
            .map(|i| std::net::SocketAddr::from(([127, 0, 0, 1], (i % 60000 + 1024) as u16)))
            .collect();
        journal.record(observation);
        let page = journal.page(Some("a"), 0, 32);
        let lines = journal_visible_lines(&page.entries, 100_001, 4);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[1].spans[0]
                .content
                .contains(&page.entries[0].announce.reply.peers[99_999].to_string())
        );
        assert!(journal_visible_lines(&page.entries, usize::MAX, 4).is_empty());
        assert!(journal_visible_lines(&page.entries, 0, 0).is_empty());
        let mut app = app();
        app.details_tab = TorrentDetailsTab::Log;
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.journal_page, 1);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.journal_page, 0);
    }

    #[test]
    fn existing_mode_does_not_claim_success_when_backend_is_offline() {
        let mut app = app();
        assert!(app.change_existing_mode("2").is_err());
        assert_eq!(
            app.control.torrents[0].overrides.download_mode,
            DownloadMode::Normal
        );
        assert!(matches!(
            app.control.torrents[0].runtime.mode,
            crate::runtime_status::ModeApplication::Failed { active: None, .. }
        ));
        assert!(app.change_existing_mode("5 0").is_err());
        assert!(app.change_existing_mode("9").is_err());
        assert!(app.change_existing_mode("1 3 extra").is_err());
    }

    #[test]
    fn tui_status_renders_the_shared_model_and_scrolls_without_changing_torrent() {
        let mut app = app();
        app.focus = Focus::Details;
        app.details_tab = TorrentDetailsTab::Status;
        let expected = crate::runtime_status::status_card(&app.control.torrents[0], None);
        let mut terminal = Terminal::new(TestBackend::new(240, 70)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for (label, value) in expected {
            assert!(text.contains(&label), "missing label {label}");
            assert!(text.contains(&value), "missing value {value}");
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_file, 1);
        assert_eq!(app.selected_torrent, 0);
    }

    #[test]
    fn completed_checking_and_failed_rows_render_distinct_progress_states() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(220, 40)).unwrap();
        for (state, completion, expected) in [
            (
                TorrentState::Seeding,
                crate::completion::CompletionStatus::Verified,
                "Done",
            ),
            (
                TorrentState::Checking,
                crate::completion::CompletionStatus::Checking {
                    checked_bytes: 25,
                    total_bytes: 100,
                },
                "Checking",
            ),
            (
                TorrentState::VerificationFailed,
                crate::completion::CompletionStatus::Failed {
                    message: "mismatch".into(),
                },
                "Verification failed",
            ),
        ] {
            app.control.torrents[0].state = state;
            app.control.torrents[0].completion = completion;
            app.control.torrents[0].downloaded_bytes = app.control.torrents[0].selected_size();
            terminal.draw(|frame| app.render(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(
                text.contains(expected),
                "missing progress state: {expected}"
            );
            if state == TorrentState::Seeding {
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|cell| cell.symbol() == "D" && cell.bg == Color::Green)
                );
            }
        }
    }

    #[test]
    fn offline_verification_toggle_does_not_change_confirmed_settings() {
        let mut app = app();
        assert!(!app.control.torrents[0].overrides.verify_after_download);
        app.handle_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE));
        assert!(!app.control.torrents[0].overrides.verify_after_download);
        assert!(app.status.contains("unavailable"));
        let record = torrent_to_record(&app.control.torrents[0], None);
        assert!(!record.verify_after_download);
    }

    #[test]
    fn pending_torrent_verification_can_be_enabled_before_addition() {
        let mut app = app();
        let metadata = TorrentMetainfo::parse(b"d4:infod6:lengthi1e4:name1:aee").unwrap();
        app.pending_add = Some((PathBuf::from("fixture.torrent"), metadata));
        app.open_prompt(PromptKind::DownloadMode, "Download mode", "1".into());
        app.handle_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        assert!(app.pending_verify_after_download);
        assert_eq!(app.input.as_ref().unwrap().value, "1");
        app.handle_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        assert!(!app.pending_verify_after_download);
    }

    pub(super) fn test_torrent() -> TorrentItem {
        TorrentItem {
            runtime: Default::default(),
            completion: crate::completion::CompletionStatus::Pending,
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

    pub(super) fn app() -> TuiApp {
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
        app.control.torrents[0].trackers = vec!["https://tracker.test/ann?uk=fixture-key".into()];
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
        let original_trackers = app.control.torrents[0].trackers.clone();
        app.apply_behavior_profile("periodic-test").unwrap();
        let id = app.control.torrents[0].info_hashes.join(";");
        assert_eq!(app.tracker_audits[&id].steps.len(), 4);
        assert_eq!(app.tracker_audits[&id].steps[0].left, 1024);
        assert_eq!(app.control.torrents[0].trackers, original_trackers);
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
        app.control.torrents[0].trackers = vec![format!("http://{address}/announce")];
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
        let id = app.control.torrents[0].info_hashes.join(";");
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
    fn status_filter_cycles_and_navigation_retains_the_original_torrent_target() {
        let mut app = app();
        let mut paused = test_torrent();
        paused.name = "Hidden paused torrent".into();
        paused.state = TorrentState::Paused;
        let mut first = test_torrent();
        first.name = "Visible download one".into();
        first.info_hashes = vec!["v1: first".into()];
        let mut second = first.clone();
        second.name = "Visible download two".into();
        second.info_hashes = vec!["v1: second".into()];
        app.control.torrents = vec![paused.clone(), first, paused, second];
        app.details_tab = TorrentDetailsTab::Info;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.status_filter, StatusFilter::Downloading);
        assert_eq!(app.selected_torrent, 1);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_torrent, 3);
        assert_eq!(app.selected().unwrap().info_hashes, ["v1: second"]);
        let mut terminal = Terminal::new(TestBackend::new(220, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("Hidden paused torrent"));
        assert!(text.contains("Downloading (2/4)"));
        app.control.torrents[3].state = TorrentState::Seeding;
        app.clamp_selection();
        assert_eq!(app.selected_torrent, 1);
        app.status_filter = StatusFilter::Error;
        app.clamp_selection();
        assert!(app.selected().is_none());
        assert!(app.selected_mut().is_none());
        app.pause_selected();
        app.resume_selected();
        app.remove_selected();
        assert_eq!(app.control.torrents.len(), 4);
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("No torrents match this status"));
        assert!(!text.contains("Hidden paused torrent"));
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.status_filter, StatusFilter::All);
        assert_eq!(app.control.torrents.len(), 4);
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
    fn upload_multiplier_prompt_uses_default_bounds_and_rejects_invalid_ranges_without_adding() {
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
        let count = app.control.torrents.len();
        app.confirm_torrent_mode("3").unwrap();
        let prompt = app.input.as_ref().unwrap();
        assert_eq!(prompt.kind, PromptKind::UploadRatioRange);
        assert_eq!(prompt.value, "1.5 2.5");
        for text in ["0 2.5", "2.6 2.5", "NaN 2.5", "1.5", "1.5 2.5 3"] {
            assert!(app.confirm_upload_ratio(text).is_err(), "{text}");
            assert!(app.pending_add.is_some());
            assert_eq!(app.control.torrents.len(), count);
        }
        app.handle_prompt_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.input.is_none());
        assert!(app.pending_add.is_none());
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
        let count = app.control.torrents.len();
        app.confirm_torrent_mode("5").unwrap();
        assert_eq!(
            app.input.as_ref().unwrap().kind,
            PromptKind::FreezeAfterBytes
        );
        assert_eq!(app.control.torrents.len(), count);
        assert!(app.confirm_freeze_threshold("0").is_err());
        assert!(app.confirm_freeze_threshold("NaN").is_err());
        assert!(app.pending_add.is_some());
    }

    #[test]
    fn log_export_keys_and_scrolling_are_separate_from_file_selection() {
        let mut app = app();
        app.focus = Focus::Details;
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
    fn offline_file_toggle_does_not_change_confirmed_selection() {
        let mut app = app();
        assert!(app.control.torrents[0].files.files()[0].selected);
        app.toggle_selected_file();
        assert!(app.control.torrents[0].files.files()[0].selected);
        assert!(app.status.contains("unavailable"));
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
        assert_eq!(app.focus, Focus::Details);
        assert_eq!(app.details_tab, TorrentDetailsTab::Files);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.details_tab, TorrentDetailsTab::Info);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Torrents);
        assert_eq!(app.details_tab, TorrentDetailsTab::Files);
        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(app.page, MainPage::Preferences);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.preferences_tab, PreferencesTab::Queue);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.preferences_tab, PreferencesTab::Downloads);
    }

    #[test]
    fn offline_pause_resume_and_batches_do_not_claim_success() {
        let mut app = app();
        let before = app.control.torrents[0].state;
        app.pause_selected();
        assert_eq!(app.control.torrents[0].state, before);
        assert!(app.status.contains("unavailable"));
        app.resume_selected();
        assert_eq!(app.control.torrents[0].state, before);
        assert!(app.status.contains("unavailable"));
        app.pause_all();
        assert_eq!(app.control.torrents[0].state, before);
        assert!(app.status.contains("queued: 0"));
        app.resume_all();
        assert_eq!(app.control.torrents[0].state, before);
        assert!(app.status.contains("queued: 0"));
        app.remove_selected();
        assert_eq!(app.control.torrents.len(), 1);
        assert!(app.status.contains("store is unavailable"));
    }

    #[test]
    fn torrent_paths_and_tracker_urls_are_validated_and_persisted_in_model() {
        let mut app = app();
        assert!(app.set_download_directory(String::new()).is_err());
        assert!(
            app.set_download_directory("downloads/other".into())
                .is_err()
        );
        assert_eq!(
            app.control.torrents[0]
                .overrides
                .download_directory
                .as_deref(),
            None
        );
        assert!(app.set_tracker_url("ftp://invalid".into()).is_err());
        assert!(
            app.set_tracker_url("http://127.0.0.1:9000/announce".into())
                .is_ok()
        );
        assert_eq!(
            app.control.torrents[0].trackers,
            ["http://127.0.0.1:9000/announce"]
        );
    }

    #[test]
    fn every_preference_section_has_a_tui_mutation() {
        let mut app = app();
        let original = app.settings.clone();
        for tab in PreferencesTab::ALL {
            app.preferences_tab = tab;
            app.selected_preference = preferences_editor::fields(
                app.preferences_draft.as_ref().unwrap_or(&app.settings),
                tab,
            )
            .iter()
            .position(|(_, value)| value.is_boolean())
            .unwrap();
            app.toggle_preference();
        }
        assert_eq!(app.settings, original);
        let draft = app.preferences_draft.as_ref().unwrap();
        assert_ne!(draft.download, original.download);
        assert_ne!(draft.queue, original.queue);
        assert_ne!(draft.speed, original.speed);
        assert_ne!(draft.connection, original.connection);
        assert_ne!(draft.bittorrent, original.bittorrent);
        assert_ne!(draft.seeding, original.seeding);
        assert_ne!(draft.privacy, original.privacy);
        assert_ne!(draft.storage, original.storage);
        assert_ne!(draft.automation, original.automation);
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
