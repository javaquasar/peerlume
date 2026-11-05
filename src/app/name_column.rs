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
    measurements: ColumnMeasurements,
}

#[derive(Clone, PartialEq)]
struct MetricKey {
    state: crate::torrent::TorrentState,
    done: bool,
    pending: Option<crate::runtime_status::TransferAction>,
    down: u64,
    up: u64,
    peers: usize,
    seeds: usize,
    eta: Option<u64>,
    ratio: u64,
}

impl MetricKey {
    fn of(torrent: &TorrentItem) -> Self {
        Self {
            state: torrent.state,
            done: torrent.completed_successfully(),
            pending: torrent.runtime.command.pending_action(),
            down: torrent.download_rate,
            up: torrent.upload_rate,
            peers: torrent.peers,
            seeds: torrent.seeds,
            eta: torrent.eta_seconds(),
            ratio: torrent.ratio().to_bits(),
        }
    }
}

#[derive(Default)]
struct ColumnMeasurements {
    font: Option<egui::FontId>,
    pixels_per_point: f32,
    headers: [f32; 6],
    rows: Vec<Option<(MetricKey, [f32; 6])>>,
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
    pub fn cached_widths(
        &mut self,
        ui: &egui::Ui,
        torrents: &[TorrentItem],
        filter: StatusFilter,
    ) -> [f32; 6] {
        let cache = &mut self.measurements;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        let measure = |text: &str| {
            ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.into(), font.clone(), egui::Color32::PLACEHOLDER)
                    .size()
                    .x
                    .ceil()
            })
        };
        if cache.font.as_ref() != Some(&font) || cache.pixels_per_point != pixels_per_point {
            cache.font = Some(font.clone());
            cache.pixels_per_point = pixels_per_point;
            cache.rows.clear();
            cache.headers = HEADERS.map(|text| measure(text).max(MIN_OTHER_WIDTH));
        }
        cache.rows.resize_with(torrents.len(), || None);
        let mut widths = cache.headers;
        for (index, torrent) in torrents
            .iter()
            .enumerate()
            .filter(|(_, t)| filter.matches(t))
        {
            let key = MetricKey::of(torrent);
            if cache.rows[index]
                .as_ref()
                .is_none_or(|(previous, _)| *previous != key)
            {
                cache.rows[index] = Some((key, metrics(torrent).map(|text| measure(&text))));
            }
            let (_, row) = cache.rows[index].as_ref().unwrap();
            for (width, value) in widths.iter_mut().zip(row) {
                *width = width.max(*value);
            }
        }
        widths
    }

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

#[cfg(test)]
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

pub(super) fn headers_with_widths(ui: &mut egui::Ui, width: f32, columns: [f32; 6]) {
    ui.add_sized(
        [width, 20.0],
        egui::Label::new(egui::RichText::new("Name").strong())
            .halign(egui::Align::Min)
            .truncate(),
    );
    ui.add_sized(
        [PROGRESS_WIDTH, 20.0],
        egui::Label::new(egui::RichText::new("Progress").strong()).truncate(),
    );
    for (title, width) in HEADERS.into_iter().zip(columns) {
        ui.add_sized(
            [width, 20.0],
            egui::Label::new(egui::RichText::new(title).strong()).truncate(),
        );
    }
    ui.end_row();
}

pub(super) fn reserved_for_widths(ui: &egui::Ui, widths: [f32; 6]) -> f32 {
    PROGRESS_WIDTH + widths.iter().sum::<f32>() + ui.spacing().item_spacing.x * 7.0
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

#[cfg(test)]
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
