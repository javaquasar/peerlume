use super::*;

fn menu_frame(
    context: &egui::Context,
    torrent: &TorrentItem,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<(String, TorrentCommand)>) {
    let mut action = None;
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
                let response = PeerLumeApp::show_torrent_name(ui, &torrent.name, false, false);
                response.context_menu(|ui| {
                    action = PeerLumeApp::show_recheck_menu(ui, torrent);
                });
            });
        },
    );
    (output, action)
}

fn text_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing menu label: {label}"))
}

fn click(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn force_recheck_targets_the_context_torrent_even_when_its_row_is_not_selected() {
    for checking in [false, true] {
        let context = egui::Context::default();
        let mut torrent = crate::client::tests::torrent();
        torrent.name = "Context torrent".into();
        torrent.info_hashes = vec!["v1: clicked".into(), "v2: clicked".into()];
        if checking {
            torrent.state = TorrentState::Checking;
        }
        menu_frame(&context, &torrent, vec![]);
        let (output, _) = menu_frame(&context, &torrent, vec![]);
        let pos = text_center(&output, &torrent.name);
        for pressed in [true, false] {
            assert_eq!(
                menu_frame(
                    &context,
                    &torrent,
                    click(pos, egui::PointerButton::Secondary, pressed)
                )
                .1,
                None
            );
        }
        let (output, action) = menu_frame(&context, &torrent, vec![]);
        assert_eq!(action, None);
        let pos = text_center(&output, "Force recheck and repair");
        menu_frame(
            &context,
            &torrent,
            click(pos, egui::PointerButton::Primary, true),
        );
        let (_, action) = menu_frame(
            &context,
            &torrent,
            click(pos, egui::PointerButton::Primary, false),
        );
        assert_eq!(
            action,
            if checking {
                None
            } else {
                Some((
                    "v1: clicked;v2: clicked".into(),
                    TorrentCommand::RecheckAndRepair,
                ))
            }
        );
        assert_eq!(
            torrent.overrides.repair_policy,
            crate::runtime_status::RepairPolicy::default()
        );
    }
}
