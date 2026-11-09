//! Installed native frontends share one loopback owner and persist confirmed commands.
#![cfg(feature = "tui")]

use peerlume::{
    control::{
        TorrentCommand,
        frontend::{AccessAction, Client, ConnectionStatus, Receipt, Snapshot, StorageAction},
        host::InstanceLock,
    },
    file_selection::FileSelectionModel,
    metainfo::TorrentMetainfo,
    session::DetailInterest,
    settings::ClientSettings,
    state::{StateStore, StoredFile, TorrentRecord},
    torrent::{TorrentItem, TorrentState},
};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

// Serialize the tests that copy and spawn executables in this process. On
// Linux a concurrent fork can hold a copied executable open for writing until
// it execs, making a second exec fail with ETXTBSY even after copy() returns.
static COPIED_PACKAGE_TESTS: Mutex<()> = Mutex::new(());

fn installed(root: &Path, source: &str, name: &str) -> PathBuf {
    let target = root.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    std::fs::copy(source, &target).unwrap();
    target
}

fn install_frontend(root: &Path) -> PathBuf {
    #[cfg(feature = "gui")]
    let (binary, name) = (env!("CARGO_BIN_EXE_peerlume"), "peerlume");
    #[cfg(not(feature = "gui"))]
    let (binary, name) = (env!("CARGO_BIN_EXE_peerlume-tui"), "peerlume-tui");
    installed(root, binary, name)
}

fn isolated_settings(downloads: &Path) -> ClientSettings {
    let mut settings = ClientSettings::default();
    settings.download.download_directory = downloads.to_string_lossy().into_owned();
    settings.connection.random_port_on_start = true;
    settings.connection.utp_enabled = false;
    settings.connection.port_forwarding = false;
    settings.bittorrent.dht = false;
    settings.bittorrent.pex = false;
    settings.bittorrent.lsd = false;
    settings.bittorrent.public_extra_trackers.clear();
    settings.privacy.blocklist_enabled = false;
    settings.privacy.blocklist_auto_update = false;
    settings
}

fn wait_for_probe(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Native frontend probe did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn capture_to_eof(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Result<Vec<u8>, String>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe
            .by_ref()
            .take(65_537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())
            .and_then(|count| {
                if count > 65_536 {
                    Err("Probe output exceeded64 KiB".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = sender.send(result);
    });
    receiver
}

#[test]
fn autolaunched_owner_does_not_keep_exited_frontend_capture_pipes_open() {
    let _package_test = COPIED_PACKAGE_TESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("pipe-profile");
    std::fs::create_dir_all(profile.join("downloads")).unwrap();
    let daemon = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-daemon"),
        "peerlume-daemon",
    );
    let frontend = install_frontend(root.path());
    let settings = isolated_settings(&profile.join("downloads"));
    settings.save_to(&profile.join("settings.toml")).unwrap();
    let mut guard = OwnerGuard {
        daemon,
        profile: profile.clone(),
        stopped: false,
    };
    let mut child = Command::new(frontend)
        .arg("--profile")
        .arg(&profile)
        .arg("--attach-check")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = capture_to_eof(child.stdout.take().unwrap());
    let stderr = capture_to_eof(child.stderr.take().unwrap());
    let status = wait_for_probe(&mut child, Duration::from_secs(25));
    // Unlike Command::output, these checks have deadlines even if a detached
    // descendant leaked a capture-pipe handle. OwnerGuard stops it on failure.
    let output = stdout
        .recv_timeout(Duration::from_secs(2))
        .expect("Exited frontend stdout did not reach EOF while its daemon remained alive")
        .unwrap();
    let errors = stderr
        .recv_timeout(Duration::from_secs(2))
        .expect("Exited frontend stderr did not reach EOF while its daemon remained alive")
        .unwrap();
    assert!(
        status.success(),
        "Native pipe probe failed: {}",
        String::from_utf8_lossy(&errors)
    );
    let reply: Value = serde_json::from_slice(&output).expect("native pipe probe JSON");
    let client =
        Client::connect(&profile).expect("EOF must arrive while the daemon is still alive");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(snapshot) = client.sync() {
            assert_eq!(snapshot.instance_id, reply["owner_instance"]);
            assert!(snapshot.torrents.is_empty());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Live owner did not return its snapshot after pipe EOF"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(InstanceLock::acquire(&profile).is_err());
    drop(client);
    guard.stop();
}

struct OwnerGuard {
    daemon: PathBuf,
    profile: PathBuf,
    stopped: bool,
}
impl OwnerGuard {
    fn stop(&mut self) {
        let output = Command::new(&self.daemon)
            .arg("--profile")
            .arg(&self.profile)
            .arg("--stop")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Explicit owner stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.stopped = true;
    }
}
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        if !self.stopped {
            // Isolated profile only. Cleanup requests normal shutdown, never kill.
            let _ = Command::new(&self.daemon)
                .arg("--profile")
                .arg(&self.profile)
                .arg("--stop")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn attach(executable: &Path, profile: &Path) -> Value {
    // A bounded process wait must not depend on pipe EOF from descendants.
    // File capture also makes a leaked inherited handle diagnosable on Windows.
    let mut output = tempfile::tempfile().unwrap();
    let mut errors = tempfile::tempfile().unwrap();
    let mut child = Command::new(executable)
        .arg("--profile")
        .arg(profile)
        .arg("--attach-check")
        .current_dir(executable.parent().unwrap())
        .stdout(output.try_clone().unwrap())
        .stderr(errors.try_clone().unwrap())
        .spawn()
        .unwrap();
    let status = wait_for_probe(&mut child, Duration::from_secs(20));
    errors.seek(SeekFrom::Start(0)).unwrap();
    let mut stderr = Vec::new();
    errors.take(64 * 1024).read_to_end(&mut stderr).unwrap();
    assert!(
        status.success(),
        "Native frontend attach failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    output.seek(SeekFrom::Start(0)).unwrap();
    let mut stdout = Vec::new();
    output.take(64 * 1024).read_to_end(&mut stdout).unwrap();
    let line = stdout
        .split(|byte| *byte == b'\n')
        .find(|line| line.first() == Some(&b'{'))
        .expect("native attach JSON");
    serde_json::from_slice(line).unwrap()
}

fn snapshot(client: &Client, minimum_uploaded: u64) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(snapshot) = client.sync()
            && snapshot.torrents.len() == 1
            && snapshot.torrents[0].item.runtime.registered
            && snapshot.torrents[0].item.state == peerlume::torrent::TorrentState::Seeding
            && snapshot.torrents[0].item.runtime.network_uploaded >= minimum_uploaded
        {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "Native snapshot/upload acknowledgement did not arrive"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_message(peer: &mut TcpStream) -> Vec<u8> {
    loop {
        let mut length = [0; 4];
        peer.read_exact(&mut length).unwrap();
        let length = u32::from_be_bytes(length) as usize;
        assert!(length <= 1024 * 1024, "Unbounded peer message");
        if length == 0 {
            continue;
        }
        let mut message = vec![0; length];
        peer.read_exact(&mut message).unwrap();
        return message;
    }
}

fn connect_seed(port: u16, info_hash: &[u8], receiver_tag: u8) -> (TcpStream, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut peer = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(peer) => break peer,
            Err(_) => {
                assert!(Instant::now() < deadline, "Seed listener did not start");
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    };
    peer.set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut handshake = b"\x13BitTorrent protocol".to_vec();
    handshake.extend_from_slice(&[0; 8]);
    handshake.extend_from_slice(info_hash);
    handshake.extend_from_slice(b"-NT0001-012345678901");
    assert_eq!(handshake.len(), 68);
    handshake[67] = receiver_tag;
    peer.write_all(&handshake).unwrap();
    let mut reply = [0; 68];
    peer.read_exact(&mut reply).unwrap();
    assert_eq!(&reply[0..20], b"\x13BitTorrent protocol");
    assert_eq!(&reply[28..48], info_hash);
    peer.write_all(&[0, 0, 0, 1, 2]).unwrap(); // interested
    loop {
        if read_message(&mut peer)[0] == 1 {
            break;
        }
    } // unchoked
    (peer, reply[48..68].to_vec())
}

fn verify_block(peer: &mut TcpStream, payload: &[u8], begin: u32) -> u64 {
    let length = 16_384u32;
    let mut request = vec![0, 0, 0, 13, 6];
    request.extend_from_slice(&0u32.to_be_bytes());
    request.extend_from_slice(&begin.to_be_bytes());
    request.extend_from_slice(&length.to_be_bytes());
    peer.write_all(&request).unwrap();
    loop {
        let message = read_message(peer);
        if message[0] != 7 {
            continue;
        }
        assert_eq!(message.len(), 9 + length as usize);
        assert_eq!(&message[1..5], &0u32.to_be_bytes());
        assert_eq!(&message[5..9], &begin.to_be_bytes());
        assert_eq!(
            &message[9..],
            &payload[begin as usize..(begin + length) as usize]
        );
        return u64::from(length);
    }
}

#[test]
fn copied_frontends_preserve_one_owner_and_tcp_seed_until_explicit_durable_stop() {
    let _package_test = COPIED_PACKAGE_TESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let downloads = profile.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let daemon = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-daemon"),
        "peerlume-daemon",
    );
    let frontend = install_frontend(root.path());
    let tui = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-tui"),
        "peerlume-tui",
    );
    let reserved = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reserved.local_addr().unwrap().port();
    let mut settings = isolated_settings(&downloads);
    // IP/interface binding is deliberately unsupported on Windows. Disable all
    // discovery and trackers; this fixture makes only explicit loopback TCP I/O.
    settings.connection.listen_port = port;
    settings.connection.random_port_on_start = false;
    assert!(settings.validate().is_empty());
    settings.save_to(&profile.join("settings.toml")).unwrap();
    let payload: Vec<u8> = (0..262_144)
        .map(|index| ((index * 17 + 31) % 251) as u8)
        .collect();
    std::fs::write(downloads.join("payload.bin"), &payload).unwrap();
    let mut info =
        b"d6:lengthi262144e4:name11:payload.bin12:piece lengthi262144e6:pieces20:".to_vec();
    info.extend_from_slice(&Sha1::digest(&payload));
    info.extend_from_slice(b"7:privatei1ee");
    let info_hash = Sha1::digest(&info);
    let hash_hex = format!("{info_hash:x}");
    let mut metainfo = b"d4:info".to_vec();
    metainfo.extend_from_slice(&info);
    metainfo.push(b'e');
    let source = profile.join("synthetic.torrent");
    std::fs::write(&source, metainfo).unwrap();
    let record = TorrentRecord {
        files_complete: true,
        resume: Default::default(),
        transfer_settings: None,
        repair_policy: Default::default(),
        verify_after_download: false,
        download_mode: Default::default(),
        freeze_after_bytes: 0,
        upload_ratio: Default::default(),
        id: format!("v1: {hash_hex}"),
        name: "payload.bin".into(),
        source: source.to_string_lossy().into_owned(),
        info_hash_v1: Some(hash_hex),
        info_hash_v2: None,
        is_private: true,
        state: "seeding".into(),
        queue_position: 0,
        download_directory: None,
        trackers: vec![],
        files: vec![StoredFile {
            index: 0,
            path: "payload.bin".into(),
            size_bytes: payload.len() as u64,
            downloaded_bytes: payload.len() as u64,
            selected: true,
            priority: 0,
        }],
        verified_pieces: vec![0],
        downloaded_bytes: payload.len() as u64,
        uploaded_bytes: 0,
        tracker_audit: None,
    };
    {
        let mut store = StateStore::open(&profile.join("state.sqlite3")).unwrap();
        store.save_torrent(&record).unwrap();
    }
    drop(reserved);
    let mut guard = OwnerGuard {
        daemon: daemon.clone(),
        profile: profile.clone(),
        stopped: false,
    };
    let first_frontend = attach(&frontend, &profile); // exact production auto-launch path
    let owner_id = first_frontend["owner_instance"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(first_frontend["torrents"], 1);
    let first = Client::connect(&profile).unwrap();
    let second = Client::connect(&profile).unwrap();
    let initial_snapshot = snapshot(&first, 0);
    assert_eq!(initial_snapshot.instance_id, owner_id);
    let presentation_peer_id = initial_snapshot.torrents[0].item.peer_id.clone();
    assert_eq!(snapshot(&second, 0).instance_id, owner_id);
    assert!(
        !first.access_view().enabled,
        "Native access must not enable the public agent"
    );
    let duplicate = Command::new(&daemon)
        .arg("--profile")
        .arg(&profile)
        .output()
        .unwrap();
    assert!(!duplicate.status.success(), "A second owner must not start");
    let (mut peer, engine_peer_id) = connect_seed(port, &info_hash, b'1');
    let mut received = verify_block(&mut peer, &payload, 0);
    assert_eq!(snapshot(&first, received).instance_id, owner_id);
    drop(first);
    assert_eq!(attach(&tui, &profile)["owner_instance"], owner_id);
    received += verify_block(&mut peer, &payload, 16_384);
    assert_eq!(snapshot(&second, received).instance_id, owner_id);
    drop(second);
    assert_eq!(attach(&frontend, &profile)["owner_instance"], owner_id);
    received += verify_block(&mut peer, &payload, 32_768);
    let reconnected = Client::connect(&profile).unwrap();
    let final_snapshot = snapshot(&reconnected, received);
    assert_eq!(final_snapshot.instance_id, owner_id);
    assert_eq!(
        final_snapshot.torrents[0].item.peer_id,
        presentation_peer_id
    );
    let (additional_peer, observed_engine_peer_id) = connect_seed(port, &info_hash, b'2');
    assert_eq!(observed_engine_peer_id, engine_peer_id);
    drop(additional_peer);
    let observed_uploaded = final_snapshot.torrents[0].item.runtime.network_uploaded;
    drop(reconnected);
    drop(peer);
    // Explicit stop must use the running owner's authenticated channel even if
    // a later on-disk configuration edit is invalid; it must not rewrite it.
    let settings_path = profile.join("settings.toml");
    let valid_settings = std::fs::read(&settings_path).unwrap();
    let invalid_settings = b"deliberately invalid TOML = [";
    std::fs::write(&settings_path, invalid_settings).unwrap();
    guard.stop();
    assert_eq!(std::fs::read(&settings_path).unwrap(), invalid_settings);
    std::fs::write(&settings_path, valid_settings).unwrap();
    let lock =
        InstanceLock::acquire(&profile).expect("Explicit stop must release profile ownership");
    drop(lock);
    let store = StateStore::open(&profile.join("state.sqlite3")).unwrap();
    let saved = store.load_torrent(&record.id).unwrap().unwrap();
    assert_eq!(saved.files, record.files);
    assert_eq!(
        saved.download_directory.as_deref(),
        Some(settings.download.download_directory.as_str())
    );
    assert!(saved.resume.uploaded >= observed_uploaded && saved.resume.uploaded >= received);
    assert_eq!(
        std::fs::read(downloads.join("payload.bin")).unwrap(),
        payload
    );
    let unavailable = Command::new(&daemon)
        .arg("--profile")
        .arg(root.path().join("absent"))
        .arg("--stop")
        .output()
        .unwrap();
    assert!(!unavailable.status.success());
    assert!(
        !root.path().join("absent").exists(),
        "Stop must not create a missing profile"
    );
}

fn wait_for_native<T>(description: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for {description}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn receipt(client: &Client, label: &str) -> Receipt {
    wait_for_native(label, || {
        client.take_receipts().into_iter().find(|receipt| {
            if receipt.label != label || receipt.queued {
                return false;
            }
            assert!(receipt.error.is_none(), "{label}: {}", receipt.message);
            true
        })
    })
}

#[test]
fn installed_frontend_confirms_file_edits_and_saves_view_separately_from_preferences() {
    let _package_test = COPIED_PACKAGE_TESTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("command-profile");
    let downloads = profile.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let settings_path = profile.join("settings.toml");
    let settings = isolated_settings(&downloads);
    settings.save_to(&settings_path).unwrap();
    let daemon = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-daemon"),
        "peerlume-daemon",
    );
    let tui = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-tui"),
        "peerlume-tui",
    );
    let mut guard = OwnerGuard {
        daemon,
        profile: profile.clone(),
        stopped: false,
    };
    let attached = attach(&tui, &profile);
    let client = Client::connect(&profile).unwrap();
    let snapshot = wait_for_native("initial owner snapshot", || client.sync());
    assert!(snapshot.torrents.is_empty());
    let instance = snapshot.instance_id;
    assert_eq!(attached["owner_instance"], instance);
    assert_eq!(client.connection().status, ConnectionStatus::Connected);

    let source = profile.join("fixture.torrent");
    let mut metainfo = b"d4:infod5:filesld6:lengthi16384e4:pathl9:first.bineed6:lengthi16384e4:pathl10:second.bineee4:name7:fixture12:piece lengthi32768e6:pieces20:".to_vec();
    metainfo.extend_from_slice(&Sha1::digest(vec![17; 32_768]));
    metainfo.extend_from_slice(b"7:privatei1eee");
    std::fs::write(&source, metainfo).unwrap();
    let metadata = TorrentMetainfo::load(&source).unwrap();
    let id = format!("v1: {}", metadata.info_hash_v1_hex.unwrap());
    let item = TorrentItem {
        name: metadata.name,
        source: source.to_string_lossy().into_owned(),
        info_hashes: vec![id.clone()],
        trackers: vec![],
        peer_id: "-NT0001-012345678901".into(),
        files: FileSelectionModel::from_metainfo_files(metadata.files),
        downloaded_bytes: 0,
        uploaded_bytes: 0,
        download_rate: 0,
        upload_rate: 0,
        peers: 0,
        seeds: 0,
        state: TorrentState::Paused,
        is_private: true,
        overrides: Default::default(),
        runtime: Default::default(),
        completion: Default::default(),
    };
    client.add(&item, &settings, None).unwrap();
    let added = receipt(&client, "Torrent registration");
    assert_eq!(added.owner_instance, instance);
    assert!(!added.completed_by_reply);
    client.set_visible_torrent(Some(id.clone()));
    client.set_detail_interest(Some((
        id.clone(),
        DetailInterest {
            files: true,
            peers: true,
        },
    )));
    let row = wait_for_native("hydrated file paths", || {
        let snapshot = client.sync()?;
        assert_eq!(snapshot.instance_id, instance);
        let row = snapshot.torrents.into_iter().next()?;
        row.item.files.selected_indices().is_some().then_some(row)
    });
    assert_eq!(row.item.state, TorrentState::Paused);
    assert_eq!(row.item.files.file_count(), 2);
    assert_eq!(row.item.files.selected_indices().unwrap(), [0, 1]);
    assert_eq!(client.cached_revision(&id), Some(row.command_revision));
    wait_for_native("selected torrent audit projection", || {
        client.audit_ready(&id).then_some(())
    });
    assert!(client.audit_profile(&id).is_none());
    assert!(
        client
            .take_audits()
            .into_iter()
            .any(|(torrent, profile)| torrent == id && profile.is_none())
    );

    let initial_file_revision = row.file_revision;
    client
        .enqueue_submit(
            &id,
            TorrentCommand::SetFiles(vec![1]),
            Some(row.command_revision),
        )
        .unwrap();
    let selected = receipt(&client, "Torrent command");
    assert!(selected.accepted.is_some());
    let row = wait_for_native("new selection and file generation", || {
        let snapshot = client.sync()?;
        let row = snapshot.torrents.into_iter().next()?;
        (row.item.files.selected_indices().as_deref() == Some(&[1])
            && row.file_revision != 0
            && row.file_revision != initial_file_revision)
            .then_some(row)
    });
    assert_eq!(row.item.state, TorrentState::Paused);
    client
        .storage(&id, StorageAction::Priorities { high: vec![1] })
        .unwrap();
    let prioritized = receipt(&client, "Storage command");
    assert_eq!(prioritized.operation, "Change file priorities");
    wait_for_native("high priority projection", || {
        let snapshot = client.sync()?;
        let row = snapshot.torrents.first()?;
        (row.file_revision != 0 && row.item.files.high_priority() == [1]).then_some(())
    });

    let mut draft = client.settings();
    draft.download.show_add_dialog = !settings.download.show_add_dialog;
    client.apply_settings(&draft).unwrap();
    let applied = receipt(&client, "Apply preferences");
    assert!(!applied.completed_by_reply);
    assert_eq!(
        client.settings().download.show_add_dialog,
        draft.download.show_add_dialog
    );
    assert_eq!(
        ClientSettings::load_from(&settings_path)
            .unwrap()
            .download
            .show_add_dialog,
        settings.download.show_add_dialog
    );
    draft.ui.search = "second.bin".into();
    client.save_view(&draft.ui).unwrap();
    let saved_view = receipt(&client, "View");
    assert!(saved_view.completed_by_reply);
    assert_eq!(saved_view.ui_saved.unwrap().search, "second.bin");
    let saved = ClientSettings::load_from(&settings_path).unwrap();
    assert_eq!(saved.ui.search, "second.bin");
    assert_eq!(
        saved.download.show_add_dialog,
        settings.download.show_add_dialog
    );
    client.save_settings(&draft).unwrap();
    let saved_preferences = receipt(&client, "Preferences");
    assert!(saved_preferences.completed_by_reply);
    assert_eq!(
        ClientSettings::load_from(&settings_path)
            .unwrap()
            .download
            .show_add_dialog,
        draft.download.show_add_dialog
    );

    client.access_action(AccessAction::Enable(true)).unwrap();
    receipt(&client, "Agent access");
    assert!(client.access_view().enabled);
    client
        .access_action(AccessAction::Grant(Default::default()))
        .unwrap();
    receipt(&client, "Agent access");
    assert_eq!(client.take_access_token().unwrap().len(), 64);
    assert!(client.take_access_token().is_none());
    let grant = client.access_view().clients.pop().unwrap().id;
    client.access_action(AccessAction::Revoke(grant)).unwrap();
    receipt(&client, "Agent access");
    assert!(client.access_view().clients.is_empty());
    client.access_action(AccessAction::Enable(false)).unwrap();
    receipt(&client, "Agent access");
    assert!(!client.access_view().enabled);

    client.set_journal_active(true);
    wait_for_native("journal projection", || client.journal().path());
    let export = profile.join("journal.jsonl");
    client.export(Some(id.clone()), export.clone()).unwrap();
    let exported = receipt(&client, "Journal export");
    assert!(exported.completed_by_reply);
    assert!(std::fs::read(export).unwrap().is_empty());

    let stop = client.enqueue_stop_owner(&instance).unwrap();
    let shutdown = receipt(&client, "Stop background client");
    assert_eq!(shutdown.job_id, stop);
    assert!(!shutdown.completed_by_reply);
    wait_for_native("owner shutdown and lock release", || {
        (client.connection().status == ConnectionStatus::Stopped).then_some(())
    });
    let lock = InstanceLock::acquire(&profile).unwrap();
    guard.stopped = true;
    let store = StateStore::open(&profile.join("state.sqlite3")).unwrap();
    let saved = store.load_torrent(&id).unwrap().unwrap();
    assert!(!saved.files[0].selected);
    assert!(saved.files[1].selected);
    assert_eq!(saved.files[1].priority, 1);
    assert_eq!(saved.state, "paused");
    drop(store);
    drop(lock);
}
