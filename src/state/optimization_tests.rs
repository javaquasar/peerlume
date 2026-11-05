use super::*;
use crate::file_selection::{FileSelectionModel, FileState};

fn fixture() -> TorrentRecord {
    super::tests::record()
}

#[test]
fn tracker_configuration_edits_preserve_cold_file_children_and_roll_back_atomically() {
    let mut store = StateStore::in_memory_with_secrets(std::sync::Arc::new(
        crate::secrets::MemorySecrets::default(),
    ))
    .unwrap();
    let record = fixture();
    store.save_torrent(&record).unwrap();
    store.connection.execute_batch("ALTER TABLE files RENAME TO hidden_files; ALTER TABLE verified_pieces RENAME TO hidden_pieces;").unwrap();
    let trackers = vec!["https://other.test/announce?passkey=SYNTHETIC_EDIT".into()];
    let mut audit = record.tracker_audit.clone().unwrap();
    audit.observation_seconds += 1;
    store.connection.execute_batch("CREATE TRIGGER reject_audit BEFORE DELETE ON tracker_audit_profiles BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;").unwrap();
    assert!(
        store
            .save_tracker_configuration(&record.id, &trackers, Some(&audit))
            .is_err()
    );
    assert_eq!(store.load_trackers(&record.id).unwrap(), record.trackers);
    assert_eq!(
        store.load_tracker_audit(&record.id).unwrap(),
        record.tracker_audit
    );
    store
        .connection
        .execute_batch("DROP TRIGGER reject_audit;")
        .unwrap();
    assert!(
        store
            .save_tracker_configuration(&record.id, &trackers, Some(&audit))
            .unwrap()
    );
    assert_eq!(store.load_trackers(&record.id).unwrap(), trackers);
    assert_eq!(store.load_tracker_audit(&record.id).unwrap(), Some(audit));
    assert!(
        store
            .save_tracker_configuration(&record.id, &trackers, None)
            .unwrap()
    );
    assert!(store.load_tracker_audit(&record.id).unwrap().is_none());
    let counts: (i64, i64) = store
        .connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM hidden_files),(SELECT COUNT(*) FROM hidden_pieces)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 2));
    assert!(
        !store
            .save_tracker_configuration("missing", &[], None)
            .unwrap()
    );
}

#[test]
fn narrow_ack_preserves_protected_tracker_references_when_secret_provider_is_unavailable() {
    struct Unavailable;
    impl crate::secrets::TrackerSecrets for Unavailable {
        fn protect(&self, _: &str) -> Result<Vec<u8>, crate::secrets::SecretUnavailable> {
            Err(crate::secrets::SecretUnavailable)
        }
        fn restore(&self, _: &[u8]) -> Result<String, crate::secrets::SecretUnavailable> {
            Err(crate::secrets::SecretUnavailable)
        }
    }
    let mut store = StateStore::in_memory_with_secrets(std::sync::Arc::new(
        crate::secrets::MemorySecrets::default(),
    ))
    .unwrap();
    let mut record = fixture();
    record.trackers = vec!["https://tracker.test/announce?passkey=SYNTHETIC_PRIVATE_KEY".into()];
    store.save_torrent(&record).unwrap();
    let reference: Vec<u8> = store
        .connection
        .query_row(
            "SELECT secret_ref FROM trackers WHERE torrent_id=?1",
            [&record.id],
            |row| row.get(0),
        )
        .unwrap();
    store.secrets = std::sync::Arc::new(Unavailable);
    let mut update = ScalarUpdate::from_record(&record);
    update.uploaded_bytes = 123;
    store
        .persist_acknowledged_with_audit(
            &update,
            PersistenceIntent::SCALAR,
            None,
            record.tracker_audit.as_ref(),
        )
        .unwrap();
    assert!(
        store
            .save_tracker_configuration(&record.id, &record.trackers, record.tracker_audit.as_ref())
            .unwrap()
    );
    let after: Vec<u8> = store
        .connection
        .query_row(
            "SELECT secret_ref FROM trackers WHERE torrent_id=?1",
            [&record.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, reference);
    assert!(
        store.load_torrent(&record.id).is_err(),
        "ordinary credential restoration still fails closed"
    );
}

#[test]
fn storage_ack_postcommit_failure_reports_error_and_retries_the_durable_location() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("state.sqlite3");
    let mut store = StateStore::open(&database).unwrap();
    let record = fixture();
    store.save_torrent(&record).unwrap();
    let journal = temporary.path().join("storage-move-synthetic.json");
    std::fs::write(&journal, b"invalid synthetic journal").unwrap();
    let mut update = ScalarUpdate::from_record(&record);
    update.download_directory = Some(temporary.path().join("destination").display().to_string());
    let error = store
        .persist_acknowledged(&update, PersistenceIntent::STORAGE, None)
        .unwrap_err();
    assert!(error.to_string().contains("move acknowledgement journals"));
    let committed = store.load_torrent(&record.id).unwrap().unwrap();
    assert_eq!(committed.download_directory, update.download_directory);
    assert_eq!(committed.files, record.files);
    assert!(
        journal.exists(),
        "failed postcommit cleanup must retain recovery evidence"
    );
    std::fs::remove_file(&journal).unwrap(); // Only the malformed synthetic journal.
    store
        .persist_acknowledged(&update, PersistenceIntent::STORAGE, None)
        .unwrap();
    drop(store);
    assert_eq!(
        StateStore::open(&database)
            .unwrap()
            .load_torrent(&record.id)
            .unwrap()
            .unwrap()
            .download_directory,
        update.download_directory
    );
}

#[test]
fn scalar_acknowledgements_do_not_read_file_piece_or_audit_children() {
    let mut store = StateStore::in_memory().unwrap();
    let record = fixture();
    store.save_torrent(&record).unwrap();
    // Renaming all three tables makes an accidental full-tree SELECT fail.
    // No acknowledgement is allowed to rewrite these canonical children either.
    store.connection.execute_batch("ALTER TABLE files RENAME TO hidden_files; ALTER TABLE verified_pieces RENAME TO hidden_pieces; ALTER TABLE tracker_audit_steps RENAME TO hidden_audit_steps; ALTER TABLE tracker_audit_profiles RENAME TO hidden_audit_profiles;").unwrap();
    let mut update = ScalarUpdate::from_record(&record);
    update.state = "seeding".into();
    update.uploaded_bytes = 987;
    update.resume.uploaded = 987;
    store
        .persist_acknowledged_with_audit(
            &update,
            PersistenceIntent::SCALAR,
            None,
            record.tracker_audit.as_ref(),
        )
        .unwrap();
    let values: (String, i64, i64, i64) = store.connection.query_row("SELECT state,uploaded_bytes,(SELECT COUNT(*) FROM hidden_files),(SELECT COUNT(*) FROM hidden_pieces) FROM torrents WHERE id=?1", [&record.id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(values, ("seeding".into(), 987, 1, 2));
    assert_eq!(
        store.resume_checkpoints.borrow()[&record.id].0.uploaded,
        987
    );
}

#[test]
fn selection_priorities_and_audit_commit_atomically_and_failed_attempt_is_retryable() {
    let mut store = StateStore::in_memory().unwrap();
    let record = fixture();
    store.save_torrent(&record).unwrap();
    let mut update = ScalarUpdate::from_record(&record);
    update.state = "downloading".into();
    update.downloaded_bytes = 0; // The acknowledged new selection excludes its sole file.
    update.uploaded_bytes = 99;
    update.resume.uploaded = 99;
    let files = FileSelectionModel::from_compact_snapshot([(100, 80, false)], &[])
        .persistence_snapshot(true)
        .unwrap();
    let mut audit = record.tracker_audit.clone().unwrap();
    audit.observation_seconds += 1;
    store.connection.execute_batch("CREATE TRIGGER reject_ack BEFORE UPDATE ON files BEGIN SELECT RAISE(ABORT,'synthetic child failure'); END;").unwrap();
    let mut intent = PersistenceIntent::SELECTION;
    intent.merge(PersistenceIntent::PRIORITIES);
    assert!(
        store
            .persist_acknowledged_with_audit(&update, intent, Some(&files), Some(&audit))
            .is_err()
    );
    assert_eq!(store.load_torrent(&record.id).unwrap().unwrap(), record);
    assert_eq!(store.resume_checkpoints.borrow()[&record.id].0.uploaded, 0);
    store
        .connection
        .execute_batch("DROP TRIGGER reject_ack;")
        .unwrap();
    store
        .persist_acknowledged_with_audit(&update, intent, Some(&files), Some(&audit))
        .unwrap();
    let after = store.load_torrent(&record.id).unwrap().unwrap();
    assert_eq!(after.files[0].path, record.files[0].path);
    assert!(!after.files[0].selected);
    assert_eq!(
        (after.files[0].downloaded_bytes, after.files[0].priority),
        (80, 0)
    );
    assert_eq!(after.verified_pieces, record.verified_pieces);
    assert_eq!(after.tracker_audit, Some(audit));
    assert_eq!(after.uploaded_bytes, 99);
    // Omitted audit preserves; explicit clear is a different operation.
    store
        .persist_acknowledged(&update, PersistenceIntent::SCALAR, None)
        .unwrap();
    assert!(
        store
            .load_torrent(&record.id)
            .unwrap()
            .unwrap()
            .tracker_audit
            .is_some()
    );
    assert!(store.clear_tracker_audit(&record.id).unwrap());
    assert!(
        store
            .load_torrent(&record.id)
            .unwrap()
            .unwrap()
            .tracker_audit
            .is_none()
    );
}

#[test]
fn deferred_restore_and_selection_updates_never_erase_canonical_paths_or_pieces() {
    let mut store = StateStore::in_memory().unwrap();
    let mut record = fixture();
    record.files.push(StoredFile {
        index: 1,
        path: "folder/b.bin".into(),
        size_bytes: 200,
        downloaded_bytes: 20,
        selected: false,
        priority: 0,
    });
    store.save_torrent(&record).unwrap();
    let mut restored = store.load_deferred_torrents().unwrap().pop().unwrap();
    assert!(!restored.record.files_complete);
    assert!(restored.record.files.is_empty() && restored.record.verified_pieces.is_empty());
    assert!(!restored.files.details_loaded());
    assert_eq!(restored.files.file_count(), 2);
    assert_eq!(restored.files.total_size(), 300);
    assert_eq!(restored.files.selected_downloaded(), 50);
    assert_eq!(restored.files.high_priority(), vec![0]);
    assert!(store.save_torrent(&restored.record).is_err());
    assert_eq!(store.load_torrent(&record.id).unwrap().unwrap(), record);
    restored.files.set_selected(1, true);
    restored.files.set_file_downloaded(1, 60);
    let files = restored.files.persistence_snapshot(true).unwrap();
    let mut update = ScalarUpdate::from_record(&record);
    update.downloaded_bytes = restored.files.selected_downloaded();
    store
        .persist_acknowledged(&update, PersistenceIntent::SELECTION, Some(&files))
        .unwrap();
    let after = store.load_torrent(&record.id).unwrap().unwrap();
    assert_eq!(
        after
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["file.bin", "folder/b.bin"]
    );
    assert!(after.files[1].selected);
    assert_eq!(after.files[1].downloaded_bytes, 60);
    assert_eq!(after.downloaded_bytes, 110);
    assert_eq!(after.verified_pieces, record.verified_pieces);
}

#[test]
fn file_pages_have_bounded_bytes_and_rows_and_reject_corrupt_index_gaps() {
    let mut store = StateStore::in_memory().unwrap();
    let mut record = fixture();
    record.files = (0..300)
        .map(|index| StoredFile {
            index,
            path: format!("folder/{}-{index}", "p".repeat(1000)),
            size_bytes: 100,
            downloaded_bytes: 0,
            selected: true,
            priority: 0,
        })
        .collect();
    store.save_torrent(&record).unwrap();
    let mut offset = 0;
    let mut all = Vec::new();
    loop {
        let page = store.load_file_page(&record.id, offset, 128).unwrap();
        assert_eq!(page.total, 300);
        assert!(page.files.len() <= 128);
        assert!(page.files.iter().map(|file| file.path.len()).sum::<usize>() <= 64 * 1024);
        all.extend(page.files);
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(all, record.files);
    assert!(store.load_file_page(&record.id, 301, 128).is_err());
    assert!(store.load_file_page(&record.id, 0, 129).is_err());
    assert!(store.load_file_page("missing", 0, 128).is_err());
    store
        .connection
        .execute(
            "DELETE FROM files WHERE torrent_id=?1 AND file_index=1",
            [&record.id],
        )
        .unwrap();
    assert!(store.load_file_page(&record.id, 0, 128).is_err());
    assert!(store.load_deferred_torrents().is_err());
}

#[test]
fn background_loader_delivers_latest_generation_and_explicit_missing_record_error() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("state.sqlite3");
    let mut store = StateStore::open(&database).unwrap();
    let record = fixture();
    store.save_torrent(&record).unwrap();
    let loader = FileDetailLoader::new(database);
    let (sender, receiver) = std::sync::mpsc::channel();
    loader.set_waker(std::sync::Arc::new(move || {
        let _ = sender.send(());
    }));
    loader.request("superseded".into(), 1);
    loader.request(record.id.clone(), 2);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let reply = loop {
        if let Some(reply) = loader.poll() {
            break reply;
        }
        assert!(std::time::Instant::now() < deadline, "loader did not wake");
        let _ = receiver.recv_timeout(std::time::Duration::from_millis(100));
    };
    assert_eq!((reply.id, reply.generation), (record.id.clone(), 2));
    assert_eq!(reply.result.unwrap(), vec!["file.bin"]);
    loader.request("missing".into(), 3);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(reply) = loader.poll() {
            assert_eq!(reply.generation, 3);
            assert!(reply.result.is_err());
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        let _ = receiver.recv_timeout(std::time::Duration::from_millis(100));
    }
    loader.cancel();
    assert!(!loader.is_loading());
}

#[test]
fn streaming_compact_constructor_propagates_read_errors_without_partial_success() {
    let states = [
        Ok(FileState {
            size_bytes: 100,
            downloaded_bytes: 50,
            selected: true,
            high_priority: false,
        }),
        Err("synthetic read error"),
    ];
    assert_eq!(
        FileSelectionModel::try_from_compact_states(states).unwrap_err(),
        "synthetic read error"
    );
}

/// Optimized-only instrumentation: authorizations count SQL preparation access
/// requests, PROFILE counts executed statements/VM work, never physical disk I/O.
#[test]
#[ignore = "synthetic 100k-file durable scalar ACK measurement; run explicitly with --nocapture"]
fn measure_scalar_ack_100k() {
    use rusqlite::ffi;
    use std::{
        ffi::{CStr, c_char, c_int, c_uint, c_void},
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    #[derive(Default)]
    struct Counters {
        statements: AtomicU64,
        vm_steps: AtomicU64,
        fullscan_steps: AtomicU64,
        child_read_authorizations: AtomicU64,
        child_write_authorizations: AtomicU64,
    }
    unsafe extern "C" fn authorize(
        context: *mut c_void,
        action: c_int,
        table: *const c_char,
        _: *const c_char,
        _: *const c_char,
        _: *const c_char,
    ) -> c_int {
        if !context.is_null() && !table.is_null() {
            // SAFETY: SQLite supplies a NUL-terminated name for this callback;
            // Probe owns Counters until both callbacks have been unregistered.
            let (counters, table) = unsafe {
                (
                    &*context.cast::<Counters>(),
                    CStr::from_ptr(table).to_bytes(),
                )
            };
            if matches!(
                table,
                b"files"
                    | b"verified_pieces"
                    | b"trackers"
                    | b"tracker_audit_profiles"
                    | b"tracker_audit_steps"
            ) {
                if action == ffi::SQLITE_READ {
                    counters
                        .child_read_authorizations
                        .fetch_add(1, Ordering::Relaxed);
                }
                if matches!(
                    action,
                    ffi::SQLITE_INSERT | ffi::SQLITE_UPDATE | ffi::SQLITE_DELETE
                ) {
                    counters
                        .child_write_authorizations
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        ffi::SQLITE_OK
    }
    unsafe extern "C" fn trace(
        event: c_uint,
        context: *mut c_void,
        statement: *mut c_void,
        _: *mut c_void,
    ) -> c_int {
        if event == ffi::SQLITE_TRACE_PROFILE && !context.is_null() && !statement.is_null() {
            // SAFETY: PROFILE supplies the live sqlite3_stmt. Reading/resetting
            // diagnostic counters issues no recursive SQL or connection mutation.
            let (counters, steps, scans) = unsafe {
                (
                    &*context.cast::<Counters>(),
                    ffi::sqlite3_stmt_status(statement.cast(), ffi::SQLITE_STMTSTATUS_VM_STEP, 1),
                    ffi::sqlite3_stmt_status(
                        statement.cast(),
                        ffi::SQLITE_STMTSTATUS_FULLSCAN_STEP,
                        1,
                    ),
                )
            };
            counters.statements.fetch_add(1, Ordering::Relaxed);
            counters
                .vm_steps
                .fetch_add(steps.max(0) as u64, Ordering::Relaxed);
            counters
                .fullscan_steps
                .fetch_add(scans.max(0) as u64, Ordering::Relaxed);
        }
        0
    }
    struct Probe {
        database: *mut ffi::sqlite3,
        counters: Box<Counters>,
    }
    impl Probe {
        fn new(connection: &Connection) -> Self {
            // SAFETY: the test keeps this Connection alive and uses it only on
            // this thread; Probe is dropped before StateStore even on unwind.
            let database = unsafe { connection.handle() };
            let mut probe = Self {
                database,
                counters: Box::default(),
            };
            let context = (&mut *probe.counters as *mut Counters).cast();
            // SAFETY: callback signatures match SQLite's ABI; the stable boxed
            // context and live database are retained until Probe unregisters both.
            unsafe {
                assert_eq!(
                    ffi::sqlite3_set_authorizer(database, Some(authorize), context),
                    ffi::SQLITE_OK
                );
                assert_eq!(
                    ffi::sqlite3_trace_v2(
                        database,
                        ffi::SQLITE_TRACE_PROFILE,
                        Some(trace),
                        context
                    ),
                    ffi::SQLITE_OK
                );
            }
            probe
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            // SAFETY: Connection outlives Probe; callbacks lose the stable boxed
            // context before that context is freed. No callback captures SQL text.
            unsafe {
                ffi::sqlite3_set_authorizer(self.database, None, std::ptr::null_mut());
                ffi::sqlite3_trace_v2(self.database, 0, None, std::ptr::null_mut());
            }
        }
    }

    let temporary = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(&temporary.path().join("state.sqlite3")).unwrap();
    let mut record = fixture();
    record.files = (0..100_000)
        .map(|index| StoredFile {
            index,
            path: format!("folder-{}/{index:06}.bin", index / 1000),
            size_bytes: 100,
            downloaded_bytes: 50,
            selected: true,
            priority: 0,
        })
        .collect();
    record.downloaded_bytes = 5_000_000;
    let setup = Instant::now();
    store.save_torrent(&record).unwrap();
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.;
    for table in [
        "files",
        "verified_pieces",
        "trackers",
        "tracker_audit_profiles",
        "tracker_audit_steps",
    ] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            // These fixed internal names cannot contain user SQL. Triggers give
            // separate preservation evidence, not a substitute for read counts.
            store.connection.execute_batch(&format!("CREATE TRIGGER preserve_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT,'scalar ACK touched canonical child'); END;")).unwrap();
        }
    }
    let synchronous: i64 = store
        .connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .unwrap();
    assert_eq!(
        synchronous, 2,
        "measurement must retain SQLite FULL durability"
    );
    let before_changes = store.connection.total_changes();
    let probe = Probe::new(&store.connection);
    let mut update = ScalarUpdate::from_record(&record);
    let mut samples_ms = Vec::new();
    let measured = Instant::now();
    for index in 1..=20 {
        update.state = if index % 2 == 0 {
            "paused"
        } else {
            "downloading"
        }
        .into();
        update.uploaded_bytes = index;
        update.resume.uploaded = index;
        let started = Instant::now();
        store
            .persist_acknowledged(&update, PersistenceIntent::SCALAR, None)
            .unwrap();
        samples_ms.push(started.elapsed().as_secs_f64() * 1000.);
    }
    let elapsed_ms = measured.elapsed().as_secs_f64() * 1000.;
    let total_changes = store.connection.total_changes() - before_changes;
    let statements = probe.counters.statements.load(Ordering::Relaxed);
    let vm_steps = probe.counters.vm_steps.load(Ordering::Relaxed);
    let fullscan_steps = probe.counters.fullscan_steps.load(Ordering::Relaxed);
    let child_reads = probe
        .counters
        .child_read_authorizations
        .load(Ordering::Relaxed);
    let child_writes = probe
        .counters
        .child_write_authorizations
        .load(Ordering::Relaxed);
    drop(probe);
    let count: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM files WHERE torrent_id=?1",
            [&record.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 100_000);
    let stored: (String, i64) = store
        .connection
        .query_row(
            "SELECT state,uploaded_bytes FROM torrents WHERE id=?1",
            [&record.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, ("paused".into(), 20));
    println!(
        "{}",
        serde_json::json!({
            "kind":"opt4_scalar_ack_100k", "schema":1, "comparison":"optimized_only_no_baseline", "debug_assertions":cfg!(debug_assertions),
            "files":100_000,"acknowledgements":20,"sqlite_synchronous":"FULL","setup_ms_excluded":setup_ms,
            "elapsed_ms":elapsed_ms,"samples_ms":samples_ms,"sqlite_total_changes_delta":total_changes,
            "profiled_statements":statements,"vm_steps":vm_steps,"fullscan_steps":fullscan_steps,
            "canonical_child_read_authorizations":child_reads,"canonical_child_write_authorizations":child_writes,
            "canonical_child_write_guards":"15 aborting triggers; all acknowledgements succeeded",
            "limits":"SQL authorizations count preparation access requests, not rows or physical reads; PROFILE reports SQLite VM counters. Instrumentation overhead is included. Setup, guard creation and post-verification excluded. No matching baseline or speedup claim."
        })
    );
    assert!(statements > 0 && vm_steps > 0);
    assert_eq!((child_reads, child_writes), (0, 0));
}
