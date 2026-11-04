//! Local controls and feedback stay next to the selected torrent on every tab.
use super::*;
use crate::runtime_status::TransferFeedback;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HeaderAction {
    Transfer(TorrentCommand),
    RetryStart,
}

pub(super) fn extra_details_height(torrent: &TorrentItem) -> f32 {
    if torrent.runtime.last_error.is_some() {
        140.0
    } else if torrent.state == TorrentState::Error {
        60.0
    } else if torrent.runtime.command.pending_action().is_some()
        || matches!(torrent.runtime.command, TransferFeedback::Failed { .. })
    {
        24.0
    } else {
        0.0
    }
}

pub(super) fn primary_control(
    torrent: &TorrentItem,
) -> (&'static str, Option<HeaderAction>, &'static str) {
    if let Some(action) = torrent.runtime.command.pending_action() {
        return (
            action.pending_label(),
            None,
            "Waiting for the engine. The displayed transfer state is not changed until acknowledged.",
        );
    }
    match torrent.state {
        TorrentState::Error
            if !torrent.runtime.registered && torrent.runtime.last_error.is_some() =>
        {
            (
                "Retry start",
                Some(HeaderAction::RetryStart),
                "Retry the saved source in the same engine. Check the .torrent path and download drive first. Downloaded files are kept.",
            )
        }
        TorrentState::Error if torrent.runtime.last_error.is_none() => (
            "Awaiting engine...",
            None,
            "The saved torrent is being restored. No failure has been reported in this session yet.",
        ),
        TorrentState::Error | TorrentState::VerificationFailed => (
            "Recheck and repair",
            Some(HeaderAction::Transfer(TorrentCommand::RecheckAndRepair)),
            "Check the existing payload and download missing or corrupt pieces. This can resume the torrent.",
        ),
        TorrentState::Paused => (
            "Resume",
            Some(HeaderAction::Transfer(TorrentCommand::Resume)),
            "Request resume and wait for the engine to confirm it.",
        ),
        _ => (
            "Pause",
            Some(HeaderAction::Transfer(TorrentCommand::Pause)),
            "Request pause. An ongoing integrity check continues, but the transfer will remain paused afterwards.",
        ),
    }
}

pub(super) fn show(ui: &mut egui::Ui, torrent: &TorrentItem) {
    let checking = torrent.state == TorrentState::Checking
        || matches!(
            torrent.completion,
            crate::completion::CompletionStatus::Checking { .. }
        );
    ui.horizontal_wrapped(|ui| {
        if torrent.runtime.command.pending_action().is_some() {
            ui.spinner();
        }
        ui.strong(format!("State: {}", torrent.state.label()));
        if let Some(message) = torrent.runtime.command.summary(checking) {
            let color = match torrent.runtime.command {
                TransferFeedback::Failed { .. } => ui.visuals().error_fg_color,
                TransferFeedback::Confirmed { .. } => ui.visuals().text_color(),
                _ => ui.visuals().warn_fg_color,
            };
            ui.add(egui::Label::new(egui::RichText::new(message).color(color)).wrap());
        }
    });
    if let Some(message) = &torrent.runtime.last_error {
        let failed = matches!(
            torrent.state,
            TorrentState::Error | TorrentState::VerificationFailed
        );
        let color = if failed {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().warn_fg_color
        };
        // Keep the reason visible without letting a long backend error consume
        // the whole detail pane or push the file list outside its viewport.
        let max_height = (ui.available_height() - 148.0).clamp(24.0, 96.0);
        egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.0, color)).show(ui, |ui| {
            ui.colored_label(color, if failed { "Transfer error" } else { "Last transfer error" });
            ui.style_mut().spacing.scroll.floating = false;
            egui::ScrollArea::vertical().id_salt(("transfer_error", &torrent.info_hashes))
                .max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
            ui.add(egui::Label::new(message).wrap());
            if !torrent.runtime.registered {
                ui.add(egui::Label::new("The engine has not confirmed startup for this torrent. Check the source .torrent path and download drive, then use Retry start. Remove selected (keep data) removes only the list entry, not downloaded files.").wrap());
            } else if failed {
                ui.add(egui::Label::new("Check download-directory access and available disk space. Recheck and repair verifies the payload and can resume missing pieces. Pausing does not fix a file or connection error.").wrap());
            }
            });
        });
    } else if torrent.state == TorrentState::Error {
        ui.add(egui::Label::new("Waiting for the engine to restore this torrent. The previous session's error reason was not saved; the current result will appear here.").wrap());
    }
    // Pending feedback must remain live even when the selected tab has no updates.
    if torrent.runtime.command.pending_action().is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}
