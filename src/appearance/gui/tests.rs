use super::*;
use std::collections::BTreeMap;

fn frame(
    ctx: &egui::Context,
    state: &mut AppearanceState,
    settings: &AppearanceSettings,
    native: f32,
    theme: egui::Theme,
) -> bool {
    let mut changed = false;
    let mut input = egui::RawInput {
        system_theme: Some(theme),
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(native);
    let _ = ctx.run(input, |ctx| {
        changed = state.apply(ctx, settings);
    });
    changed
}
#[test]
fn repeated_application_and_native_dpi_changes_do_not_compound_zoom_or_spacing() {
    let ctx = egui::Context::default();
    let mut state = AppearanceState::default();
    let mut settings = AppearanceSettings {
        zoom_percent: 150,
        ..Default::default()
    };
    for _ in 0..20 {
        frame(&ctx, &mut state, &settings, 2.0, egui::Theme::Light);
    }
    assert_eq!(state.rebuild_count(), 1);
    assert!((ctx.zoom_factor() - 1.5).abs() < 0.001);
    assert!((ctx.pixels_per_point() - 3.0).abs() < 0.001);
    assert!(!frame(&ctx, &mut state, &settings, 2.0, egui::Theme::Light));
    assert!(frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Light));
    assert!((ctx.zoom_factor() - 1.5).abs() < 0.001);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        assert_eq!(ctx.style_of(theme).spacing.interact_size.y, 28.0);
    }
    settings.density = Density::Compact;
    frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Light);
    assert_eq!(state.rebuild_count(), 2);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        assert_eq!(ctx.style_of(theme).spacing.interact_size.y, 22.0);
    }
}
#[test]
fn system_theme_changes_without_rebuilding_the_same_preferences() {
    let ctx = egui::Context::default();
    let mut state = AppearanceState::default();
    let mut settings = AppearanceSettings::default();
    frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Light);
    assert!(!ctx.style().visuals.dark_mode);
    frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Dark);
    assert!(ctx.style().visuals.dark_mode);
    assert_eq!(state.rebuild_count(), 1);
    settings.theme = ThemePreference::Light;
    frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Dark);
    assert!(!ctx.style().visuals.dark_mode);
    settings.theme = ThemePreference::Dark;
    frame(&ctx, &mut state, &settings, 1.0, egui::Theme::Light);
    assert!(ctx.style().visuals.dark_mode);
}

fn controls_frame(
    ctx: &egui::Context,
    settings: &mut AppearanceSettings,
    events: Vec<egui::Event>,
) -> BTreeMap<Control, egui::Rect> {
    let mut trace = BTreeMap::new();
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 350.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                controls_inner(ui, settings, |id, rect| {
                    trace.insert(id, rect);
                });
            });
        },
    );
    trace
}
fn click(
    ctx: &egui::Context,
    settings: &mut AppearanceSettings,
    pos: egui::Pos2,
) -> BTreeMap<Control, egui::Rect> {
    controls_frame(
        ctx,
        settings,
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
    controls_frame(
        ctx,
        settings,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    )
}
#[test]
fn controls_drive_theme_density_zoom_and_reset_with_real_input() {
    let ctx = egui::Context::default();
    let mut settings = AppearanceSettings::default();
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::Compact].center());
    assert_eq!(settings.density, Density::Compact);
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::ThemeMenu].center());
    controls_frame(&ctx, &mut settings, vec![]);
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::Dark].center());
    assert_eq!(settings.theme, ThemePreference::Dark);
    for (choice, expected) in [
        (Control::Light, ThemePreference::Light),
        (Control::System, ThemePreference::System),
        (Control::Dark, ThemePreference::Dark),
    ] {
        let trace = controls_frame(&ctx, &mut settings, vec![]);
        click(&ctx, &mut settings, trace[&Control::ThemeMenu].center());
        controls_frame(&ctx, &mut settings, vec![]);
        let trace = controls_frame(&ctx, &mut settings, vec![]);
        click(&ctx, &mut settings, trace[&choice].center());
        assert_eq!(settings.theme, expected);
    }
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::Comfortable].center());
    assert_eq!(settings.density, Density::Comfortable);
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::Compact].center());
    assert_eq!(settings.density, Density::Compact);
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(
        &ctx,
        &mut settings,
        egui::pos2(
            trace[&Control::Zoom].left() + 80.0,
            trace[&Control::Zoom].center().y,
        ),
    );
    assert!(settings.zoom_percent > 100);
    assert!((MIN_ZOOM_PERCENT..=MAX_ZOOM_PERCENT).contains(&settings.zoom_percent));
    let trace = controls_frame(&ctx, &mut settings, vec![]);
    click(&ctx, &mut settings, trace[&Control::Reset].center());
    assert_eq!(settings, AppearanceSettings::default());
}
