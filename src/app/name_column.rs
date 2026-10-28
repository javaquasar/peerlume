//! Session-local column sizing. No settings, payloads or transfer commands change.
use crate::{torrent::TorrentItem, torrent_filter::StatusFilter};
use eframe::egui;

pub(super) const MIN_WIDTH: f32 = 160.0;
pub(super) const PROGRESS_WIDTH: f32 = 145.0;
const MIN_OTHER_WIDTH: f32 = 72.0;
const HEADERS: [&str; 6] = ["Status", "Down", "Up", "Peers", "ETA", "Ratio"];

pub(super) fn viewport_width(ui: &egui::Ui) -> f32 {
    ui.available_width()
        .min(ui.clip_rect().right() - ui.cursor().left())
        .max(0.0)
}

pub(super) fn scroll_area(width: f32) -> egui::ScrollArea {
    // An unwrapped toolbar can expand the parent Ui beyond the window. Never
    // mistake that virtual width for the visible viewport when clamping offsets.
    egui::ScrollArea::horizontal()
        .id_salt("torrent_table_scroll")
        .max_width(width)
        .min_scrolled_width(0.0)
        .auto_shrink([false, true])
}

#[derive(Default)]
pub(super) struct NameColumn {
    // Zero fills the viewport. A user-created gap stays constant when the
    // window grows, so all newly available space still goes to Name.
    trailing_space: f32,
    last_layout_width: Option<f32>,
    last_input_frame: Option<u64>,
}

pub(super) struct ColumnLayout {
    pub width: f32,
    gap: f32,
    highlighted: bool,
}

impl ColumnLayout {
    pub fn paint_divider(&self, ui: &egui::Ui, table: egui::Rect) {
        let x = table.left() + self.width + self.gap * 0.5;
        let stroke = if self.highlighted {
            ui.visuals().selection.stroke
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        ui.painter().line_segment(
            [
                egui::pos2(x, table.top() + 2.0),
                egui::pos2(x, table.bottom() - 2.0),
            ],
            stroke,
        );
    }
}

impl NameColumn {
    pub fn from_gap(gap: u32) -> Self {
        Self {
            trailing_space: gap.min(10_000) as f32,
            ..Default::default()
        }
    }
    pub fn saved_gap(&self) -> u32 {
        self.trailing_space.round().clamp(0.0, 10_000.0) as u32
    }
    fn width(&self, available: f32, reserved: f32) -> f32 {
        (available - reserved - self.trailing_space).max(MIN_WIDTH)
    }

    /// Call inside the table's horizontal scroll area, with the OUTER viewport
    /// width. The scroll area's unconstrained content width is not a resize budget.
    pub fn begin(&mut self, ui: &mut egui::Ui, available: f32, reserved: f32) -> ColumnLayout {
        let maximum = (available - reserved).max(MIN_WIDTH);
        let mut width = self.width(available, reserved);
        let gap = ui.spacing().item_spacing.x;
        let x = ui.cursor().left() + width + gap * 0.5;
        let hit_rect = egui::Rect::from_center_size(
            egui::pos2(x, ui.cursor().top() + 10.0),
            egui::vec2(gap.max(8.0), 20.0),
        );
        let response = ui
            .interact(
                hit_rect,
                ui.make_persistent_id("torrent_name_resize"),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(if maximum > MIN_WIDTH {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Default
            })
            .on_hover_text(
                "Drag to resize Name. Double-click to fill the available width. The other columns keep their space.",
            );
        let frame = ui.ctx().cumulative_frame_nr();
        if self.last_input_frame != Some(frame) {
            if response.double_clicked_by(egui::PointerButton::Primary) {
                self.trailing_space = 0.0;
                self.last_input_frame = Some(frame);
            } else if response.dragged_by(egui::PointerButton::Primary)
                && response.drag_delta().x != 0.0
            {
                self.trailing_space =
                    maximum - (width + response.drag_delta().x).clamp(MIN_WIDTH, maximum);
                self.last_input_frame = Some(frame);
            }
            width = self.width(available, reserved);
        }
        // Grid uses the previous pass's column sizes. Relayout once when the
        // visible width changes, without accumulating drag input twice or
        // allocating a new Grid ID on every mouse movement.
        if self
            .last_layout_width
            .is_some_and(|previous| previous != width)
        {
            ui.ctx().request_discard("torrent Name column resized");
        }
        self.last_layout_width = Some(width);
        ColumnLayout {
            width,
            gap,
            highlighted: response.hovered() || response.dragged(),
        }
    }
}

pub(super) fn headers(ui: &mut egui::Ui, width: f32) {
    ui.add_sized(
        [width, 20.0],
        egui::Label::new(egui::RichText::new("Name").strong())
            .halign(egui::Align::Min)
            .truncate(),
    );
    // Reserve the same column width even when a filter shows no torrent rows.
    ui.add_sized(
        [PROGRESS_WIDTH, 20.0],
        egui::Label::new(egui::RichText::new("Progress").strong()).truncate(),
    );
    for title in HEADERS {
        ui.strong(title);
    }
    ui.end_row();
}

pub(super) fn metrics(torrent: &TorrentItem) -> [String; 6] {
    [
        torrent.status_text(),
        super::format_rate(torrent.download_rate),
        super::format_rate(torrent.upload_rate),
        format!("{} / {}", torrent.peers, torrent.seeds),
        torrent
            .eta_seconds()
            .map(super::format_duration)
            .unwrap_or_else(|| "--".into()),
        format!("{:.2}", torrent.ratio()),
    ]
}

pub(super) fn reserved_width(ui: &egui::Ui, torrents: &[TorrentItem], filter: StatusFilter) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let measure = |text: &str| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.into(), font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
                .ceil()
        })
    };
    let mut widths = HEADERS.map(|title| measure(title).max(MIN_OTHER_WIDTH));
    for torrent in torrents.iter().filter(|torrent| filter.matches(torrent)) {
        for (width, text) in widths.iter_mut().zip(metrics(torrent)) {
            *width = width.max(measure(&text));
        }
    }
    PROGRESS_WIDTH + widths.iter().sum::<f32>() + ui.spacing().item_spacing.x * 7.0
}

#[cfg(test)]
mod tests;
