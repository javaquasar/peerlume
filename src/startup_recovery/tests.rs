use super::*;

#[test]
fn missing_profile_and_record_are_read_only() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    assert_eq!(read_failure(&missing).unwrap(), None);
    assert!(!missing.exists());
    assert_eq!(read_failure(root.path()).unwrap(), None);
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
fn only_same_profile_lock_holder_can_replace_or_clear_a_typed_record() {
    let root = tempfile::tempdir().unwrap();
    let owner = StartupRecovery::acquire(root.path()).unwrap();
    owner
        .record_failure(StartupFailureCode::InvalidSettings)
        .unwrap();
    let initial = read_failure(root.path()).unwrap().unwrap();
    assert_eq!(initial.code, StartupFailureCode::InvalidSettings);
    assert_eq!(initial.native_protocol, PROTOCOL_VERSION);
    assert!(initial.recorded_unix_ms > 0);
    assert!(
        matches!(StartupRecovery::acquire(root.path()), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(read_failure(root.path()).unwrap(), Some(initial.clone()));
    owner
        .record_failure(StartupFailureCode::InvalidSettings)
        .unwrap();
    let next = read_failure(root.path()).unwrap().unwrap();
    assert_eq!(next.code, initial.code);
    assert_ne!(next, initial, "repeated failures must carry fresh evidence");
    owner.clear_failure().unwrap();
    assert_eq!(read_failure(root.path()).unwrap(), None);
    assert!(
        matches!(StartupRecovery::acquire(root.path()), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    drop(owner);
    assert!(StartupRecovery::acquire(root.path()).is_ok());
}

#[test]
fn malformed_oversized_and_future_schema_records_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let owner = StartupRecovery::acquire(root.path()).unwrap();
    owner
        .record_failure(StartupFailureCode::StateUnavailable)
        .unwrap();
    let path = root.path().join(FILE_NAME);
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (key, value) in [
        ("schema", serde_json::json!(255)),
        ("code", serde_json::json!("unexpected_error")),
        ("native_protocol", serde_json::json!(0)),
        ("recorded_unix_ms", serde_json::json!(0)),
        ("attempt", serde_json::json!("arbitrary path")),
        ("error_text", serde_json::json!("untrusted raw error")),
    ] {
        let mut changed = original.clone();
        changed[key] = value;
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(
            read_failure(root.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
            "{key}"
        );
    }
    fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
    assert_eq!(
        read_failure(root.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn a_real_credential_conversion_is_distinct_from_other_sqlite_errors() {
    let credential = rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Blob,
        Box::new(crate::secrets::SecretUnavailable),
    );
    assert_eq!(
        StartupFailureCode::from_state_error(&credential),
        StartupFailureCode::CredentialStoreUnavailable
    );
    assert_eq!(
        StartupFailureCode::from_state_error(&rusqlite::Error::InvalidQuery),
        StartupFailureCode::StateUnavailable
    );
}

#[cfg(unix)]
#[test]
fn refuses_links_and_shared_permission_without_touching_the_target() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let owner = StartupRecovery::acquire(root.path()).unwrap();
    owner
        .record_failure(StartupFailureCode::SettingsUnavailable)
        .unwrap();
    let path = root.path().join(FILE_NAME);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        read_failure(root.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let other = tempfile::NamedTempFile::new().unwrap();
    fs::remove_file(&path).unwrap();
    symlink(other.path(), &path).unwrap();
    assert_eq!(
        read_failure(root.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        owner
            .record_failure(StartupFailureCode::StateUnavailable)
            .is_err()
    );
    assert!(owner.clear_failure().is_err());
    assert!(fs::read(other.path()).unwrap().is_empty());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
