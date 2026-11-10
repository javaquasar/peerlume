use super::*;

fn frame(
    ctx: &egui::Context,
    busy: bool,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<Action>) {
    let mut action = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = recovery_panel(
                    ui,
                    Path::new("fixture-profile"),
                    "Settings are invalid.",
                    busy,
                );
            });
        },
    );
    (output, action)
}

fn label(output: &egui::FullOutput, value: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape
                && text.galley.job.text == value
            {
                return Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
            }
            None
        })
        .unwrap_or_else(|| panic!("Missing {value}"))
}

fn click(ctx: &egui::Context, busy: bool, text: &str) -> Option<Action> {
    frame(ctx, busy, Vec::new());
    let (output, _) = frame(ctx, busy, Vec::new());
    let point = label(&output, text).center();
    frame(ctx, busy, vec![egui::Event::PointerMoved(point)]);
    let mut action = None;
    for pressed in [true, false] {
        action = frame(
            ctx,
            busy,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }],
        )
        .1
        .or(action);
    }
    action
}

#[test]
fn recovery_actions_are_visible_and_cannot_overlap_an_active_request() {
    let ctx = egui::Context::default();
    let (output, _) = frame(&ctx, true, Vec::new());
    label(&output, "Settings are invalid.");
    label(&output, "fixture-profile");
    assert_eq!(click(&ctx, true, "Retry connection"), None);
    assert_eq!(click(&ctx, true, "Validate configuration"), None);
    assert_eq!(
        click(&ctx, true, "Copy profile location"),
        Some(Action::CopyProfile)
    );
    assert_eq!(
        click(&ctx, false, "Validate configuration"),
        Some(Action::Validate)
    );
    assert_eq!(click(&ctx, false, "Retry connection"), Some(Action::Retry));
}
