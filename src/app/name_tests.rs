//! Regression tests for alignment and interaction of the real table name cell.
use super::*;

fn frame(
    context: &egui::Context,
    name: &str,
    private: bool,
    selected: bool,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, egui::Response) {
    let mut response = None;
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::Grid::new("name_alignment")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Progress");
                        ui.end_row();
                        response = Some(PeerLumeApp::show_torrent_name(
                            ui,
                            name,
                            private,
                            selected,
                            name_column::DEFAULT_WIDTH,
                        ));
                        ui.label("50%");
                        ui.end_row();
                    });
            });
        },
    );
    (output, response.unwrap())
}

#[test]
fn all_name_lengths_and_private_selected_states_start_at_the_left_padding() {
    let names = [
        "SS".to_owned(),
        "A normal torrent filename.mp4".to_owned(),
        "A very long torrent filename ".repeat(30),
    ];
    let mut offsets = Vec::new();
    for name in &names {
        for private in [false, true] {
            for selected in [false, true] {
                let context = egui::Context::default();
                frame(&context, name, private, selected, vec![]);
                let (output, response) = frame(&context, name, private, selected, vec![]);
                let label = if private {
                    format!("[Private] {name}")
                } else {
                    name.clone()
                };
                let text = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                            Some(text)
                        }
                        _ => None,
                    })
                    .expect("Name text is rendered");
                let offset = text.pos.x - response.rect.left();
                assert!(
                    (offset - context.style().spacing.button_padding.x).abs() <= 1.0,
                    "Centered or incorrectly padded text: {offset}"
                );
                assert!((response.rect.width() - 420.0).abs() <= 1.0);
                assert!((response.rect.height() - 20.0).abs() <= 1.0);
                assert!(text.pos.x + text.galley.size().x <= response.rect.right() + 1.0);
                assert_eq!(
                    text.galley.rows.len(),
                    1,
                    "Long names must truncate, not wrap"
                );
                offsets.push(offset);
            }
        }
    }
    assert!(
        offsets
            .iter()
            .all(|offset| (offset - offsets[0]).abs() <= 1.0)
    );
}

#[test]
fn the_entire_name_cell_remains_clickable_for_selection_and_context_menus() {
    for selected in [false, true] {
        for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
            let context = egui::Context::default();
            frame(&context, "SS", false, selected, vec![]);
            let (_, response) = frame(&context, "SS", false, selected, vec![]);
            // Click the blank right-hand side, far away from the short text.
            let pos = egui::pos2(response.rect.right() - 10.0, response.rect.center().y);
            for pressed in [true, false] {
                let (_, response) = frame(
                    &context,
                    "SS",
                    false,
                    selected,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                assert_eq!(
                    response.clicked(),
                    !pressed && button == egui::PointerButton::Primary
                );
                assert_eq!(
                    response.secondary_clicked(),
                    !pressed && button == egui::PointerButton::Secondary
                );
            }
        }
    }
}
