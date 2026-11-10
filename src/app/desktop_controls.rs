//! Responsive desktop presentation. Every action is an intent for the owner;
//! these widgets never send IPC, open pickers or write settings themselves.
use crate::appearance::{self, AppearanceSettings};
use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DesktopAction {
    AddTorrent,
    AddMagnet,
    PauseSelected,
    ResumeSelected,
    RemoveSelected,
    RecheckSelected,
    PauseAll,
    ResumeAll,
    UseExistingData,
    CopyData,
    MoveData,
    RemoveCopyOriginals,
    CancelCopy,
    SaveView,
    ResetColumns,
    Appearance,
    SpeedProfiles,
    AgentAccess,
    ShowOperations,
    ClearHiddenSelection,
    CloseWindow,
    StopBackground,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ToolbarProps {
    pub connected: bool,
    pub visible_selected: usize,
    pub hidden_selected: usize,
    pub visible_torrents: usize,
    pub total_torrents: usize,
    pub can_pause: bool,
    pub can_resume: bool,
    pub one_paused: bool,
    pub can_cancel_copy: bool,
}

fn action_button(
    ui: &mut egui::Ui,
    actions: &mut Vec<DesktopAction>,
    action: DesktopAction,
    label: &str,
    enabled: bool,
    explanation: &str,
) {
    if ui
        .add_enabled(enabled, egui::Button::new(label).wrap())
        .on_hover_text(explanation)
        .on_disabled_hover_text(explanation)
        .clicked()
    {
        actions.push(action);
    }
}

pub(super) fn toolbar(
    ui: &mut egui::Ui,
    search: &mut String,
    props: ToolbarProps,
) -> Vec<DesktopAction> {
    let mut actions = Vec::new();
    let width = ui.available_width().max(1.0);
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.horizontal_wrapped(|ui| {
            action_button(
                ui,
                &mut actions,
                DesktopAction::AddTorrent,
                "+ Add torrent",
                props.connected,
                "Choose a .torrent file. The background client must be connected.",
            );
            action_button(
                ui,
                &mut actions,
                DesktopAction::AddMagnet,
                "Add magnet",
                props.connected,
                "Paste a magnet link in a separate dialog.",
            );
            action_button(
                ui,
                &mut actions,
                DesktopAction::PauseSelected,
                "Pause",
                props.connected && props.visible_selected > 0 && props.can_pause,
                "Pause selected visible torrents only. Hidden selected rows are excluded.",
            );
            action_button(
                ui,
                &mut actions,
                DesktopAction::ResumeSelected,
                "Resume",
                props.connected && props.visible_selected > 0 && props.can_resume,
                "Resume selected visible torrents only. Hidden selected rows are excluded.",
            );
            ui.menu_button("More", |ui| {
                ui.set_max_width(width.min(340.0));
                egui::ScrollArea::vertical()
                    .id_salt("desktop_more_scroll")
                    .max_height((ui.ctx().content_rect().height() - 64.0).max(48.0))
                    .show(ui, |ui| actions.extend(more_actions(ui, props)));
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(search)
                    .desired_width(width.min(300.0))
                    .hint_text("Search name, category or labels"),
            );
            ui.label(format!(
                "{} selected visible / {} hidden · {} shown / {} total",
                props.visible_selected,
                props.hidden_selected,
                props.visible_torrents,
                props.total_torrents
            ));
        });
    });
    actions
}

/// Shared secondary intents for command bar and context-menu integration.
pub(super) fn more_actions(ui: &mut egui::Ui, props: ToolbarProps) -> Vec<DesktopAction> {
    let mut actions = Vec::new();
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    ui.label(format!(
        "Selected visible: {} · hidden selected: {}",
        props.visible_selected, props.hidden_selected
    ));
    let selected = props.connected && props.visible_selected > 0;
    action_button(
        ui,
        &mut actions,
        DesktopAction::RemoveSelected,
        "Remove selected (keep data)",
        selected,
        "Review the frozen selected-visible list before removal. Downloaded files and source .torrent files are kept.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::RecheckSelected,
        "Force recheck and repair",
        selected,
        "Verify selected visible torrents, then download missing or damaged pieces through the normal engine.",
    );
    ui.separator();
    let storage = props.connected && props.one_paused && props.visible_selected == 1;
    let storage_help = "Select exactly one visible paused torrent. Existing data is hash-checked; verified copy creates a new subdirectory and keeps originals.";
    action_button(
        ui,
        &mut actions,
        DesktopAction::UseExistingData,
        "Use existing data...",
        storage,
        storage_help,
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::CopyData,
        "Copy data to...",
        storage,
        storage_help,
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::MoveData,
        "Fast move directory...",
        storage,
        "Pause one torrent first. Only an exclusive complete directory on the same volume can be renamed; other cases require verified copy.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::RemoveCopyOriginals,
        "Remove duplicate originals...",
        storage,
        "Only verified originals from a copy in this session can be removed. Each file is checked again and an explicit confirmation is required.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::CancelCopy,
        "Cancel copy",
        props.connected && props.can_cancel_copy && props.visible_selected == 1,
        "Cancel the current copy; originals remain intact.",
    );
    ui.separator();
    ui.label(format!(
        "All torrents: {} (including hidden rows)",
        props.total_torrents
    ));
    action_button(
        ui,
        &mut actions,
        DesktopAction::PauseAll,
        "Pause all",
        props.connected && props.total_torrents > 0,
        "Applies to all torrents, including hidden rows.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::ResumeAll,
        "Resume all",
        props.connected && props.total_torrents > 0,
        "Applies to all torrents, including hidden rows.",
    );
    ui.separator();
    action_button(
        ui,
        &mut actions,
        DesktopAction::ClearHiddenSelection,
        "Clear hidden selection",
        props.hidden_selected > 0,
        "Keep visible selected rows and clear only selected identities hidden by the current view.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::SaveView,
        "Save view",
        props.connected,
        "Save UI search, filters, columns, split and appearance without saving preference drafts.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::ResetColumns,
        "Restore default columns",
        true,
        "Restore column order, visibility and automatic widths in this view.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::Appearance,
        "Appearance...",
        true,
        "Theme, density and text zoom. Save through the UI-only settings path.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::SpeedProfiles,
        "Speed profiles...",
        true,
        "View the active limits and switch the background client's speed profile.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::AgentAccess,
        "Agent access...",
        props.connected,
        "Review or change access using the owner's existing confirmation flow.",
    );
    action_button(
        ui,
        &mut actions,
        DesktopAction::ShowOperations,
        "Operations",
        true,
        "Review queued, accepted, completed and failed operations.",
    );
    if !actions.is_empty() {
        ui.close();
    }
    actions
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FooterProps<'a> {
    pub connection: &'a str,
    pub download_rate: u64,
    pub upload_rate: u64,
    pub connected: bool,
    pub can_stop: bool,
    pub operation_count: usize,
}

pub(super) fn footer(ui: &mut egui::Ui, props: FooterProps<'_>) -> Option<DesktopAction> {
    let mut actions = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(props.connection).wrap());
        ui.label(format!(
            "↓ {} · ↑ {}",
            super::format_rate(props.download_rate),
            super::format_rate(props.upload_rate)
        ));
        action_button(
            ui,
            &mut actions,
            DesktopAction::ShowOperations,
            &format!("Operations ({})", props.operation_count),
            true,
            "Inspect operation progress and errors.",
        );
        action_button(
            ui,
            &mut actions,
            DesktopAction::CloseWindow,
            "Close window",
            true,
            "Close this window while the background client continues transfers.",
        );
        action_button(
            ui,
            &mut actions,
            DesktopAction::StopBackground,
            "Stop background client...",
            props.connected && props.can_stop,
            "Requires confirmation. Stops transfers for every window attached to this profile.",
        );
    });
    actions.into_iter().next()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Cancel,
    Confirm,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RemovalTarget {
    pub id: String,
    pub name: String,
}

fn modal_width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 40.0).clamp(64.0, 480.0)
}
fn finish_decision(open: &mut bool, decision: Option<Decision>, close: bool) -> Option<Decision> {
    let decision = decision.or_else(|| close.then_some(Decision::Cancel));
    if decision.is_some() {
        *open = false;
    }
    decision
}

pub(super) fn remove_confirmation(
    ctx: &egui::Context,
    targets: &[RemovalTarget],
    open: &mut bool,
) -> Option<Decision> {
    if !*open {
        return None;
    }
    let mut decision = None;
    let modal = egui::Modal::new(egui::Id::new("desktop_remove_confirmation")).show(ctx, |ui| {
        ui.set_width(modal_width(ctx));
        ui.heading("Remove selected torrents?");
        ui.label(format!("{} selected visible torrent(s) will leave the list.", targets.len()));
        ui.label("Downloaded files and source .torrent files will be kept. Hidden selected torrents are excluded.");
        egui::ScrollArea::vertical().max_height((ctx.content_rect().height()*0.35).max(40.0)).show(ui,|ui|{
            for target in targets { ui.add(egui::Label::new(&target.name).wrap()).on_hover_text(&target.id); }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!targets.is_empty(),egui::Button::new("Remove from list").wrap()).clicked(){ decision=Some(Decision::Confirm); }
            if ui.button("Cancel").clicked(){decision=Some(Decision::Cancel);}
        });
    });
    finish_decision(open, decision, modal.should_close())
}

pub(super) fn stop_confirmation(
    ctx: &egui::Context,
    owner: &str,
    open: &mut bool,
) -> Option<Decision> {
    if !*open {
        return None;
    }
    let mut decision = None;
    let modal=egui::Modal::new(egui::Id::new("desktop_stop_confirmation")).show(ctx,|ui|{
        ui.set_width(modal_width(ctx));
        ui.heading("Stop background client?");
        ui.label(format!("Background owner: {}",owner.chars().take(16).collect::<String>()));
        ui.label("This stops transfers for every window attached to this profile. Closing only this window leaves transfers running.");
        ui.horizontal_wrapped(|ui|{
            if ui.add_enabled(!owner.is_empty(),egui::Button::new("Stop background client").wrap()).clicked(){decision=Some(Decision::Confirm);}
            if ui.button("Cancel").clicked(){decision=Some(Decision::Cancel);}
        });
    });
    finish_decision(open, decision, modal.should_close())
}

pub(super) fn appearance_dialog(
    ctx: &egui::Context,
    settings: &mut AppearanceSettings,
    open: &mut bool,
) -> Option<DesktopAction> {
    if !*open {
        return None;
    }
    let mut action = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("desktop_appearance")).show(ctx, |ui| {
        ui.set_width(modal_width(ctx));
        ui.heading("Appearance");
        egui::ScrollArea::both()
            .max_height((ctx.content_rect().height() * 0.6).max(60.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                appearance::gui::controls(ui, settings);
            });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Save view").clicked() {
                action = Some(DesktopAction::SaveView);
                close = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if close || modal.should_close() {
        *open = false;
    }
    action
}

pub(super) fn magnet_dialog(
    ctx: &egui::Context,
    text: &mut String,
    open: &mut bool,
    connected: bool,
) -> Option<DesktopAction> {
    if !*open {
        return None;
    }
    let mut action = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("desktop_add_magnet")).show(ctx, |ui| {
        ui.set_width(modal_width(ctx));
        ui.heading("Add magnet link");
        ui.label("Retrieve metadata first. Choose the files, location and start policy after review.");
        ui.label("A link with explicit trackers uses those trackers for lookup. Without trackers, an enabled DHT can share the info hash before the metadata reveals whether the torrent is private.");
        ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(ui.available_width())
                .hint_text("magnet:?xt=..."),
        );
        if !connected {
            ui.label("Background client disconnected. Reconnect before requesting metadata.");
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    connected && !text.trim().is_empty(),
                    egui::Button::new("Review metadata").wrap(),
                )
                .clicked()
            {
                action = Some(DesktopAction::AddMagnet);
                close = true;
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    if close || modal.should_close() {
        *open = false;
    }
    action
}

#[cfg(test)]
mod tests;
