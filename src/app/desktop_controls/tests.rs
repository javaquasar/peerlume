use super::*;

#[derive(Clone, Copy)]
enum Surface {
    Toolbar,
    Footer,
    Remove,
    Stop,
    Magnet,
    Appearance,
}
struct Fixture {
    props: ToolbarProps,
    surface: Surface,
    text: String,
    open: bool,
    actions: Vec<DesktopAction>,
    decision: Option<Decision>,
    appearance: AppearanceSettings,
}
impl Fixture {
    fn new(surface: Surface) -> Self {
        Self {
            props: ToolbarProps {
                connected: true,
                visible_selected: 1,
                hidden_selected: 2,
                visible_torrents: 3,
                total_torrents: 9,
                can_pause: true,
                can_resume: true,
                one_paused: true,
                can_cancel_copy: true,
            },
            surface,
            text: String::new(),
            open: true,
            actions: Vec::new(),
            decision: None,
            appearance: AppearanceSettings::default(),
        }
    }
}
fn frame(
    ctx: &egui::Context,
    f: &mut Fixture,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    f.actions.clear();
    f.decision = None;
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| match f.surface {
                Surface::Toolbar => f.actions.extend(toolbar(ui, &mut f.text, f.props)),
                Surface::Footer => f.actions.extend(footer(
                    ui,
                    FooterProps {
                        connection: if f.props.connected {
                            "Background client connected"
                        } else {
                            "Background client disconnected"
                        },
                        download_rate: 2048,
                        upload_rate: 1024,
                        connected: f.props.connected,
                        can_stop: true,
                        operation_count: 7,
                    },
                )),
                _ => {}
            });
            match f.surface {
                Surface::Remove => {
                    f.decision = remove_confirmation(
                        ctx,
                        &[
                            RemovalTarget {
                                id: "frozen-id-a".into(),
                                name: "Torrent Alpha".into(),
                            },
                            RemovalTarget {
                                id: "frozen-id-b".into(),
                                name: "Torrent Beta".into(),
                            },
                        ],
                        &mut f.open,
                    )
                }
                Surface::Stop => {
                    f.decision = stop_confirmation(ctx, "owner-identity-1234567890", &mut f.open)
                }
                Surface::Magnet => f.actions.extend(magnet_dialog(
                    ctx,
                    &mut f.text,
                    &mut f.open,
                    f.props.connected,
                )),
                Surface::Appearance => {
                    f.actions
                        .extend(appearance_dialog(ctx, &mut f.appearance, &mut f.open))
                }
                _ => {}
            }
        },
    )
}
fn settle(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2) -> egui::FullOutput {
    for _ in 0..3 {
        frame(ctx, f, size, Vec::new());
    }
    frame(ctx, f, size, Vec::new())
}
fn text_rect(out: &egui::FullOutput, label: &str) -> egui::Rect {
    out.shapes
        .iter()
        .rev()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing text {label}"))
}
fn click(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2, pos: egui::Pos2) {
    frame(ctx, f, size, vec![egui::Event::PointerMoved(pos)]);
    for pressed in [true, false] {
        frame(
            ctx,
            f,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn click_text(ctx: &egui::Context, f: &mut Fixture, size: egui::Vec2, label: &str) {
    let out = settle(ctx, f, size);
    click(ctx, f, size, text_rect(&out, label).center());
}

#[test]
fn every_toolbar_and_overflow_action_emits_its_exact_intent() {
    let size = egui::vec2(1100.0, 1600.0);
    for (label, expected, more) in [
        ("+ Add torrent", DesktopAction::AddTorrent, false),
        ("Add magnet", DesktopAction::AddMagnet, false),
        ("Pause", DesktopAction::PauseSelected, false),
        ("Resume", DesktopAction::ResumeSelected, false),
        (
            "Remove selected (keep data)",
            DesktopAction::RemoveSelected,
            true,
        ),
        (
            "Force recheck and repair",
            DesktopAction::RecheckSelected,
            true,
        ),
        ("Use existing data...", DesktopAction::UseExistingData, true),
        ("Copy data to...", DesktopAction::CopyData, true),
        ("Fast move directory...", DesktopAction::MoveData, true),
        (
            "Remove duplicate originals...",
            DesktopAction::RemoveCopyOriginals,
            true,
        ),
        ("Cancel copy", DesktopAction::CancelCopy, true),
        ("Pause all", DesktopAction::PauseAll, true),
        ("Resume all", DesktopAction::ResumeAll, true),
        (
            "Clear hidden selection",
            DesktopAction::ClearHiddenSelection,
            true,
        ),
        ("Save view", DesktopAction::SaveView, true),
        ("Restore default columns", DesktopAction::ResetColumns, true),
        ("Appearance...", DesktopAction::Appearance, true),
        ("Speed profiles...", DesktopAction::SpeedProfiles, true),
        ("Agent access...", DesktopAction::AgentAccess, true),
        ("Operations", DesktopAction::ShowOperations, true),
    ] {
        let ctx = egui::Context::default();
        let mut f = Fixture::new(Surface::Toolbar);
        if more {
            click_text(&ctx, &mut f, size, "More");
        }
        click_text(&ctx, &mut f, size, label);
        assert_eq!(f.actions, [expected], "{label}");
    }
}

#[test]
fn offline_and_nonpaused_storage_controls_never_emit_mutations() {
    let size = egui::vec2(1100.0, 1600.0);
    for (label, more) in [
        ("+ Add torrent", false),
        ("Add magnet", false),
        ("Pause", false),
        ("Resume", false),
        ("Remove selected (keep data)", true),
        ("Force recheck and repair", true),
        ("Pause all", true),
        ("Resume all", true),
        ("Save view", true),
        ("Agent access...", true),
        ("Copy data to...", true),
        ("Cancel copy", true),
    ] {
        let ctx = egui::Context::default();
        let mut f = Fixture::new(Surface::Toolbar);
        f.props.connected = false;
        if more {
            click_text(&ctx, &mut f, size, "More");
        }
        click_text(&ctx, &mut f, size, label);
        assert!(f.actions.is_empty(), "offline {label}");
    }
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Toolbar);
    f.props.one_paused = false;
    click_text(&ctx, &mut f, size, "More");
    let out = settle(&ctx, &mut f, size);
    assert!(out.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Text(t)if t.galley.job.text.contains("All torrents: 9 (including hidden rows)"))));
    click_text(&ctx, &mut f, size, "Copy data to...");
    assert!(f.actions.is_empty());
}

#[test]
fn compact_layout_keeps_primary_controls_and_search_inside_narrow_large_text_windows() {
    for width in [220.0, 360.0, 1100.0] {
        for scale in [1.0, 1.8] {
            let ctx = egui::Context::default();
            let mut style = (*ctx.style()).clone();
            for font in style.text_styles.values_mut() {
                font.size *= scale;
            }
            ctx.set_style(style);
            let size = egui::vec2(width, 1200.0);
            let mut f = Fixture::new(Surface::Toolbar);
            f.text = "A long search ".repeat(30);
            let out = settle(&ctx, &mut f, size);
            for label in ["+ Add torrent", "Add magnet", "Pause", "Resume", "More"] {
                let rect = text_rect(&out, label);
                assert!(
                    rect.left() >= 0.0 && rect.right() <= width,
                    "{label} width={width} scale={scale} rect={rect:?}"
                );
                click(&ctx, &mut f, size, rect.center());
                assert!(!f.actions.is_empty() || label == "More");
            }
            let out = settle(&ctx, &mut f, size);
            assert!(
                text_rect(&out, "1 selected visible / 2 hidden · 3 shown / 9 total").right()
                    <= width
            );
        }
    }
}

#[test]
fn footer_distinguishes_close_window_from_confirmed_background_stop() {
    let size = egui::vec2(900.0, 700.0);
    for (label, expected) in [
        ("Operations (7)", DesktopAction::ShowOperations),
        ("Close window", DesktopAction::CloseWindow),
        ("Stop background client...", DesktopAction::StopBackground),
    ] {
        let ctx = egui::Context::default();
        let mut f = Fixture::new(Surface::Footer);
        click_text(&ctx, &mut f, size, label);
        assert_eq!(f.actions, [expected]);
    }
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Footer);
    f.props.connected = false;
    click_text(&ctx, &mut f, size, "Stop background client...");
    assert!(f.actions.is_empty());
    click_text(&ctx, &mut f, size, "Close window");
    assert_eq!(f.actions, [DesktopAction::CloseWindow]);
}

#[test]
fn narrow_enlarged_overflow_scroll_keeps_the_last_action_reachable() {
    let ctx = egui::Context::default();
    let mut style = (*ctx.style()).clone();
    for font in style.text_styles.values_mut() {
        font.size *= 1.8;
    }
    ctx.set_style(style);
    let size = egui::vec2(220.0, 480.0);
    let mut f = Fixture::new(Surface::Toolbar);
    click_text(&ctx, &mut f, size, "More");
    let out = settle(&ctx, &mut f, size);
    let pointer = text_rect(&out, "Remove selected (keep data)").center();
    frame(&ctx, &mut f, size, vec![egui::Event::PointerMoved(pointer)]);
    frame(
        &ctx,
        &mut f,
        size,
        vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -100000.0),
            modifiers: egui::Modifiers::NONE,
        }],
    );
    let out = settle(&ctx, &mut f, size);
    let rect = text_rect(&out, "Operations");
    assert!(rect.left() >= 0.0 && rect.right() <= size.x);
    assert!(rect.top() >= 0.0 && rect.bottom() <= size.y);
    click(&ctx, &mut f, size, rect.center());
    assert_eq!(f.actions, [DesktopAction::ShowOperations]);
}

#[test]
fn remove_and_stop_modals_confirm_frozen_scope_and_cancel_without_an_action() {
    let size = egui::vec2(700.0, 650.0);
    for (surface, confirm) in [
        (Surface::Remove, "Remove from list"),
        (Surface::Stop, "Stop background client"),
    ] {
        for (label, expected) in [("Cancel", Decision::Cancel), (confirm, Decision::Confirm)] {
            let ctx = egui::Context::default();
            let mut f = Fixture::new(surface);
            let out = settle(&ctx, &mut f, size);
            if matches!(surface, Surface::Remove) {
                text_rect(&out, "Torrent Alpha");
                text_rect(&out, "Torrent Beta");
            } else {
                text_rect(&out, "Background owner: owner-identity-1");
            }
            click(&ctx, &mut f, size, text_rect(&out, label).center());
            assert_eq!(f.decision, Some(expected));
            assert!(!f.open);
            assert!(f.actions.is_empty());
        }
        let ctx = egui::Context::default();
        let mut f = Fixture::new(surface);
        settle(&ctx, &mut f, size);
        frame(
            &ctx,
            &mut f,
            size,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(f.decision, Some(Decision::Cancel));
        assert!(!f.open);
    }
}

#[test]
fn magnet_and_appearance_dialogs_emit_only_explicit_add_or_save() {
    let size = egui::vec2(700.0, 650.0);
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Magnet);
    click_text(&ctx, &mut f, size, "Review metadata");
    assert!(f.actions.is_empty());
    assert!(f.open);
    f.text = "magnet:?xt=urn:btih:synthetic".into();
    f.props.connected = false;
    click_text(&ctx, &mut f, size, "Review metadata");
    assert!(f.actions.is_empty());
    f.props.connected = true;
    click_text(&ctx, &mut f, size, "Review metadata");
    assert_eq!(f.actions, [DesktopAction::AddMagnet]);
    assert!(!f.open);
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Magnet);
    f.text = "magnet:?xt=unsent".into();
    click_text(&ctx, &mut f, size, "Cancel");
    assert!(f.actions.is_empty());
    assert!(!f.open);
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Appearance);
    click_text(&ctx, &mut f, size, "Comfortable");
    assert_eq!(f.appearance.density, appearance::Density::Comfortable);
    assert!(f.actions.is_empty());
    click_text(&ctx, &mut f, size, "Save view");
    assert_eq!(f.actions, [DesktopAction::SaveView]);
    assert!(!f.open);
    let ctx = egui::Context::default();
    let mut f = Fixture::new(Surface::Appearance);
    click_text(&ctx, &mut f, size, "Close");
    assert!(f.actions.is_empty());
    assert!(!f.open);
}
