//! Main-window wiring: widgets emit intents; this layer freezes identities and
//! dispatches them through the existing native owner queue.
use super::*;
use crate::control::frontend::{ConnectionStatus, ConnectionView};
use desktop_controls::{Decision, DesktopAction as Action, RemovalTarget, ToolbarProps};
use std::sync::Arc;

/// Cache canonical strings only when the authoritative identity layout changes.
/// Idle frames compare borrowed bytes; no full-list join or selected-ID clone is
/// needed for rendering, even when every row is selected.
#[derive(Default)]
pub(super) struct IdentityCache {
    ids: Vec<String>,
    indices: HashMap<String, usize>,
    selected: Vec<String>,
    visible: Option<Arc<Vec<usize>>>,
    targets: Arc<Vec<String>>,
}
pub(super) struct RemovalReview {
    targets: Vec<RemovalTarget>,
    owner: String,
    revisions: HashMap<String, u64>,
}
impl IdentityCache {
    fn refresh(&mut self, torrents: &[TorrentItem]) -> bool {
        if self.ids.len() == torrents.len()
            && self.ids.iter().zip(torrents).all(|(id, torrent)| {
                let mut remaining = id.as_str();
                for (index, hash) in torrent.info_hashes.iter().enumerate() {
                    if index > 0 {
                        let Some(rest) = remaining.strip_prefix(';') else {
                            return false;
                        };
                        remaining = rest;
                    }
                    let Some(rest) = remaining.strip_prefix(hash.as_str()) else {
                        return false;
                    };
                    remaining = rest;
                }
                remaining.is_empty()
            })
        {
            return false;
        }
        self.ids = torrents
            .iter()
            .map(|item| item.info_hashes.join(";"))
            .collect();
        self.indices = self
            .ids
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        self.visible = None;
        true
    }
    fn targets(
        &mut self,
        selection: &crate::torrent_selection::TorrentSelection,
        visible: Arc<Vec<usize>>,
    ) -> Arc<Vec<String>> {
        let same_selection = selection
            .selected_ids()
            .eq(self.selected.iter().map(String::as_str));
        if !same_selection
            || self
                .visible
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, &visible))
        {
            self.selected = selection.selected_ids().map(str::to_owned).collect();
            self.targets = Arc::new(
                visible
                    .iter()
                    .filter_map(|index| self.ids.get(*index))
                    .filter(|id| selection.is_selected(id))
                    .cloned()
                    .collect(),
            );
            self.visible = Some(visible);
        }
        self.targets.clone()
    }
}

impl PeerLumeApp {
    pub(super) fn desktop_connection(&self) -> ConnectionView {
        self.control
            .backend
            .attached_client()
            .map(|client| client.connection())
            .unwrap_or(ConnectionView {
                status: ConnectionStatus::Reconnecting("Background client unavailable".into()),
                owner_instance: None,
                snapshot_serial: 0,
            })
    }

    fn reconcile_selection(&mut self) {
        if self.identities.refresh(&self.control.torrents) {
            self.selection.retain_live(&self.identities.ids);
        }
        if let Some(id) = self.identities.ids.get(self.selected_torrent) {
            self.selection.initialize(id);
        }
        if let Some(index) = self
            .selection
            .focus()
            .and_then(|id| self.control.torrent_index(id))
        {
            self.selected_torrent = index;
        }
    }

    fn desktop_targets(&mut self) -> Arc<Vec<String>> {
        self.identities.refresh(&self.control.torrents);
        let visible = self.presentation_indices();
        self.identities.targets(&self.selection, visible)
    }
    fn toolbar_props(&self, targets: &[String]) -> ToolbarProps {
        desktop_props(
            &self.control.torrents,
            targets
                .iter()
                .filter_map(|id| self.identities.indices.get(id).copied()),
            self.desktop_connection().status == ConnectionStatus::Connected,
            self.selection.len().saturating_sub(targets.len()),
            self.presentation_indices().len(),
        )
    }

    pub(super) fn desktop_error(&mut self, kind: &str, id: Option<&str>, message: String) {
        let name = id
            .and_then(|id| self.control.torrent_index(id))
            .map(|index| self.control.torrents[index].name.as_str())
            .unwrap_or("Application");
        if let Err(error) = self
            .operation_tracker
            .record_error(kind, id, name, &message)
        {
            self.add_status = format!("{message}; operation history: {error:?}");
        } else {
            self.add_status = message;
        }
    }

    fn dispatch_desktop(&mut self, ctx: &egui::Context, action: Action, targets: Vec<String>) {
        match action {
            Action::CloseWindow => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            Action::ShowOperations => {
                self.operations_open = true;
                return;
            }
            Action::Appearance => {
                self.appearance_open = true;
                return;
            }
            Action::AgentAccess => {
                self.open_agent_access = true;
                return;
            }
            Action::ResetColumns => {
                self.settings.ui.table_columns = Default::default();
                self.name_column.reset_width();
                return;
            }
            Action::ClearHiddenSelection => {
                let visible = self.presentation_indices();
                self.selection.clear_hidden(
                    visible
                        .iter()
                        .filter_map(|&index| self.identities.ids.get(index)),
                );
                return;
            }
            _ => {}
        }
        if self.desktop_connection().status != ConnectionStatus::Connected {
            self.desktop_error("Command not sent", None, "Background client is not connected; wait for its current state before changing downloads".into());
            return;
        }
        match action {
            Action::AddTorrent => self.open_torrent_picker(),
            Action::AddMagnet => {
                if self.magnet_open {
                    self.add_magnet();
                } else {
                    self.magnet_open = true;
                }
            }
            Action::PauseSelected | Action::ResumeSelected | Action::RecheckSelected => {
                let command = match action {
                    Action::PauseSelected => TorrentCommand::Pause,
                    Action::ResumeSelected => TorrentCommand::Resume,
                    _ => TorrentCommand::RecheckAndRepair,
                };
                self.submit_desktop_batch(targets, command);
            }
            Action::PauseAll | Action::ResumeAll => {
                let ids = self
                    .control
                    .torrents
                    .iter()
                    .map(|item| item.info_hashes.join(";"))
                    .collect();
                self.submit_desktop_batch(
                    ids,
                    if action == Action::PauseAll {
                        TorrentCommand::Pause
                    } else {
                        TorrentCommand::Resume
                    },
                );
            }
            Action::RemoveSelected => {
                let Some(owner) = self.desktop_connection().owner_instance else {
                    return;
                };
                let mut review = RemovalReview {
                    owner,
                    targets: Vec::new(),
                    revisions: HashMap::new(),
                };
                for id in targets {
                    let Some(index) = self.control.torrent_index(&id) else {
                        continue;
                    };
                    match self.control.command_revision(&id) {
                        Ok(revision) => {
                            review.revisions.insert(id.clone(), revision);
                            review.targets.push(RemovalTarget {
                                id,
                                name: self.control.torrents[index].name.clone(),
                            });
                        }
                        Err(error) => {
                            self.desktop_error("Removal review", Some(&id), error.to_string())
                        }
                    }
                }
                if !review.targets.is_empty() {
                    self.batch_remove = Some(review);
                }
            }
            Action::StopBackground => {
                self.stop_confirmation = self.desktop_connection().owner_instance;
            }
            Action::SaveView => {
                let result = self
                    .control
                    .backend
                    .attached_client()
                    .ok_or("Background client is unavailable".to_owned())
                    .and_then(|client| client.save_view(&self.settings.ui));
                self.desktop_result("Save view", None, result);
            }
            Action::UseExistingData
            | Action::CopyData
            | Action::MoveData
            | Action::RemoveCopyOriginals
            | Action::CancelCopy => {
                let [id] = targets.as_slice() else {
                    self.desktop_error(
                        "Storage command",
                        None,
                        "Select exactly one visible torrent for this storage operation".into(),
                    );
                    return;
                };
                let Some(index) = self.control.torrent_index(id) else {
                    self.desktop_error(
                        "Storage command",
                        Some(id),
                        "The selected torrent no longer exists".into(),
                    );
                    return;
                };
                if action != Action::CancelCopy
                    && (self.control.torrents[index].state != TorrentState::Paused
                        || self.control.torrents[index]
                            .runtime
                            .command
                            .pending_action()
                            .is_some())
                {
                    self.desktop_error(
                        "Storage command",
                        Some(id),
                        "Pause this torrent and wait for confirmation before changing its storage"
                            .into(),
                    );
                    return;
                }
                let result = match action {
                    Action::CancelCopy => self.control.cancel_storage(id),
                    Action::RemoveCopyOriginals => {
                        if rfd::MessageDialog::new().set_title("Permanently remove originals?")
                            .set_description("Only originals from a verified copy in this session can be removed. Every file is compared again before deletion. The destination is retained. Originals are not moved to the Recycle Bin.")
                            .set_buttons(rfd::MessageButtons::YesNo).show() != rfd::MessageDialogResult::Yes { return; }
                        self.control.remove_copy_originals(id)
                    }
                    _ => {
                        let title = match action {
                            Action::UseExistingData => {
                                "Choose existing payload root; hash-check and resume"
                            }
                            Action::CopyData => {
                                "Choose copy destination parent; originals retained"
                            }
                            _ => "Choose parent for an exclusive same-volume move",
                        };
                        let Some(directory) = rfd::FileDialog::new().set_title(title).pick_folder()
                        else {
                            return;
                        };
                        // Resolve again after the picker; never retarget an index.
                        if self.control.torrent_index(id).is_none() {
                            self.desktop_error(
                                "Storage command",
                                Some(id),
                                "Selected torrent disappeared while the picker was open".into(),
                            );
                            return;
                        }
                        if action == Action::MoveData {
                            self.control
                                .move_storage(id, directory.to_string_lossy().into_owned())
                        } else {
                            self.control.select_storage(
                                id,
                                directory.to_string_lossy().into_owned(),
                                action == Action::CopyData,
                            )
                        }
                    }
                };
                self.desktop_result("Storage command", Some(id), result);
            }
            _ => {}
        }
    }
    fn desktop_result(&mut self, kind: &str, id: Option<&str>, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.add_status = "Request queued; inspect Operations for confirmation".into()
            }
            Err(error) => self.desktop_error(kind, id, error),
        }
    }
    fn submit_desktop_batch(&mut self, ids: Vec<String>, command: TorrentCommand) {
        self.submit_desktop_batch_fenced(ids, command, None);
    }
    fn confirm_desktop_removal(&mut self, review: RemovalReview) {
        let connection = self.desktop_connection();
        if connection.status != ConnectionStatus::Connected
            || connection.owner_instance.as_deref() != Some(review.owner.as_str())
        {
            self.desktop_error("Removal not sent",None,"Background owner changed or disconnected; review removal again. No requests were sent".into());
            return;
        }
        self.submit_desktop_batch_fenced(
            review.targets.into_iter().map(|target| target.id).collect(),
            TorrentCommand::Remove,
            Some(&review.revisions),
        );
    }
    fn submit_desktop_batch_fenced(
        &mut self,
        ids: Vec<String>,
        command: TorrentCommand,
        expected: Option<&HashMap<String, u64>>,
    ) {
        let mut sent = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for id in ids {
            let Some(index) = self.control.torrent_index(&id) else {
                failed += 1;
                self.desktop_error(
                    "Batch command",
                    Some(&id),
                    "The selected torrent no longer exists; request was not retargeted".into(),
                );
                continue;
            };
            if matches!(&command, TorrentCommand::Pause | TorrentCommand::Resume)
                && !matches!(transfer_feedback::primary_control(&self.control.torrents[index]).1,Some(HeaderAction::Transfer(offered)) if offered==command)
            {
                skipped += 1;
                continue;
            }
            let result = self.control.command_revision(&id).map_err(|error|error.to_string()).and_then(|revision|{
                    if expected.is_some_and(|expected|expected.get(&id)!=Some(&revision)) {
                        return Err("Torrent changed after removal review; review it again. Request was not sent".into());
                    }
                    self.control.backend.attached_client().ok_or("Background client unavailable".to_owned()).and_then(|client|client.enqueue_submit(&id,command.clone(),Some(revision)))
                });
            match result {
                Ok(()) => {
                    sent += 1;
                    if let Some(action) = command.feedback_action() {
                        self.control.torrents[index].runtime.command.begin(action);
                    }
                }
                Err(error) => {
                    failed += 1;
                    self.desktop_error("Batch command", Some(&id), error);
                }
            }
        }
        self.add_status = format!(
            "{sent} request(s) queued, {failed} rejected, {skipped} skipped; awaiting owner and engine confirmation"
        );
    }

    pub(super) fn show_desktop_torrents(&mut self, ctx: &egui::Context) {
        self.reconcile_selection();
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ctx,|ui|{
            let bounds=ui.available_rect_before_wrap();
            let targets=self.desktop_targets();
            let props=self.toolbar_props(&targets);
            actions.extend(desktop_controls::toolbar(ui,&mut self.settings.ui.search,props).into_iter().map(|action|(action,targets.as_ref().clone())));
            ui.horizontal_wrapped(|ui|{
                ui.add(egui::TextEdit::singleline(&mut self.settings.ui.category_filter).hint_text("Category (empty = all)").desired_width(160.0));
                egui::ComboBox::from_id_salt("torrent_sort").selected_text(self.settings.ui.sort.label()).show_ui(ui,|ui|{
                    for sort in crate::torrent_view::Sort::ALL {ui.selectable_value(&mut self.settings.ui.sort,sort,sort.label());}
                });
                ui.checkbox(&mut self.settings.ui.descending,"Descending");
            });
            if !self.add_status.is_empty() { ui.add(egui::Label::new(&self.add_status).truncate()).on_hover_text(&self.add_status); }
            let visible=self.presentation_indices();
            let panes_rect=egui::Rect::from_min_max(egui::pos2(bounds.left(),ui.cursor().top().min(bounds.bottom())),bounds.max);
            let mut reveal=None;
            let mut directory=None;
            let mut trash=None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(panes_rect),|ui|{
                let feedback_height=self.selection.focus().and_then(|id|self.control.torrent_index(id)).map(|index|transfer_feedback::extra_details_height(&self.control.torrents[index])).unwrap_or(0.0);
                let connected=self.desktop_connection().status==ConnectionStatus::Connected;
                let torrents=&self.control.torrents;
                let identities=&self.identities.indices;
                let pane=torrent_panes::table_with_percent(ui,self.settings.ui.torrent_pane_percent,feedback_height,|ui|{
                    torrent_table::show(ui,torrent_table::TableInput{
                        torrents,visible:&visible,filter:self.status_filter,
                        columns:&mut self.settings.ui.table_columns,sort:&mut self.settings.ui.sort,descending:&mut self.settings.ui.descending,
                        name_column:&mut self.name_column,selection:&mut self.selection,
                    },|ui,_,torrent,ids,hidden|{
                        let props=desktop_props(torrents,ids.iter().filter_map(|id|identities.get(id).copied()),connected,hidden,visible.len());
                        if ui.add_enabled(connected&&props.can_pause,egui::Button::new("Pause selected")).clicked(){actions.push((Action::PauseSelected,ids.to_vec()));ui.close();}
                        if ui.add_enabled(connected&&props.can_resume,egui::Button::new("Resume selected")).clicked(){actions.push((Action::ResumeSelected,ids.to_vec()));ui.close();}
                        actions.extend(desktop_controls::more_actions(ui,props).into_iter().map(|action|(action,ids.to_vec())));
                        ui.separator();
                        if ui.add_enabled(ids.len()==1,egui::Button::new("Open download folder")).clicked(){
                            directory=Some(torrent.info_hashes.join(";"));ui.close();
                        }
                        if ui.add_enabled(ids.len()==1,egui::Button::new("Open .torrent location")).clicked(){reveal=Some(PathBuf::from(&torrent.source));ui.close();}
                        for (label,scope) in [
                            ("Remove torrent and recycle downloaded files",TrashScope::DownloadedFiles),
                            ("Recycle source .torrent file",TrashScope::TorrentFile),
                            ("Remove torrent and recycle source and data",TrashScope::Everything),
                        ] {
                            if ui.add_enabled(connected&&ids.len()==1,egui::Button::new(label)).on_hover_text("Review the owner's exact per-torrent plan before recycling. For a group use Remove selected (keep data).").clicked(){trash=Some((torrent.info_hashes.join(";"),scope));ui.close();}
                        }
                    })
                });
                if let Some(index)=pane.inner.focused_index {self.selected_torrent=index;}
                if pane.inner.view_changed && self.add_status.is_empty(){self.add_status="View layout changed; More > Save view to keep it".into();}
                debug_assert!(pane.inner.rendered_rows<=visible.len());
                self.settings.ui.torrent_pane_percent=(pane.response.rect.height()/panes_rect.height().max(1.0)*100.0).round().clamp(10.0,80.0) as u16;
                self.settings.ui.name_gap_pixels=self.name_column.saved_gap();
                ui.add_space(4.0);
                torrent_panes::details(ui,|ui|{
                    if self.selection.focus().is_some_and(|id| self.control.torrent_index(id).is_some_and(|index|visible.contains(&index))){self.show_selected_torrent(ui);}
                    else {ui.label("Select a visible torrent to inspect its files and details.");}
                });
            });
            if let Some(path)=reveal {self.add_status=self.file_navigation.reveal_source(&path);}
            if let Some(id)=directory {
                let path=self.control.torrent_index(&id).map(|index|PathBuf::from(EffectiveTorrentSettings::resolve(&self.settings,&self.control.torrents[index].overrides,self.control.torrents[index].is_private).download_directory));
                if let Some(path)=path {self.record_directory_result(&id,open_directory(&path));}}
            if let Some((id,scope))=trash {self.prepare_trash_request(&id,scope);}
        });
        for (action, targets) in actions {
            self.dispatch_desktop(ctx, action, targets);
        }
    }

    fn record_directory_result(&mut self, id: &str, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => self.add_status = format!("Opened {}", path.display()),
            Err(error) => self.desktop_error(
                "Open download folder",
                Some(id),
                format!("Could not open folder: {error}"),
            ),
        }
    }

    pub(super) fn show_desktop_footer(&mut self, ctx: &egui::Context) {
        let connection = self.desktop_connection();
        let text = match &connection.status {
            ConnectionStatus::Connecting => "Connecting to background client".to_owned(),
            ConnectionStatus::Connected => format!(
                "Connected · {}",
                connection
                    .owner_instance
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>()
            ),
            ConnectionStatus::Reconnecting(error) => {
                format!("Reconnecting — displayed transfer state may be stale: {error}")
            }
            ConnectionStatus::StopRequested => {
                "Stopping background client; awaiting profile lock release".into()
            }
            ConnectionStatus::Stopped => {
                "Background client stopped · close and reopen to start it again".into()
            }
        };
        let (down, up) = if connection.status == ConnectionStatus::Connected {
            self.control
                .torrents
                .iter()
                .fold((0u64, 0u64), |(down, up), item| {
                    (
                        down.saturating_add(item.download_rate),
                        up.saturating_add(item.upload_rate),
                    )
                })
        } else {
            (0, 0)
        };
        let mut action = None;
        egui::TopBottomPanel::bottom("desktop_footer").show(ctx, |ui| {
            action = desktop_controls::footer(
                ui,
                desktop_controls::FooterProps {
                    connection: &text,
                    download_rate: down,
                    upload_rate: up,
                    connected: connection.status == ConnectionStatus::Connected,
                    can_stop: connection.owner_instance.is_some(),
                    operation_count: self.operation_tracker.history.len(),
                },
            );
        });
        if let Some(action) = action {
            self.dispatch_desktop(ctx, action, Vec::new());
        }
    }

    pub(super) fn show_desktop_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(review) = self.batch_remove.as_ref() {
            let mut open = true;
            let decision = desktop_controls::remove_confirmation(ctx, &review.targets, &mut open);
            if decision.is_some() {
                let review = self.batch_remove.take().expect("review exists");
                if decision == Some(Decision::Confirm) {
                    self.confirm_desktop_removal(review);
                }
            }
        } else if let Some(owner) = self.stop_confirmation.clone() {
            let mut open = true;
            let decision = desktop_controls::stop_confirmation(ctx, &owner, &mut open);
            if decision.is_some() {
                self.stop_confirmation = None;
                if decision == Some(Decision::Confirm) {
                    let result = self
                        .control
                        .backend
                        .attached_client()
                        .ok_or("Background client unavailable".to_owned())
                        .and_then(|client| client.enqueue_stop_owner(&owner).map(|_| ()));
                    self.desktop_result("Stop background client", None, result);
                }
            }
        } else if self.appearance_open {
            if let Some(action) = desktop_controls::appearance_dialog(
                ctx,
                &mut self.settings.ui.appearance,
                &mut self.appearance_open,
            ) {
                self.dispatch_desktop(ctx, action, Vec::new());
            }
        } else if self.magnet_open {
            let connected = self.desktop_connection().status == ConnectionStatus::Connected;
            if let Some(action) = desktop_controls::magnet_dialog(
                ctx,
                &mut self.magnet_input,
                &mut self.magnet_open,
                connected,
            ) {
                // The modal closes before returning AddMagnet; execute the import,
                // rather than treating the action as a request to reopen it.
                if action == Action::AddMagnet && connected {
                    self.add_magnet();
                }
            }
        }
        if self.operations_open {
            let mut open = self.operations_open;
            let mut intents = Vec::new();
            egui::Window::new("Operations")
                .open(&mut open)
                .default_width(620.0)
                .show(ctx, |ui| {
                    intents = crate::ui_operations::gui::panel(
                        ui,
                        &self.operation_tracker.history,
                        (ctx.content_rect().height() * 0.65).max(80.0),
                    );
                });
            self.operations_open = open;
            for intent in intents {
                use crate::ui_operations::HistoryIntent;
                match intent {
                    HistoryIntent::FocusTorrent(id) => {
                        if let Some(index) = self.control.torrent_index(&id) {
                            self.selection.replace(&id);
                            self.selected_torrent = index;
                            self.page = MainPage::Torrents;
                            self.status_filter = Default::default();
                            self.settings.ui.search.clear();
                            self.settings.ui.category_filter.clear();
                        }
                    }
                    HistoryIntent::CopyError(error) => ctx.copy_text(error),
                    HistoryIntent::Dismiss(id) => {
                        let _ = self.operation_tracker.history.dismiss(id);
                    }
                    HistoryIntent::DismissFinished => {
                        self.operation_tracker.history.clear_finished();
                    }
                    HistoryIntent::AcknowledgeReview(id) => {
                        let _ = self.operation_tracker.history.acknowledge_review(id);
                    }
                }
            }
        }
    }
}

fn desktop_props(
    torrents: &[TorrentItem],
    indices: impl IntoIterator<Item = usize>,
    connected: bool,
    hidden: usize,
    visible: usize,
) -> ToolbarProps {
    let mut props = ToolbarProps {
        connected,
        hidden_selected: hidden,
        visible_torrents: visible,
        total_torrents: torrents.len(),
        ..Default::default()
    };
    for item in indices.into_iter().filter_map(|index| torrents.get(index)) {
        props.visible_selected += 1;
        match transfer_feedback::primary_control(item).1 {
            Some(HeaderAction::Transfer(TorrentCommand::Pause)) => props.can_pause = true,
            Some(HeaderAction::Transfer(TorrentCommand::Resume)) => props.can_resume = true,
            _ => {}
        }
        props.one_paused = props.visible_selected == 1
            && item.state == TorrentState::Paused
            && item.runtime.command.pending_action().is_none();
        props.can_cancel_copy = props.visible_selected == 1
            && matches!(
                item.runtime.storage,
                crate::storage::relocation::StorageStatus::Copying { .. }
            );
    }
    props
}

#[cfg(test)]
mod tests;
