use super::*;
use crate::{
    client::{ClientBackend, torrent_to_record},
    control::{TorrentCommand, access::Grants, operations::now_ms, wire as agent_wire},
    runtime_status::{TransferAction, TransferFeedback},
    session::SessionRuntime,
    state::{StateStore, StoredTrackerAuditProfile, StoredTrackerAuditStep},
};
use sha1::{Digest, Sha1};
use std::{path::Path, thread, time::Instant};
mod magnet_loopback;

struct NativeOwner {
    server: Server,
    owner: Arc<Mutex<Controller>>,
    profile: tempfile::TempDir,
    settings: ClientSettings,
    instance: String,
    token: String,
}
impl NativeOwner {
    fn new(items: Vec<TorrentItem>, engine: bool) -> Self {
        Self::new_with_listener(items, engine, None)
    }
    fn new_with_listener(
        items: Vec<TorrentItem>,
        engine: bool,
        listen: Option<librqbit::ListenerOptions>,
    ) -> Self {
        Self::new_with_options(
            items,
            engine.then_some(librqbit::SessionOptions {
                dht: None,
                listen,
                disable_trackers: true,
                disable_local_service_discovery: true,
                ..Default::default()
            }),
        )
    }
    fn new_with_options(
        items: Vec<TorrentItem>,
        options: Option<librqbit::SessionOptions>,
    ) -> Self {
        let profile = tempfile::tempdir().unwrap();
        let root = profile.path().join("payloads");
        std::fs::create_dir(&root).unwrap();
        let mut settings = ClientSettings::default();
        settings.download.download_directory = root.to_string_lossy().into_owned();
        let store = StateStore::open(&profile.path().join("state.sqlite3")).unwrap();
        let mut backend = if let Some(options) = options {
            let runtime = SessionRuntime::start_with_options(32, root.clone(), options).unwrap();
            ClientBackend::with_session_and_store(runtime, store)
        } else {
            ClientBackend::with_state_store(store)
        };
        for item in &items {
            backend
                .save_torrent(&torrent_to_record(item, None))
                .unwrap();
        }
        let mut owner = Controller::new(backend, items);
        owner.configure_agents(profile.path(), root).unwrap();
        owner.set_agent_settings(&settings).unwrap();
        let owner = Arc::new(Mutex::new(owner));
        let server = Server::start(owner.clone(), profile.path().into(), settings.clone()).unwrap();
        let (instance, token) = transport::read_credentials(profile.path()).unwrap();
        Self {
            server,
            owner,
            profile,
            settings,
            instance,
            token,
        }
    }
    fn request(&self, action: Action) -> Request {
        Request {
            protocol: PROTOCOL_VERSION,
            request_id: crate::control::access::random_id().unwrap(),
            client_id: "a".repeat(64),
            token: self.token.clone(),
            instance_id: Some(self.instance.clone()),
            action,
        }
    }
    fn rpc(&self, action: Action) -> Result<Reply, Error> {
        self.response(action)?.result
    }
    fn response(&self, action: Action) -> Result<Response, Error> {
        transport::call_blocking(self.profile.path().into(), self.request(action))
    }
    fn current(&self, id: &str) -> WireTorrent {
        let Reply::Snapshot(page) = self
            .rpc(Action::Snapshot {
                offset: 0,
                limit: 16,
                revision: None,
                detail: Some(id.into()),
            })
            .unwrap()
        else {
            panic!("owner snapshot required")
        };
        page.torrents
            .into_iter()
            .find(|row| row.info_hashes.join(";") == id)
            .unwrap()
    }
    #[track_caller]
    fn wait_for(&self, mut ready: impl FnMut(&Controller) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            {
                let mut owner = self.owner.lock().unwrap();
                owner.tick();
                if ready(&owner) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "synthetic owner state did not arrive"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop_engine(&self) {
        self.owner.lock().unwrap().shutdown().unwrap();
    }
}

fn local_metainfo(source: &Path, download_root: &Path) -> NewTorrent {
    let mut info = b"d5:filesld6:lengthi3e4:pathl5:a.bineed6:lengthi3e4:pathl5:b.bineee4:name7:payload12:piece lengthi16384e6:pieces20:".to_vec();
    info.extend_from_slice(&Sha1::digest(b"abcdef"));
    info.extend_from_slice(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut metainfo = b"d4:info".to_vec();
    metainfo.extend_from_slice(&info);
    metainfo.push(b'e');
    std::fs::write(source, metainfo).unwrap();
    let item = crate::client::tests::torrent();
    NewTorrent {
        source: source.to_string_lossy().into_owned(),
        hashes: vec![id],
        trackers: Vec::new(),
        peer_id: item.peer_id,
        overrides: crate::torrent::TorrentOverrides {
            download_directory: Some(download_root.to_string_lossy().into_owned()),
            ..Default::default()
        },
        selected: PackedIndices::new(2, &[0, 1]).unwrap(),
        high: PackedIndices::new(2, &[]).unwrap(),
        start: false,
        audit: None,
    }
}

#[test]
fn native_import_rejects_changed_identity_and_invalid_metadata_before_persisting() {
    let fixture = NativeOwner::new(vec![], false);
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    let source = fixture.profile.path().join("synthetic.torrent");
    let request = local_metainfo(&source, &root);
    let mut wrong = request.clone();
    wrong.hashes = vec!["v1: stale-metainfo".into()];
    assert!(matches!(fixture.rpc(Action::Add(wrong)), Err(Error::Stale)));
    let mut wrong = request.clone();
    wrong.peer_id = "short".into();
    assert!(matches!(
        fixture.rpc(Action::Add(wrong)),
        Err(Error::Failed(_))
    ));
    let mut wrong = request.clone();
    wrong.trackers = vec!["x".repeat(8193)];
    assert!(matches!(
        fixture.rpc(Action::Add(wrong)),
        Err(Error::TooLarge)
    ));
    let mut wrong = request.clone();
    wrong.selected.bits.clear();
    assert!(matches!(
        fixture.rpc(Action::Add(wrong)),
        Err(Error::TooLarge)
    ));
    let mut wrong = request.clone();
    wrong.overrides.download_directory = Some("relative".into());
    assert!(
        matches!(fixture.rpc(Action::Add(wrong)), Err(Error::Failed(message)) if message.contains("absolute"))
    );
    let mut wrong = request;
    wrong.source = fixture
        .profile
        .path()
        .join("missing.torrent")
        .display()
        .to_string();
    assert!(matches!(
        fixture.rpc(Action::Add(wrong)),
        Err(Error::Failed(_))
    ));
    let owner = fixture.owner.lock().unwrap();
    assert!(owner.torrents.is_empty());
    assert!(owner.backend.load_torrents().unwrap().is_empty());
}

#[test]
fn native_magnet_cannot_bypass_metadata_review() {
    let fixture = NativeOwner::new(vec![], false);
    let source = fixture.profile.path().join("synthetic.torrent");
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    let mut request = local_metainfo(&source, &root);
    let hash = request.hashes[0].strip_prefix("v1: ").unwrap();
    let topic = format!("urn:btih:{hash}");
    request.source = format!("magnet:?xt={topic}");
    request.hashes = vec![topic];
    assert!(matches!(
        prepare_new(request.clone(), &fixture.settings),
        Err(Error::Failed(message)) if message.contains("Review magnet metadata")
    ));
    request.selected = PackedIndices::new(0, &[]).unwrap();
    request.high = PackedIndices::new(0, &[]).unwrap();
    assert!(
        matches!(fixture.rpc(Action::Add(request)), Err(Error::Failed(message)) if message.contains("Review magnet metadata"))
    );
    assert!(fixture.owner.lock().unwrap().torrents.is_empty());
}

#[test]
fn native_storage_selection_rechecks_existing_files_before_persisting_the_destination() {
    let fixture = NativeOwner::new(vec![], true);
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    let destination = fixture.profile.path().join("reuse");
    std::fs::create_dir(&destination).unwrap();
    for directory in [&root, &destination] {
        std::fs::write(directory.join("a.bin"), b"abc").unwrap();
        std::fs::write(directory.join("b.bin"), b"def").unwrap();
    }
    let source = fixture.profile.path().join("storage.torrent");
    let request = local_metainfo(&source, &root);
    let id = request.hashes[0].clone();
    assert!(matches!(
        fixture.rpc(Action::Add(request)),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|owner| {
        owner.torrents.first().is_some_and(|item| {
            item.runtime.registered
                && item.state == TorrentState::Paused
                && item.completion == crate::completion::CompletionStatus::Verified
        })
    });
    let before = fixture.current(&id).command_revision;
    assert!(matches!(
        fixture.rpc(Action::Storage {
            id: id.clone(),
            action: StorageAction::Cancel,
            revision: before
        }),
        Ok(Reply::Done(_))
    ));
    let cancelled = fixture.current(&id).command_revision;
    assert!(cancelled > before);
    let directory = destination.display().to_string();
    let action = StorageAction::Select {
        directory: directory.clone(),
        copy: false,
    };
    assert!(matches!(
        fixture.rpc(Action::Storage {
            id: id.clone(),
            action: action.clone(),
            revision: before
        }),
        Err(Error::Stale)
    ));
    assert!(matches!(
        fixture.rpc(Action::Storage {
            id: id.clone(),
            action,
            revision: cancelled
        }),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|owner| {
        owner.torrents[0].overrides.download_directory.as_deref() == Some(directory.as_str())
            && !owner.has_confirmed_update(&id)
    });
    let record = fixture
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        record.download_directory.as_deref(),
        Some(directory.as_str())
    );
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), b"abc");
    assert_eq!(std::fs::read(destination.join("b.bin")).unwrap(), b"def");
    fixture.stop_engine();
}

#[test]
fn native_retry_uses_the_same_row_after_the_download_root_is_corrected() {
    let fixture = NativeOwner::new(vec![], true);
    let blocked = fixture.profile.path().join("blocked-download-root");
    std::fs::write(&blocked, b"synthetic blocker").unwrap();
    let source = fixture.profile.path().join("retry.torrent");
    let request = local_metainfo(&source, &blocked);
    let id = request.hashes[0].clone();
    assert!(matches!(
        fixture.rpc(Action::Add(request)),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|owner| {
        owner.torrents.first().is_some_and(|item| {
            !item.runtime.registered
                && item.state == TorrentState::Error
                && item.runtime.last_error.is_some()
        })
    });
    let failed = fixture.current(&id);
    std::fs::remove_file(&blocked).unwrap();
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("a.bin"), b"abc").unwrap();
    std::fs::write(blocked.join("b.bin"), b"def").unwrap();
    assert!(matches!(
        fixture.rpc(Action::Retry {
            id: id.clone(),
            revision: failed.command_revision
        }),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|owner| {
        owner.torrents[0].runtime.registered
            && matches!(
                owner.torrents[0].runtime.command,
                TransferFeedback::Confirmed {
                    action: TransferAction::Start
                }
            )
            && !owner.has_confirmed_update(&id)
    });
    let retried = fixture.current(&id);
    assert!(retried.command_revision > failed.command_revision);
    assert!(retried.runtime.registered && retried.durable);
    assert!(matches!(
        retried.runtime.command,
        FeedbackWire::Confirmed(TransferAction::Start)
    ));
    let stored = fixture
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, id);
    assert_eq!(stored[0].download_directory.as_deref(), blocked.to_str());
    fixture.stop_engine();
}

#[test]
fn empty_native_recycle_plan_still_requires_detach_before_removing_the_saved_row() {
    let mut item = crate::client::tests::torrent();
    item.state = TorrentState::Error;
    let id = item.info_hashes.join(";");
    let fixture = NativeOwner::new(vec![item], true);
    let row = fixture.current(&id);
    let Reply::TrashPlan(plan) = fixture
        .rpc(Action::PrepareTrash {
            id: id.clone(),
            revision: row.command_revision,
            scope: TrashScope::DownloadedFiles,
        })
        .unwrap()
    else {
        panic!("empty data plan must be reviewable")
    };
    assert_eq!(plan.count, 0);
    assert!(plan.preview.is_empty());
    let wrong = fixture.request(Action::Trash {
        plan_id: plan.plan_id.clone(),
        digest: "changed".into(),
    });
    assert!(matches!(
        transport::call_blocking(fixture.profile.path().into(), wrong)
            .unwrap()
            .result,
        Err(Error::Stale)
    ));
    assert_eq!(
        fixture
            .owner
            .lock()
            .unwrap()
            .backend
            .load_torrents()
            .unwrap()
            .len(),
        1
    );
    let request = fixture.request(Action::Trash {
        plan_id: plan.plan_id,
        digest: plan.digest,
    });
    let directory = fixture.profile.path().to_path_buf();
    let task = thread::spawn(move || transport::call_blocking(directory, request).unwrap().result);
    fixture.wait_for(|owner| owner.torrents.is_empty());
    assert!(matches!(task.join().unwrap(), Ok(Reply::Done(_))));
    let owner = fixture.owner.lock().unwrap();
    assert!(owner.backend.load_torrents().unwrap().is_empty());
    assert!(!owner.is_busy(&id));
    drop(owner);
    fixture.stop_engine();
}

#[test]
fn native_view_save_and_tracker_edit_preserve_unrelated_durable_state() {
    let item = crate::client::tests::torrent();
    let id = item.info_hashes.join(";");
    let fixture = NativeOwner::new(vec![item], false);
    let mut disk = fixture.settings.clone();
    disk.ui.search = "previous search".into();
    let path = fixture.profile.path().join("settings.toml");
    disk.save_to(&path).unwrap();
    let mut applied = fixture.settings.clone();
    applied.download.download_directory = fixture
        .profile
        .path()
        .join("later-root")
        .display()
        .to_string();
    assert!(matches!(
        fixture.rpc(Action::ApplySettings(applied.clone())),
        Ok(Reply::Done(_))
    ));
    let mut ui = applied.ui.clone();
    ui.search = "saved view".into();
    assert!(matches!(
        fixture.rpc(Action::SaveView(ui.clone())),
        Ok(Reply::Done(_))
    ));
    let stored = ClientSettings::load_from(&path).unwrap();
    assert_eq!(stored.ui, ui);
    assert_eq!(
        stored.download.download_directory,
        disk.download.download_directory
    );
    let Reply::Hello {
        settings,
        instance_id,
    } = fixture.rpc(Action::Hello).unwrap()
    else {
        panic!("native Hello required")
    };
    assert_eq!(instance_id, fixture.instance);
    assert_eq!(settings.ui, ui);
    assert_eq!(
        settings.download.download_directory,
        applied.download.download_directory
    );
    let audit = StoredTrackerAuditProfile {
        allowed_host: "example.invalid".into(),
        port: 80,
        started_only: true,
        observation_seconds: 10,
        steps: vec![StoredTrackerAuditStep {
            delay_seconds: 0,
            uploaded: 0,
            downloaded: 0,
            left: 100,
            event: Some("started".into()),
        }],
    };
    let revision = fixture.current(&id).command_revision;
    let trackers = vec!["http://example.invalid/announce".into()];
    assert!(matches!(
        fixture.rpc(Action::SaveEditable {
            id: id.clone(),
            revision,
            trackers: trackers.clone(),
            audit: Some(audit.clone())
        }),
        Ok(Reply::Done(_))
    ));
    assert!(matches!(
        fixture.rpc(Action::SaveEditable {
            id: id.clone(),
            revision,
            trackers: Vec::new(),
            audit: None
        }),
        Err(Error::Stale)
    ));
    let current = fixture.current(&id);
    assert!(current.command_revision > revision);
    assert_eq!(current.trackers, trackers);
    let owner = fixture.owner.lock().unwrap();
    let record = owner.backend.load_torrents().unwrap().pop().unwrap();
    assert_eq!(record.trackers, trackers);
    assert_eq!(record.tracker_audit, Some(audit));
    assert_eq!(record.files.len(), 1);
}

#[test]
fn native_live_speed_distinguishes_applied_and_saved_settings_across_frontend_detach() {
    use crate::{control::frontend::Client, settings::speed_policy::SpeedPolicy};

    let fixture = NativeOwner::new_with_listener(
        vec![],
        true,
        Some(librqbit::ListenerOptions {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        }),
    );
    let mut saved = fixture.settings.clone();
    saved.speed.download_limit_kib = 900;
    saved.speed.alternate_download_kib = 300;
    saved.queue.enabled = false;
    saved.queue.max_active_downloads = 7;
    assert!(matches!(
        fixture.rpc(Action::SaveSettings(saved.clone())),
        Ok(Reply::Done(_))
    ));
    let engine = fixture.owner.lock().unwrap().backend.test_engine().unwrap();
    let listener = engine.listen_addr().unwrap();
    let first = Client::connect(fixture.profile.path()).unwrap();
    let second = Client::connect(fixture.profile.path()).unwrap();
    fixture.wait_for(|_| {
        let _ = first.sync();
        let _ = second.sync();
        first.speed_view().is_some() && second.speed_view().is_some()
    });

    let mut desired = saved.clone();
    desired.speed.download_limit_kib = 1200;
    desired.speed.alternate_download_kib = 400;
    desired.speed.alternate_upload_kib = 200;
    desired.queue.enabled = true;
    desired.queue.max_active_downloads = 2;
    desired.ui.search = "applied only".into();
    first.apply_settings(&desired).unwrap();
    let mut settings_receipt = None;
    fixture.wait_for(|_| {
        settings_receipt = first
            .take_receipts()
            .into_iter()
            .find(|receipt| receipt.label == "Apply preferences" && !receipt.queued);
        settings_receipt.is_some()
    });
    let settings_receipt = settings_receipt.unwrap();
    assert_eq!(settings_receipt.error, None);
    assert_eq!(settings_receipt.settings.as_ref(), Some(&desired));
    let accepted = settings_receipt.speed_view.unwrap();
    assert_eq!(accepted.desired, desired.speed);
    assert_eq!(accepted.saved.as_ref(), Some(&saved.speed));
    assert_eq!(accepted.desired_max_active_downloads, 2);
    assert!(accepted.desired_queue_enabled);
    assert_eq!(accepted.saved_queue_enabled, Some(false));
    assert_eq!(accepted.saved_max_active_downloads, Some(7));

    first.set_speed_policy(SpeedPolicy::Quiet).unwrap();
    let mut profile_receipt = None;
    fixture.wait_for(|_| {
        profile_receipt = first
            .take_receipts()
            .into_iter()
            .find(|receipt| receipt.label == "Speed profile" && !receipt.queued);
        profile_receipt.is_some()
    });
    let receipt = profile_receipt.unwrap();
    assert!(receipt.completed_by_reply);
    assert!(receipt.error.is_none());
    saved.speed.set_policy(SpeedPolicy::Quiet);
    desired.speed.set_policy(SpeedPolicy::Quiet);
    let accepted = receipt.speed_view.unwrap();
    assert_eq!(accepted.desired, desired.speed);
    assert_eq!(accepted.saved.as_ref(), Some(&saved.speed));
    let path = fixture.profile.path().join("settings.toml");
    assert_eq!(ClientSettings::load_from(&path).unwrap(), saved);
    fixture.wait_for(|_| {
        second.speed_view().is_some_and(|speed| {
            !speed.pending
                && speed.applied.is_some_and(|applied| {
                    applied.quiet
                        && applied.download_limit_kib == 400
                        && applied.upload_limit_kib == 200
                        && applied.max_active_downloads == 2
                })
        })
    });

    let mut invalid = desired.clone();
    invalid.speed.alternate_download_kib = u64::MAX;
    assert!(matches!(
        fixture.rpc(Action::SaveSettings(invalid)),
        Err(Error::Failed(_))
    ));
    drop((first, second));
    let current = fixture.owner.lock().unwrap().backend.test_engine().unwrap();
    assert!(Arc::ptr_eq(&engine, &current));
    assert_eq!(current.listen_addr(), Some(listener));
    assert_eq!(
        current.ratelimits.get_download_bps().unwrap().get(),
        400 * 1024
    );
    assert_eq!(
        current.ratelimits.get_upload_bps().unwrap().get(),
        200 * 1024
    );
    assert_eq!(ClientSettings::load_from(&path).unwrap(), saved);
    let attached = Client::connect(fixture.profile.path()).unwrap();
    fixture.wait_for(|_| {
        let _ = attached.sync();
        attached.speed_view().is_some_and(|speed| {
            speed.desired == desired.speed && speed.saved.as_ref() == Some(&saved.speed)
        })
    });
    drop((attached, engine, current));
    fixture.stop_engine();
}

#[test]
fn native_history_tracks_engine_ack_and_waits_for_durable_owner_update() {
    let fixture = NativeOwner::new(vec![], true);
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    std::fs::write(root.join("a.bin"), b"abc").unwrap();
    std::fs::write(root.join("b.bin"), b"def").unwrap();
    let request = local_metainfo(&fixture.profile.path().join("history.torrent"), &root);
    let id = request.hashes[0].clone();
    assert!(matches!(
        fixture.rpc(Action::Add(request)),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|owner| {
        owner
            .torrents
            .first()
            .is_some_and(|item| item.runtime.registered && item.state == TorrentState::Paused)
    });
    let db = rusqlite::Connection::open(fixture.profile.path().join("state.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_history_ack BEFORE UPDATE ON torrents
        BEGIN SELECT RAISE(ABORT,'synthetic ACK write failure'); END;",
    )
    .unwrap();
    let before = fixture.current(&id).command_revision;
    let response = fixture
        .response(Action::Mutate {
            id: id.clone(),
            command: TorrentCommand::Resume,
            revision: Some(before),
        })
        .unwrap();
    let operation_id = response.operation_id.unwrap();
    let Reply::Accepted(accepted) = response.result.unwrap() else {
        panic!("native resume acceptance required");
    };
    let read = || {
        let Reply::Operations(page) = fixture
            .rpc(Action::Operations {
                before: None,
                limit: 8,
            })
            .unwrap()
        else {
            panic!("owner history page required");
        };
        page.entries
            .into_iter()
            .find(|row| row.id == operation_id)
            .unwrap()
    };
    let queued = read();
    assert_eq!(queued.state, NativeOperationState::Accepted);
    assert_eq!(queued.revision, Some(accepted.command_revision));
    assert_eq!(queued.target.as_deref(), Some(id.as_str()));
    fixture.wait_for(|owner| owner.has_confirmed_update(&id));
    assert_eq!(read().state, NativeOperationState::EffectConfirmed);
    db.execute_batch("DROP TRIGGER reject_history_ack").unwrap();
    fixture.wait_for(|owner| !owner.has_confirmed_update(&id));
    assert_eq!(read().state, NativeOperationState::Succeeded);
    let stale = fixture
        .response(Action::Mutate {
            id,
            command: TorrentCommand::Pause,
            revision: Some(before),
        })
        .unwrap();
    assert!(stale.operation_id.is_some());
    assert!(matches!(
        stale.result,
        Err(Error::Command(
            crate::control::CommandError::RevisionConflict { .. }
        ))
    ));
    fixture.stop_engine();
}

#[test]
fn native_access_grants_review_and_revoke_are_distinct_from_agent_authority() {
    let item = crate::client::tests::torrent();
    let id = item.info_hashes.join(";");
    let fixture = NativeOwner::new(vec![item], false);
    let Reply::Access(disabled) = fixture.rpc(Action::Access(AccessAction::View)).unwrap() else {
        panic!("access view required")
    };
    assert!(!disabled.enabled && disabled.clients.is_empty() && disabled.token.is_none());
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    assert!(matches!(
        fixture.rpc(Action::ConfigureAgents {
            download_root: root.clone()
        }),
        Ok(Reply::Done(_))
    ));
    assert!(matches!(
        fixture.rpc(Action::ConfigureAgents {
            download_root: root.join("other")
        }),
        Err(Error::Stale)
    ));
    assert!(matches!(
        fixture.rpc(Action::AgentSettings(fixture.settings.clone())),
        Ok(Reply::Done(_))
    ));
    let Reply::Access(enabled) = fixture
        .rpc(Action::Access(AccessAction::Enable(true)))
        .unwrap()
    else {
        panic!("enabled access view required")
    };
    assert!(enabled.enabled && enabled.import_root.is_dir() && enabled.export_root.is_dir());
    let Reply::Access(grant) = fixture
        .rpc(Action::Access(AccessAction::Grant(Grants {
            manage: true,
            remove: true,
            ..Default::default()
        })))
        .unwrap()
    else {
        panic!("grant required")
    };
    let token = grant.token.unwrap();
    let client_id = grant.clients[0].id.clone();
    let revision = fixture.current(&id).command_revision;
    let agent_call = |method| {
        let mut owner = fixture.owner.lock().unwrap();
        let instance = owner.agent.as_ref().unwrap().instance_id.clone();
        owner.handle_request(agent_wire::Request {
            protocol_version: crate::control::PROTOCOL_VERSION,
            request_id: "review-fixture".into(),
            token: token.clone(),
            instance_id: Some(instance),
            method,
        })
    };
    let accepted = agent_call(agent_wire::Method::Mutate {
        torrent_id: id,
        command: TorrentCommand::Remove,
        expected_revision: revision,
        idempotency_key: "confirm-removal".into(),
        deadline_unix_ms: now_ms() + 60_000,
    });
    let operation = accepted.result.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let Reply::Access(review) = fixture.rpc(Action::Access(AccessAction::View)).unwrap() else {
        panic!("approval view required")
    };
    assert!(review.token.is_none());
    assert_eq!(review.approvals.len(), 1);
    assert_eq!(review.approvals[0].0.id, operation);
    assert!(review.approvals[0].1.contains("keep all downloaded files"));
    let Reply::Access(cancelled) = fixture
        .rpc(Action::Access(AccessAction::Approve {
            operation,
            allow: false,
        }))
        .unwrap()
    else {
        panic!("cancelled view required")
    };
    assert!(cancelled.approvals.is_empty());
    assert_eq!(
        fixture
            .owner
            .lock()
            .unwrap()
            .backend
            .load_torrents()
            .unwrap()
            .len(),
        1
    );
    let Reply::Access(revoked) = fixture
        .rpc(Action::Access(AccessAction::Revoke(client_id)))
        .unwrap()
    else {
        panic!("revocation view required")
    };
    assert!(revoked.clients.is_empty());
    assert_eq!(
        agent_call(agent_wire::Method::Discover).error,
        Some(agent_wire::ErrorCode::Unauthorized)
    );
    for _ in 0..2 {
        assert!(matches!(
            fixture.rpc(Action::Access(AccessAction::Grant(Grants::default()))),
            Ok(Reply::Access(_))
        ));
    }
    let Reply::Access(revoked_all) = fixture
        .rpc(Action::Access(AccessAction::RevokeAll))
        .unwrap()
    else {
        panic!("bulk revocation view required")
    };
    assert!(revoked_all.clients.is_empty());
    let Reply::Access(disabled) = fixture
        .rpc(Action::Access(AccessAction::Enable(false)))
        .unwrap()
    else {
        panic!("disabled access view required")
    };
    assert!(!disabled.enabled);
    assert!(matches!(
        fixture.rpc(Action::Hello),
        Ok(Reply::Hello { .. })
    ));
}

#[test]
fn invalid_native_authentication_cannot_request_stop_and_valid_reply_wakes_waiter() {
    let fixture = NativeOwner::new(vec![], false);
    let call = |request| transport::call_blocking(fixture.profile.path().into(), request);
    let mut request = fixture.request(Action::Stop);
    request.token = "b".repeat(64);
    assert!(matches!(
        call(request).unwrap().result,
        Err(Error::Unauthorized)
    ));
    let mut request = fixture.request(Action::Stop);
    request.protocol = 1;
    assert!(matches!(
        call(request).unwrap().result,
        Err(Error::Incompatible)
    ));
    let mut request = fixture.request(Action::Stop);
    request.request_id.clear();
    assert!(matches!(
        call(request).unwrap().result,
        Err(Error::TooLarge)
    ));
    let mut request = fixture.request(Action::Stop);
    request.instance_id = Some("previous-owner".into());
    assert!(matches!(call(request), Err(Error::Transport(_))));
    assert!(!fixture.server.stop_requested());
    thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), fixture.server.stopped())
                    .await
                    .unwrap()
            });
        });
        assert!(matches!(fixture.rpc(Action::Stop), Ok(Reply::Done(_))));
        waiter.join().unwrap();
    });
    assert!(fixture.server.stop_requested());
}

#[test]
fn native_detail_snapshot_keeps_diagnostics_and_feedback_on_the_requested_row() {
    use crate::diagnostics::{PeerSnapshot, RateSample};
    let mut checking = crate::client::tests::torrent();
    checking.runtime.command = TransferFeedback::Pending {
        action: TransferAction::Recheck,
        since: Instant::now(),
    };
    checking.runtime.diagnostics.history.push_back(RateSample {
        elapsed_ms: 1000,
        down: 42,
        up: 3,
    });
    checking.runtime.diagnostics.peers.push(PeerSnapshot {
        address: "127.0.0.1:1234".into(),
        state: "synthetic".into(),
        client: "test".into(),
        downloaded: 40,
        uploaded: 3,
        errors: 0,
    });
    let id = checking.info_hashes.join(";");
    let mut failed = checking.clone();
    failed.info_hashes = vec!["v1: def".into()];
    failed.runtime.command = TransferFeedback::Failed {
        action: TransferAction::Pause,
        message: "synthetic unavailable engine".into(),
    };
    let other = failed.info_hashes.join(";");
    let fixture = NativeOwner::new(vec![checking, failed], false);
    let Reply::Snapshot(page) = fixture
        .rpc(Action::Snapshot {
            offset: 0,
            limit: 2,
            revision: None,
            detail: Some(id),
        })
        .unwrap()
    else {
        panic!("native detail page required")
    };
    let mut rows = page
        .torrents
        .into_iter()
        .map(|row| row.into_native().unwrap());
    let selected = rows.next().unwrap().item;
    let other_row = rows.next().unwrap().item;
    assert_eq!(
        selected.runtime.command.pending_action(),
        Some(TransferAction::Recheck)
    );
    assert_eq!(selected.runtime.diagnostics.history[0].down, 42);
    assert_eq!(selected.runtime.diagnostics.peers[0].downloaded, 40);
    assert_eq!(other_row.info_hashes.join(";"), other);
    assert!(
        other_row.runtime.diagnostics.peers.is_empty()
            && other_row.runtime.diagnostics.history.is_empty()
    );
    assert!(
        matches!(other_row.runtime.command, TransferFeedback::Failed { action: TransferAction::Pause, message } if message == "synthetic unavailable engine")
    );
}

#[cfg(unix)]
#[test]
fn unix_native_socket_and_credentials_fail_closed_on_foreign_files_or_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let endpoint = directory.path().join("frontend.sock");
    std::fs::write(&endpoint, b"foreign sentinel").unwrap();
    let owner = Arc::new(Mutex::new(Controller::new(
        ClientBackend::offline(),
        vec![],
    )));
    assert!(Server::start(owner, directory.path().into(), ClientSettings::default()).is_err());
    assert_eq!(std::fs::read(&endpoint).unwrap(), b"foreign sentinel");
    let descriptor = directory.path().join("frontend-auth.json");
    assert!(!descriptor.exists());
    assert!(
        transport::read_credentials(directory.path())
            .unwrap_err()
            .contains("not ready")
    );
    transport::write_credentials(directory.path(), "synthetic-owner".into(), &"a".repeat(64))
        .unwrap();
    let original = std::fs::read(&descriptor).unwrap();
    std::fs::set_permissions(&descriptor, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        transport::read_credentials(directory.path())
            .unwrap_err()
            .contains("not private")
    );
    std::fs::set_permissions(&descriptor, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&descriptor, b"corrupt descriptor").unwrap();
    assert!(transport::read_credentials(directory.path()).is_err());
    let mut obsolete: serde_json::Value = serde_json::from_slice(&original).unwrap();
    obsolete["protocol"] = serde_json::json!(1);
    std::fs::write(&descriptor, serde_json::to_vec(&obsolete).unwrap()).unwrap();
    assert!(
        transport::read_credentials(directory.path())
            .unwrap_err()
            .contains("incompatible")
    );
    transport::write_credentials(directory.path(), "new-owner".into(), &"b".repeat(64)).unwrap();
    assert_eq!(
        transport::read_credentials(directory.path()).unwrap(),
        ("new-owner".into(), "b".repeat(64))
    );
}

#[cfg(unix)]
#[test]
fn malformed_reply_after_request_delivery_is_a_transport_uncertainty() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let endpoint = transport::endpoint(directory.path()).unwrap();
        let listener = tokio::net::UnixListener::bind(endpoint).unwrap();
        let request = Request {
            protocol: PROTOCOL_VERSION, request_id: "sent-before-corrupt-reply".into(),
            client_id: "a".repeat(64), token: "b".repeat(64), instance_id: Some("synthetic-owner".into()),
            action: Action::SaveView(Default::default()),
        };
        let fake_owner = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let bytes = transport::read(&mut stream).await.unwrap();
            let received: Request = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(received.request_id, request.request_id);
            assert!(matches!(received.action, Action::SaveView(_)));
            stream.get_mut().write_all(b"malformed reply\n").await.unwrap();
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(transport::call(directory.path(), &request), fake_owner)
        })
        .await
        .expect("synthetic owner did not exchange the native request");
        assert!(matches!(response, Err(Error::Transport(message)) if message.contains("could not be decoded")));
    });
}
