use super::*;
use crate::{
    client::{ClientBackend, SessionNotice, tests::torrent, torrent_to_record},
    session::SessionEvent,
    state::{StateStore, StoredTrackerAuditProfile, StoredTrackerAuditStep},
    torrent::EffectiveTorrentSettings,
};

struct Fixture {
    service: Arc<Service>,
    directory: tempfile::TempDir,
    original_root: String,
    explicit_root: String,
    replacement_root: String,
}
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = |name: &str| {
        let path = directory.path().join(name);
        std::fs::create_dir(&path).unwrap();
        path.display().to_string()
    };
    let original_root = root("original-root");
    let explicit_root = root("explicit-root");
    let replacement_root = root("new-default");
    let mut settings = ClientSettings::default();
    settings.download.download_directory = original_root.clone();
    let mut first = torrent();
    first.state = TorrentState::Paused;
    first.overrides.download_directory = None;
    let mut second = torrent();
    second.name = "unrelated synthetic row".into();
    second.info_hashes = vec!["v1: def".into()];
    second.state = TorrentState::Paused;
    second.overrides.download_directory = Some(explicit_root.clone());
    let mut backend = ClientBackend::with_state_store(
        StateStore::open(&directory.path().join("state.sqlite3")).unwrap(),
    );
    for item in [&first, &second] {
        backend
            .save_torrent(&torrent_to_record(item, None))
            .unwrap();
    }
    first.files.defer_details();
    second.files.defer_details();
    let mut owner = Controller::new(backend, vec![first, second]);
    owner
        .configure_agents(directory.path(), PathBuf::from(&original_root))
        .unwrap();
    owner.set_agent_settings(&settings).unwrap();
    let service = Arc::new(Service {
        owner: Arc::new(Mutex::new(owner)),
        directory: directory.path().to_owned(),
        settings: Mutex::new(settings),
        saved_speed: Default::default(),
        history: Some(History::memory()),
        instance: "synthetic-native-owner".into(),
        token_hash: "not-used-by-direct-dispatch-tests".into(),
        requested: Default::default(),
        request_wake: Default::default(),
        stop: Default::default(),
        wake: Default::default(),
        imports: tokio::sync::Semaphore::new(1),
        plans: Default::default(),
    });
    Fixture {
        directory,
        service,
        original_root,
        explicit_root,
        replacement_root,
    }
}

#[test]
fn native_history_unavailable_preserves_existing_preference_command() {
    let mut fixture = fixture();
    Arc::get_mut(&mut fixture.service).unwrap().history = None;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let view = crate::settings::UiSettings {
        search: "durable without optional history".into(),
        ..Default::default()
    };
    let (operation, result) = runtime.block_on(fixture.service.dispatch_tracked(
        "synthetic-client".into(),
        &"a".repeat(64),
        Action::SaveView(view.clone()),
    ));
    assert!(operation.is_none());
    assert!(matches!(result, Ok(Reply::Done(_))));
    let saved = ClientSettings::load_from(&fixture.directory.path().join("settings.toml")).unwrap();
    assert_eq!(saved.ui, view);
    assert!(matches!(
        dispatch(&fixture.service, Action::Operations { before: None, limit: 8 }),
        Err(Error::Failed(message)) if message == "Owner operation history is unavailable"
    ));
}

#[test]
fn stop_is_requested_only_after_the_authenticated_reply_is_transmitted() {
    let mut fixture = fixture();
    let token = "a".repeat(64);
    Arc::get_mut(&mut fixture.service).unwrap().token_hash =
        crate::control::access::digest(token.as_bytes());
    assert!(matches!(
        dispatch(&fixture.service, Action::Stop).unwrap(),
        Reply::Done(_)
    ));
    assert!(!fixture.service.requested.load(Ordering::Acquire));
    let service = fixture.service.clone();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            use tokio::io::AsyncWriteExt;
            let (mut client, server) = tokio::io::duplex(4096);
            let task = tokio::spawn(connection(server, service.clone()));
            let request = Request {
                protocol: PROTOCOL_VERSION,
                request_id: "stop-test".into(),
                client_id: "b".repeat(64),
                instance_id: Some(service.instance.clone()),
                token,
                action: Action::Stop,
            };
            let mut bytes = serde_json::to_vec(&request).unwrap();
            bytes.push(b'\n');
            client.write_all(&bytes).await.unwrap();
            let reply = transport::read(&mut BufReader::new(client)).await.unwrap();
            let reply: Response = serde_json::from_slice(&reply).unwrap();
            assert!(matches!(reply.result.unwrap(), Reply::Done(_)));
            task.await.unwrap();
            assert!(service.requested.load(Ordering::Acquire));
        });
}

#[test]
fn invalid_background_preferences_are_rejected_before_root_or_file_changes() {
    let fixture = fixture();
    let before = revision(&fixture.service, "v1: abc");
    let records = fixture
        .service
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap();
    let mut relative = fixture.service.settings.lock().unwrap().clone();
    relative.download.download_directory = "relative-downloads".into();
    assert!(dispatch(&fixture.service, Action::SaveSettings(relative)).is_err());
    let mut unsupported = fixture.service.settings.lock().unwrap().clone();
    unsupported.connection.bind_address = "127.0.0.1".into();
    assert!(dispatch(&fixture.service, Action::ApplySettings(unsupported)).is_err());
    assert!(!fixture.directory.path().join("settings.toml").exists());
    assert_eq!(revision(&fixture.service, "v1: abc"), before);
    assert_eq!(
        fixture
            .service
            .owner
            .lock()
            .unwrap()
            .backend
            .load_torrents()
            .unwrap(),
        records
    );
}

#[test]
fn a_removed_ack_between_native_pages_invalidates_the_pinned_epoch() {
    let fixture = fixture();
    {
        let mut owner = fixture.service.owner.lock().unwrap();
        let template = owner.torrents[0].clone();
        for index in 0..260 {
            let mut item = template.clone();
            item.info_hashes = vec![format!("v1: extra-{index}")];
            owner.torrents.push(item);
        }
        owner.publish();
        owner.pending_removals.insert("v1: abc".into());
    }
    let Reply::Snapshot(first) = dispatch(
        &fixture.service,
        Action::Snapshot {
            offset: 0,
            limit: 128,
            revision: None,
            detail: None,
        },
    )
    .unwrap() else {
        panic!("snapshot required")
    };
    assert_eq!(first.total, 262);
    fixture
        .service
        .owner
        .lock()
        .unwrap()
        .apply(SessionEvent::Removed {
            id: "v1: abc".into(),
        });
    // Deliberately omit publish: an IPC read can win the mutex before the next
    // owner tick, but it must still reject the old structural page cursor.
    assert!(matches!(
        dispatch(
            &fixture.service,
            Action::Snapshot {
                offset: 128,
                limit: 128,
                revision: Some(first.epoch),
                detail: None,
            }
        ),
        Err(Error::Stale)
    ));
    let Reply::Snapshot(current) = dispatch(
        &fixture.service,
        Action::Snapshot {
            offset: 0,
            limit: 128,
            revision: None,
            detail: None,
        },
    )
    .unwrap() else {
        panic!("snapshot required")
    };
    assert_eq!(current.total, 261);
    assert_ne!(current.epoch, first.epoch);
}

#[test]
fn journal_page_and_complete_export_resolve_canonical_torrent_identity() {
    let fixture = fixture();
    let journal =
        crate::announce_journal::AnnounceJournal::open(&fixture.directory.path().join("journal"));
    journal.record(crate::announce_journal::tests::observation("abc"));
    journal.record(crate::announce_journal::tests::observation("def"));
    let runtime = crate::session::SessionRuntime::start_test_with_journal(
        8,
        fixture.directory.path().to_owned(),
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
    fixture.service.owner.lock().unwrap().backend = ClientBackend::with_session_and_store(
        runtime,
        StateStore::open(&fixture.directory.path().join("state.sqlite3")).unwrap(),
    );
    let Reply::Journal { entries, total, .. } = dispatch(
        &fixture.service,
        Action::Journal {
            id: Some("v1: abc".into()),
            offset: 0,
            limit: 32,
        },
    )
    .unwrap() else {
        panic!("missing journal")
    };
    assert_eq!(total, 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].announce.info_hash, "abc");
    let output = fixture.directory.path().join("export.jsonl");
    assert!(matches!(
        dispatch(
            &fixture.service,
            Action::Export {
                id: Some("v1: abc".into()),
                destination: output.clone()
            }
        )
        .unwrap(),
        Reply::Done(_)
    ));
    let records: Vec<crate::announce_journal::JournalEntry> = std::fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].announce.info_hash, "abc");
}
fn dispatch(service: &Arc<Service>, action: Action) -> Result<Reply, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(service.dispatch("synthetic-client".into(), action, None))
}
fn revision(service: &Service, id: &str) -> u64 {
    service.owner.lock().unwrap().command_revision(id).unwrap()
}
fn audit() -> StoredTrackerAuditProfile {
    StoredTrackerAuditProfile {
        allowed_host: "synthetic.tracker.test".into(),
        port: 51413,
        started_only: false,
        observation_seconds: 30,
        steps: vec![StoredTrackerAuditStep {
            delay_seconds: 0,
            uploaded: 1,
            downloaded: 2,
            left: 98,
            event: Some("started".into()),
        }],
    }
}

#[test]
fn saving_audit_advances_only_addressed_cas_and_rejects_old_revision_without_db_change() {
    let fixture = fixture();
    let service = &fixture.service;
    let before = revision(service, "v1: abc");
    let other_before = revision(service, "v1: def");
    let expected = audit();
    assert!(matches!(
        dispatch(
            service,
            Action::SaveAudit {
                id: "v1: abc".into(),
                revision: before,
                audit: Some(expected.clone())
            }
        ),
        Ok(Reply::Done(_))
    ));
    let after = revision(service, "v1: abc");
    assert!(after > before);
    assert_eq!(revision(service, "v1: def"), other_before);
    assert!(matches!(
        dispatch(
            service,
            Action::SaveAudit {
                id: "v1: abc".into(),
                revision: before,
                audit: None
            }
        ),
        Err(Error::Stale)
    ));
    let owner = service.owner.lock().unwrap();
    assert!(
        owner
            .torrents
            .iter()
            .all(|item| !item.files.details_loaded() && item.files.files().is_empty())
    );
    assert_eq!(
        owner.backend.load_tracker_audit("v1: abc").unwrap(),
        Some(expected.clone())
    );
    let stored = owner.backend.load_torrents().unwrap();
    assert_eq!(stored[0].files.len(), 1);
    assert_eq!(stored[1].tracker_audit, None);
    drop(owner);
    let Reply::TrackerAudit { id, audit } = dispatch(
        service,
        Action::TrackerAudit {
            id: "v1: abc".into(),
        },
    )
    .unwrap() else {
        panic!("audit metadata reply required")
    };
    assert_eq!(id, "v1: abc");
    assert_eq!(audit, Some(expected));
    assert_eq!(
        revision(service, "v1: abc"),
        after,
        "read-only metadata cannot consume a CAS revision"
    );
}

#[test]
fn failed_audit_transaction_keeps_cas_revision_and_retry_uses_the_same_revision() {
    let fixture = fixture();
    let service = &fixture.service;
    let before = revision(service, "v1: abc");
    let connection =
        rusqlite::Connection::open(fixture.directory.path().join("state.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_synthetic_audit BEFORE INSERT ON tracker_audit_profiles BEGIN SELECT RAISE(ABORT,'synthetic audit write failure'); END;").unwrap();
    assert!(
        matches!(dispatch(service,Action::SaveAudit{id:"v1: abc".into(),revision:before,audit:Some(audit())}),Err(Error::Failed(message)) if message.contains("synthetic audit write failure"))
    );
    assert_eq!(revision(service, "v1: abc"), before);
    assert!(
        service
            .owner
            .lock()
            .unwrap()
            .backend
            .load_tracker_audit("v1: abc")
            .unwrap()
            .is_none()
    );
    connection
        .execute_batch("DROP TRIGGER reject_synthetic_audit;")
        .unwrap();
    assert!(matches!(
        dispatch(
            service,
            Action::SaveAudit {
                id: "v1: abc".into(),
                revision: before,
                audit: Some(audit())
            }
        ),
        Ok(Reply::Done(_))
    ));
    assert!(revision(service, "v1: abc") > before);
}

#[test]
fn changing_default_pins_existing_inherited_storage_before_saving_new_preferences() {
    let fixture = fixture();
    let service = &fixture.service;
    let other_before = revision(service, "v1: def");
    let mut settings = service.settings.lock().unwrap().clone();
    settings.download.download_directory = fixture.replacement_root.clone();
    assert!(matches!(
        dispatch(service, Action::SaveSettings(settings.clone())),
        Ok(Reply::Done(_))
    ));
    assert_eq!(
        revision(service, "v1: def"),
        other_before,
        "an explicit unrelated root was not edited"
    );
    let owner = service.owner.lock().unwrap();
    assert_eq!(
        owner.torrents[0].overrides.download_directory.as_deref(),
        Some(fixture.original_root.as_str())
    );
    assert_eq!(
        owner.torrents[1].overrides.download_directory.as_deref(),
        Some(fixture.explicit_root.as_str())
    );
    assert_eq!(
        EffectiveTorrentSettings::resolve(&settings, &owner.torrents[0].overrides, false)
            .download_directory,
        fixture.original_root
    );
    assert!(
        owner
            .torrents
            .iter()
            .all(|item| !item.files.details_loaded())
    );
    drop(owner);
    assert_eq!(
        ClientSettings::load_from(&fixture.directory.path().join("settings.toml"))
            .unwrap()
            .download
            .download_directory,
        fixture.replacement_root
    );
    let reopened = StateStore::open(&fixture.directory.path().join("state.sqlite3")).unwrap();
    let records = reopened.load_all_torrents().unwrap();
    assert_eq!(
        records[0].download_directory.as_deref(),
        Some(fixture.original_root.as_str())
    );
    assert_eq!(
        records[1].download_directory.as_deref(),
        Some(fixture.explicit_root.as_str())
    );
    assert!(records.iter().all(|record| record.files.len() == 1));
}

#[test]
fn preferences_failure_keeps_previously_inherited_engine_path_durably_pinned() {
    let fixture = fixture();
    let service = &fixture.service;
    let mut settings = service.settings.lock().unwrap().clone();
    settings.download.download_directory = fixture.replacement_root.clone();
    let destination = fixture.directory.path().join("settings.toml");
    std::fs::create_dir(&destination).unwrap(); // Disposable synthetic write blocker.
    assert!(matches!(
        dispatch(service, Action::SaveSettings(settings)),
        Err(Error::Failed(_))
    ));
    assert_eq!(
        service.settings.lock().unwrap().download.download_directory,
        fixture.original_root
    );
    let owner = service.owner.lock().unwrap();
    assert_eq!(
        owner.torrents[0].overrides.download_directory.as_deref(),
        Some(fixture.original_root.as_str())
    );
    let records = owner.backend.load_torrents().unwrap();
    assert_eq!(
        records[0].download_directory.as_deref(),
        Some(fixture.original_root.as_str())
    );
    assert_eq!(
        records[1].download_directory.as_deref(),
        Some(fixture.explicit_root.as_str())
    );
}

#[test]
fn native_notice_reads_have_independent_cursors_and_do_not_change_owner_or_database() {
    let fixture = fixture();
    let service = &fixture.service;
    let (command_revision, frontend_revision, stored) = {
        let mut owner = service.owner.lock().unwrap();
        owner.apply(SessionEvent::Error {
            id: "v1: abc".into(),
            message: "synthetic native diagnostic".into(),
        });
        (
            owner.command_revision("v1: abc").unwrap(),
            owner.frontend_revision(),
            owner.backend.load_torrents().unwrap(),
        )
    };
    let read = || match dispatch(service, Action::Notices { after: 0, limit: 1 }).unwrap() {
        Reply::Notices {
            cursor,
            resync,
            entries,
        } => (cursor, resync, entries),
        _ => panic!("diagnostic reply required"),
    };
    let first = read();
    assert_eq!(
        first.2,
        vec![SessionNotice::Error {
            id: "v1: abc".into(),
            message: "synthetic native diagnostic".into()
        }]
    );
    assert_eq!(
        read(),
        first,
        "another native reader cannot consume shared diagnostics"
    );
    let Reply::Notices {
        cursor,
        resync,
        entries,
    } = dispatch(
        service,
        Action::Notices {
            after: first.0,
            limit: 128,
        },
    )
    .unwrap()
    else {
        panic!("diagnostic reply required")
    };
    assert_eq!(cursor, first.0);
    assert!(!resync && entries.is_empty());
    let owner = service.owner.lock().unwrap();
    assert_eq!(owner.command_revision("v1: abc").unwrap(), command_revision);
    assert_eq!(owner.frontend_revision(), frontend_revision);
    assert_eq!(owner.backend.load_torrents().unwrap(), stored);
}
