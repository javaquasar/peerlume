use super::*;
use crate::control::frontend::{NativeOperation, NativeOperationPage, NativeOperationProgress};
use std::sync::Arc;

fn connection(owner: &str) -> ConnectionView {
    ConnectionView {
        status: ConnectionStatus::Connected,
        owner_instance: Some(owner.into()),
        snapshot_serial: 1,
    }
}

fn view(
    owner: &str,
    before: Option<u64>,
    next: Option<u64>,
    state: NativeOperationState,
) -> OperationView {
    let sequence = before.unwrap_or(10) - 1;
    OperationView {
        before,
        owner_instance: owner.into(),
        stale: false,
        page: Arc::new(NativeOperationPage {
            next_before: next,
            entries: vec![NativeOperation {
                id: format!("operation-{sequence}"),
                sequence,
                owner_instance: owner.into(),
                request_id: format!("request-{sequence}"),
                target: Some("hash-a".into()),
                command: "Copy data".into(),
                revision: Some(7),
                state,
                progress: Some(NativeOperationProgress {
                    bytes: 256,
                    total: Some(512),
                }),
                message: Some("Review https://user:SECRET@host.invalid/private".into()),
                created_unix_ms: 100,
            }],
        }),
    }
}

fn frame(
    ctx: &egui::Context,
    panel: &mut Panel,
    connection: &ConnectionView,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<HistoryIntent>) {
    let mut intents = Vec::new();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                intents = panel.show(ui, connection, 500.0, |id| {
                    (id == "hash-a").then_some("Fixture torrent")
                });
            });
        },
    );
    (output, intents)
}

fn label(output: &egui::FullOutput, name: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|shape| {
        if let egui::epaint::Shape::Text(text) = &shape.shape
            && text.galley.job.text == name
        {
            return Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
        }
        None
    })
}

fn click(
    ctx: &egui::Context,
    panel: &mut Panel,
    connection: &ConnectionView,
    name: &str,
) -> Vec<HistoryIntent> {
    frame(ctx, panel, connection, Vec::new());
    let (output, _) = frame(ctx, panel, connection, Vec::new());
    let point = label(&output, name)
        .unwrap_or_else(|| panic!("Missing {name}"))
        .center();
    frame(
        ctx,
        panel,
        connection,
        vec![egui::Event::PointerMoved(point)],
    );
    let mut intents = Vec::new();
    for pressed in [true, false] {
        intents.extend(
            frame(
                ctx,
                panel,
                connection,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            )
            .1,
        );
    }
    intents
}

#[test]
fn owner_page_fences_cursors_and_displays_only_the_acknowledged_phase() {
    let ctx = egui::Context::default();
    let connection = connection("owner-a");
    let mut panel = Panel::default();
    panel.observe(
        &connection,
        Some(view(
            "owner-a",
            None,
            Some(9),
            NativeOperationState::Accepted,
        )),
    );
    let (output, _) = frame(&ctx, &mut panel, &connection, Vec::new());
    assert!(label(&output, "Accepted. Awaiting engine confirmation.").is_some());
    assert!(label(&output, "Succeeded").is_none());
    assert_eq!(
        click(&ctx, &mut panel, &connection, "Focus torrent"),
        [HistoryIntent::FocusTorrent("hash-a".into())]
    );
    assert!(click(&ctx, &mut panel, &connection, "Older").is_empty());
    assert_eq!(panel.before, Some(9));

    panel.observe(
        &connection,
        Some(view("owner-a", None, None, NativeOperationState::Succeeded)),
    );
    let (output, _) = frame(&ctx, &mut panel, &connection, Vec::new());
    assert!(label(&output, "Succeeded").is_none());
    assert!(
        label(
            &output,
            "Loading the requested page from the background owner..."
        )
        .is_some()
    );
    panel.observe(
        &connection,
        Some(view(
            "owner-a",
            Some(9),
            None,
            NativeOperationState::EffectConfirmed,
        )),
    );
    let (output, _) = frame(&ctx, &mut panel, &connection, Vec::new());
    assert!(label(&output, "Engine effect confirmed. Awaiting persistence.").is_some());
    assert!(label(&output, "Succeeded").is_none());
    click(&ctx, &mut panel, &connection, "Newer");
    assert_eq!(panel.before, None);
}

#[test]
fn stale_and_replaced_owner_pages_retain_uncertainty_and_cannot_focus_old_targets() {
    let ctx = egui::Context::default();
    let owner = connection("owner-a");
    let mut panel = Panel::default();
    let mut stale = view("owner-a", None, Some(9), NativeOperationState::Running);
    stale.stale = true;
    panel.observe(&owner, Some(stale));
    let (output, _) = frame(&ctx, &mut panel, &owner, Vec::new());
    assert!(label(&output, "Outcome unknown in this view").is_some());
    assert!(click(&ctx, &mut panel, &owner, "Focus torrent").is_empty());
    click(&ctx, &mut panel, &owner, "Older");
    assert_eq!(panel.before, None);

    let replacement = connection("owner-b");
    panel.observe(
        &replacement,
        Some(view("owner-a", None, None, NativeOperationState::Succeeded)),
    );
    assert!(panel.page.is_none());
    assert_eq!(
        panel.previous_owner.as_ref().unwrap().owner_instance,
        "owner-a"
    );
    assert_eq!(
        panel.previous_owner.as_ref().unwrap().page.entries[0].state,
        NativeOperationState::Running
    );
}

#[test]
fn failed_owner_row_copies_a_sanitized_error() {
    let ctx = egui::Context::default();
    let connection = connection("owner-a");
    let mut panel = Panel::default();
    panel.observe(
        &connection,
        Some(view("owner-a", None, None, NativeOperationState::Failed)),
    );
    assert_eq!(
        click(&ctx, &mut panel, &connection, "Copy error"),
        [HistoryIntent::CopyError(
            "Review [redacted endpoint]".into()
        )]
    );
}
