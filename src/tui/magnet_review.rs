//! Terminal review of a resolved magnet before any torrent is registered.
use super::*;
use crate::control::frontend::{
    Client, MagnetReview, MagnetReviewTask, MagnetReviewToken, MagnetSelection, Receipt,
};

#[derive(Default)]
pub(super) struct Panel {
    lookup: Option<MagnetReviewTask>,
    draft: Option<Draft>,
}

struct Draft {
    token: MagnetReviewToken,
    name: String,
    private: bool,
    files: FileSelectionModel,
    selected_row: usize,
    directory: String,
    start: bool,
    submission: Submission,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Submission {
    Editing,
    Pending(u64),
    Uncertain,
}

impl Panel {
    pub(super) fn is_open(&self) -> bool {
        self.lookup.is_some() || self.draft.is_some()
    }

    pub(super) fn begin(&mut self, client: &Client, uri: String) -> Result<(), String> {
        if self.is_open() {
            return Err("A magnet review is already open".into());
        }
        self.lookup = Some(MagnetReviewTask::start(client, uri)?);
        Ok(())
    }

    pub(super) fn poll(&mut self, settings: &ClientSettings) -> Option<String> {
        let result = self.lookup.as_ref()?.poll()?;
        self.lookup = None;
        match result {
            Ok(MagnetReview { token, metadata }) => {
                let mut files = FileSelectionModel::from_metainfo_files(metadata.files);
                files.select_all(settings.download.select_all_files);
                self.draft = Some(Draft {
                    token,
                    name: metadata.name,
                    private: metadata.is_private,
                    files,
                    selected_row: 0,
                    directory: settings.download.download_directory.clone(),
                    start: settings.download.start_when_added,
                    submission: Submission::Editing,
                });
                Some("Review the files and destination before confirming".into())
            }
            Err(error) => Some(error),
        }
    }

    pub(super) fn set_directory(&mut self, value: String) -> Result<String, String> {
        let draft = self.draft.as_mut().ok_or("Magnet review is closed")?;
        let directory = value.trim();
        if !Path::new(directory).is_absolute() {
            return Err("Enter an absolute download directory".into());
        }
        draft.directory = directory.to_owned();
        Ok("Download directory updated for this review".into())
    }

    pub(super) fn apply_receipt(&mut self, receipt: &Receipt) {
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        if receipt.queued
            || receipt.label != "Magnet registration"
            || draft.submission != Submission::Pending(receipt.job_id)
        {
            return;
        }
        if receipt.error.is_none() {
            draft.token.confirmed();
            self.draft = None;
        } else {
            draft.submission = if receipt.ambiguous_error {
                Submission::Uncertain
            } else {
                Submission::Editing
            };
        }
    }

    pub(super) fn mark_receipt_loss(&mut self) -> bool {
        if let Some(draft) = self.draft.as_mut()
            && matches!(draft.submission, Submission::Pending(_))
        {
            draft.submission = Submission::Uncertain;
            true
        } else {
            false
        }
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        directory_prompt: Option<&InputPrompt>,
        status: &str,
    ) {
        let block = Block::default()
            .title(" Magnet review ")
            .borders(Borders::ALL);
        let inner = block.inner(frame.area());
        frame.render_widget(block, frame.area());
        let Some(draft) = &self.draft else {
            frame.render_widget(Paragraph::new(vec![
                Line::from("Resolving magnet metadata. Payload transfer starts after confirmation."),
                Line::from("Trackerless review uses DHT when enabled and shares the hash with that network."),
                Line::from("Esc cancels"),
            ]).wrap(Wrap { trim: true }), inner);
            return;
        };
        let areas = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(inner);
        let start = if draft.start {
            "Start after adding"
        } else {
            "Add paused"
        };
        let kind = if draft.private {
            "Private torrent"
        } else {
            "Public torrent"
        };
        let directory_line = if let Some(prompt) = directory_prompt {
            let (text, column) = prompt.window(false, usize::from(areas[0].width));
            if areas[0].height >= 3 && areas[0].width > 0 {
                frame.set_cursor_position((
                    areas[0].x + (column as u16).min(areas[0].width - 1),
                    areas[0].y + 2,
                ));
            }
            text
        } else {
            draft.directory.clone()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("{} · {kind}", draft.name)),
                Line::from(format!(
                    "{} of {} files · {} selected · {start}",
                    draft.files.selected_count(),
                    draft.files.file_count(),
                    format_size(draft.files.selected_size())
                )),
                Line::from(directory_line),
                Line::from(if directory_prompt.is_some() {
                    "Enter sets the absolute path · Esc returns"
                } else {
                    "Download directory shown above"
                }),
            ]),
            areas[0],
        );
        let limit = usize::from(areas[1].height).max(1);
        let offset = (draft.selected_row + 1).saturating_sub(limit);
        let rows = draft
            .files
            .files()
            .iter()
            .skip(offset)
            .take(limit)
            .map(|file| {
                ListItem::new(format!(
                    "{} {} · {}",
                    if file.selected { "[x]" } else { "[ ]" },
                    file.path,
                    format_size(file.size_bytes)
                ))
            });
        let mut list = ListState::default().with_selected(Some(draft.selected_row - offset));
        frame.render_stateful_widget(
            List::new(rows).highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            areas[1],
            &mut list,
        );
        let help = match draft.submission {
            Submission::Editing => {
                "↑↓ move · Space toggle · a all · n none · d directory · s start policy · Enter confirm · Esc cancel"
            }
            Submission::Pending(_) => {
                "Waiting for the background client to accept the reviewed torrent"
            }
            Submission::Uncertain => {
                "Review the torrent list before retrying · Esc closes this review"
            }
        };
        frame.render_widget(
            Paragraph::new(vec![Line::from(help), Line::from(status)]).wrap(Wrap { trim: true }),
            areas[2],
        );
    }
}

impl TuiApp {
    pub(super) fn handle_magnet_review_key(&mut self, key: KeyCode) {
        let Some(draft) = self.magnet_review.draft.as_mut() else {
            if key == KeyCode::Esc {
                self.magnet_review.lookup = None;
                self.status = "Magnet review cancelled".into();
            }
            return;
        };
        if key == KeyCode::Esc && !matches!(draft.submission, Submission::Pending(_)) {
            self.magnet_review.draft = None;
            self.status = "Magnet review closed".into();
            return;
        }
        if draft.submission != Submission::Editing {
            return;
        }
        match key {
            KeyCode::Up => draft.selected_row = draft.selected_row.saturating_sub(1),
            KeyCode::Down => {
                draft.selected_row = move_index(draft.selected_row, draft.files.file_count(), 1)
            }
            KeyCode::Home => draft.selected_row = 0,
            KeyCode::End => draft.selected_row = draft.files.file_count().saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(file) = draft.files.file_state(draft.selected_row) {
                    draft.files.set_selected(draft.selected_row, !file.selected);
                }
            }
            KeyCode::Char('a') => draft.files.select_all(true),
            KeyCode::Char('n') => draft.files.select_all(false),
            KeyCode::Char('s') => draft.start = !draft.start,
            KeyCode::Char('d') => {
                let directory = draft.directory.clone();
                self.open_prompt(
                    PromptKind::MagnetDirectory,
                    "Absolute download directory",
                    directory,
                );
            }
            KeyCode::Enter => {
                let selection = MagnetSelection {
                    files: &draft.files,
                    peer_id: metainfo::generate_peer_id(&self.settings.bittorrent.peer_id_prefix),
                    overrides: TorrentOverrides {
                        download_directory: Some(draft.directory.clone()),
                        ..Default::default()
                    },
                    start: draft.start,
                    audit: None,
                };
                self.status = match draft.token.confirm(selection) {
                    Ok(job) => {
                        draft.submission = Submission::Pending(job);
                        "Awaiting background registration".into()
                    }
                    Err(error) => error,
                };
            }
            _ => {}
        }
    }
}
