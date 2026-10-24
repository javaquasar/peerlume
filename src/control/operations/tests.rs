use super::*;
fn store() -> ControlStore {
    ControlStore::open(std::path::Path::new(":memory:")).unwrap()
}
fn create(store: &ControlStore, key: &str) -> Operation {
    store
        .create(
            ("client", "torrent", "pause"),
            key,
            "fingerprint",
            now_ms() + 60_000,
            false,
        )
        .unwrap()
}

#[test]
fn newly_created_operation_is_queued_and_has_unique_identity() {
    let store = store();
    let before = now_ms();
    let a = create(&store, "a");
    let b = create(&store, "b");
    assert_ne!(a.id, b.id);
    assert_eq!(a.state, OperationState::Queued);
    assert!(a.created_unix_ms >= before);
    assert!(a.error.is_none());
    assert!(!a.approval_required);
}
#[test]
fn lookup_is_bound_to_client_as_well_as_key() {
    let store = store();
    let a = create(&store, "shared");
    let b = store
        .create(
            ("other", "torrent", "pause"),
            "shared",
            "fingerprint",
            now_ms() + 60_000,
            false,
        )
        .unwrap();
    assert_eq!(store.lookup("client", "shared").unwrap().unwrap().id, a.id);
    assert_eq!(store.lookup("other", "shared").unwrap().unwrap().id, b.id);
    assert!(store.lookup("missing", "shared").unwrap().is_none());
}
#[test]
fn duplicate_client_key_is_rejected_without_inserting_an_audit_row() {
    let store = store();
    create(&store, "same");
    let before: i64 = store
        .connection
        .query_row("SELECT count(*) FROM agent_audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        store
            .create(
                ("client", "torrent", "resume"),
                "same",
                "changed",
                now_ms() + 60_000,
                false
            )
            .err(),
        Some(ErrorCode::IdempotencyConflict)
    );
    assert_eq!(store.list().unwrap().len(), 1);
    let after: i64 = store
        .connection
        .query_row("SELECT count(*) FROM agent_audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}
#[test]
fn operation_and_audit_updates_roll_back_together_on_failure() {
    let store = store();
    let mut op = create(&store, "key");
    store.connection.execute_batch("CREATE TEMP TRIGGER refuse_audit BEFORE INSERT ON agent_audit BEGIN SELECT RAISE(ABORT,'synthetic write failure'); END;").unwrap();
    op.state = OperationState::Succeeded;
    assert_eq!(store.save(&op), Err(ErrorCode::PersistenceUnavailable));
    assert_eq!(
        store.lookup("client", "key").unwrap().unwrap().state,
        OperationState::Queued
    );
    store
        .connection
        .execute_batch("DROP TRIGGER refuse_audit;")
        .unwrap();
    store.save(&op).unwrap();
    assert_eq!(
        store.lookup("client", "key").unwrap().unwrap().state,
        OperationState::Succeeded
    );
}
#[test]
fn corrupted_operation_json_is_a_typed_failure_in_list_and_lookup() {
    let store = store();
    let op = create(&store, "key");
    store
        .connection
        .execute(
            "UPDATE agent_operations SET record='not JSON' WHERE id=?",
            [op.id],
        )
        .unwrap();
    assert_eq!(store.list().err(), Some(ErrorCode::PersistenceUnavailable));
    assert_eq!(
        store.lookup("client", "key").err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
}
#[test]
fn corrupt_sqlite_file_does_not_fall_back_to_an_empty_database() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bad.sqlite3");
    std::fs::write(&path, b"not a database").unwrap();
    assert_eq!(
        ControlStore::open(&path).err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
    assert_eq!(std::fs::read(path).unwrap(), b"not a database");
}
#[test]
fn missing_parent_and_directory_as_database_are_typed_failures() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        ControlStore::open(&root.path().join("missing/db")).err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
    assert_eq!(
        ControlStore::open(root.path()).err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
}
#[test]
fn restart_reconciles_queued_and_running_but_preserves_all_terminal_states() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store.sqlite3");
    let store = ControlStore::open(&path).unwrap();
    let states = [
        OperationState::Queued,
        OperationState::Running,
        OperationState::Succeeded,
        OperationState::Failed,
        OperationState::Cancelled,
        OperationState::ReconciliationRequired,
    ];
    for (index, state) in states.iter().enumerate() {
        let mut op = create(&store, &index.to_string());
        op.state = *state;
        op.error = Some(ErrorCode::EngineRejected);
        store.save(&op).unwrap();
    }
    drop(store);
    let reopened = ControlStore::open(&path).unwrap();
    for (index, state) in states.iter().enumerate() {
        let op = reopened
            .lookup("client", &index.to_string())
            .unwrap()
            .unwrap();
        if matches!(state, OperationState::Queued | OperationState::Running) {
            assert_eq!(op.state, OperationState::ReconciliationRequired);
            assert_eq!(op.error, Some(ErrorCode::ReconciliationRequired));
        } else {
            assert_eq!(op.state, *state);
            assert_eq!(op.error, Some(ErrorCode::EngineRejected));
        }
    }
}
#[test]
fn repeated_open_does_not_duplicate_operation_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store.sqlite3");
    {
        let store = ControlStore::open(&path).unwrap();
        create(&store, "key");
    }
    for _ in 0..3 {
        let store = ControlStore::open(&path).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
#[test]
fn audit_retention_discards_oldest_records_not_latest_decisions() {
    let store = store();
    let tx = store.connection.unchecked_transaction().unwrap();
    for index in 0..4100 {
        tx.execute("INSERT INTO agent_audit(stamp,client,request,decision) VALUES(1,'client',?,'decision')",[index.to_string()]).unwrap();
    }
    tx.commit().unwrap();
    store.audit("client", "last", "denied").unwrap();
    let (count,last):(i64,String)=store.connection.query_row("SELECT count(*),(SELECT request FROM agent_audit ORDER BY sequence DESC LIMIT 1) FROM agent_audit",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(count, 4096);
    assert_eq!(last, "last");
}
#[test]
fn over_capacity_records_are_not_silently_truncated_on_reopen() {
    let store = store();
    let sample = create(&store, "key");
    let value = serde_json::to_string(&sample).unwrap();
    let tx = store.connection.unchecked_transaction().unwrap();
    for index in 0..MAX_OPERATIONS {
        tx.execute(
            "INSERT INTO agent_operations(id,client,key,record) VALUES(?,'extra',?,?)",
            params![format!("id-{index}"), index.to_string(), value],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(store.list().err(), Some(ErrorCode::PersistenceUnavailable));
}
#[test]
fn dropped_table_reports_failure_instead_of_an_unknown_operation() {
    let store = store();
    store
        .connection
        .execute_batch("DROP TABLE agent_operations;")
        .unwrap();
    assert_eq!(store.list().err(), Some(ErrorCode::PersistenceUnavailable));
    assert_eq!(
        store.lookup("client", "key").err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
}

#[test]
fn sequential_work_exceeds_hot_capacity_without_losing_retry_results() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("operations.sqlite3");
    let store = ControlStore::open(&path).unwrap();
    let mut first = String::new();
    for index in 0..MAX_OPERATIONS * 3 {
        let mut op = create(&store, &format!("sequential-{index}"));
        if index == 0 {
            first.clone_from(&op.id);
        }
        op.state = OperationState::Succeeded;
        store.save(&op).unwrap();
    }
    assert!(store.list().unwrap().len() <= MAX_OPERATIONS);
    assert_eq!(store.page("client", 0, 128).unwrap().1, MAX_OPERATIONS * 3);
    assert_eq!(
        store.operation("client", &first).unwrap().unwrap().state,
        OperationState::Succeeded
    );
    assert!(store.operation("another", &first).unwrap().is_none());
    assert!(store.lookup("another", "sequential-0").unwrap().is_none());
    let old = store.lookup("client", "sequential-0").unwrap().unwrap();
    assert_eq!(store.save(&old).err(), Some(ErrorCode::Conflict));
    assert_eq!(
        store
            .create(
                ("client", "torrent", "pause"),
                "sequential-0",
                "new",
                now_ms() + 1000,
                false
            )
            .err(),
        Some(ErrorCode::IdempotencyConflict)
    );
    drop(store);
    let store = ControlStore::open(&path).unwrap();
    assert_eq!(
        store.lookup("client", "sequential-0").unwrap().unwrap().id,
        first
    );
    let mut ids = std::collections::BTreeSet::new();
    for offset in (0..MAX_OPERATIONS * 3).step_by(128) {
        for op in store.page("client", offset, 128).unwrap().0 {
            assert!(ids.insert(op.id));
        }
    }
    assert_eq!(ids.len(), MAX_OPERATIONS * 3);
}

#[test]
fn archive_transaction_rolls_back_if_source_deletion_fails() {
    let store = store();
    for index in 0..MAX_OPERATIONS {
        let mut op = create(&store, &format!("terminal-{index}"));
        op.state = OperationState::Succeeded;
        store.save(&op).unwrap();
    }
    store.connection.execute_batch("CREATE TEMP TRIGGER reject_archive BEFORE DELETE ON agent_operations BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert_eq!(
        store
            .create(
                ("client", "torrent", "pause"),
                "next",
                "hash",
                now_ms() + 1000,
                false
            )
            .err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
    assert_eq!(store.list().unwrap().len(), MAX_OPERATIONS);
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM agent_operation_archive", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn unresolved_operations_are_never_archived_to_make_room() {
    let store = store();
    for index in 0..MAX_OPERATIONS {
        let mut op = create(&store, &format!("uncertain-{index}"));
        op.state = OperationState::ReconciliationRequired;
        store.save(&op).unwrap();
    }
    assert_eq!(
        store
            .create(
                ("client", "torrent", "pause"),
                "next",
                "hash",
                now_ms() + 1000,
                false
            )
            .err(),
        Some(ErrorCode::Busy)
    );
    assert_eq!(store.list().unwrap().len(), MAX_OPERATIONS);
}
