use super::*;
fn frame(
    ctx: &egui::Context,
    history: &OperationHistory,
    events: Vec<egui::Event>,
) -> (Vec<HistoryIntent>, Vec<(Control, egui::Rect)>) {
    let (mut intents, mut trace) = (Vec::new(), Vec::new());
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(420.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                intents = panel_inner(ui, history, 550.0, |id, rect| trace.push((id, rect)));
            });
        },
    );
    (intents, trace)
}
fn click(ctx: &egui::Context, history: &OperationHistory, pos: egui::Pos2) -> Vec<HistoryIntent> {
    frame(
        ctx,
        history,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    frame(
        ctx,
        history,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
    .0
}
#[test]
fn error_controls_emit_only_safe_intents_and_cannot_dismiss_active_work() {
    let ctx = egui::Context::default();
    let mut history = OperationHistory::default();
    let failed = history
        .begin(
            OperationKind::Pause,
            Some("torrent-a"),
            "Synthetic torrent",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    history
        .fail(failed, "Disk error https://host.test/?token=private")
        .unwrap();
    let (_, trace) = frame(&ctx, &history, vec![]);
    let pos = |kind| trace.iter().find(|(id, _)| *id == kind).unwrap().1.center();
    assert_eq!(
        click(&ctx, &history, pos(Control::Focus(failed))),
        vec![HistoryIntent::FocusTorrent("torrent-a".into())]
    );
    let copied = click(&ctx, &history, pos(Control::Copy(failed)));
    assert_eq!(
        copied,
        vec![HistoryIntent::CopyError(
            "Disk error [redacted endpoint]".into()
        )]
    );
    assert_eq!(
        click(&ctx, &history, pos(Control::Dismiss(failed))),
        vec![HistoryIntent::Dismiss(failed)]
    );
    assert_eq!(
        click(&ctx, &history, pos(Control::Clear)),
        vec![HistoryIntent::DismissFinished]
    );
    history.dismiss(failed).unwrap();
    let active = history
        .begin(
            OperationKind::Recheck,
            Some("torrent-a"),
            "Checking",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    let (_, trace) = frame(&ctx, &history, vec![]);
    let pos = trace
        .iter()
        .find(|(id, _)| *id == Control::Dismiss(active))
        .unwrap()
        .1
        .center();
    assert!(click(&ctx, &history, pos).is_empty());
    assert_eq!(history.get(active).unwrap().phase, OperationPhase::Queued);
}
#[test]
fn empty_and_unknown_total_panels_are_renderable_without_speculative_actions() {
    let ctx = egui::Context::default();
    let mut history = OperationHistory::default();
    assert!(frame(&ctx, &history, vec![]).0.is_empty());
    let id = history
        .begin(
            OperationKind::Copy,
            None,
            "Controlled copy",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    history.accepted(id, "owner", Some(1)).unwrap();
    history.progress(id, 4, None, "Copying").unwrap();
    assert!(frame(&ctx, &history, vec![]).0.is_empty());
}

#[test]
fn unconfirmed_dismissal_is_an_explicit_safe_intent_and_does_not_claim_completion() {
    let ctx = egui::Context::default();
    let mut history = OperationHistory::default();
    let id = history
        .begin(
            OperationKind::FileSelection,
            Some("hash-a"),
            "Synthetic",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    history
        .needs_review(id, "Final effect not established")
        .unwrap();
    frame(&ctx, &history, vec![]);
    let (_, trace) = frame(&ctx, &history, vec![]);
    let pos = trace
        .iter()
        .find(|(control, _)| *control == Control::AcknowledgeReview(id))
        .unwrap()
        .1
        .center();
    assert_eq!(
        click(&ctx, &history, pos),
        vec![HistoryIntent::AcknowledgeReview(id)]
    );
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::NeedsReview);
}
