//! Paged operation history from the background owner.
use super::*;
use crate::control::frontend::{Client, ConnectionStatus, NativeOperationState, OperationView};

#[derive(Default)]
pub(super) struct Panel {
    open: bool,
    before: Option<u64>,
    newer: Vec<Option<u64>>,
    view: Option<OperationView>,
    current: bool,
    selection: ListState,
    status: &'static str,
}

impl Panel {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self) {
        *self = Self {
            open: true,
            ..Default::default()
        };
    }

    pub(super) fn poll(&mut self, client: Option<&Client>) -> bool {
        let Some(client) = client else {
            return false;
        };
        client.set_operations_interest(self.open, self.before);
        if !self.open {
            return false;
        }
        let connection = client.connection();
        let view = client.operation_view();
        let current = view.as_ref().is_some_and(|view| {
            !view.stale
                && connection.status == ConnectionStatus::Connected
                && connection.owner_instance.as_deref() == Some(view.owner_instance.as_str())
        });
        if self.view == view && self.current == current {
            return false;
        }
        let selected = self
            .view
            .as_ref()
            .and_then(|view| view.page.entries.get(self.selection.selected()?))
            .map(|entry| entry.id.as_str());
        let next_selection = view.as_ref().and_then(|view| {
            let entries = &view.page.entries;
            selected
                .and_then(|id| entries.iter().position(|entry| entry.id == id))
                .or_else(|| (!entries.is_empty()).then_some(0))
        });
        self.selection.select(next_selection);
        self.view = view;
        self.current = current;
        true
    }

    fn change_page(&mut self, before: Option<u64>) {
        self.before = before;
        self.view = None;
        self.current = false;
        self.selection.select(None);
        self.status = "";
    }

    fn key(&mut self, key: KeyCode) -> Option<&str> {
        match key {
            KeyCode::Esc | KeyCode::Char('3') => self.open = false,
            KeyCode::PageUp => {
                if let Some(before) = self.newer.pop() {
                    self.change_page(before);
                }
            }
            KeyCode::PageDown if self.current && self.newer.len() < 16 => {
                if let Some(before) = self.view.as_ref().and_then(|view| view.page.next_before) {
                    self.newer.push(self.before);
                    self.change_page(Some(before));
                }
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(view) = &self.view {
                    let direction = if key == KeyCode::Up { -1 } else { 1 };
                    if !view.page.entries.is_empty() {
                        self.selection.select(Some(move_index(
                            self.selection.selected().unwrap_or_default(),
                            view.page.entries.len(),
                            direction,
                        )));
                        self.status = "";
                    }
                }
            }
            KeyCode::Enter if self.current => {
                let view = self.view.as_ref()?;
                let entry = view.page.entries.get(self.selection.selected()?)?;
                if entry.owner_instance == view.owner_instance {
                    return entry.target.as_deref();
                }
                self.status = "This operation belongs to an earlier owner.";
            }
            _ => {}
        }
        None
    }

    pub(super) fn render(&mut self, frame: &mut Frame<'_>) {
        let block = Block::default()
            .title(" Background operations ")
            .borders(Borders::ALL);
        let inner = block.inner(frame.area());
        frame.render_widget(block, frame.area());
        let areas = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);
        let heading = match &self.view {
            Some(view) if view.stale && view.page.entries.is_empty() => {
                "Owner history unavailable".into()
            }
            Some(view) if !self.current => {
                format!(
                    "Last known owner data · page {} · {} entries",
                    self.newer.len() + 1,
                    view.page.entries.len()
                )
            }
            Some(view) => format!(
                "Owner history · page {} · {} entries",
                self.newer.len() + 1,
                view.page.entries.len()
            ),
            None => "Waiting for the background client's history".into(),
        };
        frame.render_widget(Paragraph::new(heading).wrap(Wrap { trim: true }), areas[0]);
        if let Some(view) = &self.view {
            if view.page.entries.is_empty() && !view.stale {
                frame.render_widget(
                    Paragraph::new("This page has no retained operations."),
                    areas[1],
                );
            } else if !view.page.entries.is_empty() {
                let rows = view.page.entries.iter().map(|entry| {
                    let phase = match entry.state {
                        NativeOperationState::Queued => "Queued at owner",
                        NativeOperationState::Accepted => "Accepted",
                        NativeOperationState::Running => "Running",
                        NativeOperationState::EffectConfirmed => "Effect confirmed",
                        NativeOperationState::Succeeded => "Succeeded",
                        NativeOperationState::Failed => "Failed",
                        NativeOperationState::NeedsReview => "Needs review",
                    };
                    let progress = entry
                        .progress
                        .as_ref()
                        .map_or_else(String::new, |progress| {
                            progress.total.map_or_else(
                                || format!(" · {} B", progress.bytes),
                                |total| format!(" · {} / {total} B", progress.bytes),
                            )
                        });
                    let mut lines = vec![Line::from(format!(
                        "#{}  {}  ·  {phase}{progress}",
                        entry.sequence, entry.command
                    ))];
                    if let Some(message) = &entry.message {
                        lines.push(Line::from(message.clone()));
                    }
                    ListItem::new(lines)
                });
                frame.render_stateful_widget(
                    List::new(rows).highlight_style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    areas[1],
                    &mut self.selection,
                );
            }
        }
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("↑↓ select · PgUp newer · PgDn older · Enter focus torrent · Esc close"),
                Line::from(self.status),
            ]),
            areas[2],
        );
    }
}

impl TuiApp {
    pub(super) fn handle_operations_key(&mut self, key: KeyCode) {
        if key == KeyCode::Char('q') {
            self.running = false;
            return;
        }
        let Some(id) = self.operations.key(key).map(str::to_owned) else {
            return;
        };
        let Some(index) = self
            .visible_indices()
            .iter()
            .copied()
            .find(|index| self.control.torrents[*index].info_hashes.join(";") == id)
        else {
            self.operations.status =
                "Torrent absent from the current view. Check the active filters.";
            return;
        };
        self.selected_torrent = index;
        self.selected_file = 0;
        self.page = MainPage::Torrents;
        self.focus = Focus::Torrents;
        self.show_tracker_identity = false;
        self.operations.open = false;
    }
}
