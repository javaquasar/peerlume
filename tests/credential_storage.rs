use peerlume::{
    secrets::{MemorySecrets, PlatformSecrets, SecretUnavailable, TrackerSecrets},
    state::StateStore,
};
use std::sync::Arc;
fn record() -> peerlume::state::TorrentRecord {
    let item = peerlume::torrent::demo_torrents().remove(0);
    let mut record = peerlume::client::torrent_to_record(&item, None);
    record.trackers = vec!["https://tracker.test/announce?passkey=SYNTHETIC_CREDENTIAL".into()];
    record
}
struct Locked;
impl TrackerSecrets for Locked {
    fn protect(&self, _: &str) -> Result<Vec<u8>, SecretUnavailable> {
        Err(SecretUnavailable)
    }
    fn restore(&self, _: &[u8]) -> Result<String, SecretUnavailable> {
        Err(SecretUnavailable)
    }
}
#[test]
fn injected_credentials_survive_database_restart_without_plaintext_in_sqlite() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.sqlite3");
    let provider = Arc::new(MemorySecrets::default());
    let record = record();
    let mut store = StateStore::open_with_secrets(&path, provider.clone()).unwrap();
    store.save_torrent(&record).unwrap();
    drop(store);
    let store = StateStore::open_with_secrets(&path, provider).unwrap();
    assert_eq!(
        store.load_torrent(&record.id).unwrap().unwrap().trackers,
        record.trackers
    );
    drop(store);
    for file in std::fs::read_dir(root.path()).unwrap() {
        let file = file.unwrap();
        if file.file_type().unwrap().is_file() {
            assert!(
                !std::fs::read(file.path())
                    .unwrap()
                    .windows(b"SYNTHETIC_CREDENTIAL".len())
                    .any(|b| b == b"SYNTHETIC_CREDENTIAL")
            );
        }
    }
}
#[test]
fn unavailable_store_rolls_back_and_errors_never_include_credentials() {
    let mut store = StateStore::in_memory_with_secrets(Arc::new(Locked)).unwrap();
    let error = store.save_torrent(&record()).unwrap_err();
    assert!(!error.to_string().contains("SYNTHETIC_CREDENTIAL"));
    assert!(store.load_all_torrents().unwrap().is_empty());
}
#[test]
fn missing_secret_is_an_explicit_load_error_not_a_redacted_tracker() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.sqlite3");
    let mut store =
        StateStore::open_with_secrets(&path, Arc::new(MemorySecrets::default())).unwrap();
    let record = record();
    store.save_torrent(&record).unwrap();
    drop(store);
    let store = StateStore::open_with_secrets(&path, Arc::new(Locked)).unwrap();
    assert!(store.load_torrent(&record.id).is_err());
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires isolated unlocked Secret Service; Reliability workflow supplies it"]
fn real_linux_secret_service_round_trip() {
    let reference = PlatformSecrets
        .protect("https://tracker.test/announce?passkey=CI_SYNTHETIC_ONLY")
        .unwrap();
    assert_eq!(
        PlatformSecrets.restore(&reference).unwrap(),
        "https://tracker.test/announce?passkey=CI_SYNTHETIC_ONLY"
    );
    let key = std::str::from_utf8(&reference).unwrap();
    keyring::Entry::new("PeerLume tracker credentials", key)
        .unwrap()
        .delete_credential()
        .unwrap();
    assert_eq!(PlatformSecrets.restore(&reference), Err(SecretUnavailable));
}
#[cfg(windows)]
#[test]
fn real_windows_dpapi_round_trip_and_corruption_fail_closed() {
    let reference = PlatformSecrets
        .protect("https://tracker.test/announce?passkey=CI_SYNTHETIC_ONLY")
        .unwrap();
    assert_eq!(
        PlatformSecrets.restore(&reference).unwrap(),
        "https://tracker.test/announce?passkey=CI_SYNTHETIC_ONLY"
    );
    let mut damaged = reference;
    damaged[0] ^= 1;
    assert_eq!(PlatformSecrets.restore(&damaged), Err(SecretUnavailable));
}
