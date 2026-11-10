use super::*;
use sha1::{Digest, Sha1};

fn info(file_count: usize, private: bool) -> Vec<u8> {
    let mut bytes = b"d5:filesl".to_vec();
    for index in 0..file_count {
        let name = format!("file-{index}.bin");
        bytes.extend(format!("d6:lengthi1e4:pathl{}:{name}ee", name.len()).as_bytes());
    }
    bytes.extend(b"e4:name7:payload12:piece lengthi16384e6:pieces20:");
    bytes.extend_from_slice(&Sha1::digest(vec![42; file_count]));
    if private {
        bytes.extend(b"7:privatei1e");
    }
    bytes.push(b'e');
    bytes
}

fn uri(raw: &[u8]) -> String {
    format!("magnet:?xt=urn:btih:{:x}", Sha1::digest(raw))
}

#[test]
fn magnet_review_binds_the_raw_hash_and_never_caches_tracker_credentials() {
    let raw = info(2, true);
    let magnet = format!(
        "{}&tr=https%3A%2F%2Fexample.invalid%2Fsecret-passkey",
        uri(&raw)
    );
    let resolved = Resolved::parse(&raw, Magnet::parse(&magnet).unwrap()).unwrap();
    assert!(resolved.metadata.is_private);
    assert_eq!(
        resolved.hashes,
        vec![format!("v1: {:x}", Sha1::digest(&raw))]
    );
    assert_eq!(
        resolved.trackers,
        vec!["https://example.invalid/secret-passkey"]
    );
    assert!(
        !resolved
            .bytes
            .windows(b"secret-passkey".len())
            .any(|part| part == b"secret-passkey")
    );
    assert!(
        Resolved::parse(&raw, Magnet::parse(&uri(&raw)).unwrap())
            .err()
            .unwrap()
            .contains("explicit tracker")
    );
    assert!(
        Resolved::parse(&info(1, true), Magnet::parse(&magnet).unwrap())
            .err()
            .unwrap()
            .contains("match")
    );
    let malformed = b"d4:name2:..e";
    assert!(Resolved::parse(malformed, Magnet::parse(&uri(malformed)).unwrap()).is_err());
}

#[test]
fn magnet_review_rejects_unusable_routes_and_ignores_untrusted_selection_ranges() {
    let raw = info(1, false);
    let magnet = uri(&raw);
    assert!(Magnet::parse(&format!("{magnet}&so=0-18446744073709551615")).is_ok());
    assert!(Magnet::parse(&format!("{magnet}&tr=file:///private")).is_err());
    assert!(Magnet::parse(&format!("{magnet}&xt=urn:btih:{}", "a".repeat(40))).is_err());
    assert!(Magnet::parse(&format!("magnet:?xt=urn:btmh:1220{}", "a".repeat(64))).is_err());
    assert!(matches!(
        Magnet::parse(&format!("{magnet}&dn={}", "a".repeat(MAX_URI))),
        Err(Error::TooLarge)
    ));
}

#[test]
fn magnet_review_pages_are_client_bound_and_cancel_discards_the_ready_result() {
    let raw = info(201, false);
    let request = Magnet::parse(&uri(&raw)).unwrap();
    let hash = request.v1;
    let data = Arc::new(Resolved::parse(&raw, request).unwrap());
    let reviews = Arc::new(Reviews::default());
    reviews.entries.lock().unwrap().insert(
        "review".into(),
        Entry {
            client: "front".into(),
            hash,
            state: State::Ready(data),
            since: Instant::now(),
            confirming: false,
        },
    );
    let first = reviews.page("front", "review", 0).unwrap();
    assert_eq!(first.files.len(), MAX_PAGE_ROWS);
    assert_eq!(first.total_files, 201);
    let second = reviews
        .page("front", "review", first.next_offset.unwrap())
        .unwrap();
    assert_eq!(second.hashes, first.hashes);
    assert_eq!(second.files.len(), 201 - MAX_PAGE_ROWS);
    assert!(second.next_offset.is_none());
    assert!(matches!(
        reviews.page("other", "review", 0),
        Err(Error::Stale)
    ));
    assert!(matches!(
        reviews.confirm("other", "review"),
        Err(Error::Stale)
    ));
    let guard = reviews.confirm("front", "review").unwrap();
    assert!(matches!(
        reviews.cancel("front", "review"),
        Err(Error::Busy)
    ));
    drop(guard);
    reviews.cancel("front", "review").unwrap();
    Reviews::finish(
        Arc::downgrade(&reviews),
        "review",
        Ok(Resolved::parse(&raw, Magnet::parse(&uri(&raw)).unwrap()).unwrap()),
    );
    assert_eq!(
        reviews.page("front", "review", 0).unwrap().state,
        MagnetReviewState::Cancelled
    );
    assert!(matches!(
        reviews.confirm("front", "review"),
        Err(Error::Stale)
    ));
}

#[test]
fn magnet_review_retains_authoritative_source_when_failed_admission_cannot_roll_back() {
    use crate::{client::ClientBackend, state::StateStore};

    let profile = tempfile::tempdir().unwrap();
    let mut settings = ClientSettings::default();
    settings.download.download_directory = profile.path().join("payloads").display().to_string();
    let database = profile.path().join("state.sqlite3");
    let backend = ClientBackend::with_state_store(StateStore::open(&database).unwrap());
    let mut owner = Controller::new(backend, vec![]);
    let sql = rusqlite::Connection::open(database).unwrap();
    sql.execute_batch(
        "CREATE TRIGGER reject_magnet_rollback BEFORE DELETE ON torrents
        BEGIN SELECT RAISE(ABORT,'synthetic rollback failure'); END;",
    )
    .unwrap();
    let prepare = |file_count| {
        let raw = info(file_count, false);
        let resolved = Resolved::parse(&raw, Magnet::parse(&uri(&raw)).unwrap()).unwrap();
        let request = NewTorrent {
            source: "untrusted source path".into(),
            hashes: resolved.hashes.clone(),
            trackers: vec![],
            peer_id: crate::client::tests::torrent().peer_id,
            overrides: Default::default(),
            selected: PackedIndices::new(file_count, &[0]).unwrap(),
            high: PackedIndices::new(file_count, &[]).unwrap(),
            start: false,
            audit: None,
        };
        prepare_confirmation(profile.path(), &settings, &resolved, request).unwrap()
    };
    let prepared = prepare(2);
    let retained_source = prepared.item.source.clone();
    assert!(matches!(
        prepared.admit(&mut owner, &settings),
        Err(Error::Transport(_))
    ));
    assert!(owner.torrents.is_empty());
    let record = owner.backend.load_torrents().unwrap().pop().unwrap();
    assert_eq!(record.source, retained_source);
    assert!(std::path::Path::new(&retained_source).is_file());

    sql.execute_batch("DROP TRIGGER reject_magnet_rollback")
        .unwrap();
    let prepared = prepare(3);
    let discarded_source = prepared.item.source.clone();
    assert!(matches!(
        prepared.admit(&mut owner, &settings),
        Err(Error::Failed(_))
    ));
    assert!(!std::path::Path::new(&discarded_source).exists());
    assert_eq!(owner.backend.load_torrents().unwrap().len(), 1);
}
