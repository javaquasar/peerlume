//! Serialized display preferences and shared, non-color-only status tokens.
use serde::{Deserialize, Deserializer, Serialize};

pub const MIN_ZOOM_PERCENT: u16 = 75;
pub const MAX_ZOOM_PERCENT: u16 = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: ThemePreference,
    pub density: Density,
    #[serde(deserialize_with = "deserialize_zoom")]
    pub zoom_percent: u16,
}
impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            density: Density::Comfortable,
            zoom_percent: 100,
        }
    }
}
fn deserialize_zoom<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Ok(u16::deserialize(deserializer)?.clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT))
}
impl AppearanceSettings {
    pub fn normalized(mut self) -> Self {
        self.normalize();
        self
    }
    pub fn normalize(&mut self) -> bool {
        let previous = self.zoom_percent;
        self.zoom_percent = previous.clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
        previous != self.zoom_percent
    }
    pub fn set_zoom_percent(&mut self, percent: u16) {
        self.zoom_percent = percent.clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
    }
    pub fn zoom_factor(self) -> f32 {
        self.normalized().zoom_percent as f32 / 100.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb8(pub u8, pub u8, pub u8);
impl Rgb8 {
    fn blend(self, other: Self, weight: u16) -> Self {
        let channel = |a: u8, b: u8| {
            ((u16::from(a) * (255 - weight) + u16::from(b) * weight + 127) / 255) as u8
        };
        Self(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }
    #[cfg(feature = "gui")]
    pub fn color32(self) -> eframe::egui::Color32 {
        eframe::egui::Color32::from_rgb(self.0, self.1, self.2)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowColors {
    pub background: Rgb8,
    pub text: Rgb8,
    pub accent: Rgb8,
}

pub fn tone_colors(tone: Tone, dark: bool, selected: bool, hovered: bool) -> RowColors {
    use Rgb8 as C;
    let (mut background, text, accent) = if selected {
        if dark {
            (C(38, 74, 108), C(241, 247, 255), C(136, 195, 251))
        } else {
            (C(193, 221, 248), C(12, 44, 80), C(31, 98, 164))
        }
    } else {
        match (dark, tone) {
            (false, Tone::Neutral) => (C(245, 247, 250), C(45, 55, 65), C(105, 116, 130)),
            (false, Tone::Info) => (C(224, 241, 255), C(0, 62, 106), C(35, 111, 165)),
            (false, Tone::Success) => (C(223, 245, 228), C(20, 83, 40), C(45, 119, 66)),
            (false, Tone::Warning) => (C(255, 242, 204), C(102, 66, 0), C(153, 103, 0)),
            (false, Tone::Danger) => (C(255, 229, 228), C(131, 30, 26), C(173, 56, 50)),
            (true, Tone::Neutral) => (C(37, 42, 50), C(227, 232, 240), C(156, 170, 190)),
            (true, Tone::Info) => (C(22, 48, 68), C(158, 213, 255), C(111, 188, 244)),
            (true, Tone::Success) => (C(24, 55, 38), C(168, 230, 186), C(119, 207, 148)),
            (true, Tone::Warning) => (C(63, 49, 16), C(255, 220, 140), C(238, 189, 79)),
            (true, Tone::Danger) => (C(64, 30, 32), C(255, 184, 184), C(241, 132, 132)),
        }
    };
    if hovered {
        background = background.blend(text, 10);
    }
    RowColors {
        background,
        text,
        accent,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusMarker {
    Download,
    Waiting,
    Pause,
    Seed,
    Check,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusPresentation {
    pub marker: StatusMarker,
    pub label: &'static str,
    pub tone: Tone,
}
impl StatusPresentation {
    pub fn for_state(state: crate::torrent::TorrentState) -> Self {
        use crate::torrent::TorrentState as S;
        use StatusMarker as M;
        let (marker, label, tone) = match state {
            S::Downloading => (M::Download, "Downloading", Tone::Info),
            S::WaitingForEngine => (M::Waiting, "Waiting", Tone::Info),
            S::Paused => (M::Pause, "Paused", Tone::Neutral),
            S::Seeding => (M::Seed, "Seeding", Tone::Success),
            S::Checking => (M::Check, "Checking", Tone::Warning),
            S::VerificationFailed => (M::Error, "Verification failed", Tone::Danger),
            S::Error => (M::Error, "Error", Tone::Danger),
        };
        Self {
            marker,
            label,
            tone,
        }
    }
}
pub fn row_colors(
    state: crate::torrent::TorrentState,
    dark: bool,
    selected: bool,
    hovered: bool,
) -> RowColors {
    tone_colors(
        StatusPresentation::for_state(state).tone,
        dark,
        selected,
        hovered,
    )
}

#[cfg(feature = "gui")]
pub use gui::AppearanceState;

#[cfg(feature = "gui")]
pub mod gui {
    use super::*;
    use eframe::egui::{self, Margin, TextStyle, Vec2};

    #[derive(Default)]
    pub struct AppearanceState {
        last: Option<AppearanceSettings>,
        last_pixels_per_point: Option<f32>,
        last_dark: Option<bool>,
        rebuilds: u64,
    }
    impl AppearanceState {
        /// Absolute logical zoom never multiplies the platform's native DPI.
        /// Returns true when dependent row/text measurements should be invalidated.
        pub fn apply(&mut self, ctx: &egui::Context, settings: &AppearanceSettings) -> bool {
            let normalized = settings.normalized();
            let changed = self.last != Some(normalized);
            if changed {
                ctx.set_theme(match normalized.theme {
                    ThemePreference::System => egui::ThemePreference::System,
                    ThemePreference::Light => egui::ThemePreference::Light,
                    ThemePreference::Dark => egui::ThemePreference::Dark,
                });
                ctx.set_zoom_factor(normalized.zoom_factor());
                ctx.all_styles_mut(|style| {
                    let (horizontal, vertical, padding, height, margin) = match normalized.density {
                        Density::Compact => (6.0, 3.0, 2.0, 22.0, 6),
                        Density::Comfortable => (8.0, 6.0, 4.0, 28.0, 10),
                    };
                    style.spacing.item_spacing = Vec2::new(horizontal, vertical);
                    style.spacing.button_padding = Vec2::new(horizontal, padding);
                    style.spacing.interact_size.y = height;
                    style.spacing.window_margin = Margin::same(margin);
                    style.spacing.menu_margin = Margin::same(margin);
                });
                self.last = Some(normalized);
                self.rebuilds = self.rebuilds.saturating_add(1);
                ctx.request_repaint();
            }
            let pixels = ctx.pixels_per_point();
            let dark = ctx.style().visuals.dark_mode;
            let measurements_changed =
                self.last_pixels_per_point != Some(pixels) || self.last_dark != Some(dark);
            self.last_pixels_per_point = Some(pixels);
            self.last_dark = Some(dark);
            changed || measurements_changed
        }
        pub fn rebuild_count(&self) -> u64 {
            self.rebuilds
        }
    }
    pub fn row_height(ui: &egui::Ui) -> f32 {
        ui.spacing()
            .interact_size
            .y
            .max(ui.text_style_height(&TextStyle::Body) + 2.0 * ui.spacing().button_padding.y)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum Control {
        ThemeMenu,
        System,
        Light,
        Dark,
        Compact,
        Comfortable,
        Zoom,
        Reset,
    }

    /// Returns only a presentation change; caller saves through its UI-only path.
    pub fn controls(ui: &mut egui::Ui, settings: &mut AppearanceSettings) -> bool {
        controls_inner(ui, settings, |_, _| {})
    }
    fn controls_inner(
        ui: &mut egui::Ui,
        settings: &mut AppearanceSettings,
        mut trace: impl FnMut(Control, egui::Rect),
    ) -> bool {
        let previous = *settings;
        ui.horizontal_wrapped(|ui| {
            ui.label("Theme");
            let menu = egui::ComboBox::from_id_salt("appearance-theme")
                .selected_text(match settings.theme {
                    ThemePreference::System => "System",
                    ThemePreference::Light => "Light",
                    ThemePreference::Dark => "Dark",
                })
                .show_ui(ui, |ui| {
                    for (value, label, id) in [
                        (ThemePreference::System, "System", Control::System),
                        (ThemePreference::Light, "Light", Control::Light),
                        (ThemePreference::Dark, "Dark", Control::Dark),
                    ] {
                        let response = ui.selectable_value(&mut settings.theme, value, label);
                        trace(id, response.rect);
                    }
                });
            trace(Control::ThemeMenu, menu.response.rect);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Density");
            for (value, label, id) in [
                (Density::Compact, "Compact", Control::Compact),
                (Density::Comfortable, "Comfortable", Control::Comfortable),
            ] {
                let response = ui.selectable_value(&mut settings.density, value, label);
                trace(id, response.rect);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let zoom = ui.add(
                egui::Slider::new(
                    &mut settings.zoom_percent,
                    MIN_ZOOM_PERCENT..=MAX_ZOOM_PERCENT,
                )
                .text("Text / zoom")
                .suffix("%"),
            );
            trace(Control::Zoom, zoom.rect);
            let reset = ui.button("Reset appearance");
            trace(Control::Reset, reset.rect);
            if reset.clicked() {
                *settings = AppearanceSettings::default();
            }
        });
        settings.normalize();
        previous != *settings
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
mod tests;
