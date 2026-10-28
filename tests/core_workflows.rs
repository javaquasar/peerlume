use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use peerlume::{
    metainfo::TorrentMetainfo,
    piece_map::{FileSpan, map_selected_files_to_pieces},
    state::{
        StateStore, StoredFile, StoredTrackerAuditProfile, StoredTrackerAuditStep, TorrentRecord,
    },
};

#[test]
fn torrent_preview_exposes_private_flag_trackers_and_file_tree() {
    let bytes = b"d8:announce24:https://tracker/announce4:infod5:filesld6:lengthi12e4:pathl5:a.txteed6:lengthi34e4:pathl3:dir5:b.bineee4:name4:demo7:privatei1eee";
    let parsed = TorrentMetainfo::parse(bytes).expect("parse synthetic metainfo");

    assert!(parsed.is_private);
    assert_eq!(parsed.trackers, ["https://tracker/announce"]);
    assert_eq!(parsed.files.len(), 2);
    assert_eq!(parsed.files[0].path, "a.txt");
    assert_eq!(parsed.files[1].path, "dir/b.bin");
}

#[test]
fn selected_file_boundaries_produce_required_piece_plan() {
    let files = [
        FileSpan {
            file_index: 0,
            offset: 0,
            length: 6,
            selected: false,
            padding: false,
        },
        FileSpan {
            file_index: 1,
            offset: 6,
            length: 8,
            selected: true,
            padding: false,
        },
    ];

    let plan = map_selected_files_to_pieces(&files, 10).expect("build piece plan");

    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].selected_bytes, 4);
    assert_eq!(plan[1].selected_bytes, 4);
    assert!(plan.iter().all(|piece| piece.required));
}

#[test]
fn sqlite_file_round_trip_restores_resume_state_and_protects_tracker_secrets() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("peerlume-state-integration-{nonce}"));
    let database = root.join("state.sqlite3");
    let secrets = std::sync::Arc::new(peerlume::secrets::MemorySecrets::default());
    let mut store =
        StateStore::open_with_secrets(&database, secrets.clone()).expect("open SQLite state");
    let record = TorrentRecord {
        transfer_settings: None,
        repair_policy: Default::default(),
        verify_after_download: false,
        download_mode: peerlume::download_mode::DownloadMode::UploadRatio15,
        freeze_after_bytes: 12345,
        upload_ratio: peerlume::download_mode::UploadRatioRange {
            min_milli: 1750,
            max_milli: 2750,
        },
        id: "v1:integration".into(),
        name: "integration".into(),
        source: "fixture.torrent".into(),
        info_hash_v1: Some("integration".into()),
        info_hash_v2: None,
        is_private: true,
        state: "paused".into(),
        queue_position: 1,
        download_directory: Some("downloads/integration".into()),
        trackers: vec![
            "https://tracker.test/0123456789abcdef01234567/announce?token=secret".into(),
        ],
        files: vec![StoredFile {
            index: 0,
            path: "dir/file.bin".into(),
            size_bytes: 100,
            downloaded_bytes: 50,
            selected: true,
            priority: 1,
        }],
        verified_pieces: vec![0, 2],
        downloaded_bytes: 50,
        uploaded_bytes: 5,
        tracker_audit: Some(StoredTrackerAuditProfile {
            allowed_host: "staging.tracker.test".into(),
            port: 51_413,
            started_only: true,
            observation_seconds: 14_400,
            steps: vec![StoredTrackerAuditStep {
                delay_seconds: 15,
                uploaded: 5,
                downloaded: 50,
                left: 50,
                event: Some("started".into()),
            }],
        }),
    };
    store.save_torrent(&record).expect("save torrent state");
    drop(store);

    let store = StateStore::open_with_secrets(&database, secrets).expect("reopen SQLite state");
    let restored = store
        .load_torrent(&record.id)
        .expect("load torrent state")
        .expect("saved torrent exists");
    assert_eq!(restored.files, record.files);
    assert_eq!(restored.verified_pieces, record.verified_pieces);
    assert_eq!(restored.downloaded_bytes, 50);
    assert_eq!(restored.uploaded_bytes, 5);
    assert_eq!(restored.tracker_audit, record.tracker_audit);
    assert_eq!(restored.trackers, record.trackers);

    drop(store);
    fs::remove_dir_all(root).expect("remove temporary SQLite directory");
}
