//! Exercise the production screen without opening profiles, sessions or sockets.
use crate::app::*;

fn fixture(count: usize, file_count: usize) -> PeerLumeApp {
    let settings = ClientSettings::default();
    let torrents = (0..count)
        .map(|index| {
            let mut torrent = crate::client::tests::torrent();
            torrent.name = format!("Torrent {index:03}");
            torrent.info_hashes = vec![format!("v1: {index:040x}")];
            torrent.state = TorrentState::Downloading;
            torrent.files = FileSelectionModel::from_files(
                (0..file_count).map(|file| (format!("file-{file:04}.bin"), 100)),
            );
            torrent
        })
        .collect();
    PeerLumeApp {
        log_offset: 0,
        log_torrent: String::new(),
        page: MainPage::Torrents,
        details_tab: DetailsTab::Files,
        preferences_tab: Default::default(),
        selected_torrent: 0,
        status_filter: Default::default(),
        name_column: Default::default(),
        settings: settings.clone(),
        saved_settings: settings.clone(),
        settings_draft: settings.clone(),
        settings_status: String::new(),
        validation_messages: vec![],
        engine: EngineRuntime::new(settings),
        blocklist: Default::default(),
        blocklist_status: String::new(),
        blocklist_update: None,
        blocklist_probe: String::new(),
        blocklist_probe_result: String::new(),
        add_dialog: None,
        last_download_directory: None,
        pending_torrent_paths: Default::default(),
        add_status: String::new(),
        file_navigation: Default::default(),
        magnet_input: String::new(),
        control: Controller::new(ClientBackend::offline(), torrents),
        tracker_audits: Default::default(),
        behavior_catalog: Err("Not used by layout tests".into()),
        trash_confirmation: None,
        pending_trash: None,
    }
}

fn frame(
    ctx: &egui::Context,
    app: &mut PeerLumeApp,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| app.show_torrents(ctx),
    )
}

fn settle(ctx: &egui::Context, app: &mut PeerLumeApp, size: egui::Vec2) -> egui::FullOutput {
    for _ in 0..4 {
        frame(ctx, app, size, vec![]);
    }
    frame(ctx, app, size, vec![])
}

fn pane(ctx: &egui::Context) -> egui::Rect {
    egui::containers::panel::PanelState::load(ctx, egui::Id::new("torrent_list_pane"))
        .unwrap()
        .rect
}

fn border(output: &egui::FullOutput, below: f32) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.stroke.width > 0.0
                    && rect.rect.width() > 500.0
                    && rect.rect.top() > below =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .expect("file-list border below the divider")
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
        .unwrap_or_else(|| panic!("Missing visible text: {label}"))
}

#[test]
fn view_controls_wrap_inside_the_actual_window_even_with_an_overflowing_status_message() {
    for width in [360.0, 600.0, 1080.0] {
        let ctx = egui::Context::default();
        let mut app = fixture(2, 3);
        app.add_status = "Long status ".repeat(80);
        let output = settle(&ctx, &mut app, egui::vec2(width, 1000.0));
        let save = text_rect(&output, "Save view");
        assert!(
            save.left() >= 0.0 && save.right() <= width,
            "view control outside window: {save:?} / {width}"
        );
        assert!(save.bottom() < pane(&ctx).top());
    }
}

#[test]
fn transfer_editor_is_a_retained_draft_even_when_the_engine_rejects_submission() {
    let ctx = egui::Context::default();
    let mut app = fixture(1, 1);
    app.details_tab = DetailsTab::Settings;
    crate::transfer_settings::TransferSettings {
        download_kib: 11,
        ..Default::default()
    }
    .apply(&mut app.control.torrents[0]);
    let confirmed = app.control.torrents[0].overrides.clone();
    let mut draft = confirmed.clone();
    draft.download_limit_kib = Some(17);
    let mut draft_id = None;
    let render = |app: &mut PeerLumeApp,
                  events: Vec<egui::Event>,
                  initial: Option<TorrentOverrides>,
                  draft_id: &mut Option<egui::Id>| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1400.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let id = ui.make_persistent_id((
                        "transfer_draft",
                        app.control.torrents[0].info_hashes.join(";"),
                    ));
                    *draft_id = Some(id);
                    if let Some(initial) = initial.clone() {
                        ctx.data_mut(|data| data.insert_temp(id, initial));
                    }
                    app.show_selected_torrent(ui);
                });
            },
        )
    };
    let output = render(&mut app, vec![], Some(draft), &mut draft_id);
    assert_eq!(app.control.torrents[0].overrides, confirmed);
    let position = text_rect(&output, "Apply transfer settings").center();
    render(&mut app, pointer(position, true), None, &mut draft_id);
    render(&mut app, pointer(position, false), None, &mut draft_id);
    assert!(app.add_status.contains("unavailable"), "{}", app.add_status);
    assert_eq!(app.control.torrents[0].overrides, confirmed);
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<TorrentOverrides>(draft_id.unwrap()))
            .unwrap()
            .download_limit_kib,
        Some(17)
    );
    assert_eq!(
        ctx.data_mut(|data| data.get_temp::<TorrentOverrides>(draft_id.unwrap()))
            .unwrap()
            .ratio_limit,
        Some(0.0),
        "Opening the editor must not silently enable a seed ratio limit"
    );
}

fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn a_live_sort_change_during_mouse_press_cannot_select_a_different_torrent() {
    let ctx = egui::Context::default();
    let mut app = fixture(3, 1);
    app.selected_torrent = 2;
    app.settings.ui.sort = crate::torrent_view::Sort::Download;
    app.settings.ui.descending = true;
    for (torrent, rate) in app.control.torrents.iter_mut().zip([100, 90, 1]) {
        torrent.download_rate = rate;
    }
    let size = egui::vec2(1080.0, 1000.0);
    let output = settle(&ctx, &mut app, size);
    let first = &app.control.torrents[0];
    let label = if first.is_private {
        format!("[Private] {}", first.name)
    } else {
        first.name.clone()
    };
    let position = text_rect(&output, &label).center();
    frame(&ctx, &mut app, size, pointer(position, true));
    app.control.torrents[0].download_rate = 80;
    app.control.torrents[1].download_rate = 110;
    frame(&ctx, &mut app, size, pointer(position, false));
    assert_eq!(
        app.selected_torrent, 2,
        "A moving row must cancel the old click, never retarget it to another torrent"
    );
}

fn drag(
    ctx: &egui::Context,
    app: &mut PeerLumeApp,
    size: egui::Vec2,
    delta: f32,
) -> egui::FullOutput {
    let top = pane(ctx);
    let start = egui::pos2(top.center().x, top.bottom());
    let end = start + egui::vec2(0.0, delta);
    frame(ctx, app, size, pointer(start, true));
    frame(ctx, app, size, vec![egui::Event::PointerMoved(end)]);
    frame(ctx, app, size, pointer(end, false));
    settle(ctx, app, size)
}

#[test]
fn extra_window_height_goes_to_files_and_both_panes_remain_bounded() {
    for files in [0, 3, 65] {
        let ctx = egui::Context::default();
        let mut app = fixture(50, files);
        // Reproduce a status message much wider than the visible window.
        app.add_status = "Long status ".repeat(80);
        let mut previous: Option<(f32, f32, f32)> = None;
        for height in [560.0, 720.0, 1200.0, 1600.0, 560.0, 720.0] {
            let output = settle(&ctx, &mut app, egui::vec2(1080.0, height));
            let top = pane(&ctx);
            let files = border(&output, top.bottom());
            assert!(
                top.right() <= 1072.5 && files.right() <= 1072.5,
                "panes overflow: {top:?}, {files:?}"
            );
            assert!(files.top() > top.bottom());
            assert!(
                (files.bottom() - (height - 8.0)).abs() <= 1.0,
                "unused vertical space: {files:?}"
            );
            assert!(files.height() > 40.0);
            if let Some((last_height, last_table, last_files)) = previous
                && height > last_height
            {
                assert!(
                    (top.height() - last_table).abs() <= 1.0,
                    "growing the window must not grow the saved top pane"
                );
                assert!((files.height() - last_files - (height - last_height)).abs() <= 1.0);
            }
            previous = Some((height, top.height(), files.height()));
        }
    }
}

#[test]
fn real_divider_drag_grows_shrinks_and_clamps_without_transfer_changes() {
    let ctx = egui::Context::default();
    let mut app = fixture(50, 65);
    let size = egui::vec2(1080.0, 1000.0);
    let output = settle(&ctx, &mut app, size);
    let initial = pane(&ctx);
    let initial_files = border(&output, initial.bottom());
    let output = drag(&ctx, &mut app, size, -100.0);
    assert!((pane(&ctx).height() - initial.height() + 100.0).abs() <= 1.0);
    assert!(
        (border(&output, pane(&ctx).bottom()).height() - initial_files.height() - 100.0).abs()
            <= 1.0
    );
    drag(&ctx, &mut app, size, 150.0);
    assert!((pane(&ctx).height() - initial.height() - 50.0).abs() <= 1.0);
    let bottom_edge = size.y - pane(&ctx).bottom() - 1.0;
    let output = drag(&ctx, &mut app, size, bottom_edge);
    assert!(
        border(&output, pane(&ctx).bottom()).height() > 40.0,
        "details must retain space"
    );
    let top_edge = -pane(&ctx).height() + 1.0;
    drag(&ctx, &mut app, size, top_edge);
    assert!((pane(&ctx).height() - super::MIN_TABLE_HEIGHT).abs() <= 1.0);
    assert_eq!(app.selected_torrent, 0);
    assert!(
        app.control
            .torrents
            .iter()
            .all(|torrent| torrent.state == TorrentState::Downloading)
    );
}

#[test]
fn torrent_and_file_lists_scroll_independently_and_last_torrent_can_be_selected() {
    let ctx = egui::Context::default();
    let mut app = fixture(100, 100);
    let size = egui::vec2(1080.0, 1000.0);
    let output = settle(&ctx, &mut app, size);
    let top = pane(&ctx);
    let files = border(&output, top.bottom());
    let first_file = text_rect(&output, "file-0000.bin");
    let scroll = |pos, delta| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta),
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    frame(&ctx, &mut app, size, scroll(top.center(), -100_000.0));
    let output = settle(&ctx, &mut app, size);
    let last = text_rect(&output, "Torrent 099");
    assert!(last.bottom() <= top.bottom());
    assert_eq!(text_rect(&output, "file-0000.bin"), first_file);
    frame(&ctx, &mut app, size, scroll(files.center(), -100_000.0));
    let output = settle(&ctx, &mut app, size);
    text_rect(&output, "file-0099.bin");
    assert_eq!(text_rect(&output, "Torrent 099"), last);
    assert_eq!(pane(&ctx), top);
    frame(&ctx, &mut app, size, pointer(last.center(), true));
    frame(&ctx, &mut app, size, pointer(last.center(), false));
    assert_eq!(
        app.selected_torrent, 99,
        "scrolling must preserve original row indices"
    );
}

#[test]
fn both_torrent_scrollbars_work_together_and_window_growth_keeps_columns_reachable() {
    let ctx = egui::Context::default();
    let mut app = fixture(60, 65);
    for torrent in &mut app.control.torrents {
        torrent.state = TorrentState::WaitingForEngine;
        torrent.peers = usize::MAX;
        torrent.seeds = usize::MAX;
    }
    let size = egui::vec2(820.0, 560.0);
    settle(&ctx, &mut app, size);
    let top = pane(&ctx);
    frame(
        &ctx,
        &mut app,
        size,
        vec![
            egui::Event::PointerMoved(top.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(-100_000.0, 0.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let output = settle(&ctx, &mut app, size);
    let ratio = text_rect(&output, "Ratio");
    assert!(
        ratio.left() >= top.left() && ratio.right() <= top.right(),
        "Ratio is clipped: {ratio:?}, {top:?}"
    );
    let files = border(&output, top.bottom());
    assert!((files.bottom() - 552.0).abs() <= 1.0);
    let output = settle(&ctx, &mut app, egui::vec2(1920.0, 1000.0));
    let top = pane(&ctx);
    let first = text_rect(&output, "Torrent 000");
    assert!(first.left() >= top.left() && first.bottom() < top.bottom());
    let ratio = text_rect(&output, "Ratio");
    assert!(ratio.right() <= top.right());
    assert!((border(&output, top.bottom()).bottom() - 992.0).abs() <= 1.0);
}

#[test]
fn empty_filtered_lists_and_extreme_short_windows_keep_the_divider_inside_the_panel() {
    let ctx = egui::Context::default();
    let mut app = fixture(5, 3);
    app.status_filter = crate::torrent_filter::StatusFilter::Seeding;
    for height in [560.0, 300.0, 160.0, 1000.0] {
        let output = settle(&ctx, &mut app, egui::vec2(820.0, height));
        let top = pane(&ctx);
        assert!(top.top() >= 0.0 && top.bottom() <= height);
        text_rect(
            &output,
            "No torrents match this status. Choose All to show every torrent.",
        );
    }
    assert_eq!(app.control.torrents.len(), 5);
}
