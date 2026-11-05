use super::*;
use crate::{
    client::tests::torrent,
    completion::CompletionStatus,
    download_mode::DownloadMode,
    runtime_status::{ModeApplication, ModeConfig},
    torrent::TorrentState,
};

fn controller() -> Controller {
    Controller::new(ClientBackend::offline(), vec![torrent()])
}

#[test]
fn unchanged_summary_telemetry_does_not_refresh_native_snapshots_but_rate_changes_do() {
    let mut control = controller();
    let event = |rate| SessionEvent::Stats {
        id: "v1: abc".into(),
        downloaded_bytes: 20,
        uploaded_bytes: 5,
        download_rate: rate,
        upload_rate: 0,
        file_progress: vec![],
        peers: 0,
        finished: false,
        paused: false,
    };
    emit(&mut control, event(10));
    let native_revision = control.frontend_revision();
    let public_revision = page(&control.read_handle()).revision;
    for _ in 0..100 {
        emit(&mut control, event(10));
    }
    assert_eq!(control.frontend_revision(), native_revision);
    emit(&mut control, event(25));
    assert!(control.frontend_revision() > native_revision);
    assert_eq!(
        page(&control.read_handle()).revision,
        public_revision,
        "rate-only native refresh must not manufacture public transfer acknowledgements"
    );
}

#[test]
fn durable_removal_retries_only_after_detachment_and_preserves_payload() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("state.sqlite3");
    let payload = root.path().join("keep.bin");
    std::fs::write(&payload, b"synthetic payload remains").unwrap();
    let mut backend =
        ClientBackend::with_state_store(crate::state::StateStore::open(&database).unwrap());
    let item = torrent();
    backend
        .save_torrent(&crate::client::torrent_to_record(&item, None))
        .unwrap();
    let faults = rusqlite::Connection::open(&database).unwrap();
    faults.execute_batch("CREATE TRIGGER blocked_delete BEFORE DELETE ON torrents BEGIN SELECT RAISE(ABORT, 'synthetic delete failure'); END;").unwrap();
    let mut control = Controller::new(backend, vec![item]);
    control.pending_removals.insert("v1: abc".into());
    control.tick();
    assert_eq!(
        control.torrents.len(),
        1,
        "queued remove is not a detachment ACK"
    );
    control.apply(SessionEvent::Removed {
        id: "v1: abc".into(),
    });
    assert_eq!(control.torrents.len(), 1);
    assert!(control.is_busy("v1: abc"));
    assert_eq!(control.backend.load_torrents().unwrap().len(), 1);
    faults
        .execute_batch("DROP TRIGGER blocked_delete;")
        .unwrap();
    control.tick();
    assert!(control.torrents.is_empty());
    assert!(control.backend.load_torrents().unwrap().is_empty());
    assert!(control.pending_removals.is_empty());
    assert_eq!(
        std::fs::read(payload).unwrap(),
        b"synthetic payload remains"
    );
}

#[test]
fn indexed_alias_events_follow_structural_removal_and_do_not_touch_neighbors() {
    let items = (0..5_000)
        .map(|index| {
            let mut item = torrent();
            item.info_hashes = vec![format!("v1: {index}"), format!("v2: {index}")];
            item
        })
        .collect();
    let mut control = Controller::new(ClientBackend::offline(), items);
    control.torrents.remove(8);
    control.apply(SessionEvent::NetworkTotals {
        id: "v2: 4999".into(),
        uploaded: 123,
        downloaded: 456,
        seeded_seconds: 7,
    });
    assert_eq!(control.torrents[4_998].runtime.network_downloaded, 456);
    assert_eq!(control.torrents[4_997].runtime.network_downloaded, 0);
    assert!(!control.observed.contains_key("v1: 8;v2: 8"));
    let mut replacement = torrent();
    replacement.info_hashes = vec!["v1: replacement".into()];
    control.torrents[4_998] = replacement;
    control.invalidate_torrent_index();
    control.apply(SessionEvent::NetworkTotals {
        id: "v1: replacement".into(),
        uploaded: 321,
        downloaded: 654,
        seeded_seconds: 9,
    });
    assert_eq!(control.torrents[4_998].runtime.network_downloaded, 654);
    assert!(!control.observed.contains_key("v1: 4999;v2: 4999"));
    control.apply(SessionEvent::Stopped);
    assert!(
        control
            .observed
            .values()
            .all(|row| row.state == ObservedState::Stopped)
    );
}

#[test]
#[ignore = "Release-only synthetic controller timing; no engine or user profile"]
fn release_indexed_controller_measurement() {
    for count in [100, 1_000, 5_000, 10_000, 100_000] {
        let items = (0..count)
            .map(|index| {
                let mut item = torrent();
                item.info_hashes = vec![format!("v1: {index}")];
                item
            })
            .collect();
        let mut control = Controller::new(ClientBackend::offline(), items);
        let start = std::time::Instant::now();
        for index in 0..20_000 {
            control.apply(SessionEvent::NetworkTotals {
                id: format!("v1: {}", index % count),
                uploaded: index as u64,
                downloaded: index as u64,
                seeded_seconds: 0,
            });
        }
        let event_micros = start.elapsed().as_micros();
        // No projection changed between these publications: structural work
        // must remain independent of the number of registered torrents.
        control.publish();
        let start = std::time::Instant::now();
        for _ in 0..20_000 {
            control.publish();
        }
        let publish_micros = start.elapsed().as_micros();
        println!(
            "{{\"kind\":\"indexed_controller\",\"torrents\":{count},\"events\":20000,\"event_total_us\":{event_micros},\"unchanged_publish_total_us\":{publish_micros}}}"
        );
        std::hint::black_box(control);
    }
}

fn page(reader: &ReadHandle) -> ReadPage {
    reader.page(0, MAX_PAGE_SIZE, None, None).unwrap()
}

fn stats(finished: bool) -> SessionEvent {
    SessionEvent::Stats {
        id: "v1: abc".into(),
        downloaded_bytes: 100,
        uploaded_bytes: 7,
        download_rate: 0,
        upload_rate: 0,
        file_progress: vec![100],
        peers: 3,
        finished,
        paused: false,
    }
}

fn emit(controller: &mut Controller, event: SessionEvent) -> SessionNotice {
    let notice = controller.apply(event);
    controller.publish();
    notice
}

#[test]
fn restored_state_and_ui_drafts_are_not_engine_acknowledgements() {
    let mut item = torrent();
    item.state = TorrentState::Seeding;
    item.downloaded_bytes = 100;
    item.completion = CompletionStatus::Verified;
    item.overrides.download_mode = DownloadMode::NoProgress;
    let mut control = Controller::new(ClientBackend::offline(), vec![item]);
    let reader = control.read_handle();
    let first = page(&reader);
    assert_eq!(first.torrents[0].state, ObservedState::AwaitingEngine);
    assert_eq!(first.torrents[0].completion, ObservedCompletion::Pending);
    assert_eq!(first.torrents[0].downloaded_bytes, 0);
    assert_eq!(first.torrents[0].active_mode, None);
    control.torrents[0].state = TorrentState::Paused;
    control.publish();
    assert_eq!(page(&reader), first);
    emit(
        &mut control,
        SessionEvent::Added {
            id: "v1: abc".into(),
        },
    );
    assert_eq!(
        page(&reader).torrents[0].state,
        ObservedState::AwaitingEngine,
        "a handle acknowledgement must not publish an optimistic UI pause"
    );
    assert_eq!(
        page(&reader).torrents[0].completion,
        ObservedCompletion::Pending,
        "an added handle does not confirm restored integrity"
    );
}

#[test]
fn controller_applies_once_and_every_reader_observes_the_result() {
    let mut control = controller();
    let first = control.read_handle();
    let second = control.read_handle();
    let notice = emit(
        &mut control,
        SessionEvent::Added {
            id: "v1: abc".into(),
        },
    );
    assert_eq!(
        notice,
        SessionNotice::Added {
            id: "v1: abc".into()
        }
    );
    emit(&mut control, stats(false));
    let snapshot = page(&first);
    assert_eq!(snapshot, page(&second));
    assert_eq!(
        snapshot.torrents[0].downloaded_bytes,
        control.torrents[0].downloaded_bytes
    );
    assert_eq!(
        snapshot.torrents[0].connected_peers,
        control.torrents[0].peers
    );
    assert_eq!(snapshot.torrents[0].state, ObservedState::Downloading);
    assert_eq!(snapshot.torrents[0].completion, ObservedCompletion::Pending);
    assert!(!control.torrents[0].completed_successfully());
    assert_eq!(page(&first), page(&first), "reading does not consume state");
    assert!(control.poll_next().is_none());
}

#[test]
fn selected_file_accounting_uses_the_canonical_reducer() {
    let mut control = controller();
    let mut event = stats(false);
    if let SessionEvent::Stats {
        file_progress,
        downloaded_bytes,
        ..
    } = &mut event
    {
        *downloaded_bytes = 1_000;
        *file_progress = vec![42];
    }
    emit(&mut control, event);
    assert_eq!(
        page(&control.read_handle()).torrents[0].downloaded_bytes,
        42
    );
    assert_eq!(control.torrents[0].downloaded_bytes, 42);
}

#[test]
fn integrity_checks_and_failure_cannot_be_overwritten_by_stale_stats() {
    let mut control = controller();
    let reader = control.read_handle();
    emit(
        &mut control,
        SessionEvent::Verification {
            id: "v1: abc".into(),
            status: CompletionStatus::Checking {
                checked_bytes: 25,
                total_bytes: 100,
            },
        },
    );
    control.torrents[0].state = TorrentState::Paused;
    emit(&mut control, stats(true));
    let checking = page(&reader).torrents.remove(0);
    assert_eq!(checking.state, ObservedState::Checking);
    assert_eq!(checking.completion, ObservedCompletion::Checking);
    assert_eq!(
        (checking.checked_bytes, checking.check_total_bytes),
        (25, 100)
    );
    emit(
        &mut control,
        SessionEvent::Verification {
            id: "v1: abc".into(),
            status: CompletionStatus::Failed {
                message: "mismatch".into(),
            },
        },
    );
    emit(&mut control, stats(true));
    assert_eq!(
        page(&reader).torrents[0].completion,
        ObservedCompletion::Failed
    );
    assert_eq!(
        page(&reader).torrents[0].state,
        ObservedState::VerificationFailed
    );
    emit(
        &mut control,
        SessionEvent::Verification {
            id: "v1: abc".into(),
            status: CompletionStatus::Verified,
        },
    );
    emit(&mut control, stats(true));
    assert_eq!(
        page(&reader).torrents[0].completion,
        ObservedCompletion::Verified
    );
    assert!(control.torrents[0].completed_successfully());
}

#[test]
fn empty_selection_is_not_verified_by_a_finished_counter() {
    let mut control = controller();
    control.torrents[0].files.select_all(false);
    emit(&mut control, stats(true));
    assert_eq!(
        page(&control.read_handle()).torrents[0].completion,
        ObservedCompletion::Pending
    );
}

#[test]
fn mode_is_confirmed_separately_from_requested_settings() {
    let mut control = controller();
    let reader = control.read_handle();
    let requested = ModeConfig {
        mode: DownloadMode::NoProgress,
        ..Default::default()
    };
    emit(
        &mut control,
        SessionEvent::Mode {
            id: "v1: abc".into(),
            application: ModeApplication::Applying {
                requested,
                active: None,
            },
        },
    );
    assert_eq!(
        page(&reader).torrents[0].mode_state,
        ObservedModeState::Applying
    );
    assert_eq!(page(&reader).torrents[0].active_mode, None);
    emit(
        &mut control,
        SessionEvent::Mode {
            id: "v1: abc".into(),
            application: ModeApplication::Active {
                config: requested,
                tracker_enabled: false,
            },
        },
    );
    let active = page(&reader).torrents.remove(0);
    assert_eq!(active.mode_state, ObservedModeState::Active);
    assert_eq!(active.active_mode, Some(DownloadMode::NoProgress));
    assert_eq!(active.tracker_enabled, Some(false));
    emit(
        &mut control,
        SessionEvent::Mode {
            id: "v1: abc".into(),
            application: ModeApplication::Failed {
                requested: ModeConfig::default(),
                active: Some(requested),
                message: "rejected".into(),
            },
        },
    );
    let failed = page(&reader).torrents.remove(0);
    assert_eq!(failed.mode_state, ObservedModeState::Failed);
    assert_eq!(failed.active_mode, Some(DownloadMode::NoProgress));
}

#[test]
fn transfer_totals_are_actual_and_separate_from_payload_progress() {
    let mut control = controller();
    emit(&mut control, stats(false));
    emit(
        &mut control,
        SessionEvent::NetworkTotals {
            seeded_seconds: 0,
            id: "v1: abc".into(),
            uploaded: 150,
            downloaded: 200,
        },
    );
    let row = page(&control.read_handle()).torrents.remove(0);
    assert_eq!((row.downloaded_bytes, row.uploaded_bytes), (100, 7));
    assert_eq!(
        (row.network_downloaded_bytes, row.network_uploaded_bytes),
        (200, 150)
    );
}

#[test]
fn aliases_match_the_same_torrent_without_creating_duplicate_records() {
    let mut item = torrent();
    item.info_hashes.push("v2: def".into());
    let mut control = Controller::new(ClientBackend::offline(), vec![item]);
    emit(&mut control, stats(false));
    let snapshot = page(&control.read_handle());
    assert_eq!(snapshot.total, 1);
    assert_eq!(snapshot.torrents[0].id, "v1: abc;v2: def");
    assert_eq!(snapshot.torrents[0].downloaded_bytes, 100);
}

#[test]
fn unrelated_events_and_repeated_snapshots_do_not_advance_revision() {
    let mut control = controller();
    let reader = control.read_handle();
    let initial = page(&reader).revision;
    emit(
        &mut control,
        SessionEvent::Error {
            id: "unknown".into(),
            message: "ignored".into(),
        },
    );
    control.publish();
    assert_eq!(page(&reader).revision, initial);
    emit(&mut control, stats(false));
    let changed = page(&reader).revision;
    emit(&mut control, stats(false));
    assert_eq!(page(&reader).revision, changed);
}

#[test]
fn additions_removals_and_stop_are_visible_without_event_consumption() {
    let mut control = controller();
    let reader = control.read_handle();
    let mut second = torrent();
    second.info_hashes = vec!["v1: xyz".into()];
    control.torrents.push(second);
    control.publish();
    assert_eq!(page(&reader).total, 2);
    emit(
        &mut control,
        SessionEvent::Removed {
            id: "v1: abc".into(),
        },
    );
    assert_eq!(page(&reader).torrents[0].state, ObservedState::Removed);
    control.torrents.remove(0);
    control.publish();
    assert_eq!(page(&reader).total, 1);
    emit(&mut control, SessionEvent::Stopped);
    assert_eq!(page(&reader).torrents[0].state, ObservedState::Stopped);
    control.torrents.clear();
    control.publish();
    assert_eq!(page(&reader).total, 0);
}

#[test]
fn page_limits_offsets_and_revision_pinning_are_enforced() {
    let items = (0..260)
        .map(|index| {
            let mut item = torrent();
            item.info_hashes = vec![format!("v1: {index:040x}")];
            item
        })
        .collect();
    let mut control = Controller::new(ClientBackend::offline(), items);
    let reader = control.read_handle();
    for limit in [0, MAX_PAGE_SIZE + 1, usize::MAX] {
        assert_eq!(
            reader.page(0, limit, None, None),
            Err(ReadPageError::InvalidLimit)
        );
    }
    assert_eq!(
        reader.page(261, 1, None, None),
        Err(ReadPageError::InvalidOffset)
    );
    assert_eq!(
        reader.page(usize::MAX, 1, None, None),
        Err(ReadPageError::InvalidOffset)
    );
    let first = reader.page(0, 128, None, None).unwrap();
    assert_eq!(first.total, 260);
    assert_eq!(first.next_offset, Some(128));
    let second = reader.page(128, 128, Some(first.revision), None).unwrap();
    let last = reader.page(256, 128, Some(first.revision), None).unwrap();
    assert_eq!(second.torrents.len(), 128);
    assert_eq!(last.torrents.len(), 4);
    assert_eq!(last.next_offset, None);
    assert!(reader.page(260, 1, None, None).unwrap().torrents.is_empty());
    control.torrents.remove(0);
    control.publish();
    assert_eq!(
        reader.page(128, 128, Some(first.revision), None),
        Err(ReadPageError::RevisionConflict)
    );
    assert_eq!(
        reader.page(0, 1, None, Some(u64::MAX)),
        Err(ReadPageError::RevisionConflict)
    );
}

#[test]
fn slow_readers_have_independent_cursors_and_resynchronize_to_latest() {
    let mut control = controller();
    let fast = control.read_handle();
    let slow = control.read_handle();
    let initial = page(&slow).revision;
    for downloaded in 1..=10_000 {
        emit(
            &mut control,
            SessionEvent::NetworkTotals {
                seeded_seconds: 0,
                id: "v1: abc".into(),
                uploaded: 0,
                downloaded,
            },
        );
        let latest = page(&fast);
        assert!(
            !fast
                .page(0, 1, None, Some(latest.revision))
                .unwrap()
                .resync_required
        );
    }
    let recovered = slow.page(0, 1, None, Some(initial)).unwrap();
    assert!(recovered.resync_required);
    assert_eq!(recovered.torrents[0].network_downloaded_bytes, 10_000);
    assert_eq!(recovered.revision, initial + 10_000);
    assert_eq!(page(&fast), page(&slow));
}

#[test]
fn concurrent_observers_do_not_block_publication_or_each_other() {
    let mut control = controller();
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let reader = control.read_handle();
            std::thread::spawn(move || {
                let mut previous = 0;
                for _ in 0..2_000 {
                    let current = page(&reader);
                    assert!(current.revision >= previous);
                    previous = current.revision;
                }
            })
        })
        .collect();
    for downloaded in 1..=1_000 {
        emit(
            &mut control,
            SessionEvent::NetworkTotals {
                seeded_seconds: 0,
                id: "v1: abc".into(),
                uploaded: 0,
                downloaded,
            },
        );
    }
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(
        page(&control.read_handle()).torrents[0].network_downloaded_bytes,
        1_000
    );
}

#[test]
fn dropping_observers_does_not_consume_events_or_drop_the_controller() {
    let mut control = controller();
    let reader = control.read_handle();
    drop(control.read_handle());
    emit(&mut control, stats(false));
    assert_eq!(page(&reader).torrents[0].downloaded_bytes, 100);
    drop(reader);
    emit(&mut control, stats(true));
    assert_eq!(
        page(&control.read_handle()).torrents[0].completion,
        ObservedCompletion::Verified
    );
}

#[test]
fn serialized_observations_exclude_untrusted_and_secret_bearing_fields() {
    let mut item = torrent();
    item.name = "untrusted-name-do-not-execute".into();
    item.source = "sensitive-source-path".into();
    item.trackers = vec!["https://tracker.test/secret-passkey/announce?token=private-token".into()];
    item.peer_id = "private-peer-identity".into();
    let mut control = Controller::new(ClientBackend::offline(), vec![item]);
    emit(
        &mut control,
        SessionEvent::Error {
            id: "v1: abc".into(),
            message: "https://tracker.test/secret-passkey?token=private-token".into(),
        },
    );
    let snapshot = page(&control.read_handle());
    let json = serde_json::to_string(&snapshot).unwrap();
    for forbidden in [
        "untrusted-name",
        "sensitive-source",
        "secret-passkey",
        "private-token",
        "private-peer",
        "https://",
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden}");
    }
    let restored: ReadPage = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, snapshot);
    assert!(json.contains("\"protocol_version\":1"));
    assert!(json.contains("\"state\":\"error\""));
}

#[test]
fn controller_shutdown_is_visible_to_surviving_read_handles() {
    let mut control = controller();
    let reader = control.read_handle();
    emit(&mut control, stats(false));
    let before = page(&reader);
    assert!(before.controller_available);
    drop(control);
    let after = page(&reader);
    assert!(!after.controller_available);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.torrents, before.torrents);
}

fn controller_with_shutdown_journal(
    directory: &std::path::Path,
    journal: crate::announce_journal::AnnounceJournal,
) -> Controller {
    let runtime = crate::session::SessionRuntime::start_test_with_journal(
        64,
        directory.to_owned(),
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
    Controller::new(ClientBackend::with_session(runtime), Vec::new())
}

#[test]
fn owner_shutdown_seals_and_syncs_journal_while_observer_clone_survives_runtime() {
    let temporary = tempfile::tempdir().unwrap();
    let retained =
        crate::announce_journal::AnnounceJournal::open(&temporary.path().join("journal"));
    let manifest = retained.manifest_path().unwrap();
    let mut owner = controller_with_shutdown_journal(temporary.path(), retained.clone());
    for _ in 0..16 {
        retained.record(crate::announce_journal::tests::observation(
            "synthetic-owner-shutdown",
        ));
    }
    owner.shutdown().unwrap();
    assert!(!owner.backend.session_available());
    let status = retained.status();
    assert!(status.closed && status.error.is_none());
    assert_eq!(
        (
            status.accepted_sequence,
            status.written_sequence,
            status.durable_sequence
        ),
        (16, 16, 16)
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(saved["clean_shutdown"], true);
    assert_eq!(saved["incomplete"], false);
    let path = retained.path().unwrap();
    let length = std::fs::metadata(&path).unwrap().len();
    retained.record(crate::announce_journal::tests::observation(
        "rejected-after-owner-shutdown",
    ));
    let after = retained.status();
    assert_eq!(
        after.accepted_sequence, 16,
        "a surviving observer cannot reopen admission after shutdown"
    );
    assert_eq!(after.rejected_records, status.rejected_records + 1);
    assert_eq!(std::fs::metadata(path).unwrap().len(), length);
    let reopened = crate::announce_journal::AnnounceJournal::reopen(&manifest).unwrap();
    assert_eq!(reopened.status().durable_sequence, 16);
}

#[test]
fn owner_shutdown_surfaces_manifest_seal_failure_and_still_joins_runtime() {
    let temporary = tempfile::tempdir().unwrap();
    let retained =
        crate::announce_journal::AnnounceJournal::open(&temporary.path().join("journal"));
    retained.record(crate::announce_journal::tests::observation(
        "synthetic-seal-failure",
    ));
    retained.flush().unwrap();
    let manifest = retained.manifest_path().unwrap();
    std::fs::remove_file(&manifest).unwrap(); // Only the disposable test manifest.
    std::fs::create_dir(&manifest).unwrap(); // Actual filesystem rename failure.
    let mut owner = controller_with_shutdown_journal(temporary.path(), retained.clone());
    let error = owner.shutdown().unwrap_err();
    assert!(error.contains("journal manifest"), "{error}");
    assert!(
        !owner.backend.session_available(),
        "journal failure must not leave the engine running"
    );
    assert!(retained.status().closed);
    assert!(retained.error().is_some());
}

#[test]
fn owner_shutdown_surfaces_initial_journal_open_failure_with_a_retained_clone() {
    let temporary = tempfile::tempdir().unwrap();
    let blocker = temporary.path().join("journal-is-a-file");
    std::fs::write(&blocker, b"synthetic directory blocker").unwrap();
    let retained = crate::announce_journal::AnnounceJournal::open(&blocker);
    let initial_error = retained
        .error()
        .expect("filesystem open failure must be visible");
    let mut owner = controller_with_shutdown_journal(temporary.path(), retained.clone());
    assert_eq!(owner.shutdown().unwrap_err(), initial_error);
    assert!(!owner.backend.session_available());
    assert_eq!(retained.error(), Some(initial_error));
}

#[test]
fn pause_resume_and_mode_reset_follow_engine_confirmations() {
    let mut control = controller();
    let reader = control.read_handle();
    emit(&mut control, stats(false));
    emit(
        &mut control,
        SessionEvent::StateChanged {
            id: "v1: abc".into(),
            state: "paused",
        },
    );
    assert_eq!(page(&reader).torrents[0].state, ObservedState::Paused);
    emit(
        &mut control,
        SessionEvent::StateChanged {
            id: "v1: abc".into(),
            state: "live",
        },
    );
    assert_eq!(page(&reader).torrents[0].state, ObservedState::Downloading);
    emit(
        &mut control,
        SessionEvent::Mode {
            id: "v1: abc".into(),
            application: ModeApplication::Active {
                config: ModeConfig {
                    mode: DownloadMode::FreezeAfter,
                    freeze_after_bytes: 1024,
                    upload_ratio: Default::default(),
                },
                tracker_enabled: true,
            },
        },
    );
    assert_eq!(
        page(&reader).torrents[0].active_freeze_after_bytes,
        Some(1024)
    );
    emit(
        &mut control,
        SessionEvent::Mode {
            id: "v1: abc".into(),
            application: ModeApplication::AwaitingEngine,
        },
    );
    let row = page(&reader).torrents.remove(0);
    assert_eq!(row.mode_state, ObservedModeState::AwaitingEngine);
    assert_eq!(row.active_mode, None);
    assert_eq!(row.active_freeze_after_bytes, None);
    assert_eq!(row.tracker_enabled, None);
}

#[test]
fn real_session_is_consumed_once_and_readers_do_not_own_its_lifecycle() {
    use crate::{
        client::add_torrent_command,
        session::{SessionCommand, SessionRuntime},
        settings::ClientSettings,
    };
    use sha1::{Digest, Sha1};
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let payload = b"local control fixture";
    let mut info = format!(
        "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces20:",
        payload.len()
    )
    .into_bytes();
    info.extend_from_slice(&Sha1::digest(payload));
    info.extend_from_slice(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut metainfo = b"d4:info".to_vec();
    metainfo.extend_from_slice(&info);
    metainfo.push(b'e');
    let source = root.path().join("fixture.torrent");
    std::fs::write(&source, &metainfo).unwrap();
    std::fs::write(root.path().join("payload.bin"), payload).unwrap();
    let runtime = SessionRuntime::start_with_options(
        32,
        root.path().into(),
        librqbit::SessionOptions {
            dht: None,
            listen: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut item = torrent();
    item.info_hashes = vec![id.clone()];
    item.source = source.display().to_string();
    item.files = crate::file_selection::FileSelectionModel::from_files([(
        "payload.bin".into(),
        payload.len() as u64,
    )]);
    item.overrides.download_directory = Some(root.path().display().to_string());
    item.state = TorrentState::Paused;
    let mut control = Controller::new(ClientBackend::with_session(runtime), vec![item]);
    let first = control.read_handle();
    let second = control.read_handle();
    control
        .backend
        .send(add_torrent_command(
            &control.torrents[0],
            &ClientSettings::default(),
        ))
        .unwrap();

    let wait = |control: &mut Controller, expected: ObservedState| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while let Some(notice) = control.poll_next() {
                assert!(!matches!(notice, SessionNotice::Error { .. }), "{notice:?}");
            }
            control.publish();
            let row = page(&control.read_handle()).torrents.remove(0);
            if row.state == expected && row.completion == ObservedCompletion::Verified {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "did not reach {expected:?}: {row:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&mut control, ObservedState::Paused);
    assert_eq!(page(&first), page(&second));
    assert!(control.torrents[0].completed_successfully());
    drop(second);
    control.backend.send(SessionCommand::Resume { id }).unwrap();
    wait(&mut control, ObservedState::Seeding);
    assert_eq!(page(&first).torrents[0].state, ObservedState::Seeding);
    assert_eq!(control.torrents[0].state, TorrentState::Seeding);
    assert!(control.torrents[0].completed_successfully());
    drop(control);
    assert!(!page(&first).controller_available);
}

proptest::proptest! {
    #[test]
    fn arbitrary_page_parameters_are_bounded_and_never_panic(
        offset in proptest::prelude::any::<usize>(),
        limit in proptest::prelude::any::<usize>(),
        expected in proptest::option::of(proptest::prelude::any::<u64>()),
        after in proptest::option::of(proptest::prelude::any::<u64>()),
    ) {
        let control = controller();
        if let Ok(result) = control.read_handle().page(offset, limit, expected, after) {
            proptest::prop_assert!(result.torrents.len() <= MAX_PAGE_SIZE);
            proptest::prop_assert!(result.torrents.len() <= limit);
            proptest::prop_assert!(offset <= result.total);
        }
    }
}
