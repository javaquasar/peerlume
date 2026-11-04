//! Headless layout and interaction regressions for the real torrent header.
use super::*;
use crate::session::SessionEvent;

fn frame(
    context: &egui::Context,
    width: f32,
    name: &str,
    state: TorrentState,
    tab: &mut DetailsTab,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<HeaderAction>) {
    let mut command = None;
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 1000.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // A horizontal parent must not put title and controls together.
                ui.horizontal(|ui| {
                    let mut torrent = crate::client::tests::torrent();
                    torrent.name = name.into();
                    torrent.state = state;
                    command = PeerLumeApp::show_torrent_header(ui, &torrent, tab);
                });
                ui.label("Details content");
            });
        },
    );
    (output, command)
}

fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing rendered label: {label}"))
}

#[test]
fn title_and_every_control_always_use_separate_rows_at_all_window_widths() {
    let names = [
        "Short".to_owned(),
        "A long torrent filename with several words and more information ".repeat(4),
    ];
    for width in [220.0, 420.0, 1000.0, 1800.0] {
        for name in &names {
            for (state, button) in [
                (TorrentState::Paused, "Resume"),
                (TorrentState::Downloading, "Pause"),
            ] {
                let context = egui::Context::default();
                let mut tab = DetailsTab::Log;
                frame(&context, width, name, state, &mut tab, vec![]);
                let (output, command) = frame(&context, width, name, state, &mut tab, vec![]);
                assert_eq!(command, None);
                assert_eq!(tab, DetailsTab::Log);
                let title = text_rect(&output, name);
                let body = text_rect(&output, "Details content");
                for label in
                    std::iter::once(button).chain(DetailsTab::ALL.iter().map(|tab| tab.label()))
                {
                    let control = text_rect(&output, label);
                    assert!(
                        control.top() >= title.bottom(),
                        "{label} shares the title row at width {width}"
                    );
                    assert!(control.right() <= width, "{label} overflows width {width}");
                    assert!(
                        body.top() >= control.bottom(),
                        "Details overlap {label} at width {width}"
                    );
                }
                if width == 220.0 {
                    assert!(text_rect(&output, "Status").top() > text_rect(&output, button).top());
                    if name.len() > 20 {
                        assert!(title.height() > 30.0, "Long title did not wrap");
                    }
                }
            }
        }
    }
}

#[test]
fn separate_control_rows_keep_pause_resume_and_every_tab_interactive() {
    for (state, button, expected) in [
        (TorrentState::Paused, "Resume", TorrentCommand::Resume),
        (TorrentState::Downloading, "Pause", TorrentCommand::Pause),
    ] {
        let context = egui::Context::default();
        let mut tab = DetailsTab::Files;
        for target in DetailsTab::ALL {
            frame(&context, 220.0, "Short", state, &mut tab, vec![]);
            let (output, _) = frame(&context, 220.0, "Short", state, &mut tab, vec![]);
            let pos = text_rect(&output, target.label()).center();
            for pressed in [true, false] {
                let (_, command) = frame(
                    &context,
                    220.0,
                    "Short",
                    state,
                    &mut tab,
                    click(pos, pressed),
                );
                assert_eq!(command, None, "Tab navigation sent a transfer command");
            }
            assert_eq!(tab, target);
        }
        let (output, _) = frame(&context, 220.0, "Short", state, &mut tab, vec![]);
        let pos = text_rect(&output, button).center();
        let (_, command) = frame(&context, 220.0, "Short", state, &mut tab, click(pos, true));
        assert_eq!(command, None);
        let (_, command) = frame(&context, 220.0, "Short", state, &mut tab, click(pos, false));
        assert_eq!(command, Some(HeaderAction::Transfer(expected)));
        assert_eq!(tab, DetailsTab::Status);
    }
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

fn feedback_frame(
    context: &egui::Context,
    width: f32,
    torrent: &TorrentItem,
    tab: &mut DetailsTab,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Option<HeaderAction>) {
    let mut action = None;
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 1200.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    action = PeerLumeApp::show_torrent_header(ui, torrent, tab);
                });
                ui.label("Details content");
            });
        },
    );
    (output, action)
}

#[test]
fn feedback_is_visible_on_every_tab_and_pending_pause_cannot_be_double_clicked() {
    let mut torrent = crate::client::tests::torrent();
    torrent.state = TorrentState::Downloading;
    torrent
        .runtime
        .command
        .begin(crate::runtime_status::TransferAction::Pause);
    for width in [220.0, 420.0, 1000.0] {
        let context = egui::Context::default();
        for mut tab in DetailsTab::ALL {
            feedback_frame(&context, width, &torrent, &mut tab, vec![]);
            let (output, _) = feedback_frame(&context, width, &torrent, &mut tab, vec![]);
            let button = text_rect(&output, "Pausing...");
            let message = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.job.text.starts_with("Pause requested") =>
                    {
                        Some(text.galley.job.text.clone())
                    }
                    _ => None,
                })
                .expect(
                    "Pending pause feedback must remain visible even after the five-second hint",
                );
            let feedback = text_rect(&output, &message);
            assert!(feedback.bottom() <= text_rect(&output, "Details content").top());
            for pressed in [true, false] {
                assert_eq!(
                    feedback_frame(
                        &context,
                        width,
                        &torrent,
                        &mut tab,
                        click(button.center(), pressed)
                    )
                    .1,
                    None
                );
            }
        }
    }
    let id = torrent.info_hashes.join(";");
    crate::client::apply_session_event(
        std::slice::from_mut(&mut torrent),
        SessionEvent::StateChanged {
            id,
            state: "paused",
        },
    );
    let context = egui::Context::default();
    let mut tab = DetailsTab::Files;
    feedback_frame(&context, 420.0, &torrent, &mut tab, vec![]);
    let (output, _) = feedback_frame(&context, 420.0, &torrent, &mut tab, vec![]);
    text_rect(&output, "Resume");
    text_rect(&output, "Paused - confirmed by engine");
}

#[test]
fn failed_start_shows_its_reason_and_retry_instead_of_pause_on_every_tab() {
    let mut torrent = crate::client::tests::torrent();
    torrent.state = TorrentState::Error;
    let reason =
        "Source .torrent file was not found. Check its path or reconnect the source drive.";
    torrent.runtime.record_error(reason);
    for width in [220.0, 420.0, 1000.0] {
        let context = egui::Context::default();
        for mut tab in DetailsTab::ALL {
            feedback_frame(&context, width, &torrent, &mut tab, vec![]);
            let (output, _) = feedback_frame(&context, width, &torrent, &mut tab, vec![]);
            let button = text_rect(&output, "Retry start");
            text_rect(&output, "Transfer error");
            let error = text_rect(&output, reason);
            assert!(
                error.right() <= width
                    && error.bottom() <= text_rect(&output, "Details content").top()
            );
            feedback_frame(
                &context,
                width,
                &torrent,
                &mut tab,
                click(button.center(), true),
            );
            assert_eq!(
                feedback_frame(
                    &context,
                    width,
                    &torrent,
                    &mut tab,
                    click(button.center(), false)
                )
                .1,
                Some(HeaderAction::RetryStart)
            );
        }
    }
}

#[test]
fn failed_registered_transfer_offers_repair_and_unconfirmed_restore_cannot_retry() {
    let mut torrent = crate::client::tests::torrent();
    torrent.state = TorrentState::Error;
    assert!(transfer_feedback::primary_control(&torrent).1.is_none());
    torrent
        .runtime
        .record_error("Download directory cannot be read");
    assert_eq!(
        transfer_feedback::primary_control(&torrent).1,
        Some(HeaderAction::RetryStart)
    );
    torrent.runtime.registered = true;
    assert_eq!(
        transfer_feedback::primary_control(&torrent).1,
        Some(HeaderAction::Transfer(TorrentCommand::RecheckAndRepair))
    );
}
