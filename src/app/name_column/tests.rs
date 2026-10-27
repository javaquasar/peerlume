use super::*;
use crate::app::PeerLumeApp;

struct Observation {
    width: f32,
    available: f32,
    reserved: f32,
    table: egui::Rect,
    handle: egui::Pos2,
    name: Option<egui::Response>,
    other_cells: Vec<egui::Rect>,
    viewport: egui::Rect,
    content_width: f32,
    scroll_offset: f32,
}

fn fixture() -> Vec<TorrentItem> {
    let mut torrent = crate::client::tests::torrent();
    torrent.name = "A long torrent filename ".repeat(30);
    torrent.state = crate::torrent::TorrentState::Downloading;
    vec![torrent]
}

fn frame(
    ctx: &egui::Context,
    column: &mut NameColumn,
    window_width: f32,
    torrents: &[TorrentItem],
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Observation) {
    frame_with_toolbar(ctx, column, window_width, torrents, events, false)
}

fn frame_with_toolbar(
    ctx: &egui::Context,
    column: &mut NameColumn,
    window_width: f32,
    torrents: &[TorrentItem],
    events: Vec<egui::Event>,
    oversized_toolbar: bool,
) -> (egui::FullOutput, Observation) {
    let mut observation = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(window_width, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let available = viewport_width(ui);
                if oversized_toolbar {
                    // Reproduce a toolbar/status message wider than the window.
                    // Its overflow must not become the scroll area's viewport.
                    ui.horizontal(|ui| {
                        ui.allocate_exact_size(egui::vec2(2400.0, 20.0), egui::Sense::hover());
                    });
                }
                let reserved = reserved_width(ui, torrents, StatusFilter::All);
                ui.style_mut().spacing.scroll.floating = false;
                let scroll_output = scroll_area(available).show(ui, |ui| {
                    let layout = column.begin(ui, available, reserved);
                    let mut name = None;
                    let mut other_cells = Vec::new();
                    let table = egui::Grid::new("resizable_name_table")
                        .striped(true)
                        .min_col_width(MIN_OTHER_WIDTH)
                        .show(ui, |ui| {
                            headers(ui, layout.width);
                            for torrent in torrents {
                                name = Some(PeerLumeApp::show_torrent_name(
                                    ui,
                                    &torrent.name,
                                    torrent.is_private,
                                    true,
                                    layout.width,
                                ));
                                other_cells.push(
                                    ui.add(
                                        egui::ProgressBar::new(0.5).desired_width(PROGRESS_WIDTH),
                                    )
                                    .rect,
                                );
                                for text in metrics(torrent) {
                                    other_cells.push(ui.label(text).rect);
                                }
                                ui.end_row();
                            }
                        })
                        .response
                        .rect;
                    layout.paint_divider(ui, table);
                    observation = Some(Observation {
                        width: layout.width,
                        available,
                        reserved,
                        table,
                        handle: egui::pos2(
                            table.left() + layout.width + layout.gap * 0.5,
                            table.top() + 10.0,
                        ),
                        name,
                        other_cells,
                        viewport: egui::Rect::NOTHING,
                        content_width: 0.0,
                        scroll_offset: 0.0,
                    });
                });
                let view = observation.as_mut().unwrap();
                view.viewport = scroll_output.inner_rect;
                view.content_width = scroll_output.content_size.x;
                view.scroll_offset = scroll_output.state.offset.x;
            });
        },
    );
    (output, observation.unwrap())
}

fn pointer(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
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

fn settle(
    ctx: &egui::Context,
    column: &mut NameColumn,
    width: f32,
    torrents: &[TorrentItem],
) -> Observation {
    frame(ctx, column, width, torrents, vec![]);
    frame(ctx, column, width, torrents, vec![]).1
}

fn drag(
    ctx: &egui::Context,
    column: &mut NameColumn,
    window: f32,
    torrents: &[TorrentItem],
    delta: f32,
) -> Observation {
    let before = settle(ctx, column, window, torrents);
    frame(
        ctx,
        column,
        window,
        torrents,
        pointer(before.handle, egui::PointerButton::Primary, true),
    );
    let target = before.handle + egui::vec2(delta, 0.0);
    frame(
        ctx,
        column,
        window,
        torrents,
        vec![egui::Event::PointerMoved(target)],
    );
    frame(
        ctx,
        column,
        window,
        torrents,
        pointer(target, egui::PointerButton::Primary, false),
    )
    .1
}

fn assert_fits(observation: &Observation) {
    assert!(
        observation.table.width() <= observation.available + 1.0,
        "Table overflows: {:?}; available {}; name width {}; reserved {}; name {:?}; other {:?}",
        observation.table,
        observation.available,
        observation.width,
        observation.reserved,
        observation.name.as_ref().map(|response| response.rect),
        observation.other_cells
    );
    if let Some(name) = &observation.name {
        assert!((name.rect.width() - observation.width).abs() < 1.0);
        assert!(observation.other_cells[0].left() >= name.rect.right());
        for cells in observation.other_cells.windows(2) {
            assert!(cells[1].left() >= cells[0].right());
        }
    }
}

#[test]
fn real_header_drag_grows_shrinks_and_clamps_without_hiding_other_columns() {
    let ctx = egui::Context::default();
    let mut column = NameColumn::default();
    let torrents = fixture();
    let initial = settle(&ctx, &mut column, 1920.0, &torrents);
    assert_eq!(initial.width, DEFAULT_WIDTH);
    let wider = drag(&ctx, &mut column, 1920.0, &torrents, 300.0);
    assert_eq!(
        wider.width,
        DEFAULT_WIDTH + 300.0,
        "Input must not accumulate twice in multi-pass layout"
    );
    assert_fits(&wider);
    let narrower = drag(&ctx, &mut column, 1920.0, &torrents, -200.0);
    assert_eq!(narrower.width, DEFAULT_WIDTH + 100.0);
    assert_fits(&narrower);
    let maximum = drag(&ctx, &mut column, 1920.0, &torrents, 5000.0);
    assert_eq!(maximum.width, maximum.available - maximum.reserved);
    assert_fits(&maximum);
    let minimum = drag(&ctx, &mut column, 1920.0, &torrents, -5000.0);
    assert_eq!(minimum.width, MIN_WIDTH);
    assert_fits(&minimum);
}

#[test]
fn shrinking_window_temporarily_clamps_and_growing_restores_the_chosen_width() {
    let ctx = egui::Context::default();
    let mut column = NameColumn::default();
    let torrents = fixture();
    let chosen = drag(&ctx, &mut column, 1920.0, &torrents, 400.0).width;
    for window in [1080.0, 820.0, 1920.0] {
        let view = settle(&ctx, &mut column, window, &torrents);
        assert_fits(&view);
        assert_eq!(
            view.width,
            chosen.min((view.available - view.reserved).max(MIN_WIDTH))
        );
        assert_eq!(column.preferred_width, chosen);
    }
}

#[test]
fn double_click_resets_and_empty_filtered_lists_still_have_a_resizable_header() {
    let ctx = egui::Context::default();
    let mut column = NameColumn::default();
    let resized = drag(&ctx, &mut column, 1920.0, &[], 200.0);
    assert_eq!(resized.width, DEFAULT_WIDTH + 200.0);
    for _ in 0..2 {
        frame(
            &ctx,
            &mut column,
            1920.0,
            &[],
            pointer(resized.handle, egui::PointerButton::Primary, true),
        );
        frame(
            &ctx,
            &mut column,
            1920.0,
            &[],
            pointer(resized.handle, egui::PointerButton::Primary, false),
        );
    }
    assert_eq!(settle(&ctx, &mut column, 1920.0, &[]).width, DEFAULT_WIDTH);
}

#[test]
fn long_status_and_counters_are_reserved_before_allowing_a_wider_name() {
    let ctx = egui::Context::default();
    let mut column = NameColumn::default();
    let mut torrents = fixture();
    torrents[0].state = crate::torrent::TorrentState::WaitingForEngine;
    torrents[0].peers = usize::MAX;
    torrents[0].seeds = usize::MAX;
    let wide = drag(&ctx, &mut column, 1920.0, &torrents, 5000.0);
    assert_fits(&wide);
    let narrow = settle(&ctx, &mut column, 820.0, &torrents);
    assert_eq!(narrow.width, MIN_WIDTH);
    assert!(
        narrow.available < narrow.reserved + MIN_WIDTH,
        "Keep horizontal scrolling when the essential columns cannot fit"
    );
}

#[test]
fn variable_width_keeps_left_alignment_truncation_and_full_row_clicks() {
    for width in [MIN_WIDTH, DEFAULT_WIDTH, 1000.0] {
        for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
            let ctx = egui::Context::default();
            let mut column = NameColumn {
                preferred_width: width,
                ..Default::default()
            };
            let torrents = fixture();
            let view = settle(&ctx, &mut column, 1920.0, &torrents);
            let (output, _) = frame(&ctx, &mut column, 1920.0, &torrents, vec![]);
            let name = view.name.unwrap();
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.job.text.contains(&torrents[0].name) =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(text.galley.rows.len(), 1);
            assert!(
                (text.pos.x - name.rect.left() - ctx.style().spacing.button_padding.x).abs() <= 1.0
            );
            assert!(text.visual_bounding_rect().right() <= name.rect.right() + 1.0);
            let pos = egui::pos2(name.rect.right() - 12.0, name.rect.center().y);
            frame(
                &ctx,
                &mut column,
                1920.0,
                &torrents,
                pointer(pos, button, true),
            );
            let clicked = frame(
                &ctx,
                &mut column,
                1920.0,
                &torrents,
                pointer(pos, button, false),
            )
            .1
            .name
            .unwrap();
            assert_eq!(clicked.clicked(), button == egui::PointerButton::Primary);
            assert_eq!(
                clicked.secondary_clicked(),
                button == egui::PointerButton::Secondary
            );
            assert_eq!(column.preferred_width, width);
        }
    }
}

#[test]
fn horizontal_scroll_reaches_ratio_at_every_window_width_despite_toolbar_overflow() {
    fn drag_bar(
        ctx: &egui::Context,
        column: &mut NameColumn,
        width: f32,
        torrents: &[TorrentItem],
        start: egui::Pos2,
        end: egui::Pos2,
    ) {
        frame_with_toolbar(
            ctx,
            column,
            width,
            torrents,
            pointer(start, egui::PointerButton::Primary, true),
            true,
        );
        frame_with_toolbar(
            ctx,
            column,
            width,
            torrents,
            vec![egui::Event::PointerMoved(end)],
            true,
        );
        frame_with_toolbar(
            ctx,
            column,
            width,
            torrents,
            pointer(end, egui::PointerButton::Primary, false),
            true,
        );
    }
    let ctx = egui::Context::default();
    let mut column = NameColumn::default();
    let mut torrents = fixture();
    torrents[0].state = crate::torrent::TorrentState::WaitingForEngine;
    torrents[0].peers = usize::MAX;
    torrents[0].seeds = usize::MAX;
    for window in [1920.0, 1045.0, 820.0, 600.0, 220.0, 820.0, 1920.0] {
        for _ in 0..3 {
            frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true);
        }
        let (output, view) = frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true);
        assert!(
            view.viewport.right() <= window - 7.0,
            "Viewport exceeds visible window at {window}: {:?}",
            view.viewport
        );
        assert!(view.viewport.width() <= view.available + 1.0);
        let bar = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.rect.width() > 20.0
                        && rect.rect.height() <= 14.0
                        && rect.rect.center().y >= view.table.bottom() =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .max_by(|a, b| a.width().total_cmp(&b.width()));
        if view.content_width > view.viewport.width() + 1.0 {
            let bar = bar.unwrap_or_else(|| {
                panic!(
                    "Missing scroll track at {window}; viewport {:?}, table {:?}, rects {:?}",
                    view.viewport,
                    view.table,
                    output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Rect(rect) => Some(rect.rect),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                )
            });
            drag_bar(
                &ctx,
                &mut column,
                window,
                &torrents,
                egui::pos2(bar.left() + 2.0, bar.center().y),
                egui::pos2(bar.right() + 2.0, bar.center().y),
            );
        }
        for _ in 0..3 {
            frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true);
        }
        let right = frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true).1;
        let maximum = (right.content_width - right.viewport.width()).max(0.0);
        assert!(
            (right.scroll_offset - maximum).abs() <= 1.0,
            "Cannot scroll to the true end at {window}: offset {}, max {maximum}",
            right.scroll_offset
        );
        let ratio = right.other_cells.last().unwrap();
        assert!(
            ratio.left() >= right.viewport.left() - 1.0
                && ratio.right() <= right.viewport.right() + 1.0,
            "Ratio is clipped at {window}: {ratio:?}, viewport {:?}",
            right.viewport
        );
        if maximum > 1.0 {
            let bar = bar.unwrap();
            drag_bar(
                &ctx,
                &mut column,
                window,
                &torrents,
                egui::pos2(bar.right() - 2.0, bar.center().y),
                egui::pos2(bar.left() - 2.0, bar.center().y),
            );
        }
        for _ in 0..3 {
            frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true);
        }
        let left = frame_with_toolbar(&ctx, &mut column, window, &torrents, vec![], true).1;
        assert!(
            left.scroll_offset <= 1.0,
            "Cannot return to Name at {window}"
        );
        assert!(left.name.unwrap().rect.left() >= left.viewport.left() - 1.0);
    }
}
