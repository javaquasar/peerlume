use super::*;
use sha1::{Digest, Sha1};
use std::{path::Path, time::Instant};

fn metadata() -> (Vec<u8>, String) {
    let mut info = b"d6:lengthi8e4:name11:payload.bin12:piece lengthi16384e6:pieces20:".to_vec();
    info.extend_from_slice(&Sha1::digest(b"verified"));
    info.extend_from_slice(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut bytes = b"d4:info".to_vec();
    bytes.extend(info);
    bytes.push(b'e');
    (bytes, id)
}
#[test]
fn hidden_file_summary_and_selection_ack_exclude_unselected_boundary_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let first = vec![b'a'; 4000];
    let second = vec![b'b'; 12384];
    let mut piece = first.clone();
    piece.extend(&second);
    let mut info = b"d5:filesld6:lengthi4000e4:pathl5:a.bineed6:lengthi12384e4:pathl5:b.bineee4:name8:boundary12:piece lengthi16384e6:pieces20:".to_vec();
    info.extend(Sha1::digest(&piece));
    info.extend(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut bytes = b"d4:info".to_vec();
    bytes.extend(info);
    bytes.push(b'e');
    let source = temp.path().join("boundary.torrent");
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(temp.path().join("a.bin"), first).unwrap();
    std::fs::write(temp.path().join("b.bin"), second).unwrap();
    let mut command = add(&id, &source, temp.path());
    if let SessionCommand::AddTorrent {
        full_size,
        only_files,
        ..
    } = &mut command
    {
        *full_size = 16384;
        *only_files = Some(vec![0]);
    }
    let runtime = runtime(temp.path());
    runtime.try_send(command).unwrap();
    wait(
        &runtime,
        |event| matches!(event, SessionEvent::Stats { downloaded_bytes: 4000, finished: true, file_progress, .. } if file_progress.is_empty()),
    );
    runtime
        .try_send(SessionCommand::SetFiles {
            id: id.clone(),
            only_files: vec![1],
        })
        .unwrap();
    let mut snapshot_seen = false;
    wait(&runtime, |event| match event {
        SessionEvent::FileSnapshot {
            id: actual,
            progress,
        } => {
            assert_eq!(actual, &id);
            assert_eq!(progress, &[4000, 12384]);
            snapshot_seen = true;
            false
        }
        SessionEvent::FilesApplied { only_files, .. } => {
            assert!(
                snapshot_seen,
                "actual counts must precede a durable selection acknowledgement"
            );
            assert_eq!(only_files, &[1]);
            true
        }
        _ => false,
    });
    wait(
        &runtime,
        |event| matches!(event, SessionEvent::Stats { downloaded_bytes: 12384, finished: true, file_progress, .. } if file_progress.is_empty()),
    );
    runtime.shutdown().unwrap();
}
#[test]
fn real_integrity_checks_wait_for_bulk_admission_and_shutdown_cancels_a_queued_check() {
    let temp = tempfile::tempdir().unwrap();
    let (bytes, id) = metadata();
    std::fs::write(temp.path().join("payload.bin"), b"verified").unwrap();
    let root = crate::storage::sandbox::Directory::open(temp.path()).unwrap();
    let runtime = runtime(temp.path());
    let budget = runtime.test_bulk_budget.clone();
    let blocker = budget.register(vec![]).unwrap();
    let permit = blocker.try_enter().unwrap();
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &temp.path().join("unused.torrent"), temp.path())),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory: root.clone(),
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Added { .. })
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(root.read_calls(), 0, "queued check must not read payloads");
    drop(permit);
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Stats { finished: true, .. })
    });
    assert!(root.read_calls() > 0);
    let blocker = budget.register(vec![]).unwrap();
    let _permit = blocker.try_enter().unwrap();
    let before = root.read_calls();
    runtime.try_send(SessionCommand::Recheck { id }).unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Added { .. })
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(root.read_calls(), before);
    let start = Instant::now();
    runtime.shutdown().unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(
        std::fs::read(temp.path().join("payload.bin")).unwrap(),
        b"verified"
    );
}

#[test]
fn indexed_handle_replacement_and_detail_leases_keep_real_summary_and_payload() {
    let root = tempfile::tempdir().unwrap();
    let (bytes, id) = metadata();
    let source = root.path().join("source.torrent");
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(root.path().join("payload.bin"), b"verified").unwrap();
    let runtime = runtime(root.path());
    runtime.try_send(add(&id, &source, root.path())).unwrap();
    wait(
        &runtime,
        |event| matches!(event, SessionEvent::Stats { finished: true, downloaded_bytes: 8, file_progress, .. } if file_progress.is_empty()),
    );
    let engine = runtime.test_engine().unwrap();
    let hash = librqbit::dht::Id20::new(
        crate::tracker::parse_info_hash_hex(id.strip_prefix("v1: ").unwrap()).unwrap(),
    );
    let lookup = || engine.get(librqbit::api::TorrentIdOrHash::Hash(hash));
    let original = lookup().unwrap();
    assert!(
        original
            .stats_with_file_progress(false)
            .file_progress
            .is_empty()
    );
    assert_eq!(original.stats().file_progress, vec![8]);
    assert_eq!(original.selected_file_progress(), Some(8));
    runtime.set_detail_interest(Some((
        id.clone(),
        DetailInterest {
            files: true,
            peers: false,
        },
    )));
    wait(
        &runtime,
        |event| matches!(event, SessionEvent::Stats { file_progress, downloaded_bytes: 8, .. } if file_progress == &[8]),
    );
    runtime.set_detail_interest(None);
    wait(
        &runtime,
        |event| matches!(event, SessionEvent::Stats { file_progress, downloaded_bytes: 8, .. } if file_progress.is_empty()),
    );
    runtime
        .try_send(SessionCommand::Recheck { id: id.clone() })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Verified,
                ..
            }
        )
    });
    let replacement = lookup().unwrap();
    assert!(!Arc::ptr_eq(&original, &replacement));
    assert_eq!(replacement.selected_file_progress(), Some(8));
    runtime
        .try_send(SessionCommand::Remove { id: id.clone() })
        .unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Removed { .. })
    });
    assert!(lookup().is_none());
    assert!(engine.get(original.id().into()).is_none());
    runtime.try_send(add(&id, &source, root.path())).unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Stats { finished: true, .. })
    });
    let readded = lookup().unwrap();
    assert!(!Arc::ptr_eq(&replacement, &readded));
    assert_eq!(readded.selected_file_progress(), Some(8));
    assert_eq!(
        std::fs::read(root.path().join("payload.bin")).unwrap(),
        b"verified"
    );
    runtime.shutdown().unwrap();
}
#[test]
fn cached_pieces_reduce_initial_reads_but_forced_check_hashes_every_piece_and_keeps_file_priority()
{
    let temp = tempfile::tempdir().unwrap();
    let piece = vec![45u8; 16384];
    let count = 128;
    let mut info = format!(
        "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces{}:",
        piece.len() * count,
        20 * count
    )
    .into_bytes();
    for _ in 0..count {
        info.extend_from_slice(&Sha1::digest(&piece));
    }
    info.extend_from_slice(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut bytes = b"d4:info".to_vec();
    bytes.extend(info);
    bytes.push(b'e');
    std::fs::write(temp.path().join("payload.bin"), piece.repeat(count)).unwrap();
    let root = crate::storage::sandbox::Directory::open(temp.path()).unwrap();
    root.supply_resume(vec![255; 16]);
    let runtime = runtime(temp.path());
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &temp.path().join("missing.torrent"), temp.path())),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory: root.clone(),
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Stats { finished: true, .. })
    });
    let sampled = root.read_calls();
    assert!(
        sampled > 0 && sampled < 128,
        "cached check read {sampled} pieces"
    );
    runtime
        .try_send(SessionCommand::SetFilePriorities {
            id: id.clone(),
            high: vec![0],
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::FilePrioritiesApplied { .. })
    });
    runtime
        .try_send(SessionCommand::Recheck { id: id.clone() })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Verified,
                ..
            }
        )
    });
    assert!(root.read_calls() >= sampled + 128);
    let engine = runtime.test_engine().unwrap();
    assert_eq!(
        engine.with_torrents(|torrents| torrents.next().unwrap().1.high_priority_files()),
        vec![0]
    );
    runtime.shutdown().unwrap();
}
fn runtime(root: &Path) -> SessionRuntime {
    SessionRuntime::start_with_options(
        64,
        root.into(),
        SessionOptions {
            dht: None,
            listen: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn startup_acknowledges_a_real_engine_and_returns_initialization_failures_synchronously() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    assert!(runtime.test_engine().is_some());
    runtime.shutdown().unwrap();
    let error = SessionRuntime::start_with_options(
        8,
        root.path().into(),
        SessionOptions {
            dht: None,
            listen: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            bind_device_name: Some("invalid\0device".into()),
            ..Default::default()
        },
    )
    .err()
    .expect("invalid device must reject initialization before returning an engine");
    assert!(error.contains("bind device"), "{error}");
}
fn add(id: &str, source: &Path, output: &Path) -> SessionCommand {
    SessionCommand::AddTorrent {
        resume: Default::default(),
        repair_policy: Default::default(),
        verify_after_download: false,
        download_mode: DownloadMode::Normal,
        freeze_after_bytes: 0,
        upload_ratio: Default::default(),
        full_size: 8,
        id: id.into(),
        source: source.display().to_string(),
        output_folder: output.display().to_string(),
        only_files: None,
        trackers: vec![],
        paused: true,
        transfer: Default::default(),
        peer_limit: None,
    }
}
fn wait(runtime: &SessionRuntime, mut predicate: impl FnMut(&SessionEvent) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(event) = runtime.try_recv() {
            if predicate(&event) {
                return;
            }
            if let SessionEvent::Error { message, .. } = &event {
                panic!("sandbox backend error: {message}");
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Instant::now() < end, "sandbox event timed out");
    }
}
#[test]
fn prepared_add_uses_the_validated_bytes_without_reopening_a_missing_source() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("payload.bin"), b"verified").unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(root.path());
    let source = root.path().join("no-longer-present.torrent");
    let prepared = PreparedTorrent {
        bytes: bytes.into(),
        directory: crate::storage::sandbox::Directory::open(root.path()).unwrap(),
    };
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &source, root.path())),
            prepared,
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: true,
                ..
            }
        )
    });
    assert!(!source.exists());
    runtime.shutdown().unwrap();
}

#[test]
fn copy_selects_verified_payload_in_the_same_engine_without_deleting_originals_or_source_metadata()
{
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("payload.bin"), b"verified").unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(source.path());
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(
                &id,
                &source.path().join("absent.torrent"),
                source.path(),
            )),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory: crate::storage::sandbox::Directory::open(source.path()).unwrap(),
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                paused: true,
                finished: true,
                ..
            }
        )
    });
    let engine = runtime.test_engine().unwrap();
    runtime
        .try_send(SessionCommand::SelectStorage {
            id: id.clone(),
            directory: destination.path().display().to_string(),
            copy: true,
        })
        .unwrap();
    let mut selected = None;
    wait(&runtime, |event| {
        if let SessionEvent::Storage {
            status: crate::storage::relocation::StorageStatus::Checking { destination },
            ..
        } = event
        {
            selected = Some(destination.clone());
            true
        } else {
            if let SessionEvent::Storage {
                status: crate::storage::relocation::StorageStatus::Failed { message },
                ..
            } = event
            {
                panic!("storage failed: {message}");
            }
            false
        }
    });
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: false,
                ..
            }
        )
    });
    assert!(Arc::ptr_eq(&engine, &runtime.test_engine().unwrap()));
    assert_eq!(
        std::fs::read(source.path().join("payload.bin")).unwrap(),
        b"verified"
    );
    assert_eq!(
        std::fs::read(std::path::Path::new(&selected.unwrap()).join("payload.bin")).unwrap(),
        b"verified"
    );
    runtime.shutdown().unwrap();
}

#[test]
fn resumed_network_totals_and_seeding_budget_are_not_reset_to_disk_progress() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("payload.bin"), b"verified").unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(root.path());
    let mut command = add(&id, &root.path().join("absent.torrent"), root.path());
    if let SessionCommand::AddTorrent {
        resume,
        paused,
        transfer,
        ..
    } = &mut command
    {
        *resume = crate::resume::ResumeCounters {
            downloaded: 37,
            uploaded: 23,
            seeded_seconds: 60,
        };
        *paused = false;
        transfer.seed_minutes = 1;
    }
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(command),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory: crate::storage::sandbox::Directory::open(root.path()).unwrap(),
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::NetworkTotals {
                downloaded: 37,
                uploaded: 23,
                seeded_seconds: 60,
                ..
            }
        )
    });
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::StateChanged {
                state: "paused",
                ..
            }
        )
    });
    runtime.shutdown().unwrap();
}
#[cfg(unix)]
#[test]
fn prepared_root_identity_survives_path_replacement_and_a_real_recheck() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("selected");
    let pinned = temp.path().join("pinned");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(original.join("payload.bin"), b"verified").unwrap();
    let directory = crate::storage::sandbox::Directory::open(&original).unwrap();
    std::fs::rename(&original, &pinned).unwrap();
    std::os::unix::fs::symlink(&outside, &original).unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(temp.path());
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &temp.path().join("absent.torrent"), &original)),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory,
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: true,
                ..
            }
        )
    });
    runtime
        .try_send(SessionCommand::Recheck { id: id.clone() })
        .unwrap();
    let mut checking = false;
    wait(&runtime, |event| match event {
        SessionEvent::Verification {
            status: CompletionStatus::Checking { .. },
            ..
        } => {
            checking = true;
            false
        }
        SessionEvent::Verification {
            status: CompletionStatus::Verified,
            ..
        } => checking,
        _ => false,
    });
    assert_eq!(
        std::fs::read(pinned.join("payload.bin")).unwrap(),
        b"verified"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    runtime.shutdown().unwrap();
}
#[cfg(unix)]
#[test]
fn actual_backend_refuses_a_symlink_payload_without_modifying_the_target() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (bytes, id) = metadata();
    let source = temp.path().join("source.torrent");
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(outside.path().join("original"), b"untouched").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("original"),
        temp.path().join("payload.bin"),
    )
    .unwrap();
    let runtime = runtime(temp.path());
    runtime.try_send(add(&id, &source, temp.path())).unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Error { .. })
    });
    assert_eq!(
        std::fs::read(outside.path().join("original")).unwrap(),
        b"untouched"
    );
    runtime.shutdown().unwrap();
}
#[test]
fn prepared_debug_never_prints_metainfo_contents() {
    let temp = tempfile::tempdir().unwrap();
    let prepared = PreparedTorrent {
        bytes: Arc::from(&b"SYNTHETIC_SECRET"[..]),
        directory: crate::storage::sandbox::Directory::open(temp.path()).unwrap(),
    };
    assert!(!format!("{prepared:?}").contains("SYNTHETIC_SECRET"));
}
