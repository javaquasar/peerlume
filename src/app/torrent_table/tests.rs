use super::*;

struct Fixture {
    torrents: Vec<TorrentItem>,
    visible: Vec<usize>,
    owner_connected: bool,
    columns: TableColumns,
    sort: Sort,
    descending: bool,
    name: NameColumn,
    selection: TorrentSelection,
    context_targets: Vec<String>,
    context_hidden: usize,
    context_pointer: usize,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let torrents = (0..count)
            .map(|index| {
                let mut item = crate::client::tests::torrent();
                item.name = format!("Table row {index:04}");
                item.info_hashes = vec![format!("v1: {index:040x}")];
                item.state = crate::torrent::TorrentState::Paused;
                item
            })
            .collect();
        Self {
            torrents,
            visible: (0..count).collect(),
            owner_connected: true,
            columns: Default::default(),
            sort: Sort::Added,
            descending: false,
            name: Default::default(),
            selection: Default::default(),
            context_targets: Vec::new(),
            context_hidden: 0,
            context_pointer: 0,
        }
    }
}
fn frame(
    ctx: &egui::Context,
    fixture: &mut Fixture,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, TableOutput) {
    let modifiers = events
        .iter()
        .find_map(|event| match event {
            egui::Event::PointerButton { modifiers, .. } => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    let mut table = None;
    let Fixture {
        torrents,
        visible,
        owner_connected,
        columns,
        sort,
        descending,
        name,
        selection,
        context_targets,
        context_hidden,
        context_pointer,
    } = fixture;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            modifiers,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                table = Some(show(
                    ui,
                    TableInput {
                        torrents,
                        visible,
                        owner_connected: *owner_connected,
                        filter: StatusFilter::All,
                        columns,
                        sort,
                        descending,
                        name_column: name,
                        selection,
                    },
                    |ui, _index, _item, targets, hidden| {
                        *context_hidden = hidden;
                        *context_pointer = targets.as_ptr() as usize;
                        if ui.button("Selected action").clicked() {
                            *context_targets = targets.to_vec();
                            ui.close();
                        }
                    },
                ));
            });
        },
    );
    (output, table.unwrap())
}
fn settle(
    ctx: &egui::Context,
    fixture: &mut Fixture,
    size: egui::Vec2,
) -> (egui::FullOutput, TableOutput) {
    for _ in 0..3 {
        frame(ctx, fixture, size, Vec::new());
    }
    frame(ctx, fixture, size, Vec::new())
}
fn pointer(
    point: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(point),
        egui::Event::PointerButton {
            pos: point,
            button,
            pressed,
            modifiers,
        },
    ]
}
fn click(
    ctx: &egui::Context,
    fixture: &mut Fixture,
    size: egui::Vec2,
    point: egui::Pos2,
    button: egui::PointerButton,
    modifiers: egui::Modifiers,
) {
    frame(ctx, fixture, size, vec![egui::Event::PointerMoved(point)]);
    frame(ctx, fixture, size, pointer(point, button, true, modifiers));
    frame(ctx, fixture, size, pointer(point, button, false, modifiers));
}
fn header(table: &TableOutput, column: Column) -> egui::Rect {
    table
        .headers
        .iter()
        .find(|(current, _)| *current == column)
        .unwrap()
        .1
}
fn text(output: &egui::FullOutput, label: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .rev()
        .find_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape
                && text.galley.job.text == label
            {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("Missing visible text {label}"))
}

fn has_arrow(output: &egui::FullOutput, rect: egui::Rect, down: bool) -> bool {
    output.shapes.iter().any(|shape| {
        let egui::Shape::LineSegment {
            points: [start, end],
            ..
        } = shape.shape
        else {
            return false;
        };
        start.x == end.x
            && (end.y - start.y) == if down { 9.0 } else { -9.0 }
            && rect.contains(start)
            && rect.contains(end)
            && shape.clip_rect.contains(start)
            && shape.clip_rect.contains(end)
    })
}

#[test]
fn status_arrows_are_painted_in_the_status_or_name_cells() {
    use crate::torrent::TorrentState;

    let ctx = egui::Context::default();
    let size = egui::vec2(1600.0, 700.0);
    let mut fixture = Fixture::new(2);
    fixture.torrents[0].state = TorrentState::Downloading;
    fixture.torrents[1].state = TorrentState::Seeding;
    for column in [Column::Status, Column::Name] {
        fixture
            .columns
            .set_visible(Column::Status, column == Column::Status);
        let (output, table) = settle(&ctx, &mut fixture, size);
        let cells: Vec<_> = table
            .cells
            .iter()
            .filter_map(|(_, key, rect)| (*key == column).then_some(*rect))
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(has_arrow(&output, cells[0], true));
        assert!(has_arrow(&output, cells[1], false));
        if column == Column::Status {
            assert!(cells[0].contains(text(&output, &fixture.torrents[0].status_text()).center()));
            assert!(cells[1].contains(text(&output, &fixture.torrents[1].status_text()).center()));
        }
    }
}

#[test]
fn status_hover_explains_confirmed_queue_wait_and_marks_a_disconnected_view() {
    use crate::torrent::TorrentState;

    let ctx = egui::Context::default();
    ctx.style_mut(|style| {
        style.interaction.tooltip_delay = 0.0;
        style.interaction.show_tooltips_only_when_still = false;
    });
    let mut fixture = Fixture::new(1);
    let torrent = &mut fixture.torrents[0];
    torrent.state = TorrentState::Downloading;
    torrent.runtime.registered = true;
    torrent.runtime.diagnostics.queue_waiting = true;
    let label = torrent.status_text();
    let size = egui::vec2(1600.0, 700.0);
    let (output, _) = settle(&ctx, &mut fixture, size);
    let point = text(&output, &label).center();
    frame(
        &ctx,
        &mut fixture,
        size,
        vec![egui::Event::PointerMoved(point)],
    );
    let (output, _) = settle(&ctx, &mut fixture, size);
    text(
        &output,
        "Waiting for a download queue slot (engine-confirmed)",
    );
    text(
        &output,
        "Wait for an active slot or review the configured download queue limit.",
    );

    fixture.owner_connected = false;
    let (output, _) = settle(&ctx, &mut fixture, size);
    text(
        &output,
        "Background client disconnected. Showing last known data.",
    );
}
fn drag(
    ctx: &egui::Context,
    fixture: &mut Fixture,
    size: egui::Vec2,
    start: egui::Pos2,
    end: egui::Pos2,
) {
    frame(
        ctx,
        fixture,
        size,
        pointer(
            start,
            egui::PointerButton::Primary,
            true,
            egui::Modifiers::NONE,
        ),
    );
    frame(
        ctx,
        fixture,
        size,
        vec![egui::Event::PointerMoved(start + egui::vec2(10.0, 0.0))],
    );
    frame(ctx, fixture, size, vec![egui::Event::PointerMoved(end)]);
    frame(
        ctx,
        fixture,
        size,
        pointer(
            end,
            egui::PointerButton::Primary,
            false,
            egui::Modifiers::NONE,
        ),
    );
}

#[test]
fn every_header_sorts_and_toggles_without_reordering_canonical_items() {
    let ctx = egui::Context::default();
    let mut fixture = Fixture::new(4);
    let size = egui::vec2(1920.0, 560.0);
    let original: Vec<_> = fixture
        .torrents
        .iter()
        .map(|item| item.info_hashes.clone())
        .collect();
    for column in Column::ALL {
        let (_, table) = settle(&ctx, &mut fixture, size);
        let point = header(&table, column).center();
        click(
            &ctx,
            &mut fixture,
            size,
            point,
            egui::PointerButton::Primary,
            egui::Modifiers::NONE,
        );
        assert_eq!(
            fixture.sort,
            column.sort(),
            "column={column:?}, headers={:?}, order={:?}",
            table.headers,
            fixture.columns.order
        );
        assert!(!fixture.descending);
        let (_, table) = settle(&ctx, &mut fixture, size);
        click(
            &ctx,
            &mut fixture,
            size,
            header(&table, column).center(),
            egui::PointerButton::Primary,
            egui::Modifiers::NONE,
        );
        assert!(fixture.descending);
        let (out, table) = settle(&ctx, &mut fixture, size);
        assert!(has_arrow(&out, header(&table, column), true));
    }
    assert_eq!(
        fixture
            .torrents
            .iter()
            .map(|item| item.info_hashes.clone())
            .collect::<Vec<_>>(),
        original
    );
}

#[test]
fn every_column_can_resize_including_the_last_edge_and_name_still_fills_growth() {
    let size = egui::vec2(1920.0, 560.0);
    for column in Column::ALL {
        let ctx = egui::Context::default();
        let mut fixture = Fixture::new(8);
        let (_, before) = settle(&ctx, &mut fixture, size);
        let cell = header(&before, column);
        let last = column == Column::Ratio;
        let start = egui::pos2(
            cell.right() + if last { -4.0 } else { 4.0 },
            cell.center().y,
        );
        drag(
            &ctx,
            &mut fixture,
            size,
            start,
            start + egui::vec2(if column == Column::Name { -50.0 } else { 50.0 }, 0.0),
        );
        let (_, after) = settle(&ctx, &mut fixture, size);
        if column == Column::Name {
            assert!(fixture.name.saved_gap() > 0);
            assert!(header(&after, column).width() < cell.width());
        } else {
            assert!(
                fixture.columns.widths.contains_key(&column),
                "{column:?} resize was unreachable"
            );
            assert!(
                header(&after, column).width() > cell.width(),
                "{column:?} width did not grow"
            );
        }
    }
    let ctx = egui::Context::default();
    let mut fixture = Fixture::new(3);
    let (_, small) = settle(&ctx, &mut fixture, egui::vec2(1080.0, 560.0));
    let (_, large) = settle(&ctx, &mut fixture, size);
    assert!(header(&large, Column::Name).width() > header(&small, Column::Name).width());
    assert!((header(&large, Column::Ratio).right() - (size.x - 8.0)).abs() < 20.0);
}

#[test]
fn pointer_reorder_and_accessible_move_hide_auto_reset_controls_work() {
    let ctx = egui::Context::default();
    let size = egui::vec2(1920.0, 700.0);
    let mut fixture = Fixture::new(5);
    let (_, table) = settle(&ctx, &mut fixture, size);
    let source = header(&table, Column::Ratio).center();
    let target = header(&table, Column::Name);
    drag(
        &ctx,
        &mut fixture,
        size,
        source,
        egui::pos2(target.left() + 10.0, target.center().y),
    );
    assert_eq!(fixture.columns.order[0], Column::Ratio);
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        header(&table, Column::Ratio).center(),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    let (out, _) = settle(&ctx, &mut fixture, size);
    let point = text(&out, "Move right").center();
    click(
        &ctx,
        &mut fixture,
        size,
        point,
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert_eq!(fixture.columns.order[1], Column::Ratio);
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        header(&table, Column::Name).center(),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    let (out, _) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        text(&out, "Peers").center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert!(!fixture.columns.is_visible(Column::Peers));
    assert!(fixture.columns.is_visible(Column::Name));
    click(
        &ctx,
        &mut fixture,
        size,
        egui::pos2(size.x - 30.0, size.y - 30.0),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    fixture.columns.set_width(Column::Progress, 280);
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        header(&table, Column::Progress).center(),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    let (out, _) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        text(&out, "Automatic width").center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert!(!fixture.columns.widths.contains_key(&Column::Progress));
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        header(&table, Column::Name).center(),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    let (out, _) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        text(&out, "Restore default columns").center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert_eq!(fixture.columns, TableColumns::default());
    assert_eq!(fixture.name.saved_gap(), 0);
}

#[test]
fn ctrl_shift_context_and_hidden_scope_are_driven_by_real_row_input() {
    let ctx = egui::Context::default();
    let size = egui::vec2(1080.0, 600.0);
    let mut fixture = Fixture::new(10);
    let (_, table) = settle(&ctx, &mut fixture, size);
    let id = |index: usize| format!("v1: {index:040x}");
    click(
        &ctx,
        &mut fixture,
        size,
        table.rows[1].1.center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        table.rows[3].1.center(),
        egui::PointerButton::Primary,
        egui::Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    assert!(fixture.selection.is_selected(&id(1)));
    assert!(fixture.selection.is_selected(&id(3)));
    assert_eq!(fixture.selection.len(), 2);
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        table.rows[5].1.center(),
        egui::PointerButton::Primary,
        egui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        fixture.selection.selected_ids().collect::<Vec<_>>(),
        [id(3), id(4), id(5)]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    fixture.visible = vec![5, 3];
    let (_, table) = settle(&ctx, &mut fixture, size);
    click(
        &ctx,
        &mut fixture,
        size,
        table.rows[0].1.center(),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    assert_eq!(fixture.selection.len(), 3);
    assert_eq!(fixture.context_hidden, 1);
    let pointer = fixture.context_pointer;
    let (out, _) = settle(&ctx, &mut fixture, size);
    assert_eq!(
        fixture.context_pointer, pointer,
        "open menu reuses canonical target strings on idle frames"
    );
    click(
        &ctx,
        &mut fixture,
        size,
        text(&out, "Selected action").center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert_eq!(fixture.context_targets, [id(5), id(3)]);
    assert!(!fixture.context_targets.contains(&id(4)));
}

#[test]
fn every_optional_column_has_a_working_visibility_control_but_name_cannot_hide() {
    let size = egui::vec2(1920.0, 700.0);
    for column in Column::ALL {
        let ctx = egui::Context::default();
        let mut fixture = Fixture::new(3);
        let (_, table) = settle(&ctx, &mut fixture, size);
        click(
            &ctx,
            &mut fixture,
            size,
            header(&table, Column::Name).center(),
            egui::PointerButton::Secondary,
            egui::Modifiers::NONE,
        );
        let (out, _) = settle(&ctx, &mut fixture, size);
        click(
            &ctx,
            &mut fixture,
            size,
            text(&out, column.label()).center(),
            egui::PointerButton::Primary,
            egui::Modifiers::NONE,
        );
        assert_eq!(fixture.columns.is_visible(column), column == Column::Name);
        if column != Column::Name {
            let (_, table) = settle(&ctx, &mut fixture, size);
            assert!(!table.headers.iter().any(|(shown, _)| *shown == column));
            click(
                &ctx,
                &mut fixture,
                size,
                header(&table, Column::Name).center(),
                egui::PointerButton::Secondary,
                egui::Modifiers::NONE,
            );
            let (out, _) = settle(&ctx, &mut fixture, size);
            click(
                &ctx,
                &mut fixture,
                size,
                text(&out, column.label()).center(),
                egui::PointerButton::Primary,
                egui::Modifiers::NONE,
            );
            assert!(fixture.columns.is_visible(column));
            let (_, table) = settle(&ctx, &mut fixture, size);
            assert!(table.headers.iter().any(|(shown, _)| *shown == column));
        }
    }
}

#[test]
fn sticky_header_shares_horizontal_offset_and_virtual_rows_reach_bottom_right() {
    let ctx = egui::Context::default();
    let size = egui::vec2(820.0, 420.0);
    let mut fixture = Fixture::new(5000);
    for column in Column::ALL {
        if column != Column::Name {
            fixture.columns.set_width(column, 190);
        }
    }
    let (_, before) = settle(&ctx, &mut fixture, size);
    assert!(before.rendered_rows < 40);
    let name_y = header(&before, Column::Name).top();
    frame(
        &ctx,
        &mut fixture,
        size,
        vec![
            egui::Event::PointerMoved(egui::pos2(400.0, 200.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(-1000000.0, -1000000.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let (out, after) = settle(&ctx, &mut fixture, size);
    assert!(after.rendered_rows < 40);
    assert!((header(&after, Column::Ratio).top() - name_y).abs() < 1.0);
    assert!(header(&after, Column::Ratio).right() <= size.x);
    assert!(header(&after, Column::Ratio).right() > size.x - 50.0);
    for (_, column, cell) in &after.cells {
        let header = header(&after, *column);
        assert!((cell.left() - header.left()).abs() < 0.1);
        assert!((cell.width() - header.width()).abs() < 0.1);
    }
    text(&out, "Ratio");
    assert!(
        after
            .rows
            .iter()
            .any(|(id, _)| id == &format!("v1: {:040x}", 4999))
    );
    let last = after
        .rows
        .iter()
        .find(|(id, _)| id == &format!("v1: {:040x}", 4999))
        .unwrap()
        .1;
    click(
        &ctx,
        &mut fixture,
        size,
        last.center(),
        egui::PointerButton::Primary,
        egui::Modifiers::NONE,
    );
    assert!(fixture.selection.is_selected(&format!("v1: {:040x}", 4999)));
    let selected = appearance::row_colors(
        crate::torrent::TorrentState::Paused,
        ctx.style().visuals.dark_mode,
        true,
        true,
    )
    .background
    .color32();
    let (out, _) = settle(&ctx, &mut fixture, size);
    assert!(out.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Rect(rect)if rect.fill==selected&&rect.rect.width()>size.x-30.0)));
}

#[test]
fn a_pressed_identity_that_moves_before_release_is_cancelled_not_retargeted() {
    let ctx = egui::Context::default();
    let mut f = Fixture::new(3);
    let id = |index: usize| format!("v1: {index:040x}");
    f.selection.replace(&id(2));
    let size = egui::vec2(1080.0, 600.0);
    let (_, table) = settle(&ctx, &mut f, size);
    let point = table.rows[0].1.center();
    frame(
        &ctx,
        &mut f,
        size,
        pointer(
            point,
            egui::PointerButton::Primary,
            true,
            egui::Modifiers::NONE,
        ),
    );
    f.visible = vec![1, 0, 2];
    frame(
        &ctx,
        &mut f,
        size,
        pointer(
            point,
            egui::PointerButton::Primary,
            false,
            egui::Modifiers::NONE,
        ),
    );
    assert_eq!(f.selection.focus(), Some(id(2).as_str()));
    assert_eq!(
        f.selection.selected_ids().collect::<Vec<_>>(),
        [id(2).as_str()]
    );
}
