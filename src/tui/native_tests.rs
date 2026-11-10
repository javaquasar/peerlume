//! Drive the TUI over a real native connection to a disposable owner.
use super::*;
use crate::control::{
    frontend::{ConnectionStatus, Server},
    host::Host,
    test_support::LiveFixture,
};
use ratatui::backend::TestBackend;
use std::sync::Mutex;

struct NativeTui {
    app: TuiApp,
    _server: Server,
    _host: Host,
    owner: Arc<Mutex<Controller>>,
    root: tempfile::TempDir,
}

impl NativeTui {
    fn live() -> Self {
        let LiveFixture {
            controller, root, ..
        } = LiveFixture::new();
        Self::attach(controller, root)
    }

    fn settings_only() -> Self {
        Self::with_settings_fixture(None)
    }

    fn with_settings_fixture(journal: Option<crate::announce_journal::AnnounceJournal>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut torrent = tests::test_torrent();
        torrent.state = TorrentState::Paused;
        let mut store = crate::state::StateStore::in_memory_with_secrets(Arc::new(
            crate::secrets::MemorySecrets::default(),
        ))
        .unwrap();
        store
            .save_torrent(&torrent_to_record(&torrent, None))
            .unwrap();
        let backend = match journal {
            Some(journal) => {
                let runtime = crate::session::SessionRuntime::start_test_with_journal(
                    64,
                    root.path().to_owned(),
                    librqbit::SessionOptions {
                        dht: None,
                        listen: None,
                        disable_trackers: true,
                        disable_local_service_discovery: true,
                        ..Default::default()
                    },
                    journal,
                )
                .unwrap();
                ClientBackend::with_session_and_store(runtime, store)
            }
            None => ClientBackend::with_state_store(store),
        };
        let owner = Controller::new(backend, vec![torrent]);
        Self::attach(owner, root)
    }

    fn attach(mut controller: Controller, root: tempfile::TempDir) -> Self {
        let profile = root.path().to_owned();
        let mut settings = ClientSettings::default();
        settings.download.download_directory = profile.display().to_string();
        settings.download.start_when_added = false;
        settings.speed.upload_limit_kib = 11;
        settings.bittorrent.dht = false;
        settings.bittorrent.lsd = false;
        settings.connection.port_forwarding = false;
        settings.privacy.blocklist_enabled = false;
        settings.save_to(&profile.join("settings.toml")).unwrap();
        controller
            .configure_agents(&profile, profile.clone())
            .unwrap();
        let owner = Arc::new(Mutex::new(controller));
        let host = Host::start(owner.clone(), |controller| controller, profile.clone()).unwrap();
        let server = Server::start(owner.clone(), profile.clone(), settings.clone()).unwrap();
        let backend = ClientBackend::attach(&profile).unwrap();
        let mut app = TuiApp::from_parts(settings, backend, Vec::new(), HashMap::new());
        tests::wait_for_tui(&mut app, |app| {
            !app.control.torrents.is_empty()
                && app
                    .control
                    .backend
                    .attached_client()
                    .unwrap()
                    .connection()
                    .status
                    == ConnectionStatus::Connected
        });
        Self {
            app,
            _server: server,
            _host: host,
            owner,
            root,
        }
    }

    fn disk_settings(&self) -> ClientSettings {
        ClientSettings::load_from(&self.root.path().join("settings.toml")).unwrap()
    }
}

fn key(app: &mut TuiApp, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn enter(app: &mut TuiApp, text: &str) {
    assert!(app.input.is_some());
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    for character in text.chars() {
        key(app, KeyCode::Char(character));
    }
    key(app, KeyCode::Enter);
}

fn command(app: &mut TuiApp, text: &str) {
    key(app, KeyCode::Char(':'));
    enter(app, text);
}

fn render(app: &mut TuiApp) -> String {
    let mut terminal = Terminal::new(TestBackend::new(220, 70)).unwrap();
    terminal
        .draw(|frame| {
            if app.agent_panel.open {
                app.agent_panel.render(&app.control, frame);
            } else {
                app.render(frame);
            }
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn details(app: &mut TuiApp, tab: TorrentDetailsTab) {
    if app.focus != Focus::Details {
        key(app, KeyCode::F(6));
    }
    for _ in 0..TorrentDetailsTab::ALL.len() {
        if app.details_tab == tab {
            break;
        }
        key(app, KeyCode::Right);
    }
    assert_eq!(app.details_tab, tab);
    app.refresh_detail_interest();
}

#[test]
fn native_keyboard_pause_resume_and_removal_follow_owner_confirmations() {
    let mut fixture = NativeTui::live();
    let payload = fixture.root.path().join("payload.bin");
    let source = fixture.root.path().join("fixture.torrent");
    let original_payload = std::fs::read(&payload).unwrap();
    let original_source = std::fs::read(&source).unwrap();
    let id = fixture.app.selected().unwrap().info_hashes.join(";");

    key(&mut fixture.app, KeyCode::Char('r'));
    assert!(fixture.app.status.contains("1/1 queued"));
    assert_eq!(fixture.app.selected().unwrap().state, TorrentState::Paused);
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.selected().unwrap().state == TorrentState::Seeding
    });
    key(&mut fixture.app, KeyCode::Char('p'));
    assert_eq!(fixture.app.selected().unwrap().state, TorrentState::Seeding);
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.selected().unwrap().state == TorrentState::Paused
    });

    key(&mut fixture.app, KeyCode::Char('d'));
    assert!(
        render(&mut fixture.app)
            .contains("All downloaded files and source .torrent files are kept")
    );
    key(&mut fixture.app, KeyCode::Char('n'));
    assert!(fixture.app.status.contains("no commands sent"));
    assert_eq!(fixture.app.control.torrents.len(), 1);
    key(&mut fixture.app, KeyCode::Char('d'));
    key(&mut fixture.app, KeyCode::Enter);
    assert!(fixture.app.status.contains("1/1 queued"));
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.control.torrents.is_empty()
            && app.log.iter().any(|line| line == &format!("removed {id}"))
    });
    assert!(
        fixture
            .owner
            .lock()
            .unwrap()
            .backend
            .load_torrents()
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read(payload).unwrap(), original_payload);
    assert_eq!(std::fs::read(source).unwrap(), original_source);
}

#[test]
fn native_files_are_hydrated_before_keyboard_edits_and_confirmed_by_owner() {
    let mut fixture = NativeTui::live();
    assert!(!fixture.app.selected().unwrap().files.details_loaded());
    assert!(render(&mut fixture.app).contains("Loading 1 authoritative file details"));
    details(&mut fixture.app, TorrentDetailsTab::Files);
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.selected().unwrap().files.details_loaded()
    });
    assert!(render(&mut fixture.app).contains("[x] payload.bin"));

    key(&mut fixture.app, KeyCode::Char(' '));
    assert!(fixture.app.selected().unwrap().files.files()[0].selected);
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.selected()
            .unwrap()
            .files
            .files()
            .first()
            .is_some_and(|file| !file.selected)
    });
    assert!(render(&mut fixture.app).contains("[ ] payload.bin"));
    key(&mut fixture.app, KeyCode::Char(' '));
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.selected()
            .unwrap()
            .files
            .files()
            .first()
            .is_some_and(|file| file.selected)
    });
    command(&mut fixture.app, "file-priority 1 high");
    tests::wait_for_tui(&mut fixture.app, |app| {
        let files = &app.selected().unwrap().files;
        files.details_loaded() && files.is_high_priority(0)
    });
    assert!(render(&mut fixture.app).contains("[high] payload.bin"));
    command(&mut fixture.app, "file-priority 1 normal");
    tests::wait_for_tui(&mut fixture.app, |app| {
        let files = &app.selected().unwrap().files;
        files.details_loaded() && !files.is_high_priority(0)
    });
    let records = fixture
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap();
    assert!(records[0].files[0].selected);
    assert_eq!(records[0].files[0].priority, 0);
}

#[test]
fn native_preferences_and_view_save_keep_the_unsaved_global_draft_separate() {
    let mut fixture = NativeTui::settings_only();
    let app = &mut fixture.app;
    key(app, KeyCode::Char('2'));
    for _ in 0..PreferencesTab::ALL.len() {
        if app.preferences_tab == PreferencesTab::Speed {
            break;
        }
        key(app, KeyCode::Right);
    }
    assert_eq!(app.preferences_tab, PreferencesTab::Speed);
    let fields = preferences_editor::fields(&app.settings, app.preferences_tab);
    let row = fields
        .iter()
        .position(|(key, _)| key == "upload_limit_kib")
        .unwrap();
    for _ in 0..row {
        key(app, KeyCode::Down);
    }
    key(app, KeyCode::Enter);
    enter(app, "41");
    assert_eq!(app.settings.speed.upload_limit_kib, 11);
    assert_eq!(
        app.preferences_draft
            .as_ref()
            .unwrap()
            .speed
            .upload_limit_kib,
        41
    );
    key(app, KeyCode::Char('s'));
    assert!(app.status.contains("draft retained"));
    tests::wait_for_tui(app, |app| {
        app.preferences_draft.is_none() && app.settings.speed.upload_limit_kib == 41
    });

    key(app, KeyCode::Enter);
    enter(app, "99");
    key(app, KeyCode::Char('1'));
    command(app, "label reviewed");
    command(app, "categorize Fixtures");
    command(app, "category Fixtures");
    command(app, "sort ratio");
    command(app, "descending true");
    command(app, "split 55");
    command(app, "save-view");
    assert_eq!(app.status, "Awaiting owner view save");
    tests::wait_for_tui(app, |app| app.status.contains("View saved"));
    assert_eq!(
        app.preferences_draft
            .as_ref()
            .unwrap()
            .speed
            .upload_limit_kib,
        99
    );
    let disk = fixture.disk_settings();
    assert_eq!(disk.speed.upload_limit_kib, 41);
    assert_eq!(disk.ui.sort, crate::torrent_view::Sort::Ratio);
    assert!(disk.ui.descending);
    assert_eq!(disk.ui.torrent_pane_percent, 55);
    assert_eq!(disk.ui.category_filter, "Fixtures");
    assert_eq!(
        disk.ui.labels.values().next().map(String::as_str),
        Some("reviewed")
    );
}

#[test]
fn native_trusted_agent_keys_require_explicit_enable_grant_and_revoke() {
    let mut fixture = NativeTui::settings_only();
    let app = &mut fixture.app;
    key(app, KeyCode::F(8));
    assert!(render(app).contains("Local IPC enabled: false"));
    key(app, KeyCode::Char('e'));
    tests::wait_for_tui(app, |app| {
        app.control
            .backend
            .attached_client()
            .unwrap()
            .access_view()
            .enabled
    });
    key(app, KeyCode::Char('m'));
    key(app, KeyCode::Char('g'));
    tests::wait_for_tui(app, |app| app.agent_panel.token.len() == 64);
    let token = app.agent_panel.token.clone();
    assert!(
        render(app).contains(&token),
        "The trusted panel must expose the new token"
    );
    assert!(
        app.control
            .backend
            .attached_client()
            .unwrap()
            .access_view()
            .clients[0]
            .grants
            .manage
    );
    key(app, KeyCode::Char('x'));
    tests::wait_for_tui(app, |app| {
        app.control
            .backend
            .attached_client()
            .unwrap()
            .access_view()
            .clients
            .is_empty()
    });
    key(app, KeyCode::Esc);
    assert!(!app.agent_panel.open);
    assert!(app.agent_panel.token.is_empty());
    assert!(!render(app).contains(&token));
}

#[test]
fn native_import_review_keeps_repair_policy_and_waits_for_registration() {
    use sha1::{Digest, Sha1};
    let mut fixture = NativeTui::live();
    let payload = b"second local TUI fixture";
    let mut torrent = format!(
        "d4:infod6:lengthi{}e4:name9:fresh.bin12:piece lengthi16384e6:pieces20:",
        payload.len()
    )
    .into_bytes();
    torrent.extend_from_slice(&Sha1::digest(payload));
    torrent.extend_from_slice(b"7:privatei1eee");
    let path = fixture.root.path().join("fresh.torrent");
    std::fs::write(&path, torrent).unwrap();
    std::fs::write(fixture.root.path().join("fresh.bin"), payload).unwrap();

    key(&mut fixture.app, KeyCode::Char('a'));
    enter(&mut fixture.app, path.to_str().unwrap());
    tests::wait_for_tui(&mut fixture.app, |app| app.pending_add.is_some());
    assert_eq!(fixture.app.control.torrents.len(), 1);
    key(&mut fixture.app, KeyCode::F(4));
    key(&mut fixture.app, KeyCode::F(3));
    assert!(render(&mut fixture.app).contains("Automatic repair: true"));
    assert!(fixture.app.pending_verify_after_download);
    enter(&mut fixture.app, "1");
    assert!(fixture.app.status.contains("Awaiting owner registration"));
    assert_eq!(fixture.app.control.torrents.len(), 1);
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.control
            .torrents
            .iter()
            .any(|item| item.name == "fresh.bin" && item.runtime.registered)
    });
    let records = fixture
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap();
    let added = records
        .iter()
        .find(|record| record.name == "fresh.bin")
        .unwrap();
    assert!(added.verify_after_download);
    assert!(added.repair_policy.enabled);
}

#[test]
fn native_log_keys_export_selected_and_complete_journals_without_overwriting() {
    let journal_root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(journal_root.path());
    let torrent = tests::test_torrent();
    let hash = crate::announce_journal::torrent_hash(&torrent.info_hashes).unwrap();
    let other_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    journal.record(crate::announce_journal::tests::observation(hash));
    journal.record(crate::announce_journal::tests::observation(other_hash));
    let mut fixture = NativeTui::with_settings_fixture(Some(journal));
    let selected = fixture.root.path().join("selected.jsonl");
    let complete = fixture.root.path().join("complete.jsonl");
    let app = &mut fixture.app;
    details(app, TorrentDetailsTab::Log);
    tests::wait_for_tui(app, |app| {
        app.control
            .backend
            .journal()
            .unwrap()
            .page(Some(hash), 0, 1)
            .total
            == 1
    });
    let screen = render(app);
    assert!(screen.contains("1 live entries"));
    assert!(screen.contains("JSONL:"));
    assert!(screen.contains("http://localhost:1"));

    for (key_code, path, expected) in [('e', &selected, 1), ('E', &complete, 2)] {
        key(app, KeyCode::Char(key_code));
        enter(app, path.to_str().unwrap());
        assert!(
            app.status
                .contains("Awaiting background owner full journal export")
        );
        tests::wait_for_tui(app, |app| {
            app.status == format!("Exported {expected} records")
        });
    }
    let selected_bytes = std::fs::read_to_string(&selected).unwrap();
    let complete_bytes = std::fs::read_to_string(&complete).unwrap();
    let hashes = |text: &str| {
        text.lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["info_hash"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(hashes(&selected_bytes), [hash]);
    assert_eq!(hashes(&complete_bytes), [hash, other_hash]);

    key(app, KeyCode::Char('e'));
    enter(app, selected.to_str().unwrap());
    tests::wait_for_tui(app, |app| {
        app.status.starts_with("Transfer error: Journal export:")
    });
    assert_eq!(std::fs::read_to_string(selected).unwrap(), selected_bytes);
}

#[test]
fn command_palette_exports_redacted_diagnostics_to_a_new_absolute_path() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("diagnostics.json");
    let mut app = tests::app();
    let torrent = app.selected_mut().unwrap();
    torrent.name = "private fixture name".into();
    torrent.trackers = vec!["http://localhost:1/announce?uk=fixture-secret".into()];
    torrent.runtime.network_downloaded = 42;

    command(&mut app, "diagnostics relative.json");
    assert_eq!(app.status, "Use an absolute report path");
    let export = format!("diagnostics {}", path.display());
    command(&mut app, &export);
    assert_eq!(app.status, "Redacted diagnostics exported");
    let report = std::fs::read_to_string(&path).unwrap();
    assert!(!report.contains("private fixture name"));
    assert!(!report.contains("fixture-secret"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&report).unwrap()["downloaded"],
        42
    );

    command(&mut app, &export);
    assert!(app.status.contains("existing files are never overwritten"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), report);
}

#[test]
fn native_speed_command_applies_and_saves_policy_without_overwriting_the_draft() {
    use crate::settings::speed_policy::SpeedPolicy;

    let mut fixture = NativeTui::live();
    let app = &mut fixture.app;
    tests::wait_for_tui(app, |app| app.live_speed.is_some());
    let mut draft = app.settings.clone();
    draft.speed.download_limit_kib = 777;
    app.preferences_draft = Some(draft);
    command(app, "speed quiet");
    tests::wait_for_tui(app, |app| {
        app.live_speed.as_ref().is_some_and(|view| {
            !view.pending
                && view
                    .applied
                    .as_ref()
                    .is_some_and(|applied| applied.policy == SpeedPolicy::Quiet && applied.quiet)
                && view
                    .saved
                    .as_ref()
                    .is_some_and(|saved| saved.policy() == SpeedPolicy::Quiet)
        })
    });
    assert_eq!(
        app.preferences_draft
            .as_ref()
            .unwrap()
            .speed
            .download_limit_kib,
        777
    );
    assert_eq!(
        app.preferences_draft.as_ref().unwrap().speed.policy(),
        SpeedPolicy::Quiet
    );
    app.page = MainPage::Preferences;
    app.preferences_tab = PreferencesTab::Speed;
    assert!(render(app).contains("Active: Quiet"));
    assert_eq!(fixture.disk_settings().speed.policy(), SpeedPolicy::Quiet);
    assert_ne!(fixture.disk_settings().speed.download_limit_kib, 777);
}

#[test]
fn native_operations_panel_shows_durable_view_save_and_closes() {
    let mut fixture = NativeTui::settings_only();
    command(&mut fixture.app, "split 56");
    command(&mut fixture.app, "save-view");
    command(&mut fixture.app, "operations");
    tests::wait_for_tui(&mut fixture.app, |app| {
        app.control
            .backend
            .attached_client()
            .and_then(|client| client.operation_view())
            .is_some_and(|view| {
                !view.stale
                    && view.page.entries.iter().any(|entry| {
                        entry.command == "Save view"
                            && entry.state
                                == crate::control::frontend::NativeOperationState::Succeeded
                    })
            })
    });
    assert_eq!(fixture.disk_settings().ui.torrent_pane_percent, 56);
    let screen = render(&mut fixture.app);
    assert!(screen.contains("Background operations"));
    assert!(screen.contains("Save view"));
    assert!(screen.contains("Succeeded"));
    key(&mut fixture.app, KeyCode::Esc);
    fixture.app.poll();
    assert!(!fixture.app.operations.is_open());
    assert!(
        fixture
            .app
            .control
            .backend
            .attached_client()
            .unwrap()
            .operation_view()
            .is_none()
    );
}
