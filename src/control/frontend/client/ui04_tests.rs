use super::tests::{disconnected_client, snapshot};
use super::*;

#[test]
fn stop_confirmation_is_queued_for_only_the_displayed_connected_owner() {
    let (client, receiver) = disconnected_client();
    assert!(client.enqueue_stop_owner("another-owner").is_err());
    assert!(receiver.try_recv().is_err());
    let sequence = client.enqueue_stop_owner("owner").unwrap();
    let queued = client.take_receipts();
    assert_eq!(queued.len(), 1);
    assert!(queued[0].queued);
    assert_eq!(queued[0].job_id, sequence);
    let job = receiver.try_recv().unwrap();
    assert!(matches!(job.action, Action::Stop));
    assert_eq!(job.owner_instance, "owner");
    assert_eq!(job.sequence, sequence);
    client.inner.credentials.lock().unwrap().0 = "replacement-owner".into();
    assert!(!client.inner.job_is_current(&job));
    assert!(client.enqueue_stop_owner("owner").is_err());
    assert!(
        receiver.try_recv().is_err(),
        "confirmation never follows replacement credentials"
    );
}

#[test]
fn offline_or_stopping_frontend_does_not_queue_download_mutations_or_stop() {
    for status in [
        ConnectionStatus::Connecting,
        ConnectionStatus::Reconnecting("synthetic unavailable endpoint".into()),
        ConnectionStatus::StopRequested,
        ConnectionStatus::Stopped,
    ] {
        let (client, receiver) = disconnected_client();
        client.inner.shared.lock().unwrap().connection = status;
        assert!(
            client
                .enqueue_submit("id", TorrentCommand::Pause, Some(7))
                .is_err()
        );
        assert!(
            client
                .enqueue_submit("id", TorrentCommand::SetFiles(vec![0]), Some(7))
                .is_err()
        );
        assert!(
            client
                .storage("id", StorageAction::Priorities { high: vec![1] })
                .is_err()
        );
        assert!(client.enqueue_stop_owner("owner").is_err());
        assert!(receiver.try_recv().is_err());
        assert!(client.take_receipts().is_empty());
        assert_eq!(client.inner.queued.load(Ordering::Acquire), 0);
    }
}

#[test]
fn file_intents_use_the_delivered_generation_even_if_background_snapshot_is_newer() {
    let (client, receiver) = disconnected_client();
    {
        let mut shared = client.inner.shared.lock().unwrap();
        let mut undelivered = snapshot("owner", false, true);
        undelivered.torrents[0].item.info_hashes = vec!["id".into()];
        undelivered.torrents[0].file_revision = 88;
        undelivered.torrents[0]
            .item
            .files
            .adopt_metadata_revision(88)
            .unwrap();
        shared.latest = Some(undelivered);
    }
    client
        .enqueue_submit("id", TorrentCommand::SetFiles(vec![0, 2]), Some(7))
        .unwrap();
    client
        .storage("id", StorageAction::Priorities { high: vec![2] })
        .unwrap();
    let selection = receiver.try_recv().unwrap();
    let priority = receiver.try_recv().unwrap();
    assert!(matches!(
        selection.action,
        Action::Selection {
            file_revision: 77,
            revision: 7,
            ..
        }
    ));
    assert!(matches!(
        priority.action,
        Action::Priorities {
            file_revision: 77,
            revision: 7,
            ..
        }
    ));
    client.sync().unwrap();
    client
        .enqueue_submit("id", TorrentCommand::SetFiles(vec![0]), Some(7))
        .unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap().action,
        Action::Selection {
            file_revision: 88,
            ..
        }
    ));
}

#[test]
fn changed_or_malformed_generation_does_not_retain_pending_loaded_bits() {
    let first = snapshot("owner", true, true);
    let mut changed = snapshot("owner", false, true);
    changed.torrents[0].file_revision = 12;
    changed.torrents[0]
        .item
        .files
        .adopt_metadata_revision(12)
        .unwrap();
    retain_pending_details(Some(first), &mut changed);
    assert!(!changed.torrents[0].item.files.details_loaded());
    let first = snapshot("owner", true, true);
    let mut malformed = snapshot("owner", false, true);
    malformed.torrents[0].file_updates = Some(crate::file_selection::CompactFileUpdate {
        file_count: 2,
        selected_bits: vec![1],
        priority_bits: vec![2],
        downloaded: vec![20, 0],
    });
    retain_pending_details(Some(first), &mut malformed);
    assert!(
        !malformed.torrents[0].item.files.details_loaded(),
        "a matching header cannot authorize changed selection bits"
    );
}

fn file_page() -> FilePage {
    FilePage {
        id: "id".into(),
        revision: 7,
        file_revision: 77,
        total: 3,
        next_offset: Some(1),
        files: vec![StoredFile {
            index: 0,
            path: "first.bin".into(),
            size_bytes: 100,
            downloaded_bytes: 20,
            selected: true,
            priority: 0,
        }],
    }
}
#[test]
fn file_page_validation_rejects_generation_and_cursor_mismatches_without_discarding_progress() {
    let page = file_page();
    assert!(validate_file_page(&page, "id", 7, 77, 3, 0, 128).is_ok());
    for invalid in [0, u64::MAX, 78] {
        let mut changed = page.clone();
        changed.file_revision = invalid;
        assert!(matches!(
            validate_file_page(&changed, "id", 7, 77, 3, 0, 128),
            Err(Error::Stale)
        ));
    }
    for invalid in [0, u64::MAX] {
        let mut changed = page.clone();
        changed.file_revision = invalid;
        assert!(matches!(
            validate_file_page(&changed, "id", 7, invalid, 3, 0, 128),
            Err(Error::Stale)
        ));
    }
    let mut changed = page.clone();
    changed.revision = 8;
    assert!(validate_file_page(&changed, "id", 7, 77, 3, 0, 128).is_err());
    changed = page.clone();
    changed.next_offset = None;
    assert!(validate_file_page(&changed, "id", 7, 77, 3, 0, 128).is_err());
    changed = page.clone();
    changed.files[0].index = 1;
    assert!(validate_file_page(&changed, "id", 7, 77, 3, 0, 128).is_err());
    changed = page;
    changed.files[0].downloaded_bytes = 80;
    assert!(
        validate_file_page(&changed, "id", 7, 77, 3, 0, 128).is_ok(),
        "ordinary progress does not change file identity"
    );
}

#[test]
fn receipt_overflow_is_bounded_visible_and_consumed_once() {
    let (client, _receiver) = disconnected_client();
    {
        let mut shared = client.inner.shared.lock().unwrap();
        for id in 1..=300 {
            shared.push_receipt(Receipt {
                job_id: id,
                ..Default::default()
            });
        }
        assert_eq!(shared.receipts.len(), 256);
    }
    assert_eq!(client.take_receipt_overflow(), 44);
    assert_eq!(client.take_receipt_overflow(), 0);
    let receipts = client.take_receipts();
    assert_eq!(receipts.first().unwrap().job_id, 45);
    assert_eq!(receipts.last().unwrap().job_id, 300);
    assert!(client.take_receipts().is_empty());
}

#[test]
fn real_native_worker_publishes_queued_then_final_receipt_with_a_fresh_snapshot_fence() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings = ClientSettings::default();
    settings.download.download_directory = directory.path().display().to_string();
    let item = crate::client::tests::torrent();
    let mut store = crate::state::StateStore::in_memory_with_secrets(Arc::new(
        crate::secrets::MemorySecrets::default(),
    ))
    .unwrap();
    store
        .save_torrent(&crate::client::torrent_to_record(&item, None))
        .unwrap();
    let mut owner = crate::control::Controller::new(
        crate::client::ClientBackend::with_state_store(store),
        vec![item],
    );
    owner
        .configure_agents(directory.path(), directory.path().to_owned())
        .unwrap();
    let server = super::super::Server::start(
        Arc::new(Mutex::new(owner)),
        directory.path().to_owned(),
        settings.clone(),
    )
    .unwrap();
    let client = Client::connect(directory.path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while client.sync().is_none() {
        assert!(
            Instant::now() < deadline,
            "synthetic owner snapshot was not delivered"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = client.connection().snapshot_serial;
    client.save_view(&settings.ui).unwrap();
    let mut receipts = Vec::new();
    while !receipts
        .iter()
        .any(|receipt: &Receipt| !receipt.queued && receipt.ui_saved.is_some())
    {
        receipts.extend(client.take_receipts());
        assert!(
            Instant::now() < deadline,
            "synthetic durable view action was not acknowledged"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let action_id = receipts
        .iter()
        .find(|receipt| receipt.ui_saved.is_some())
        .unwrap()
        .job_id;
    receipts.retain(|receipt| receipt.job_id == action_id);
    assert_eq!(
        receipts.len(),
        2,
        "queued/final receipts correlate independently of background access refreshes"
    );
    assert!(receipts[0].queued);
    assert!(!receipts[1].queued);
    assert_eq!(receipts[0].job_id, receipts[1].job_id);
    assert_ne!(receipts[0].job_id, 0);
    assert_eq!(receipts[0].owner_instance, receipts[1].owner_instance);
    assert!(receipts[1].snapshot_after > before);
    assert!(receipts[1].error.is_none() && !receipts[1].ambiguous_error);
    assert!(directory.path().join("settings.toml").is_file());
    drop(client);
    drop(server);
}
