//! Real peer transfer after an IPC client has disconnected; no frontend polling.
use super::*;
use super::{
    access::Grants,
    operations::now_ms,
    wire::{Method, Request},
};
use crate::{
    protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage},
    session::SessionRuntime,
    state::StateStore,
};
use sha1::{Digest, Sha1};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[test]
fn real_download_finishes_after_agent_disconnect_and_both_views_agree() {
    let root = tempfile::tempdir().unwrap();
    let payload = b"verified payload delivered after the control client disconnected".to_vec();
    let mut info = format!(
        "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces20:",
        payload.len()
    )
    .into_bytes();
    info.extend_from_slice(&Sha1::digest(&payload));
    info.extend_from_slice(b"7:privatei1ee");
    let hash = format!("{:x}", Sha1::digest(&info));
    let id = format!("v1: {hash}");
    let mut meta = b"d4:info".to_vec();
    meta.extend_from_slice(&info);
    meta.push(b'e');
    let source = root.path().join("fixture.torrent");
    std::fs::write(&source, meta).unwrap();
    let tracker = TcpListener::bind("127.0.0.1:0").unwrap();
    tracker.set_nonblocking(true).unwrap();
    let tracker_url = format!("http://{}/announce", tracker.local_addr().unwrap());
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    peer.set_nonblocking(true).unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let tracker_stop = stop.clone();
    let tracker_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !tracker_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            let (mut stream, _) = match tracker.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let mut reply = b"d8:intervali1e5:peers6:".to_vec();
            reply.extend_from_slice(&[127, 0, 0, 1]);
            reply.extend_from_slice(&peer_addr.port().to_be_bytes());
            reply.push(b'e');
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(&reply);
        }
    });
    let seed_data = payload.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let peer_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut stream = loop {
            match peer.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut bytes = [0; HANDSHAKE_LENGTH];
        stream.read_exact(&mut bytes).unwrap();
        let incoming = Handshake::decode(&bytes).unwrap();
        stream
            .write_all(
                &Handshake {
                    reserved: [0; 8],
                    info_hash: incoming.info_hash,
                    peer_id: *b"-PLTEST-seeder000001",
                }
                .encode(),
            )
            .unwrap();
        stream
            .write_all(&PeerMessage::Bitfield(vec![0x80]).encode())
            .unwrap();
        stream.write_all(&PeerMessage::Unchoke.encode()).unwrap();
        loop {
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let len = u32::from_be_bytes(prefix) as usize;
            assert!(len < 65536);
            let mut bytes = prefix.to_vec();
            bytes.resize(len + 4, 0);
            stream.read_exact(&mut bytes[4..]).unwrap();
            if let Ok(PeerMessage::Request {
                index,
                begin,
                length,
            }) = PeerMessage::decode(&bytes, 65536)
            {
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                stream
                    .write_all(
                        &PeerMessage::Piece {
                            index,
                            begin,
                            block: seed_data[begin as usize..(begin + length) as usize].to_vec(),
                        }
                        .encode(),
                    )
                    .unwrap();
                break;
            }
        }
    });
    let runtime = SessionRuntime::start_with_options(
        64,
        root.path().into(),
        librqbit::SessionOptions {
            dht: None,
            listen: None,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .unwrap();
    let store = StateStore::open(&root.path().join("state.sqlite3")).unwrap();
    let mut control = Controller::new(
        ClientBackend::with_session_and_store(runtime, store),
        vec![],
    );
    let mut item = crate::client::tests::torrent();
    item.name = "payload.bin".into();
    item.source = source.display().to_string();
    item.info_hashes = vec![id.clone()];
    item.trackers = vec![tracker_url];
    item.files = crate::file_selection::FileSelectionModel::from_files([(
        "payload.bin".into(),
        payload.len() as u64,
    )]);
    item.overrides.download_directory = Some(root.path().display().to_string());
    item.state = crate::torrent::TorrentState::Paused;
    control
        .add(item, &crate::settings::ClientSettings::default(), None)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while control
        .read_handle()
        .page(0, 1, None, None)
        .unwrap()
        .torrents[0]
        .state
        != ObservedState::Paused
    {
        control.tick();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    control
        .configure_agents(root.path(), root.path().into())
        .unwrap();
    let service = control.agent_access().unwrap();
    service.enabled = true;
    let token = service
        .grant(Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let instance = service.instance_id.clone();
    let revision = control.command_revision(&id).unwrap();
    let reader = control.read_handle();
    let owner = Arc::new(Mutex::new(control));
    let _lock = host::InstanceLock::acquire(root.path()).unwrap();
    let host = host::Host::start(owner.clone(), |c| c, root.path().into()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "start-local-fixture".into(),
        token,
        instance_id: Some(instance),
        method: Method::Mutate {
            torrent_id: id,
            command: TorrentCommand::Resume,
            expected_revision: revision,
            idempotency_key: "resume-once".into(),
            deadline_unix_ms: now_ms() + 15_000,
        },
    };
    let reply = loop {
        if let Ok(reply) = rt.block_on(host::call(root.path(), &request)) {
            break reply;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(reply.error.is_none());
    drop(rt);
    drop(request); // Agent connection and its runtime are gone.
    ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_ne!(
        reader.page(0, 1, None, None).unwrap().torrents[0].completion,
        ObservedCompletion::Verified
    );
    release_tx.send(()).unwrap();
    loop {
        let page = reader.page(0, 1, None, None).unwrap();
        if page.torrents[0].completion == ObservedCompletion::Verified {
            let owner = owner.lock().unwrap();
            assert_eq!(
                owner.torrents[0].completion,
                crate::completion::CompletionStatus::Verified
            );
            assert_eq!(page.torrents[0].downloaded_bytes, payload.len() as u64);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "download stalled after agent disconnect"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        std::fs::read(root.path().join("payload.bin")).unwrap(),
        payload
    );
    stop.store(true, Ordering::Relaxed);
    tracker_thread.join().unwrap();
    peer_thread.join().unwrap();
    drop(host);
    drop(owner);
}
