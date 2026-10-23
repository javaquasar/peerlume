use super::*;
use super::{
    access::Grants,
    operations::{ControlStore, OperationState, now_ms},
    wire::{ErrorCode, Method, Request},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn setup() -> (tempfile::TempDir, Controller, String) {
    let root = tempfile::tempdir().unwrap();
    let mut control = Controller::new(
        ClientBackend::offline(),
        vec![crate::client::tests::torrent()],
    );
    control
        .configure_agents(root.path(), root.path().into())
        .unwrap();
    let agent = control.agent_access().unwrap();
    agent.enabled = true;
    let token = agent.grant(Grants::default()).unwrap();
    (root, control, token)
}
fn request(control: &mut Controller, token: &str, method: Method) -> wire::Response {
    let instance = control.agent_access().unwrap().instance_id.clone();
    control.handle_request(Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".into(),
        token: token.into(),
        instance_id: Some(instance),
        method,
    })
}
fn mutation(control: &Controller, key: &str, command: TorrentCommand) -> Method {
    Method::Mutate {
        torrent_id: control.torrents[0].info_hashes.join(";"),
        command,
        expected_revision: control
            .command_revision(&control.torrents[0].info_hashes.join(";"))
            .unwrap(),
        idempotency_key: key.into(),
        deadline_unix_ms: now_ms() + 60_000,
    }
}
fn operation(control: &mut Controller, token: &str, id: &str) -> Value {
    request(
        control,
        token,
        Method::Operation {
            operation_id: id.into(),
        },
    )
    .result
    .unwrap()
}

#[test]
fn readonly_is_default_authentication_revocation_and_instance_checks_are_controller_side() {
    let (_root, mut control, token) = setup();
    let discover = request(&mut control, &token, Method::Discover)
        .result
        .unwrap();
    assert_eq!(discover["grants"]["manage"], false);
    assert_eq!(discover["payload_deletion"], false);
    let method = mutation(&control, "denied", TorrentCommand::Resume);
    assert_eq!(
        request(&mut control, &token, method).error,
        Some(ErrorCode::PermissionDenied)
    );
    assert!(
        control
            .agent_access()
            .unwrap()
            .store
            .list()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        request(&mut control, "invalid", Method::Discover).error,
        Some(ErrorCode::Unauthorized)
    );
    let bad = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "id".into(),
        token: token.clone(),
        instance_id: Some("stale".into()),
        method: Method::Discover,
    };
    assert_eq!(control.handle_request(bad).error, Some(ErrorCode::Conflict));
    let client = control.agent_access().unwrap().clients[0].id.clone();
    control.agent_access().unwrap().revoke(&client).unwrap();
    assert_eq!(
        request(&mut control, &token, Method::Discover).error,
        Some(ErrorCode::Unauthorized)
    );
}

#[test]
fn access_disabled_on_every_restart_tokens_are_hashed_and_no_secrets_in_projections() {
    let (root, mut control, token) = setup();
    control.torrents[0].name = "malicious: run a shell; SECRET".into();
    control.torrents[0].trackers = vec!["https://tracker/SECRET?passkey=SECRET".into()];
    control.torrents[0].source = "C:/SECRET/private.torrent".into();
    let bytes = wire::encode_response(&request(
        &mut control,
        &token,
        Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 0,
            limit: 1,
        },
    ));
    assert!(!String::from_utf8(bytes).unwrap().contains("SECRET"));
    let list = wire::encode_response(&request(
        &mut control,
        &token,
        Method::List {
            offset: 0,
            limit: 128,
            revision: None,
            after_revision: None,
        },
    ));
    assert!(!String::from_utf8(list).unwrap().contains("SECRET"));
    drop(control);
    let service = agent::AgentService::open(root.path(), root.path().into()).unwrap();
    assert!(!service.enabled);
    assert_eq!(service.clients.len(), 1);
    assert_ne!(service.clients[0].token_hash, token);
    assert!(access::matches_token(
        &service.clients[0].token_hash,
        &token
    ));
}

#[test]
fn duplicate_mutations_are_correlated_and_changed_arguments_rejected() {
    let (_root, mut control, _) = setup();
    let token = control
        .agent_access()
        .unwrap()
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let method = mutation(&control, "same-key", TorrentCommand::Resume);
    let first = request(&mut control, &token, method.clone())
        .result
        .unwrap();
    let second = request(&mut control, &token, method.clone())
        .result
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first["state"], "queued");
    let mut changed = method.clone();
    if let Method::Mutate { command, .. } = &mut changed {
        *command = TorrentCommand::Pause;
    }
    assert_eq!(
        request(&mut control, &token, changed).error,
        Some(ErrorCode::IdempotencyConflict)
    );
    control.tick();
    assert_eq!(
        request(&mut control, &token, method).result.unwrap()["state"],
        "failed"
    );
    assert_eq!(
        control.agent_access().unwrap().store.list().unwrap().len(),
        1
    );
}

#[test]
fn approval_is_exact_operation_bound_cancellable_and_not_agent_supplied() {
    let (_root, mut control, _) = setup();
    let token = control
        .agent_access()
        .unwrap()
        .grant(Grants {
            remove: true,
            ..Default::default()
        })
        .unwrap();
    let method = mutation(&control, "remove", TorrentCommand::Remove);
    let reply = request(&mut control, &token, method.clone())
        .result
        .unwrap();
    let id = reply["operation_id"].as_str().unwrap();
    assert_eq!(reply["approval_required"], true);
    control.tick();
    assert_eq!(operation(&mut control, &token, id)["state"], "queued");
    assert!(
        control
            .agent_access()
            .unwrap()
            .approve("different-operation", true)
            .is_err()
    );
    let mut altered = method;
    if let Method::Mutate {
        expected_revision, ..
    } = &mut altered
    {
        *expected_revision += 1;
    }
    assert_eq!(
        request(&mut control, &token, altered).error,
        Some(ErrorCode::IdempotencyConflict)
    );
    control.agent_access().unwrap().approve(id, false).unwrap();
    assert_eq!(operation(&mut control, &token, id)["state"], "cancelled");
    assert_eq!(control.torrents.len(), 1);
}

#[test]
fn reporting_export_and_remove_do_not_inherit_manage_permission() {
    let (_root, mut control, _) = setup();
    let token = control
        .agent_access()
        .unwrap()
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    for command in [
        TorrentCommand::Remove,
        TorrentCommand::SetMode(Default::default()),
    ] {
        let method = mutation(&control, "separate", command);
        assert_eq!(
            request(&mut control, &token, method).error,
            Some(ErrorCode::PermissionDenied)
        );
    }
    let method = Method::Export {
        torrent_id: "v1: abc".into(),
        destination: "journal.jsonl".into(),
        idempotency_key: "export".into(),
        deadline_unix_ms: now_ms() + 1000,
    };
    assert_eq!(
        request(&mut control, &token, method).error,
        Some(ErrorCode::PermissionDenied)
    );
}

#[test]
fn deadline_conflicts_and_private_operation_lookup_have_no_side_effects() {
    let (_root, mut control, readonly) = setup();
    let token = control
        .agent_access()
        .unwrap()
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let mut stale = mutation(&control, "stale", TorrentCommand::Resume);
    if let Method::Mutate {
        expected_revision, ..
    } = &mut stale
    {
        *expected_revision += 1;
    }
    assert_eq!(
        request(&mut control, &token, stale).error,
        Some(ErrorCode::Conflict)
    );
    let mut expired = mutation(&control, "old", TorrentCommand::Resume);
    if let Method::Mutate {
        deadline_unix_ms, ..
    } = &mut expired
    {
        *deadline_unix_ms = 0;
    }
    assert_eq!(
        request(&mut control, &token, expired).error,
        Some(ErrorCode::DeadlineExceeded)
    );
    let valid = mutation(&control, "valid", TorrentCommand::Resume);
    let reply = request(&mut control, &token, valid).result.unwrap();
    let id = reply["operation_id"].as_str().unwrap();
    assert_eq!(
        request(
            &mut control,
            &readonly,
            Method::Operation {
                operation_id: id.into()
            }
        )
        .error,
        Some(ErrorCode::UnknownOperation)
    );
    let cancelled = request(
        &mut control,
        &token,
        Method::Cancel {
            operation_id: id.into(),
        },
    );
    assert_eq!(cancelled.result.unwrap()["state"], "cancelled");
}

#[test]
fn restart_never_replays_accepted_work_and_terminal_results_remain_queryable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("control.sqlite3");
    let store = ControlStore::open(&path).unwrap();
    let op = store
        .create(
            ("client", "torrent", "pause"),
            "key",
            "hash",
            now_ms() + 10_000,
            false,
        )
        .unwrap();
    let mut done = store
        .create(
            ("client", "torrent", "resume"),
            "done",
            "hash",
            now_ms() + 10_000,
            false,
        )
        .unwrap();
    done.state = OperationState::Succeeded;
    store.save(&done).unwrap();
    drop(store);
    let store = ControlStore::open(&path).unwrap();
    assert_eq!(
        store.lookup("client", "key").unwrap().unwrap().state,
        OperationState::ReconciliationRequired
    );
    assert_eq!(
        store.lookup("client", "done").unwrap().unwrap().state,
        OperationState::Succeeded
    );
    assert_eq!(store.list().unwrap().len(), 2);
    assert!(!op.id.is_empty());
}

#[test]
fn confined_paths_reject_traversal_ads_devices_absolute_and_existing_outputs() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ok.torrent"), b"fixture").unwrap();
    for path in [
        "../secret",
        "/absolute",
        "C:/secret",
        "x:stream",
        "a\\b",
        ".",
        "NUL",
        "COM1.txt",
        "",
    ] {
        assert!(
            access::confined_existing(root.path(), path).is_err(),
            "{path}"
        );
        assert!(access::confined_new(root.path(), path).is_err(), "{path}");
    }
    assert!(access::confined_existing(root.path(), "ok.torrent").is_ok());
    assert!(access::confined_new(root.path(), "ok.torrent").is_err());
    assert!(access::confined_new(root.path(), "new.jsonl").is_ok());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("ok.torrent"), root.path().join("link"))
            .unwrap();
        assert!(access::confined_existing(root.path(), "link").is_err());
    }
}

#[test]
fn malicious_wire_is_bounded_typed_and_never_echoes_secrets() {
    assert_eq!(
        wire::parse_request(&vec![b'x'; wire::MAX_REQUEST_BYTES + 1]).err(),
        Some(ErrorCode::TooLarge)
    );
    for bytes in [b"not JSON SECRET".as_slice(),br#"{"protocol_version":1,"request_id":"a","token":"SECRET","instance_id":null,"method":{"name":"discover"},"confirmed":true}"#] {
        let error=wire::parse_request(bytes).err().unwrap();
        assert!(!String::from_utf8(wire::encode_response(&wire::Response::error(error))).unwrap().contains("SECRET"));
    }
    let mut response =
        wire::Response::success(json!({"large":"x".repeat(wire::MAX_RESPONSE_BYTES)}));
    response.request_id = "id".into();
    let bytes = wire::encode_response(&response);
    assert!(bytes.len() < 1024);
    assert_eq!(
        serde_json::from_slice::<wire::Response>(&bytes)
            .unwrap()
            .error,
        Some(ErrorCode::TooLarge)
    );
}

#[test]
fn safe_announces_exclude_all_server_controlled_text_but_preserve_actual_and_sent() {
    let journal = crate::announce_journal::AnnounceJournal::memory();
    let mut observation = crate::announce_journal::tests::observation("hash");
    observation.tracker = "SECRET".into();
    observation.mode = "SECRET".into();
    observation.event = Some("SECRET".into());
    observation.outcome = "SECRET".into();
    observation.reply.failure = Some("SECRET".into());
    journal.record(observation);
    let safe = agent::safe_announce(&journal.entries(None)[0]);
    assert!(!safe.to_string().contains("SECRET"));
    assert_eq!(safe["actual"]["downloaded"], 7);
    assert_eq!(safe["sent"]["downloaded"], 0);
    assert_eq!(safe["failure_present"], true);
}

#[test]
fn live_operation_confirms_engine_ack_and_sqlite_before_success() {
    let mut fixture = test_support::LiveFixture::new();
    fixture
        .controller
        .configure_agents(fixture.root.path(), fixture.root.path().into())
        .unwrap();
    let service = fixture.controller.agent_access().unwrap();
    service.enabled = true;
    let token = service
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let method = mutation(&fixture.controller, "resume", TorrentCommand::Resume);
    let reply = request(&mut fixture.controller, &token, method)
        .result
        .unwrap();
    let op = reply["operation_id"].as_str().unwrap().to_string();
    assert_eq!(reply["state"], "queued");
    fixture.controller.tick();
    assert_eq!(
        operation(&mut fixture.controller, &token, &op)["state"],
        "running"
    );
    fixture.wait_for(|fixture| {
        fixture
            .controller
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()[0]
            .state
            == OperationState::Succeeded
    });
    let record = fixture
        .controller
        .backend
        .load_torrents()
        .unwrap()
        .remove(0);
    assert_ne!(record.state, "paused");
    let method = mutation(&fixture.controller, "pause", TorrentCommand::Pause);
    let reply = request(&mut fixture.controller, &token, method)
        .result
        .unwrap();
    let op = reply["operation_id"].as_str().unwrap().to_string();
    fixture.controller.tick();
    assert_eq!(
        request(
            &mut fixture.controller,
            &token,
            Method::Cancel {
                operation_id: op.clone()
            }
        )
        .error,
        Some(ErrorCode::ReconciliationRequired)
    );
    fixture.wait_for(|fixture| {
        fixture
            .controller
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .iter()
            .find(|o| o.id == op)
            .unwrap()
            .state
            == OperationState::Succeeded
    });
    assert_eq!(
        fixture.controller.backend.load_torrents().unwrap()[0].state,
        "paused"
    );
}

#[test]
fn host_owns_one_session_independent_of_frontend_polling_and_disconnect() {
    let mut fixture = test_support::LiveFixture::new();
    fixture
        .controller
        .configure_agents(fixture.root.path(), fixture.root.path().into())
        .unwrap();
    let service = fixture.controller.agent_access().unwrap();
    service.enabled = true;
    let token = service
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let root_path = fixture.root.path().to_owned();
    let _lock = host::InstanceLock::acquire(&root_path).unwrap();
    assert!(host::InstanceLock::acquire(&root_path).is_err());
    let instance = fixture
        .controller
        .agent
        .as_ref()
        .unwrap()
        .instance_id
        .clone();
    let method = mutation(
        &fixture.controller,
        "attached-resume",
        TorrentCommand::Resume,
    );
    let owner = Arc::new(Mutex::new(fixture.controller));
    let host = host::Host::start(owner.clone(), |c| c, root_path.clone()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut response = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let req = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "attached".into(),
            token: token.clone(),
            instance_id: Some(instance.clone()),
            method: method.clone(),
        };
        if let Ok(reply) = rt.block_on(host::call(&root_path, &req)) {
            response = Some(reply);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let op = response.unwrap().result.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // No frontend event polling and no connected client during confirmation.
    loop {
        let guard = owner.lock().unwrap();
        let op = guard
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .into_iter()
            .find(|o| o.id == op)
            .unwrap();
        if op.state == OperationState::Succeeded {
            assert_ne!(
                guard.torrents[0].state,
                crate::torrent::TorrentState::Paused
            );
            break;
        }
        drop(guard);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(host.error.lock().unwrap().is_none());
    drop(host);
    drop(owner);
    drop(fixture.root);
}

#[test]
fn mcp_schema_handshake_notifications_and_cli_exit_codes_are_structured() {
    let mut initialized = false;
    assert!(
        adapters::mcp_local(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            &mut initialized
        )
        .is_none()
    );
    assert!(
        adapters::mcp_local(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &mut initialized
        )
        .unwrap()
        .get("error")
        .is_some()
    );
    let reply=adapters::mcp_local(&json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),&mut initialized).unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    let tools = adapters::mcp_local(
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        &mut initialized,
    )
    .unwrap();
    assert_eq!(tools["result"]["tools"][0]["inputSchema"]["type"], "object");
    assert_eq!(adapters::exit_code(None), 0);
    assert_eq!(adapters::exit_code(Some(ErrorCode::NotRunning)), 3);
    assert_eq!(adapters::exit_code(Some(ErrorCode::PermissionDenied)), 4);
    assert_eq!(adapters::exit_code(Some(ErrorCode::Conflict)), 5);
}

#[test]
fn synchronous_and_async_frames_reject_oversize_and_truncated_requests() {
    let mut cursor = std::io::Cursor::new(vec![b'x'; wire::MAX_REQUEST_BYTES + 1]);
    assert_eq!(
        adapters::read_line(&mut cursor).err(),
        Some(ErrorCode::TooLarge)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = tokio::io::BufReader::new(b"truncated".as_slice());
        assert_eq!(
            host::read_frame(&mut reader, 128).await.err(),
            Some(ErrorCode::InvalidRequest)
        );
        let mut reader = tokio::io::BufReader::new(b"123456\n".as_slice());
        assert_eq!(
            host::read_frame(&mut reader, 4).await.err(),
            Some(ErrorCode::TooLarge)
        );
    });
}

proptest::proptest! {
    #[test]
    fn arbitrary_wire_never_panics_or_echoes_input(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(),0..4096)) {
        if let Err(code)=wire::parse_request(&bytes) {
            proptest::prop_assert!(wire::encode_response(&wire::Response::error(code)).len()<1024);
        }
    }
}

#[test]
fn approved_remove_waits_for_engine_and_keeps_payload() {
    let mut fixture = test_support::LiveFixture::new();
    fixture
        .controller
        .configure_agents(fixture.root.path(), fixture.root.path().into())
        .unwrap();
    let agent = fixture.controller.agent_access().unwrap();
    agent.enabled = true;
    let token = agent
        .grant(Grants {
            remove: true,
            ..Default::default()
        })
        .unwrap();
    let method = mutation(&fixture.controller, "remove", TorrentCommand::Remove);
    let response = request(&mut fixture.controller, &token, method)
        .result
        .unwrap();
    let op = response["operation_id"].as_str().unwrap().to_owned();
    fixture.controller.tick();
    assert_eq!(fixture.controller.torrents.len(), 1);
    fixture
        .controller
        .agent_access()
        .unwrap()
        .approve(&op, true)
        .unwrap();
    fixture.controller.tick();
    fixture.wait_for(|f| f.controller.torrents.is_empty());
    assert!(fixture.root.path().join("payload.bin").is_file());
    assert!(
        fixture
            .controller
            .backend
            .load_torrents()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        operation(&mut fixture.controller, &token, &op)["state"],
        "succeeded"
    );
}

#[test]
fn bounded_retention_refuses_new_work_without_forgetting_idempotency() {
    let root = tempfile::tempdir().unwrap();
    let store = ControlStore::open(&root.path().join("ops.sqlite3")).unwrap();
    let mut op = store
        .create(
            ("client", "torrent", "pause"),
            "key",
            "hash",
            now_ms() + 1000,
            false,
        )
        .unwrap();
    for index in 1..wire::MAX_OPERATIONS {
        op.id = format!("operation-{index}");
        op.key = format!("key-{index}");
        store.save(&op).unwrap();
    }
    assert_eq!(
        store
            .create(
                ("client", "torrent", "pause"),
                "extra",
                "hash",
                now_ms() + 1000,
                false
            )
            .err(),
        Some(ErrorCode::Busy)
    );
    assert!(store.lookup("client", "key").unwrap().is_some());
}

#[test]
fn streamed_export_reads_evicted_disk_entries_and_redacts_hostile_text() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(root.path());
    for _ in 0..crate::announce_journal::LIVE_CAPACITY + 5 {
        let mut observation = crate::announce_journal::tests::observation("hash");
        observation.tracker = "SECRET".into();
        observation.reply.warning = Some("SECRET".into());
        journal.record(observation);
    }
    let path = root.path().join("export.jsonl");
    let output = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .unwrap();
    let job = super::export::start(&journal, "hash".into(), output, now_ms() + 10_000).unwrap();
    job.receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let content = std::fs::read_to_string(path).unwrap();
    assert!(!content.contains("SECRET"));
    assert_eq!(
        content.lines().count(),
        crate::announce_journal::LIVE_CAPACITY + 5
    );
    assert_eq!(
        job.entries.load(std::sync::atomic::Ordering::Relaxed) as usize,
        content.lines().count()
    );
}

#[test]
fn live_preview_and_add_use_confined_paths_and_same_engine() {
    use sha1::{Digest, Sha1};
    let mut fixture = test_support::LiveFixture::new();
    let imports = fixture.root.path().join("imports");
    let downloads = fixture.root.path().join("target");
    std::fs::create_dir(&imports).unwrap();
    std::fs::create_dir(&downloads).unwrap();
    let mut info = b"d6:lengthi3e4:name7:new.bin12:piece lengthi16384e6:pieces20:".to_vec();
    info.extend_from_slice(&Sha1::digest(b"new"));
    info.extend_from_slice(b"7:privatei1ee");
    let mut meta = b"d4:info".to_vec();
    meta.extend_from_slice(&info);
    meta.push(b'e');
    std::fs::write(imports.join("new.torrent"), meta).unwrap();
    fixture
        .controller
        .configure_agents(fixture.root.path(), fixture.root.path().into())
        .unwrap();
    let agent = fixture.controller.agent_access().unwrap();
    agent.enabled = true;
    let token = agent
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let preview = request(
        &mut fixture.controller,
        &token,
        Method::Preview {
            source: "new.torrent".into(),
        },
    );
    assert_eq!(preview.result.unwrap()["total_bytes"], 3);
    let method = Method::Add {
        source: "new.torrent".into(),
        destination: "target".into(),
        start: false,
        idempotency_key: "add-one".into(),
        deadline_unix_ms: now_ms() + 10_000,
    };
    let response = request(&mut fixture.controller, &token, method.clone())
        .result
        .unwrap();
    let op = response["operation_id"].as_str().unwrap().to_owned();
    fixture.controller.tick();
    fixture.wait_for(|f| {
        f.controller
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .iter()
            .find(|o| o.id == op)
            .unwrap()
            .state
            == OperationState::Succeeded
    });
    assert_eq!(fixture.controller.torrents.len(), 2);
    assert_eq!(
        request(&mut fixture.controller, &token, method)
            .result
            .unwrap()["operation_id"],
        op
    );
    assert_eq!(fixture.controller.torrents.len(), 2);
    assert_eq!(fixture.controller.backend.load_torrents().unwrap().len(), 2);
}
