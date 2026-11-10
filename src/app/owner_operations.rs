//! Read-only GUI projection of the background owner's operation pages.
use crate::{
    appearance::{self, Tone},
    control::frontend::{ConnectionStatus, ConnectionView, NativeOperationState, OperationView},
    ui_operations::{HistoryIntent, MAX_HISTORY_ROWS},
};
use eframe::egui;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Panel {
    pub local_tab: bool,
    pub before: Option<u64>,
    owner: Option<String>,
    page: Option<OperationView>,
    previous_owner: Option<OperationView>,
    newer: Vec<Option<u64>>,
}

impl Panel {
    pub fn observe(&mut self, connection: &ConnectionView, view: Option<OperationView>) {
        if let Some(owner) = connection.owner_instance.as_ref()
            && self.owner.as_ref() != Some(owner)
        {
            if self.page.is_some() {
                self.previous_owner = self.page.take();
            }
            self.owner = Some(owner.clone());
            self.before = None;
            self.newer.clear();
        }
        if let Some(view) = view
            && self.owner.as_deref() == Some(view.owner_instance.as_str())
            && self.before == view.before
        {
            self.page = Some(view);
        }
    }

    pub fn prefer_local_errors(&mut self, has_local_entries: bool) {
        self.local_tab = has_local_entries
            && self
                .page
                .as_ref()
                .is_none_or(|view| view.page.entries.is_empty());
    }

    pub fn show<'a>(
        &mut self,
        ui: &mut egui::Ui,
        connection: &ConnectionView,
        max_height: f32,
        target_name: impl Fn(&str) -> Option<&'a str>,
    ) -> Vec<HistoryIntent> {
        let mut intents = Vec::new();
        let requested_page = self
            .page
            .as_ref()
            .is_some_and(|view| view.before == self.before);
        let live = connection.status == ConnectionStatus::Connected
            && self.owner.is_some()
            && self.owner == connection.owner_instance
            && self.page.as_ref().is_none_or(|view| !view.stale);
        let older = self
            .page
            .as_ref()
            .and_then(|view| view.page.next_before)
            .filter(|cursor| self.before.is_none_or(|before| *cursor < before));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(live && !self.newer.is_empty(), egui::Button::new("Newer"))
                .clicked()
            {
                self.before = self.newer.pop().expect("enabled only with a newer cursor");
            }
            if ui
                .add_enabled(
                    live && requested_page
                        && older.is_some()
                        && self.newer.len() < MAX_HISTORY_ROWS,
                    egui::Button::new("Older"),
                )
                .clicked()
                && let Some(cursor) = older
            {
                self.newer.push(self.before);
                self.before = Some(cursor);
            }
        });

        if !live {
            ui.label(
                if self
                    .page
                    .as_ref()
                    .is_some_and(|view| !view.page.entries.is_empty())
                {
                    "Owner history is stale. Showing the last received records."
                } else {
                    "Owner history is unavailable."
                },
            );
        } else if self
            .page
            .as_ref()
            .is_none_or(|view| view.before != self.before)
        {
            ui.label("Loading the requested page from the background owner...");
        }

        egui::ScrollArea::vertical()
            .id_salt(("owner_operations", self.owner.as_deref(), self.before))
            .max_height(max_height)
            .show(ui, |ui| {
                if let Some(view) = &self.page {
                    let current = live && view.before == self.before;
                    if current && view.page.entries.is_empty() {
                        ui.label("No operations on this owner page.");
                    }
                    show_page(ui, view, current, &target_name, &mut intents);
                }
                if let Some(previous) = &self.previous_owner {
                    egui::CollapsingHeader::new("Previous background owner")
                        .id_salt(&previous.owner_instance)
                        .show(ui, |ui| {
                            ui.label("These are the last received records. The outcome of unfinished operations is unknown.");
                            show_page(ui, previous, false, &target_name, &mut intents);
                        });
                }
            });
        intents
    }
}

fn show_page<'a>(
    ui: &mut egui::Ui,
    view: &OperationView,
    current: bool,
    target_name: &impl Fn(&str) -> Option<&'a str>,
    intents: &mut Vec<HistoryIntent>,
) {
    for entry in &view.page.entries {
        ui.push_id((&view.owner_instance, &entry.id), |ui| {
            ui.separator();
            let (phase, tone) = phase_label(entry.state, current);
            ui.horizontal_wrapped(|ui| {
                ui.strong(&entry.command);
                ui.small(format!("#{}", entry.sequence));
                ui.colored_label(
                    appearance::tone_colors(tone, ui.visuals().dark_mode, false, false)
                        .text
                        .color32(),
                    phase,
                );
            });
            let name = entry
                .target
                .as_deref()
                .filter(|_| current)
                .and_then(target_name);
            if let Some(target) = &entry.target {
                ui.add(egui::Label::new(name.unwrap_or(target)).wrap())
                    .on_hover_text(target);
            }
            if let Some(revision) = entry.revision {
                ui.small(format!("Command revision {revision}"));
            }
            if let Some(progress) = &entry.progress {
                let bytes = super::format_size(progress.bytes);
                if let Some(total) = progress.total.filter(|total| *total > 0) {
                    let fraction = (progress.bytes as f64 / total as f64).clamp(0.0, 1.0) as f32;
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .text(format!("{bytes} / {}", super::format_size(total))),
                    );
                } else {
                    ui.label(format!("{bytes} processed"));
                }
            }
            let message = entry
                .message
                .as_deref()
                .map(crate::runtime_status::display_error);
            if let Some(message) = &message {
                ui.add(egui::Label::new(message).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(target) = &entry.target
                    && ui
                        .add_enabled(name.is_some(), egui::Button::new("Focus torrent"))
                        .clicked()
                {
                    intents.push(HistoryIntent::FocusTorrent(target.clone()));
                }
                if matches!(
                    entry.state,
                    NativeOperationState::Failed | NativeOperationState::NeedsReview
                ) && let Some(message) = message
                    && ui.button("Copy error").clicked()
                {
                    intents.push(HistoryIntent::CopyError(message));
                }
            });
        });
    }
}

fn phase_label(state: NativeOperationState, current: bool) -> (&'static str, Tone) {
    use NativeOperationState::*;
    if !current && !matches!(state, Succeeded | Failed | NeedsReview) {
        return ("Outcome unknown in this view", Tone::Warning);
    }
    match state {
        Queued => ("Queued", Tone::Neutral),
        Accepted => ("Accepted. Awaiting engine confirmation.", Tone::Info),
        Running => ("In progress", Tone::Info),
        EffectConfirmed => (
            "Engine effect confirmed. Awaiting persistence.",
            Tone::Warning,
        ),
        Succeeded => ("Succeeded", Tone::Success),
        Failed => ("Failed", Tone::Danger),
        NeedsReview => ("Outcome unknown. Inspect current state.", Tone::Warning),
    }
}
