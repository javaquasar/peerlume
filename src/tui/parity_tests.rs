use super::tests::{app, test_torrent, wait_for_tui};
use super::*;
use ratatui::backend::TestBackend;

fn key(app: &mut TuiApp, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn text(app: &mut TuiApp, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn focus_search_marks_and_confirmation_use_ids_not_row_positions() {
    let mut app = app();
    let mut other = test_torrent();
    other.name = "other row".into();
    other.info_hashes = vec!["v1: other".into()];
    app.control.torrents.push(other);
    app.control.publish();
    let first = app.control.torrents[0].info_hashes.join(";");
    key(&mut app, KeyCode::Char(' '));
    assert!(app.marked.contains(&first));
    app.control.torrents.swap(0, 1);
    app.request_batch(interaction::BatchAction::Remove);
    assert!(text(&mut app, 100, 25).contains("All downloaded files"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.control.torrents.len(), 2);
    assert!(app.status.contains("no commands sent"));
    assert!(app.run_command("search other").is_ok());
    assert!(app.marked.is_empty());
    assert_eq!(app.selected().unwrap().info_hashes, ["v1: other"]);
    assert_eq!(app.visible_indices(), [0]);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Details);
    let row = app.selected_torrent;
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected_torrent, row);
    assert!(app.run_command("rm -rf /").is_err());
    assert!(app.run_command("split 81").is_err());
    assert!(app.run_command("split 65").is_ok());
    assert_eq!(app.split_percent, 65);
}

#[test]
fn every_terminal_size_renders_and_selected_last_rows_remain_visible() {
    let mut app = app();
    for index in 0..300 {
        let mut torrent = test_torrent();
        torrent.name = format!("last-row-{index}");
        torrent.info_hashes = vec![format!("v1: {index}")];
        app.control.torrents.push(torrent);
    }
    app.selected_torrent = 300;
    for (width, height) in [(1, 1), (20, 8), (60, 24), (119, 35), (120, 40), (240, 60)] {
        let rendered = text(&mut app, width, height);
        if width >= 60 && height >= 24 {
            assert!(rendered.contains("last-row-299"), "{width}x{height}");
        }
    }
    key(&mut app, KeyCode::F(1));
    assert!(text(&mut app, 100, 40).contains("Ctrl+C"));
    key(&mut app, KeyCode::Esc);
    assert!(!app.help);
    key(&mut app, KeyCode::Char('/'));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!app.running);
    assert!(app.input.is_none());
}

#[test]
fn every_details_tab_keeps_its_focus_and_scroll_survives_redraw() {
    let mut app = app();
    let mut second = test_torrent();
    second.info_hashes = vec!["v1: second".into()];
    app.control.torrents.push(second);
    app.focus = Focus::Details;
    for tab in TorrentDetailsTab::ALL {
        app.details_tab = tab;
        app.selected_file = 0;
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected_torrent, 0, "{tab:?}");
        let offset = app.selected_file;
        text(&mut app, 100, 25);
        assert_eq!(app.selected_file, offset, "{tab:?}");
    }
}

#[test]
fn preferences_are_drafts_mask_secrets_and_reject_bad_values_without_mutation() {
    let mut app = app();
    app.page = MainPage::Preferences;
    app.preferences_tab = PreferencesTab::Connection;
    let before = app.settings.clone();
    app.editing_preference = Some((PreferencesTab::Connection, "listen_port".into()));
    assert!(app.commit_preference("70000").is_err());
    assert!(app.preferences_draft.is_none());
    app.commit_preference("50000").unwrap();
    assert_eq!(app.settings, before);
    assert_eq!(
        app.preferences_draft
            .as_ref()
            .unwrap()
            .connection
            .listen_port,
        50000
    );
    key(&mut app, KeyCode::Esc);
    assert!(app.preferences_draft.is_none());
    app.preferences_tab = PreferencesTab::Connection;
    app.editing_preference = Some((PreferencesTab::Connection, "proxy_url".into()));
    app.commit_preference("http://user:private-key@example.test")
        .unwrap();
    assert!(!text(&mut app, 180, 40).contains("private-key"));
    app.open_prompt(PromptKind::Preference, "Secret value", "private-key".into());
    assert!(!text(&mut app, 180, 40).contains("private-key"));
    assert_eq!(app.settings, before);
}

#[test]
fn offline_transfer_updates_never_change_confirmed_limits_or_file_paths() {
    let mut app = app();
    let before = app.control.torrents[0].overrides.clone();
    assert!(app.change_transfer("1 2 128 true high 2.0 60").is_err());
    assert_eq!(app.control.torrents[0].overrides, before);
    for command in [
        "set max_peers 0",
        "set download_kib -1",
        "set sequential yes",
        "set ratio_milli 1000001",
        "set unknown 1",
    ] {
        assert!(app.run_command(command).is_err(), "{command}");
        assert_eq!(app.control.torrents[0].overrides, before);
    }
    assert!(app.change_transfer("0 0 1 true high NaN 0").is_err());
}

#[test]
fn preference_validation_and_disk_failure_preserve_draft_and_previous_file() {
    let mut app = app();
    let before = app.settings.clone();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    before.save_to(&path).unwrap();
    let previous = std::fs::read(&path).unwrap();
    let mut draft = before.clone();
    draft.connection.listen_port = 50000;
    app.preferences_draft = Some(draft);
    app.save_preferences_with(|_| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixture disk error",
        ))
    });
    assert_eq!(app.settings, before);
    assert!(app.preferences_draft.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    app.preferences_draft
        .as_mut()
        .unwrap()
        .connection
        .max_connections_per_torrent = 0;
    app.save_preferences_with(|_| panic!("Invalid draft must not be written"));
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    app.preferences_draft
        .as_mut()
        .unwrap()
        .connection
        .max_connections_per_torrent = before.connection.max_connections_per_torrent;
    app.save_preferences_with(|settings| settings.save_to(&path));
    assert!(app.preferences_draft.is_none());
    assert_eq!(app.settings.connection.listen_port, 50000);
    assert!(app.status.contains("active engine unchanged"));
}

#[test]
fn live_transfer_limits_persist_only_after_ack_and_peer_recheck_keeps_session() {
    let crate::control::test_support::LiveFixture {
        controller,
        root: _root,
        id,
        ..
    } = crate::control::test_support::LiveFixture::new();
    let mut app = app();
    app.control = controller;
    let engine = app.control.backend.test_engine().unwrap();
    let handle = engine.with_torrents(|torrents| torrents.next().unwrap().1.clone());
    let peers = crate::transfer_settings::TransferSettings::from_item(
        &app.control.torrents[0],
        &app.settings,
    )
    .max_peers;
    app.change_transfer(&format!("1 2 {peers} true high 0 0"))
        .unwrap();
    assert!(
        app.control.torrents[0]
            .overrides
            .transfer_settings
            .is_none()
    );
    assert!(
        app.control.backend.load_torrents().unwrap()[0]
            .transfer_settings
            .is_none()
    );
    wait_for_tui(&mut app, |app| {
        app.control.torrents[0]
            .overrides
            .transfer_settings
            .is_some()
    });
    let confirmed = app.control.torrents[0].overrides.transfer_settings.unwrap();
    assert_eq!(confirmed.download_kib, 1);
    assert_eq!(handle.transfer_settings(), confirmed.engine());
    assert_eq!(
        app.control.backend.load_torrents().unwrap()[0].transfer_settings,
        Some(confirmed)
    );
    app.run_command("set max_peers 7").unwrap();
    wait_for_tui(&mut app, |app| {
        app.control.torrents[0]
            .overrides
            .transfer_settings
            .is_some_and(|settings| settings.max_peers == 7)
            && app.control.torrents[0].state == TorrentState::Paused
    });
    let after = app.control.backend.test_engine().unwrap();
    assert!(Arc::ptr_eq(&engine, &after));
    let next = engine.with_torrents(|torrents| torrents.next().unwrap().1.clone());
    assert!(!Arc::ptr_eq(&handle, &next));
    assert_eq!(next.transfer_settings(), confirmed.engine());
    assert_eq!(
        app.control.command_revision(&id).unwrap(),
        app.control
            .read_handle()
            .page(0, 1, None, None)
            .unwrap()
            .torrents[0]
            .command_revision
    );
}

#[test]
fn stale_confirmation_rejects_mutated_targets_and_remove_keeps_payload() {
    let crate::control::test_support::LiveFixture {
        controller,
        root,
        id,
        ..
    } = crate::control::test_support::LiveFixture::new();
    let mut app = app();
    app.control = controller;
    app.request_batch(interaction::BatchAction::Remove);
    app.control
        .submit(&id, TorrentCommand::Resume, None)
        .unwrap();
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.control.torrents.len(), 1);
    assert!(app.status.contains("changed while awaiting"));
    wait_for_tui(&mut app, |app| {
        app.control.torrents[0].state == TorrentState::Seeding
    });
    app.request_batch(interaction::BatchAction::Remove);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.control.torrents.len(), 1);
    wait_for_tui(&mut app, |app| app.control.torrents.is_empty());
    assert!(root.path().join("payload.bin").is_file());
    assert!(root.path().join("fixture.torrent").is_file());
}

#[test]
fn live_queue_admits_high_priority_and_releases_a_slot_on_pause() {
    use sha1::{Digest, Sha1};
    let crate::control::test_support::LiveFixture {
        controller, root, ..
    } = crate::control::test_support::LiveFixture::new();
    let mut app = app();
    app.control = controller;
    let engine = app.control.backend.test_engine().unwrap();
    app.control
        .backend
        .send(crate::session::SessionCommand::ConfigureQueue {
            max_active_downloads: 1,
        })
        .unwrap();
    for name in ["second.bin", "third.bin"] {
        let mut info = format!(
            "d6:lengthi4e4:name{}:{}12:piece lengthi16384e6:pieces20:",
            name.len(),
            name
        )
        .into_bytes();
        info.extend_from_slice(&Sha1::digest(b"data"));
        info.extend_from_slice(b"7:privatei1ee");
        let hash = format!("v1: {:x}", Sha1::digest(&info));
        let mut bytes = b"d4:info".to_vec();
        bytes.extend(info);
        bytes.push(b'e');
        let path = root.path().join(format!("{name}.torrent"));
        std::fs::write(&path, bytes).unwrap();
        let mut item = app.control.torrents[0].clone();
        item.name = name.into();
        item.info_hashes = vec![hash];
        item.source = path.display().to_string();
        item.files = FileSelectionModel::from_files([(name.into(), 4)]);
        item.completion = crate::completion::CompletionStatus::Pending;
        app.control.add(item, &app.settings, None).unwrap();
    }
    wait_for_tui(&mut app, |_| {
        engine.with_torrents(|torrents| {
            let mut ready = 0;
            for (_, torrent) in torrents {
                if matches!(torrent.stats().state, librqbit::TorrentStatsState::Paused) {
                    ready += 1;
                }
            }
            ready == 3
        })
    });
    for (index, priority) in [(1, "low"), (2, "high")] {
        app.selected_torrent = index;
        app.run_command(&format!("set priority {priority}"))
            .unwrap();
        app.resume_selected();
    }
    wait_for_tui(&mut app, |app| {
        app.control.torrents[1..].iter().all(|torrent| {
            torrent.state == TorrentState::Downloading
                && torrent.overrides.transfer_settings.is_some()
        })
    });
    let handles = engine.with_torrents(|torrents| {
        let mut handles = Vec::new();
        for (_, torrent) in torrents {
            if !torrent.stats().finished {
                handles.push(torrent.clone());
            }
        }
        handles.sort_by_key(|torrent| torrent.transfer_settings().priority);
        handles
    });
    assert_eq!(handles.len(), 2);
    assert!(!handles[0].download_admitted());
    assert!(handles[1].download_admitted());
    app.selected_torrent = 2;
    app.pause_selected();
    wait_for_tui(&mut app, |app| {
        app.control.torrents[2].state == TorrentState::Paused
    });
    assert!(handles[0].download_admitted());
    app.control
        .backend
        .send(crate::session::SessionCommand::ConfigureQueue {
            max_active_downloads: 0,
        })
        .unwrap();
    wait_for_tui(&mut app, |_| {
        engine.max_active_downloads.load(Ordering::Relaxed) == 0
    });
    assert!(handles[0].download_admitted());
}
