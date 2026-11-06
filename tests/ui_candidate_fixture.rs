//! Disposable native-window fixture writer. Never opens a session or network.
use peerlume::{
    metainfo::TorrentMetainfo,
    settings::ClientSettings,
    state::{StateStore, StoredFile, TorrentRecord},
};
use sha1::{Digest, Sha1};
use std::{fs, path::Path};

fn bstring(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(value.len().to_string().as_bytes());
    output.push(b':');
    output.extend_from_slice(value);
}

fn prepare(profile: &Path, count: usize) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if !profile.is_absolute() || profile.exists() || !(1..=100).contains(&count) {
        return Err("Fixture requires a new absolute profile path and 1..100 torrents".into());
    }
    fs::create_dir(profile)?;
    let downloads = profile.join("downloads");
    let sources = profile.join("sources");
    fs::create_dir(&downloads)?;
    fs::create_dir(&sources)?;
    let mut settings = ClientSettings::default();
    settings.download.download_directory = downloads.display().to_string();
    settings.download.start_when_added = false;
    settings.queue.start_next_automatically = false;
    settings.connection.random_port_on_start = true;
    settings.connection.port_forwarding = false;
    settings.connection.utp_enabled = false;
    settings.connection.prefer_utp = false;
    settings.bittorrent.dht = false;
    settings.bittorrent.pex = false;
    settings.bittorrent.lsd = false;
    settings.privacy.blocklist_enabled = false;
    settings.privacy.blocklist_auto_update = false;
    settings.save_to(&profile.join("settings.toml"))?;
    let mut store = StateStore::open(&profile.join("state.sqlite3"))?;
    let mut ids = Vec::new();
    for index in 0..count {
        let name = if index % 4 == 0 {
            format!(
                "Synthetic UI {index:03} long title for column resizing and clipping {}",
                "readable title ".repeat(3)
            )
        } else {
            format!("Synthetic UI {index:03} nested files")
        };
        let files = if index == 0 { 150 } else { 3 };
        let mut metadata = b"d4:infod5:filesl".to_vec();
        let mut content = Vec::new();
        let mut payloads = Vec::new();
        for file in 0..files {
            let components = [
                format!("Season {:02}", file / 30 + 1),
                format!("Chapter {:02}", file / 5 + 1),
                format!("file-{file:03}-synthetic-readable-name.txt"),
            ];
            let payload =
                format!("Synthetic local fixture torrent {index}, file {file}.\n").into_bytes();
            metadata.extend_from_slice(format!("d6:lengthi{}e4:pathl", payload.len()).as_bytes());
            for component in &components {
                bstring(&mut metadata, component.as_bytes());
            }
            metadata.extend_from_slice(b"ee");
            content.extend_from_slice(&payload);
            payloads.push(payload);
        }
        metadata.extend_from_slice(b"e4:name");
        bstring(&mut metadata, name.as_bytes());
        metadata.extend_from_slice(b"12:piece lengthi16384e6:pieces");
        let pieces: Vec<_> = content
            .chunks(16384)
            .flat_map(|chunk| Sha1::digest(chunk).to_vec())
            .collect();
        bstring(&mut metadata, &pieces);
        metadata.extend_from_slice(b"7:privatei1eee");
        let parsed = TorrentMetainfo::parse(&metadata)?;
        assert!(parsed.is_private && parsed.trackers.is_empty());
        let hash = parsed
            .info_hash_v1_hex
            .as_ref()
            .ok_or("Fixture has no v1 hash")?
            .clone();
        let id = format!("v1: {hash}");
        for (file, payload) in parsed.files.iter().zip(payloads) {
            let destination = downloads.join(&file.path);
            fs::create_dir_all(destination.parent().ok_or("Missing fixture parent")?)?;
            fs::write(destination, payload)?;
        }
        let source = sources.join(format!("synthetic-{index:03}.torrent"));
        fs::write(&source, metadata)?;
        store.save_torrent(&TorrentRecord {
            files_complete: true,
            resume: Default::default(),
            transfer_settings: None,
            repair_policy: Default::default(),
            verify_after_download: false,
            download_mode: Default::default(),
            freeze_after_bytes: 0,
            upload_ratio: Default::default(),
            id: id.clone(),
            name,
            source: source.display().to_string(),
            info_hash_v1: Some(hash),
            info_hash_v2: None,
            is_private: true,
            state: "paused".into(),
            queue_position: index as i64,
            download_directory: Some(downloads.display().to_string()),
            trackers: vec![],
            files: parsed
                .files
                .into_iter()
                .enumerate()
                .map(|(index, file)| StoredFile {
                    index: index as u32,
                    path: file.path,
                    size_bytes: file.size_bytes,
                    downloaded_bytes: 0,
                    selected: true,
                    priority: 0,
                })
                .collect(),
            verified_pieces: vec![],
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            tracker_audit: None,
        })?;
        ids.push(id);
    }
    let records = store.load_all_torrents()?;
    assert_eq!(records.len(), count);
    assert!(
        records
            .iter()
            .all(|record| record.state == "paused" && record.trackers.is_empty())
    );
    drop(store);
    let result = serde_json::json!({"kind":"ui_candidate_fixture","torrents":count,"first_torrent_files":150,
        "profile":profile,"ids":ids,"saved_state":"paused","engine_started":false,"network_started":false,
        "note":"Local files exist; downloaded/verified counters remain zero until real engine evidence."});
    fs::write(
        profile.join("fixture.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(result)
}

#[test]
fn fixture_uses_real_state_schema_and_refuses_existing_or_relative_directories() {
    let root = tempfile::tempdir().unwrap();
    assert!(prepare(Path::new("relative-profile"), 2).is_err());
    let existing = root.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("preserve.txt"), b"preserve").unwrap();
    assert!(prepare(&existing, 2).is_err());
    assert_eq!(
        fs::read(existing.join("preserve.txt")).unwrap(),
        b"preserve"
    );
    let profile = root.path().join("new-profile");
    let result = prepare(&profile, 2).unwrap();
    assert_eq!(result["torrents"], 2);
    let settings = ClientSettings::load_from(&profile.join("settings.toml")).unwrap();
    assert!(
        !settings.download.start_when_added
            && !settings.bittorrent.dht
            && !settings.bittorrent.pex
            && !settings.bittorrent.lsd
    );
    assert!(!settings.connection.port_forwarding && !settings.privacy.blocklist_auto_update);
    let records = StateStore::open(&profile.join("state.sqlite3"))
        .unwrap()
        .load_all_torrents()
        .unwrap();
    assert_eq!(records[0].files.len(), 150);
    for record in records {
        let metadata = TorrentMetainfo::load(Path::new(&record.source)).unwrap();
        assert!(metadata.trackers.is_empty() && metadata.is_private);
        assert!(
            record
                .files
                .iter()
                .all(|file| profile.join("downloads").join(&file.path).is_file())
        );
    }
}

#[test]
#[ignore = "Writes an explicitly requested disposable UI profile; no engine is launched"]
fn prepare_native_ui_candidate_profile() {
    let directory = std::env::var_os("PEERLUME_UI_CANDIDATE_PROFILE")
        .expect("Set a NEW absolute PEERLUME_UI_CANDIDATE_PROFILE directory");
    let result =
        prepare(Path::new(&directory), 80).expect("Disposable native fixture preparation failed");
    println!("{result}");
}
