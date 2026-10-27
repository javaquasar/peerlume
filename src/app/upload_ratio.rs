use crate::download_mode::UploadRatioRange;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, range: &mut UploadRatioRange) {
    ui.horizontal(|ui| {
        ui.label("Upload multiplier range");
        for (label, value) in [
            ("Minimum ×", &mut range.min_milli),
            ("Maximum ×", &mut range.max_milli),
        ] {
            ui.label(label);
            ui.add(
                egui::DragValue::new(value)
                    .range(1..=u32::MAX)
                    .speed(50.0)
                    .custom_formatter(|value, _| UploadRatioRange::format_multiplier(value as u32))
                    .custom_parser(|text| {
                        UploadRatioRange::parse_multiplier(text).ok().map(f64::from)
                    }),
            );
        }
    });
    ui.label("A new multiplier is chosen for each announce. Equal bounds fix the multiplier. Reported upload may decrease; actual transfer totals are unchanged.");
    if let Err(message) = range.validate() {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_editable_bounds_and_validation_are_visible_without_mutating_a_draft() {
        for mut range in [
            UploadRatioRange::default(),
            UploadRatioRange {
                min_milli: 1250,
                max_milli: 3750,
            },
            UploadRatioRange {
                min_milli: 3000,
                max_milli: 2000,
            },
        ] {
            let original = range;
            let context = egui::Context::default();
            let output = context.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &mut range));
            });
            let texts: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                    _ => None,
                })
                .collect();
            for label in [
                "Minimum ×",
                "Maximum ×",
                &UploadRatioRange::format_multiplier(range.min_milli),
                &UploadRatioRange::format_multiplier(range.max_milli),
            ] {
                assert!(texts.contains(&label), "Missing range control: {label}");
            }
            if let Err(message) = range.validate() {
                assert!(texts.contains(&message));
            }
            assert_eq!(range, original);
        }
    }
}
