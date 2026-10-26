use crate::{torrent::TorrentItem, torrent_filter::StatusFilter};
use eframe::egui;

#[cfg(test)]
mod tests;

/// Placed right-to-left next to the existing session-wide counter.
pub(super) fn show(
    ui: &mut egui::Ui,
    filter: &mut StatusFilter,
    torrents: &[TorrentItem],
) -> egui::Response {
    let active = StatusFilter::Downloading.count(torrents);
    let visible = filter.count(torrents);
    let mut summary = format!("{active} active / {} total", torrents.len());
    if *filter != StatusFilter::All {
        summary.push_str(&format!(" / {visible} shown"));
    }
    ui.label(summary);
    let response = egui::ComboBox::from_id_salt("torrent_status_filter")
        .width(170.0)
        .wrap_mode(egui::TextWrapMode::Truncate)
        .selected_text(format!("{} ({visible})", filter.label()))
        .show_ui(ui, |ui| {
            for choice in StatusFilter::ALL {
                ui.selectable_value(
                    filter,
                    choice,
                    format!("{} ({})", choice.label(), choice.count(torrents)),
                );
            }
        })
        .response
        .on_hover_text(
            "Filter the torrent list by status. This does not pause or resume transfers.",
        );
    ui.label("Status:");
    response
}
