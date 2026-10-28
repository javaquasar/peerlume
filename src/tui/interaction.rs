use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    Torrents,
    Details,
}
impl Focus {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Torrents => Self::Details,
            Self::Details => Self::Torrents,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BatchAction {
    Pause,
    Resume,
    Check,
    Repair,
    Remove,
}
#[derive(Clone, Debug)]
pub(super) struct PendingBatch {
    action: BatchAction,
    targets: Vec<(String, u64)>,
}

impl TuiApp {
    pub(super) fn visible_indices(&self) -> Vec<usize> {
        crate::torrent_view::indices_with_query(
            &self.control.torrents,
            self.status_filter,
            &self.settings.ui,
            &self.search,
        )
    }

    pub(super) fn retain_visible_marks(&mut self) {
        let visible: std::collections::BTreeSet<_> = self
            .visible_indices()
            .iter()
            .map(|index| self.control.torrents[*index].info_hashes.join(";"))
            .collect();
        self.marked.retain(|id| visible.contains(id));
    }

    fn targets(&self) -> Vec<(String, u64)> {
        let ids: Vec<_> = if self.marked.is_empty() {
            self.selected()
                .map(|torrent| vec![torrent.info_hashes.join(";")])
                .unwrap_or_default()
        } else {
            self.visible_indices()
                .iter()
                .map(|index| self.control.torrents[*index].info_hashes.join(";"))
                .filter(|id| self.marked.contains(id))
                .collect()
        };
        ids.into_iter()
            .filter_map(|id| {
                self.control
                    .command_revision(&id)
                    .ok()
                    .map(|revision| (id, revision))
            })
            .collect()
    }

    pub(super) fn request_batch(&mut self, action: BatchAction) {
        let targets = self.targets();
        if targets.is_empty() {
            self.status = "No visible torrent targets".into();
            return;
        }
        self.confirmation = Some(PendingBatch { action, targets });
    }

    pub(super) fn execute_batch(
        &mut self,
        action: BatchAction,
        snapshot: Option<Vec<(String, u64)>>,
    ) {
        let targets = snapshot.unwrap_or_else(|| self.targets());
        let total = targets.len();
        let mut accepted = 0;
        let mut errors = Vec::new();
        for (id, revision) in targets {
            let result = if self.control.command_revision(&id).ok() != Some(revision) {
                Err("Torrent changed while awaiting confirmation; refresh and retry".to_owned())
            } else if action == BatchAction::Remove {
                self.control
                    .remove(&id)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            } else {
                let command = match action {
                    BatchAction::Pause => TorrentCommand::Pause,
                    BatchAction::Resume => TorrentCommand::Resume,
                    BatchAction::Check => TorrentCommand::Recheck,
                    BatchAction::Repair => TorrentCommand::RecheckAndRepair,
                    BatchAction::Remove => unreachable!(),
                };
                self.control
                    .submit(&id, command, Some(revision))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            };
            match result {
                Ok(()) => accepted += 1,
                Err(error) => errors.push(error),
            }
        }
        self.status = if total == 1 && accepted == 1 && action != BatchAction::Remove {
            COMMAND_ACCEPTED.into()
        } else {
            format!(
                "Queued {accepted}/{total}; rejected {}. Awaiting engine confirmation.{}{}",
                errors.len(),
                if action == BatchAction::Remove {
                    " Downloaded files are kept."
                } else {
                    ""
                },
                errors
                    .first()
                    .map(|error| format!(" {error}"))
                    .unwrap_or_default()
            )
        };
    }

    pub(super) fn handle_interaction(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.running = false;
            self.input = None;
            self.confirmation = None;
            return true;
        }
        if self.input.is_some() {
            return false;
        }
        if let Some(pending) = self.confirmation.take() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.execute_batch(pending.action, Some(pending.targets))
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.status = "Cancelled; no commands sent".into()
                }
                _ => self.confirmation = Some(pending),
            }
            return true;
        }
        if key.code == KeyCode::F(1) {
            self.help = !self.help;
            return true;
        }
        if self.help {
            if key.code == KeyCode::Esc {
                self.help = false;
            }
            return true;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    self.split_percent = self.split_percent.saturating_sub(5).max(20)
                }
                KeyCode::Right | KeyCode::Down => {
                    self.split_percent = self.split_percent.saturating_add(5).min(80)
                }
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::F(7) if self.page == MainPage::Torrents => {
                if let Some(torrent) = self.selected() {
                    let settings = crate::transfer_settings::TransferSettings::from_item(
                        torrent,
                        &self.settings,
                    );
                    let text = format!(
                        "{} {} {} {} {} {:.3} {}",
                        settings.download_kib,
                        settings.upload_kib,
                        settings.max_peers,
                        settings.sequential,
                        match settings.priority {
                            crate::transfer_settings::Priority::Low => "low",
                            crate::transfer_settings::Priority::Normal => "normal",
                            crate::transfer_settings::Priority::High => "high",
                        },
                        f64::from(settings.ratio_milli) / 1000.0,
                        settings.seed_minutes
                    );
                    self.open_prompt(PromptKind::Transfer, "DownKiB UpKiB Peers Sequential Priority SeedRatio SeedMinutes (0 = no rate/seed limit)", text);
                }
            }
            KeyCode::Char(':') => self.open_prompt(
                PromptKind::Command,
                "Command (: help lists commands; no shell execution)",
                String::new(),
            ),
            KeyCode::Char('/') if self.page == MainPage::Torrents => self.open_prompt(
                PromptKind::Search,
                "Search torrent names (empty clears)",
                self.search.clone(),
            ),
            KeyCode::F(6) if self.page == MainPage::Torrents => self.focus = self.focus.next(),
            KeyCode::Left if self.page == MainPage::Torrents && self.focus == Focus::Details => {
                self.details_tab = self.details_tab.previous();
                self.selected_file = 0;
            }
            KeyCode::Right if self.page == MainPage::Torrents && self.focus == Focus::Details => {
                self.details_tab = self.details_tab.next();
                self.selected_file = 0;
            }
            KeyCode::Enter if self.page == MainPage::Preferences => self.edit_preference(),
            KeyCode::Esc if self.page == MainPage::Preferences => {
                self.preferences_draft = None;
                self.status = "Preference draft discarded".into();
            }
            KeyCode::Esc => {
                self.marked.clear();
                self.status = "Selection cleared".into();
            }
            KeyCode::Char(' ')
                if self.page == MainPage::Torrents && self.focus == Focus::Torrents =>
            {
                if let Some(torrent) = self.selected() {
                    let id = torrent.info_hashes.join(";");
                    if !self.marked.remove(&id) {
                        self.marked.insert(id);
                    }
                }
            }
            KeyCode::Char('*') if self.page == MainPage::Torrents => {
                self.marked = self
                    .visible_indices()
                    .iter()
                    .map(|index| self.control.torrents[*index].info_hashes.join(";"))
                    .collect();
            }
            KeyCode::Char('c') if self.page == MainPage::Torrents => {
                self.request_batch(BatchAction::Repair)
            }
            KeyCode::Char('v') if self.page == MainPage::Torrents => {
                self.execute_batch(BatchAction::Check, None)
            }
            _ => return false,
        }
        true
    }

    pub(super) fn run_command(&mut self, command: &str) -> Result<String, String> {
        let (verb, argument) = command
            .trim()
            .split_once(' ')
            .unwrap_or((command.trim(), ""));
        match verb {
            "diagnostics" => {
                let torrent = self.selected().ok_or("No selected torrent")?;
                let path = std::path::Path::new(argument);
                if !path.is_absolute() {
                    return Err("Use an absolute report path".into());
                }
                crate::diagnostics::export(torrent, path)
                    .map_err(|_| "Could not create report; existing files are never overwritten")?;
                return Ok("Redacted diagnostics exported".into());
            }
            "file-priority" => {
                let (index, value) = argument
                    .split_once(' ')
                    .ok_or("Use: file-priority FILE_NUMBER normal|high|skip")?;
                let index = index
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .ok_or("File number starts at 1")?;
                let torrent = self.selected().ok_or("No selected torrent")?;
                if index >= torrent.files.files().len() {
                    return Err("File number out of range".into());
                }
                let id = torrent.info_hashes.join(";");
                let mut high = torrent.files.high_priority();
                high.retain(|value| *value != index);
                match value.trim() {
                    "high" => high.push(index),
                    "normal" => {}
                    "skip" => {
                        let files = torrent
                            .files
                            .files()
                            .iter()
                            .enumerate()
                            .filter_map(|(i, file)| (i != index && file.selected).then_some(i))
                            .collect();
                        self.control
                            .backend
                            .send(crate::session::SessionCommand::SetFiles {
                                id: id.clone(),
                                only_files: files,
                            })?;
                    }
                    _ => return Err("Priority: normal, high or skip".into()),
                }
                self.control.set_file_priorities(&id, high)?;
                return Ok(COMMAND_ACCEPTED.into());
            }
            "remove-originals" => {
                if argument != "CONFIRM" {
                    return Err("Permanently remove duplicate originals after revalidation: remove-originals CONFIRM".into());
                }
                let id = self
                    .selected()
                    .ok_or("No selected torrent")?
                    .info_hashes
                    .join(";");
                self.control.remove_copy_originals(&id)?;
                return Ok(
                    "Original cleanup queued; destination retained; inspect result before resuming"
                        .into(),
                );
            }
            "move-data" => {
                let id = self
                    .selected()
                    .ok_or("No selected torrent")?
                    .info_hashes
                    .join(";");
                self.control.move_storage(&id, argument.into())?;
                return Ok(
                    "Fast move queued; exclusive complete directory on one volume required".into(),
                );
            }
            "use-data" | "copy-data" => {
                let id = self
                    .selected()
                    .ok_or("No selected torrent")?
                    .info_hashes
                    .join(";");
                self.control
                    .select_storage(&id, argument.into(), verb == "copy-data")?;
                return Ok(COMMAND_ACCEPTED.into());
            }
            "cancel-copy" => {
                let id = self
                    .selected()
                    .ok_or("No selected torrent")?
                    .info_hashes
                    .join(";");
                self.control.cancel_storage(&id)?;
                return Ok("Copy cancellation queued; originals retained".into());
            }
            "sort" => {
                self.settings.ui.sort = crate::torrent_view::Sort::ALL
                    .into_iter()
                    .find(|sort| sort.label().eq_ignore_ascii_case(argument))
                    .ok_or("Sort: added, name, progress, download, upload, size")?
            }
            "descending" => {
                self.settings.ui.descending =
                    argument.parse().map_err(|_| "Use :descending true|false")?
            }
            "category" => self.settings.ui.category_filter = argument.into(),
            "label" | "categorize" => {
                let id = self
                    .selected()
                    .ok_or("No selected torrent")?
                    .info_hashes
                    .join(";");
                let map = if verb == "label" {
                    &mut self.settings.ui.labels
                } else {
                    &mut self.settings.ui.categories
                };
                map.insert(id, argument.into());
            }
            "save-view" => {
                let path = crate::settings::data_directory().join("settings.toml");
                let mut saved = match ClientSettings::load_from(&path) {
                    Ok(saved) => saved,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        ClientSettings::default()
                    }
                    Err(error) => return Err(error.to_string()),
                };
                saved.ui = self.settings.ui.clone();
                saved.ui.search = self.search.clone();
                saved.ui.torrent_pane_percent = self.split_percent;
                saved.save_to(&path).map_err(|error| error.to_string())?;
            }
            "set" => return self.set_transfer_field(argument),
            "help" => self.help = true,
            "pause" => self.execute_batch(BatchAction::Pause, None),
            "resume" => self.execute_batch(BatchAction::Resume, None),
            "check" => self.execute_batch(BatchAction::Check, None),
            "repair" => self.request_batch(BatchAction::Repair),
            "remove" => self.request_batch(BatchAction::Remove),
            "preferences" => self.page = MainPage::Preferences,
            "torrents" => self.page = MainPage::Torrents,
            "search" => {
                self.search = argument.into();
                self.clamp_selection();
                self.retain_visible_marks();
            }
            "split" => {
                self.split_percent = argument
                    .parse::<u16>()
                    .ok()
                    .filter(|value| (20..=80).contains(value))
                    .ok_or("Split must be between 20 and 80 percent")?;
            }
            "clear" => self.marked.clear(),
            _ => return Err("Unknown command. Use :help. Commands never execute a shell.".into()),
        }
        Ok(if matches!(verb, "pause" | "resume" | "check") {
            self.status.clone()
        } else {
            format!("Command: {verb}")
        })
    }

    pub(super) fn edit_preference(&mut self) {
        let draft = self.preferences_draft.as_ref().unwrap_or(&self.settings);
        let fields = preferences_editor::fields(draft, self.preferences_tab);
        let Some((key, value)) = fields.get(self.selected_preference) else {
            return;
        };
        let value = if preferences_editor::secret(key) {
            String::new()
        } else {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        };
        let choices = preferences_editor::choices(key);
        self.status = if choices.is_empty() {
            format!(
                "Editing {}.{}. Esc cancels; strings may be unquoted.",
                preferences_editor::section(self.preferences_tab),
                key
            )
        } else {
            format!("Choices: {}", choices.join(", "))
        };
        self.editing_preference = Some((self.preferences_tab, key.clone()));
        self.open_prompt(
            PromptKind::Preference,
            "Preference value (Enter updates draft; s saves preferences)",
            value,
        );
    }

    pub(super) fn change_transfer(&mut self, text: &str) -> Result<String, String> {
        use crate::transfer_settings::{Priority, TransferSettings};
        let fields: Vec<_> = text.split_whitespace().collect();
        if fields.len() != 7 {
            return Err("Enter seven values: download_KiB upload_KiB peers sequential priority seed_ratio seed_minutes".into());
        }
        let ratio: f64 = fields[5].parse().map_err(|_| "Invalid seed ratio")?;
        if !ratio.is_finite() || !(0.0..=1000.0).contains(&ratio) {
            return Err("Seed ratio must be 0..1000".into());
        }
        let settings = TransferSettings {
            download_kib: fields[0].parse().map_err(|_| "Invalid download rate")?,
            upload_kib: fields[1].parse().map_err(|_| "Invalid upload rate")?,
            max_peers: fields[2].parse().map_err(|_| "Invalid peer count")?,
            sequential: fields[3]
                .parse()
                .map_err(|_| "Sequential must be true or false")?,
            priority: match fields[4] {
                "low" => Priority::Low,
                "normal" => Priority::Normal,
                "high" => Priority::High,
                _ => return Err("Priority must be low, normal or high".into()),
            },
            ratio_milli: (ratio * 1000.0).round() as u32,
            seed_minutes: fields[6].parse().map_err(|_| "Invalid seeding minutes")?,
        };
        settings.validate()?;
        self.submit_selected(TorrentCommand::SetTransferSettings(settings))
    }

    fn set_transfer_field(&mut self, argument: &str) -> Result<String, String> {
        let (key, text) = argument.split_once(' ').ok_or("Use :set download_kib|upload_kib|max_peers|sequential|priority|ratio_milli|seed_minutes VALUE")?;
        let torrent = self.selected().ok_or("No selected torrent")?;
        let mut value = serde_json::to_value(
            crate::transfer_settings::TransferSettings::from_item(torrent, &self.settings),
        )
        .map_err(|_| "Could not serialize settings")?;
        let current = value.get_mut(key).ok_or("Unknown transfer setting")?;
        *current = if current.is_string() {
            serde_json::Value::String(text.trim().into())
        } else {
            serde_json::from_str(text.trim()).map_err(|_| "Invalid transfer value")?
        };
        let settings: crate::transfer_settings::TransferSettings =
            serde_json::from_value(value).map_err(|_| "Invalid transfer value or type")?;
        settings.validate()?;
        self.submit_selected(TorrentCommand::SetTransferSettings(settings))
    }

    pub(super) fn commit_preference(&mut self, text: &str) -> Result<String, String> {
        let (tab, key) = self
            .editing_preference
            .as_ref()
            .ok_or("No preference selected")?;
        let next = preferences_editor::edit(
            self.preferences_draft.as_ref().unwrap_or(&self.settings),
            *tab,
            key,
            text,
        )?;
        self.preferences_draft = Some(next);
        Ok(
            "Draft updated; s validates/saves, Esc discards. Active engine settings are unchanged."
                .into(),
        )
    }

    pub(super) fn toggle_preference(&mut self) {
        let draft = self.preferences_draft.as_ref().unwrap_or(&self.settings);
        let fields = preferences_editor::fields(draft, self.preferences_tab);
        let Some((key, value)) = fields.get(self.selected_preference) else {
            return;
        };
        let Some(value) = value.as_bool() else {
            self.edit_preference();
            return;
        };
        match preferences_editor::edit(draft, self.preferences_tab, key, &(!value).to_string()) {
            Ok(next) => {
                self.preferences_draft = Some(next);
                self.status = "Preference draft changed; s validates/saves".into();
            }
            Err(error) => self.status = error,
        }
    }

    pub(super) fn save_preferences_draft(&mut self) {
        self.save_preferences_with(ClientSettings::save);
    }

    pub(super) fn save_preferences_with(
        &mut self,
        save: impl FnOnce(&ClientSettings) -> io::Result<()>,
    ) {
        let mut next = self
            .preferences_draft
            .clone()
            .unwrap_or_else(|| self.settings.clone());
        if let Err(error) = preferences_editor::validate(&next) {
            self.status = error;
            return;
        }
        next.ui.last_preferences_tab = self.preferences_tab.index();
        match save(&next) {
            Ok(()) => {
                self.settings = next;
                self.preferences_draft = None;
                let _ = self.control.set_agent_settings(&self.settings);
                self.status = "Preferences saved; active engine unchanged. See each field's support label before restarting.".into();
            }
            Err(_) => {
                self.status =
                    "Could not save preferences; draft retained and active settings unchanged"
                        .into()
            }
        }
    }

    pub(super) fn render_interaction(&self, frame: &mut Frame<'_>) {
        let lines: Vec<Line<'static>> = if let Some(pending) = &self.confirmation {
            vec![
                Line::from(format!(
                    "Confirm {:?} for {} torrent(s)?",
                    pending.action,
                    pending.targets.len()
                )),
                Line::from(if pending.action == BatchAction::Remove {
                    "Remove from client only. All downloaded files and source .torrent files are kept."
                } else {
                    "Force a full hash check and download missing/corrupt pieces. Paused torrents may resume."
                }),
                Line::from(
                    "Enter/y confirms; Esc/n cancels. Targets are pinned by ID and command revision.",
                ),
            ]
        } else if self.help {
            [
                "Tab/F6 changes focus; arrows move within the focused pane.",
                "Left/right in Details changes its tab; Alt+arrows resizes panes.",
                "/ searches names; f cycles status; Space marks torrents or toggles a file.",
                "* marks visible torrents; Esc clears marks. Hidden rows are not batch targets.",
                "p/r pauses/resumes marked/current rows; P/R applies to ALL, including hidden rows.",
                "v checks; c checks and repairs (confirm); d removes keeping ALL files (confirm).",
                "Preferences: arrows select, Enter edits, Space toggles, s saves, Esc discards.",
                "Input: left/right/Home/End moves, Delete/Backspace edits, Ctrl+U clears (8 KiB max).",
                "F7 edits transfer limits/priority/seeding. :set FIELD VALUE changes one field.",
                "M changes announce mode/bounds; H toggles repair; L edits its attempt limit.",
                "F8: agent access. q/Ctrl+C exits. Never share a profile between running clients.",
                "Palette: pause/resume/check/repair/remove, search TEXT, split 20..80, clear, help.",
            ].into_iter().map(Line::from).collect()
        } else {
            return;
        };
        let content_width = (usize::from(frame.area().width) * 90 / 100)
            .saturating_sub(2)
            .max(1);
        let height = lines
            .iter()
            .map(|line| line.width().div_ceil(content_width).max(1))
            .sum::<usize>()
            .saturating_add(4)
            .min(usize::from(frame.area().height)) as u16;
        let area = centered_rect(90, height, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .title(if self.confirmation.is_some() {
                        " Confirm action "
                    } else {
                        " Help · F1/Esc closes "
                    })
                    .borders(Borders::ALL),
            ),
            area,
        );
    }
}
