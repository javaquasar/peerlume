use super::*;

fn frame(
    ctx: &egui::Context,
    filter: &mut StatusFilter,
    torrents: &[TorrentItem],
    width: f32,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, egui::Response) {
    let mut response = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 650.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::TopBottomPanel::top("filter_header").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("PeerLume");
                    ui.separator();
                    let mut page = crate::ui_schema::MainPage::Torrents;
                    ui.selectable_value(
                        &mut page,
                        crate::ui_schema::MainPage::Torrents,
                        "Torrents",
                    );
                    ui.selectable_value(
                        &mut page,
                        crate::ui_schema::MainPage::Preferences,
                        "Preferences",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        response = Some(show(ui, filter, torrents));
                    });
                });
            });
        },
    );
    (output, response.unwrap())
}

fn click(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn torrents() -> Vec<TorrentItem> {
    [
        crate::torrent::TorrentState::Downloading,
        crate::torrent::TorrentState::Paused,
        crate::torrent::TorrentState::Seeding,
    ]
    .into_iter()
    .map(|state| {
        let mut torrent = crate::client::tests::torrent();
        torrent.state = state;
        torrent
    })
    .collect()
}

#[test]
fn every_status_is_selectable_with_the_actual_combo_and_live_counts() {
    let torrents = torrents();
    for choice in StatusFilter::ALL {
        let ctx = egui::Context::default();
        let mut filter = if choice == StatusFilter::All {
            StatusFilter::Error
        } else {
            StatusFilter::All
        };
        frame(&ctx, &mut filter, &torrents, 1080.0, vec![]);
        let (_, response) = frame(&ctx, &mut filter, &torrents, 1080.0, vec![]);
        let pos = response.rect.center();
        for pressed in [true, false] {
            frame(&ctx, &mut filter, &torrents, 1080.0, click(pos, pressed));
        }
        let (output, _) = frame(&ctx, &mut filter, &torrents, 1080.0, vec![]);
        let label = format!("{} ({})", choice.label(), choice.count(&torrents));
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing status choice {label}"));
        for pressed in [true, false] {
            frame(&ctx, &mut filter, &torrents, 1080.0, click(pos, pressed));
        }
        assert_eq!(filter, choice);
    }
}

#[test]
fn summary_stays_inside_the_minimum_window_width_and_reports_zero_visible_rows() {
    for width in [820.0, 1080.0] {
        for mut filter in StatusFilter::ALL {
            let ctx = egui::Context::default();
            let torrents = torrents();
            for _ in 0..2 {
                frame(&ctx, &mut filter, &torrents, width, vec![]);
            }
            let (output, response) = frame(&ctx, &mut filter, &torrents, width, vec![]);
            assert!(response.rect.left() >= 250.0 && response.rect.right() <= width);
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    let bounds = text.visual_bounding_rect();
                    assert!(bounds.left() >= 0.0);
                    assert!(
                        bounds.right() <= width,
                        "Header overflows at {width}: {}",
                        text.galley.job.text
                    );
                }
            }
            let expected = if filter == StatusFilter::All {
                "1 active / 3 total".into()
            } else {
                format!("1 active / 3 total / {} shown", filter.count(&torrents))
            };
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == expected)));
        }
    }
}
