//! Main-window dispatch over a disposable native owner with no transfer engine.
//! No application constructor, user profile, picker or real download is opened.
use super::*;
use crate::control::frontend::{Receipt, Server};
use std::sync::Mutex;

fn app(count: usize) -> PeerLumeApp {
    super::super::torrent_panes::tests::fixture(count, 1)
}
fn ids(app: &PeerLumeApp) -> Vec<String> {
    app.control
        .torrents
        .iter()
        .map(|item| item.info_hashes.join(";"))
        .collect()
}
struct Connected {
    app: PeerLumeApp,
    _server: Option<Server>,
    owner: Arc<Mutex<Controller>>,
    root: tempfile::TempDir,
}
impl Connected {
    fn new(count: usize) -> Self {
        Self::configured(count, |_| {})
    }
    fn configured(count: usize, configure: impl FnOnce(&mut PeerLumeApp)) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(count);
        configure(&mut app);
        let mut settings = ClientSettings::default();
        settings.download.download_directory = root.path().display().to_string();
        settings.speed.upload_limit_kib = 11;
        settings
            .save_to(&root.path().join("settings.toml"))
            .unwrap();
        let mut store = crate::state::StateStore::in_memory_with_secrets(Arc::new(
            crate::secrets::MemorySecrets::default(),
        ))
        .unwrap();
        for item in &app.control.torrents {
            store.save_torrent(&torrent_to_record(item, None)).unwrap();
        }
        let mut owner = Controller::new(
            ClientBackend::with_state_store(store),
            app.control.torrents.clone(),
        );
        owner
            .configure_agents(root.path(), root.path().to_owned())
            .unwrap();
        let owner = Arc::new(Mutex::new(owner));
        let server =
            Server::start(owner.clone(), root.path().to_owned(), settings.clone()).unwrap();
        app.control = Controller::new(ClientBackend::attach(root.path()).unwrap(), Vec::new());
        app.settings = settings.clone();
        app.saved_settings = settings.clone();
        app.settings_draft = settings;
        let deadline = Instant::now() + Duration::from_secs(6);
        while app.desktop_connection().status != ConnectionStatus::Connected {
            app.control.poll_next();
            assert!(
                Instant::now() < deadline,
                "synthetic native owner did not connect"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        app.reconcile_selection();
        app.control
            .backend
            .attached_client()
            .unwrap()
            .take_receipts();
        Self {
            app,
            _server: Some(server),
            owner,
            root,
        }
    }
    fn receipts(&self) -> Vec<Receipt> {
        self.app
            .control
            .backend
            .attached_client()
            .unwrap()
            .take_receipts()
    }
}
fn frame(ctx: &egui::Context, app: &mut PeerLumeApp, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 800.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| app.show_desktop_torrents(ctx),
    )
}
fn first_text(out: &egui::FullOutput, label: &str) -> egui::Rect {
    out.shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap_or_else(|| panic!("Missing {label}"))
}
fn click(ctx: &egui::Context, app: &mut PeerLumeApp, pos: egui::Pos2) {
    frame(ctx, app, vec![egui::Event::PointerMoved(pos)]);
    for pressed in [true, false] {
        frame(
            ctx,
            app,
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

#[test]
fn real_pause_toolbar_queues_only_visible_frozen_ids_without_waiting_for_the_owner() {
    let mut f = Connected::new(4);
    let all = ids(&f.app);
    for (index, item) in f.app.control.torrents.iter_mut().enumerate() {
        item.name = if index % 2 == 0 {
            format!("Visible {index}")
        } else {
            format!("Hidden {index}")
        };
    }
    f.app.selection.select_visible(&all);
    f.app.settings.ui.search = "visible".into();
    let ctx = egui::Context::default();
    for _ in 0..3 {
        frame(&ctx, &mut f.app, Vec::new());
    }
    let out = frame(&ctx, &mut f.app, Vec::new());
    let pos = first_text(&out, "Pause").center();
    let owner = f.owner.lock().unwrap();
    let started = Instant::now();
    click(&ctx, &mut f.app, pos);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "GUI waited on native owner"
    );
    f.app.control.torrents.swap(0, 2);
    let commands: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|r| r.queued && r.command.is_some())
        .map(|r| (r.id, r.command.unwrap()))
        .collect();
    assert_eq!(
        commands,
        [
            (all[0].clone(), TorrentCommand::Pause),
            (all[2].clone(), TorrentCommand::Pause)
        ]
    );
    assert!(
        owner
            .torrents
            .iter()
            .all(|item| item.state == TorrentState::Downloading),
        "queued does not mean engine-confirmed pause"
    );
}

#[test]
fn hidden_focus_and_ctrl_deselection_never_retarget_details_or_a_batch() {
    let mut app = app(3);
    let all = ids(&app);
    app.reconcile_selection();
    app.selection.replace(&all[1]);
    app.reconcile_selection();
    assert_eq!(app.selected_torrent, 1);
    app.settings.ui.search = "Torrent 000".into();
    app.reconcile_selection();
    assert_eq!(app.selection.focus(), Some(all[1].as_str()));
    assert_eq!(app.selected_torrent, 1);
    assert!(app.desktop_targets().is_empty());
    app.settings.ui.search.clear();
    app.selection.click(
        &all[1],
        &all,
        crate::torrent_selection::Modifiers {
            ctrl: true,
            shift: false,
        },
    );
    app.reconcile_selection();
    assert!(app.selection.is_empty());
    assert_eq!(app.selection.focus(), Some(all[1].as_str()));
    assert!(app.desktop_targets().is_empty());
    app.control.torrents.remove(1);
    app.reconcile_selection();
    assert_eq!(app.selection.focus(), None);
    assert!(app.desktop_targets().is_empty());
}

#[test]
fn cached_full_selection_reuses_targets_until_view_or_identity_changes() {
    let mut app = app(5000);
    let all = ids(&app);
    app.reconcile_selection();
    app.selection.select_visible(&all);
    let first = app.desktop_targets();
    let pointers: Vec<_> = app.identities.ids.iter().map(|id| id.as_ptr()).collect();
    for item in &mut app.control.torrents {
        item.uploaded_bytes += 1;
    }
    app.reconcile_selection();
    let second = app.desktop_targets();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        pointers,
        app.identities
            .ids
            .iter()
            .map(|id| id.as_ptr())
            .collect::<Vec<_>>()
    );
    app.settings.ui.search = "Torrent 000".into();
    let filtered = app.desktop_targets();
    assert!(!Arc::ptr_eq(&second, &filtered));
    assert_eq!(filtered.len(), 1);
    app.control.torrents.swap(0, 1);
    app.reconcile_selection();
    assert_eq!(app.desktop_targets().as_slice(), [all[0].clone()]);
    assert_eq!(app.selection.focus(), Some(all[0].as_str()));
    assert_eq!(app.selected_torrent, 1);
}

#[test]
fn disconnected_dispatch_has_no_picker_or_queued_mutation_and_zero_targets_stay_empty() {
    let mut app = app(1);
    let ctx = egui::Context::default();
    let all = ids(&app);
    for action in [
        Action::AddTorrent,
        Action::AddMagnet,
        Action::PauseSelected,
        Action::ResumeSelected,
        Action::RemoveSelected,
        Action::RecheckSelected,
        Action::PauseAll,
        Action::ResumeAll,
        Action::UseExistingData,
        Action::CopyData,
        Action::MoveData,
        Action::RemoveCopyOriginals,
        Action::CancelCopy,
        Action::SaveView,
        Action::StopBackground,
    ] {
        app.dispatch_desktop(&ctx, action, all.clone());
        assert!(app.add_status.contains("not connected"));
        assert!(app.batch_remove.is_none());
        assert!(app.stop_confirmation.is_none());
        assert!(!app.magnet_open);
    }
    let mut f = Connected::new(1);
    f.app
        .dispatch_desktop(&ctx, Action::PauseSelected, Vec::new());
    f.app
        .dispatch_desktop(&ctx, Action::RemoveSelected, Vec::new());
    assert!(f.app.batch_remove.is_none());
    assert!(
        f.receipts()
            .into_iter()
            .all(|r| !r.queued || r.command.is_none())
    );
}

#[test]
fn missing_target_is_reported_separately_and_batch_is_not_replayed() {
    let mut f = Connected::configured(2, |app| {
        app.control.torrents[1].state = TorrentState::Paused
    });
    let all = ids(&f.app);
    let ctx = egui::Context::default();
    f.app.dispatch_desktop(
        &ctx,
        Action::ResumeSelected,
        vec![all[1].clone(), "disappeared-id".into()],
    );
    assert!(f.app.add_status.contains("1 request(s) queued, 1 rejected"));
    let queued: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|r| r.queued && r.command.is_some())
        .map(|r| r.id)
        .collect();
    assert_eq!(queued, [all[1].clone()]);
    assert!(
        f.app
            .operation_tracker
            .history
            .entries()
            .any(|entry| entry.target_id.as_deref() == Some("disappeared-id"))
    );
    assert!(f.receipts().into_iter().all(|r| !r.queued));
}

#[test]
fn removal_confirmation_rejects_a_replacement_owner_even_with_matching_ids_and_revision() {
    let mut first = Connected::new(1);
    let all = ids(&first.app);
    let ctx = egui::Context::default();
    first
        .app
        .dispatch_desktop(&ctx, Action::RemoveSelected, all);
    let review = first.app.batch_remove.take().unwrap();
    let mut second = Connected::new(1);
    let backend = std::mem::replace(
        &mut second.app.control.backend,
        ClientBackend::blocked("test detached".into()),
    );
    first.app.control.backend = backend;
    first.app.confirm_desktop_removal(review);
    assert!(first.app.add_status.contains("owner changed"));
    assert!(
        first
            .receipts()
            .into_iter()
            .all(|r| !r.queued || r.command.is_none())
    );
}

#[test]
fn removal_confirmation_rejects_a_real_superseding_owner_revision() {
    let mut f = Connected::new(1);
    let id = ids(&f.app)[0].clone();
    let ctx = egui::Context::default();
    f.app
        .dispatch_desktop(&ctx, Action::RemoveSelected, vec![id.clone()]);
    let review = f.app.batch_remove.take().unwrap();
    let old = review.revisions[&id];
    f.app
        .control
        .backend
        .attached_client()
        .unwrap()
        .save_audit(&id, None)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while f.app.control.command_revision(&id).unwrap() == old {
        f.app.control.poll_next();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    f.receipts();
    f.app.confirm_desktop_removal(review);
    assert!(f.app.add_status.contains("0 request(s) queued, 1 rejected"));
    assert!(
        f.receipts()
            .into_iter()
            .all(|r| !r.queued || r.command != Some(TorrentCommand::Remove))
    );
    assert_eq!(f.owner.lock().unwrap().torrents.len(), 1);
}

#[test]
fn save_view_preserves_unsaved_preference_drafts_and_owner_disk_preferences() {
    let mut f = Connected::new(1);
    f.app.settings.ui.search = "presentation-only".into();
    f.app
        .settings
        .ui
        .table_columns
        .set_visible(crate::table_view::Column::Peers, false);
    f.app.settings_draft.speed.upload_limit_kib = 999;
    f.app
        .dispatch_desktop(&egui::Context::default(), Action::SaveView, Vec::new());
    let path = f.root.path().join("settings.toml");
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        f.app.poll_session_events();
        let disk = ClientSettings::load_from(&path).unwrap();
        if disk.ui.search == "presentation-only" {
            assert_eq!(disk.speed.upload_limit_kib, 11);
            assert!(
                !disk
                    .ui
                    .table_columns
                    .is_visible(crate::table_view::Column::Peers)
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.app.settings_draft.speed.upload_limit_kib, 999);
    assert_eq!(f.app.settings.speed.upload_limit_kib, 11);
}

#[test]
fn preferences_save_carries_current_layout_instead_of_the_old_preferences_ui_copy() {
    let mut f = Connected::new(1);
    f.app.settings.ui.search = "current-window-view".into();
    f.app
        .settings
        .ui
        .table_columns
        .set_visible(crate::table_view::Column::Ratio, false);
    f.app.settings_draft.ui.search = "old-preferences-copy".into();
    f.app.settings_draft.speed.upload_limit_kib = 99;
    f.app.handle_footer_action(FooterAction::Save);
    assert_eq!(f.app.settings_draft.ui.search, "current-window-view");
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        f.app.poll_session_events();
        let disk = ClientSettings::load_from(&f.root.path().join("settings.toml")).unwrap();
        if disk.speed.upload_limit_kib == 99 {
            assert_eq!(disk.ui.search, "current-window-view");
            assert!(
                !disk
                    .ui
                    .table_columns
                    .is_visible(crate::table_view::Column::Ratio)
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "preferences save did not finish: {}",
            f.app.settings_status
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn offline_primary_header_and_preferences_dispatch_do_not_change_models_or_persist() {
    let mut app = app(1);
    let ctx = egui::Context::default();
    for _ in 0..3 {
        frame(&ctx, &mut app, Vec::new());
    }
    let out = frame(&ctx, &mut app, Vec::new());
    let pause = out
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "Pause" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .max_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("real details primary button");
    click(&ctx, &mut app, pause.center());
    assert_eq!(app.control.torrents[0].state, TorrentState::Downloading);
    assert!(
        app.control.torrents[0]
            .runtime
            .command
            .pending_action()
            .is_none()
    );
    assert!(app.operation_tracker.history.is_empty());
    let settings = app.settings.clone();
    app.settings_draft.speed.upload_limit_kib = 999;
    for action in [FooterAction::Apply, FooterAction::Save] {
        app.handle_footer_action(action);
        assert_eq!(app.settings, settings);
        assert!(app.settings_status.contains("connect"));
    }
}

#[test]
fn real_app_header_wraps_at_820_pixels_with_double_scale_and_filter_remains_interactive() {
    fn header(
        ctx: &egui::Context,
        app: &mut PeerLumeApp,
        width: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 400.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.show_header(ctx),
        )
    }
    for (width, font_scale) in [(410.0, 1.0), (820.0, 2.0)] {
        let ctx = egui::Context::default();
        let mut style = (*ctx.style()).clone();
        for font in style.text_styles.values_mut() {
            font.size *= font_scale;
        }
        ctx.set_style(style);
        let mut app = app(3);
        for _ in 0..3 {
            header(&ctx, &mut app, width, Vec::new());
        }
        let out = header(&ctx, &mut app, width, Vec::new());
        for shape in &out.shapes {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                assert!(
                    rect.left() >= 0.0 && rect.right() <= width,
                    "header overflow {width}@{font_scale}: {} {rect:?}",
                    text.galley.job.text
                );
            }
        }
        let point = first_text(&out, "All (3)").center();
        for pressed in [true, false] {
            header(
                &ctx,
                &mut app,
                width,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let out = header(&ctx, &mut app, width, Vec::new());
        let point = first_text(&out, "Paused (0)").center();
        for pressed in [true, false] {
            header(
                &ctx,
                &mut app,
                width,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(
            app.status_filter,
            crate::torrent_filter::StatusFilter::Paused
        );
        assert!(
            app.control
                .torrents
                .iter()
                .all(|item| item.state == TorrentState::Downloading)
        );
    }
}

#[test]
fn mixed_group_and_global_controls_queue_only_the_matching_primary_action() {
    let mut f = Connected::configured(4, |app| {
        app.control.torrents[0].state = TorrentState::Paused;
        app.control.torrents[2].state = TorrentState::Error;
        app.control.torrents[2].runtime.registered = false;
        app.control.torrents[2]
            .runtime
            .record_error("Synthetic saved-source startup failure");
        app.control.torrents[3]
            .runtime
            .command
            .begin(crate::runtime_status::TransferAction::Pause);
    });
    let all = ids(&f.app);
    let props = desktop_props(&f.app.control.torrents, 0..4, true, 0, 4);
    assert!(props.can_pause && props.can_resume);
    let errors = desktop_props(&f.app.control.torrents, [2, 3], true, 0, 4);
    assert!(
        !errors.can_pause && !errors.can_resume,
        "error recovery and pending work are not ordinary Pause/Resume"
    );
    f.app.settings.ui.search = "hidden-by-filter".into();
    let ctx = egui::Context::default();
    f.app.dispatch_desktop(&ctx, Action::ResumeAll, Vec::new());
    let resumed: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|r| r.queued && r.command == Some(TorrentCommand::Resume))
        .map(|r| r.id)
        .collect();
    assert_eq!(resumed, [all[0].clone()]);
    assert!(f.app.add_status.contains("3 skipped"));
    assert_eq!(f.app.control.torrents[0].state, TorrentState::Paused);
    assert_eq!(
        f.app.control.torrents[0].runtime.command.pending_action(),
        Some(crate::runtime_status::TransferAction::Resume)
    );
    assert!(
        !desktop_props(&f.app.control.torrents, [0], true, 0, 4).one_paused,
        "pending Resume is not a confirmed storage precondition"
    );
    f.app.dispatch_desktop(&ctx, Action::PauseAll, Vec::new());
    let paused: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|r| r.queued && r.command == Some(TorrentCommand::Pause))
        .map(|r| r.id)
        .collect();
    assert_eq!(paused, [all[1].clone()]);
    assert!(f.app.add_status.contains("3 skipped"));
    assert_eq!(f.app.control.torrents[1].state, TorrentState::Downloading);
    assert_eq!(
        f.app.control.torrents[1].runtime.command.pending_action(),
        Some(crate::runtime_status::TransferAction::Pause)
    );
}

#[test]
fn directory_navigation_failure_retains_its_cause_in_operations_without_a_real_shell() {
    let mut app = app(1);
    let id = ids(&app)[0].clone();
    app.record_directory_result(&id, Err("Synthetic directory is unavailable".into()));
    let entry = app.operation_tracker.history.entries().next().unwrap();
    assert_eq!(entry.target_id.as_deref(), Some(id.as_str()));
    assert!(
        entry
            .error()
            .unwrap()
            .contains("Synthetic directory is unavailable")
    );
    app.add_status.clear();
    assert!(
        app.operation_tracker
            .history
            .entries()
            .next()
            .unwrap()
            .error()
            .is_some()
    );
}

// Render the actual App wiring, rather than the individual stateless widgets.
fn desktop_frame(
    ctx: &egui::Context,
    app: &mut PeerLumeApp,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 1000.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            app.appearance_state.apply(ctx, &app.settings.ui.appearance);
            app.show_desktop_footer(ctx);
            app.show_desktop_torrents(ctx);
            app.show_desktop_dialogs(ctx);
        },
    )
}
fn desktop_settle(ctx: &egui::Context, app: &mut PeerLumeApp) -> egui::FullOutput {
    for _ in 0..4 {
        desktop_frame(ctx, app, Vec::new());
    }
    desktop_frame(ctx, app, Vec::new())
}
fn desktop_click(ctx: &egui::Context, app: &mut PeerLumeApp, label: &str) -> egui::FullOutput {
    let out = desktop_settle(ctx, app);
    let point = out
        .shapes
        .iter()
        .rev()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing topmost desktop control: {label}"));
    desktop_frame(ctx, app, vec![egui::Event::PointerMoved(point)]);
    desktop_frame(
        ctx,
        app,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    desktop_frame(
        ctx,
        app,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    )
}
fn desktop_more(ctx: &egui::Context, app: &mut PeerLumeApp, label: &str) {
    desktop_click(ctx, app, "More");
    let out = desktop_settle(ctx, app);
    if !out.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Text(text)if text.galley.job.text==label)) {
        assert!(egui::Popup::is_any_open(ctx),"More popup must be open before scrolling");
        let point=first_text(&out,"Remove selected (keep data)").center();
        desktop_frame(ctx,app,vec![egui::Event::PointerMoved(point)]);
        desktop_frame(ctx,app,vec![egui::Event::MouseWheel{unit:egui::MouseWheelUnit::Point,delta:egui::vec2(0.0,-100_000.0),modifiers:egui::Modifiers::NONE}]);
    }
    desktop_click(ctx, app, label);
}

#[test]
fn footer_operations_and_close_window_bind_to_app_without_stopping_the_owner() {
    let mut f = Connected::new(2);
    let all = ids(&f.app);
    let ctx = egui::Context::default();
    let owner = f.app.desktop_connection().owner_instance;
    f.app.desktop_error(
        "Synthetic operation",
        Some(&all[1]),
        "A retained binding-test cause".into(),
    );
    desktop_click(&ctx, &mut f.app, "Operations (1)");
    assert!(f.app.operations_open);
    let out = desktop_settle(&ctx, &mut f.app);
    first_text(&out, "Operations");
    first_text(&out, "A retained binding-test cause");
    f.app.status_filter = crate::torrent_filter::StatusFilter::Seeding;
    f.app.settings.ui.search = "hidden".into();
    f.app.settings.ui.category_filter = "hidden".into();
    desktop_click(&ctx, &mut f.app, "Focus torrent");
    assert_eq!(f.app.selection.focus(), Some(all[1].as_str()));
    assert_eq!(f.app.selected_torrent, 1);
    assert!(f.app.settings.ui.search.is_empty());
    assert!(f.app.settings.ui.category_filter.is_empty());
    assert_eq!(f.app.status_filter, Default::default());
    let out = desktop_click(&ctx, &mut f.app, "Copy error");
    assert!(out.platform_output.commands.iter().any(|command|matches!(command,egui::OutputCommand::CopyText(text) if text.contains("retained binding-test cause"))));
    desktop_click(&ctx, &mut f.app, "Dismiss");
    assert!(f.app.operation_tracker.history.is_empty());
    f.app.operations_open = false;
    let out = desktop_click(&ctx, &mut f.app, "Close window");
    assert!(out.viewport_output.values().any(|viewport| {
        viewport
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Close))
    }));
    assert!(!f._server.as_ref().unwrap().stop_requested());
    assert_eq!(f.app.desktop_connection().owner_instance, owner);
    assert_eq!(
        f.app.desktop_connection().status,
        ConnectionStatus::Connected
    );
    assert!(f.receipts().into_iter().all(|receipt| !receipt.queued));
}

#[test]
fn footer_stop_review_cancel_and_confirm_use_the_displayed_owner_and_lock_evidence() {
    let mut f = Connected::new(1);
    let ctx = egui::Context::default();
    let owner = f.app.desktop_connection().owner_instance.unwrap();
    let lock = crate::control::host::InstanceLock::acquire(f.root.path()).unwrap();
    desktop_click(&ctx, &mut f.app, "Stop background client...");
    assert_eq!(f.app.stop_confirmation.as_deref(), Some(owner.as_str()));
    desktop_click(&ctx, &mut f.app, "Cancel");
    assert!(f.app.stop_confirmation.is_none());
    assert!(!f._server.as_ref().unwrap().stop_requested());
    assert!(f.receipts().into_iter().all(|receipt| !receipt.queued));
    desktop_click(&ctx, &mut f.app, "Stop background client...");
    desktop_click(&ctx, &mut f.app, "Stop background client");
    assert!(f.app.stop_confirmation.is_none());
    let queued: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|receipt| receipt.queued && receipt.label == "Stop background client")
        .collect();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].owner_instance, owner);
    let deadline = Instant::now() + Duration::from_secs(6);
    while !f._server.as_ref().unwrap().stop_requested() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    while f.app.desktop_connection().status != ConnectionStatus::StopRequested {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = desktop_settle(&ctx, &mut f.app);
    first_text(
        &out,
        "Stopping background client; awaiting profile lock release",
    );
    // The synthetic owner has no daemon loop. Close only its disposable native
    // listener before releasing the lock, as the real daemon does at shutdown.
    drop(f._server.take());
    drop(lock);
    while f.app.desktop_connection().status != ConnectionStatus::Stopped {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = desktop_settle(&ctx, &mut f.app);
    first_text(
        &out,
        "Background client stopped · close and reopen to start it again",
    );
}

#[test]
fn actual_remove_dialog_cancel_or_confirm_keeps_the_frozen_group_after_selection_changes() {
    for confirm in [false, true] {
        let mut f = Connected::new(3);
        let all = ids(&f.app);
        let ctx = egui::Context::default();
        f.app.selection.select_visible(&all[..2]);
        desktop_more(&ctx, &mut f.app, "Remove selected (keep data)");
        assert!(f.app.batch_remove.is_some());
        f.app.selection.replace(&all[2]);
        f.app.settings.ui.search = "Torrent 002".into();
        let out = desktop_settle(&ctx, &mut f.app);
        first_text(&out, "Torrent 000");
        first_text(&out, "Torrent 001");
        first_text(&out, "2 selected visible torrent(s) will leave the list.");
        desktop_click(
            &ctx,
            &mut f.app,
            if confirm {
                "Remove from list"
            } else {
                "Cancel"
            },
        );
        assert!(f.app.batch_remove.is_none());
        let queued: Vec<_> = f
            .receipts()
            .into_iter()
            .filter(|receipt| receipt.queued && receipt.command == Some(TorrentCommand::Remove))
            .map(|receipt| receipt.id)
            .collect();
        assert_eq!(
            queued,
            if confirm {
                all[..2].to_vec()
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            f.app.control.torrents.len(),
            3,
            "a queued removal is not a confirmed deletion"
        );
    }
}

#[test]
fn actual_appearance_dialog_saves_only_ui_and_keeps_unsaved_preferences() {
    let mut f = Connected::new(1);
    let ctx = egui::Context::default();
    f.app.settings_draft.speed.upload_limit_kib = 999;
    desktop_more(&ctx, &mut f.app, "Appearance...");
    assert!(f.app.appearance_open);
    desktop_click(&ctx, &mut f.app, "Compact");
    assert_eq!(
        f.app.settings.ui.appearance.density,
        crate::appearance::Density::Compact
    );
    desktop_click(&ctx, &mut f.app, "System");
    desktop_click(&ctx, &mut f.app, "Dark");
    assert_eq!(
        f.app.settings.ui.appearance.theme,
        crate::appearance::ThemePreference::Dark
    );
    desktop_click(&ctx, &mut f.app, "Save view");
    assert!(!f.app.appearance_open);
    let queued: Vec<_> = f
        .receipts()
        .into_iter()
        .filter(|receipt| receipt.queued && receipt.label == "View")
        .collect();
    assert_eq!(queued.len(), 1);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        f.app.poll_session_events();
        let disk = ClientSettings::load_from(&f.root.path().join("settings.toml")).unwrap();
        if disk.ui.appearance.theme == crate::appearance::ThemePreference::Dark {
            assert_eq!(
                disk.ui.appearance.density,
                crate::appearance::Density::Compact
            );
            assert_eq!(disk.speed.upload_limit_kib, 11);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.app.settings_draft.speed.upload_limit_kib, 999);
}

#[test]
fn actual_magnet_dialog_cancel_and_invalid_add_do_not_import_or_reopen() {
    let mut f = Connected::new(1);
    let ctx = egui::Context::default();
    desktop_click(&ctx, &mut f.app, "Add magnet");
    assert!(f.app.magnet_open);
    let out = desktop_settle(&ctx, &mut f.app);
    let point = first_text(&out, "magnet:?xt=...").center();
    desktop_frame(&ctx, &mut f.app, vec![egui::Event::PointerMoved(point)]);
    for pressed in [true, false] {
        desktop_frame(
            &ctx,
            &mut f.app,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    desktop_frame(
        &ctx,
        &mut f.app,
        vec![egui::Event::Text("invalid magnet text".into())],
    );
    assert_eq!(f.app.magnet_input, "invalid magnet text");
    desktop_click(&ctx, &mut f.app, "Add magnet");
    assert!(!f.app.magnet_open);
    assert!(!f.app.add_status.is_empty());
    assert_eq!(f.app.control.torrents.len(), 1);
    assert!(f.receipts().into_iter().all(|receipt| !receipt.queued));
    f.app.add_status = "Validation was already handled".into();
    desktop_settle(&ctx, &mut f.app);
    assert_eq!(f.app.add_status, "Validation was already handled");
    assert!(!f.app.magnet_open);
    desktop_click(&ctx, &mut f.app, "Add magnet");
    assert!(f.app.magnet_open);
    desktop_click(&ctx, &mut f.app, "Cancel");
    assert!(!f.app.magnet_open);
    assert!(f.receipts().into_iter().all(|receipt| !receipt.queued));
}

#[test]
fn actual_disconnected_footer_explains_stale_state_and_keeps_close_available() {
    let mut app = app(1);
    let ctx = egui::Context::default();
    let out = desktop_settle(&ctx, &mut app);
    assert!(out.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Text(text)if text.galley.job.text.starts_with("Reconnecting — displayed transfer state may be stale:"))));
    desktop_click(&ctx, &mut app, "Stop background client...");
    assert!(app.stop_confirmation.is_none());
    let out = desktop_click(&ctx, &mut app, "Close window");
    assert!(out.viewport_output.values().any(|viewport| {
        viewport
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Close))
    }));
}
