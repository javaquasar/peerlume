//! Session-local vertical split. No transfer, selection or saved-setting changes.
use eframe::egui;

const MIN_TABLE_HEIGHT: f32 = 96.0;
const MIN_DETAILS_HEIGHT: f32 = 180.0;

pub(super) fn table<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let available = ui.available_height().max(0.0);
    let minimum = MIN_TABLE_HEIGHT.min(available * 0.35);
    let maximum = (available - MIN_DETAILS_HEIGHT).max(minimum);
    egui::TopBottomPanel::top("torrent_list_pane")
        .frame(egui::Frame::NONE.inner_margin(egui::Margin {
            bottom: 6,
            ..Default::default()
        }))
        .resizable(true)
        .default_height(available * 0.45)
        .height_range(minimum..=maximum)
        .show_inside(ui, add_contents)
}

pub(super) fn details(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.available_rect_before_wrap();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(rect));
        add_contents(ui);
    });
}

#[cfg(test)]
mod tests;
