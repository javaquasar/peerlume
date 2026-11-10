use super::*;
use crate::{
    control::frontend::{Client, ConnectionStatus, MagnetReviewTask, MagnetSelection},
    metainfo::TorrentMetainfo,
    protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage},
};
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, AtomicUsize},
};

const PIECE: usize = 16 * 1024;

#[derive(Default)]
struct Evidence {
    stop: AtomicBool,
    metadata_requests: AtomicUsize,
    preview_piece_requests: AtomicUsize,
    preview_closed: AtomicBool,
    payload_requests: Mutex<Vec<u32>>,
    announces: Mutex<Vec<String>>,
}

struct Network {
    evidence: Arc<Evidence>,
    workers: Vec<thread::JoinHandle<io::Result<()>>>,
}
impl Drop for Network {
    fn drop(&mut self) {
        self.evidence.stop.store(true, Ordering::Release);
        for worker in self.workers.drain(..) {
            let result = worker.join();
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) if thread::panicking() => {
                    eprintln!("Loopback fixture error: {error}")
                }
                Ok(Err(error)) => panic!("Loopback fixture error: {error}"),
                Err(_) if thread::panicking() => {}
                Err(_) => panic!("Loopback fixture worker panicked"),
            }
        }
    }
}

fn bind() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn accept(listener: &TcpListener, evidence: &Evidence) -> io::Result<Option<TcpStream>> {
    while !evidence.stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                stream.set_write_timeout(Some(Duration::from_secs(3)))?;
                return Ok(Some(stream));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn serve_tracker(listener: TcpListener, port: u16, evidence: Arc<Evidence>) -> io::Result<()> {
    while let Some(mut stream) = accept(&listener, &evidence)? {
        let mut reader = BufReader::new(&mut stream);
        let mut first = String::new();
        if reader.read_line(&mut first)? == 0 {
            continue;
        }
        let mut header = String::new();
        loop {
            if reader.read_line(&mut header)? == 0 || header == "\r\n" {
                break;
            }
            header.clear();
        }
        evidence.announces.lock().unwrap().push(first);
        let mut body = b"d8:intervali3600e5:peers6:".to_vec();
        body.extend([127, 0, 0, 1]);
        body.extend(port.to_be_bytes());
        body.push(b'e');
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)?;
    }
    Ok(())
}

fn handshake(stream: &mut TcpStream, hash: [u8; 20], preview: bool) -> io::Result<()> {
    let mut incoming = [0; HANDSHAKE_LENGTH];
    stream.read_exact(&mut incoming)?;
    assert_eq!(Handshake::decode(&incoming).unwrap().info_hash, hash);
    let mut reserved = [0; 8];
    if preview {
        reserved[5] = 0x10;
    }
    stream.write_all(
        &Handshake {
            reserved,
            info_hash: hash,
            peer_id: *b"-PLMETA-000000000001",
        }
        .encode(),
    )
}

fn read_peer(stream: &mut TcpStream) -> io::Result<PeerMessage> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let len = u32::from_be_bytes(prefix) as usize;
    assert!(len <= 65536);
    let mut packet = prefix.to_vec();
    packet.resize(len + 4, 0);
    stream.read_exact(&mut packet[4..])?;
    PeerMessage::decode(&packet, 65536).map_err(io::Error::other)
}

fn our_metadata_id(payload: &[u8]) -> u8 {
    let key = b"11:ut_metadatai";
    let start = payload
        .windows(key.len())
        .position(|bytes| bytes == key)
        .unwrap()
        + key.len();
    let length = payload[start..]
        .iter()
        .position(|byte| *byte == b'e')
        .unwrap();
    std::str::from_utf8(&payload[start..start + length])
        .unwrap()
        .parse()
        .unwrap()
}

fn serve_metadata(
    stream: &mut TcpStream,
    hash: [u8; 20],
    info: &[u8],
    evidence: &Evidence,
    deliver: bool,
) -> io::Result<()> {
    handshake(stream, hash, true)?;
    stream.write_all(
        &PeerMessage::Extended {
            extension_id: 0,
            payload: format!("d1:md11:ut_metadatai1ee13:metadata_sizei{}ee", info.len())
                .into_bytes(),
        }
        .encode(),
    )?;
    let mut client_id = None;
    loop {
        match read_peer(stream) {
            Ok(PeerMessage::Extended {
                extension_id: 0,
                payload,
            }) => {
                let disabled_pex = b"6:ut_pexi0e";
                assert!(
                    payload
                        .windows(disabled_pex.len())
                        .any(|value| value == disabled_pex)
                );
                client_id = Some(our_metadata_id(&payload));
            }
            Ok(PeerMessage::Extended {
                extension_id: 1,
                payload,
            }) => {
                assert!(payload.windows(13).any(|value| value == b"8:msg_typei0e"));
                evidence.metadata_requests.fetch_add(1, Ordering::AcqRel);
                if !deliver {
                    continue;
                }
                let mut body =
                    format!("d8:msg_typei1e5:piecei0e10:total_sizei{}ee", info.len()).into_bytes();
                body.extend(info);
                stream.write_all(
                    &PeerMessage::Extended {
                        extension_id: client_id.expect("metadata extension handshake"),
                        payload: body,
                    }
                    .encode(),
                )?;
            }
            Ok(PeerMessage::Request { .. }) => {
                evidence
                    .preview_piece_requests
                    .fetch_add(1, Ordering::AcqRel);
            }
            Ok(PeerMessage::Unchoke | PeerMessage::Interested | PeerMessage::Bitfield(_)) => {
                panic!("Metadata review sent a payload transfer message");
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
}

fn serve_payload(stream: &mut TcpStream, hash: [u8; 20], evidence: &Evidence) -> io::Result<()> {
    handshake(stream, hash, false)?;
    stream.write_all(&PeerMessage::Bitfield(vec![0xc0]).encode())?;
    stream.write_all(&PeerMessage::Unchoke.encode())?;
    while !evidence.stop.load(Ordering::Acquire) {
        match read_peer(stream) {
            Ok(PeerMessage::Request {
                index,
                begin,
                length,
            }) => {
                evidence.payload_requests.lock().unwrap().push(index);
                assert_eq!((index, begin, length as usize), (0, 0, PIECE));
                stream.write_all(
                    &PeerMessage::Piece {
                        index,
                        begin,
                        block: vec![17; PIECE],
                    }
                    .encode(),
                )?;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[test]
fn native_magnet_review_fetches_only_metadata_until_explicit_selected_download() {
    let mut info = b"d5:filesld6:lengthi16384e4:pathl5:a.bineed6:lengthi16384e4:pathl5:b.bineee4:name7:payload12:piece lengthi16384e6:pieces40:".to_vec();
    info.extend_from_slice(&Sha1::digest(vec![17; PIECE]));
    info.extend_from_slice(&Sha1::digest(vec![43; PIECE]));
    info.extend_from_slice(b"7:privatei1ee");
    let hash: [u8; 20] = Sha1::digest(&info).into();
    let hash_text = format!("{:x}", Sha1::digest(&info));
    let peer = bind();
    let tracker = bind();
    let sentinel = bind();
    let peer_port = peer.local_addr().unwrap().port();
    let tracker_url = format!("http://{}/announce", tracker.local_addr().unwrap());
    let mut magnet = url::Url::parse(&format!("magnet:?xt=urn:btih:{hash_text}")).unwrap();
    magnet.query_pairs_mut().append_pair("tr", &tracker_url);
    let evidence = Arc::new(Evidence::default());
    let mut network = Network {
        evidence: evidence.clone(),
        workers: vec![],
    };
    let tracker_evidence = evidence.clone();
    network.workers.push(thread::spawn(move || {
        serve_tracker(tracker, peer_port, tracker_evidence)
    }));
    let peer_evidence = evidence.clone();
    network.workers.push(thread::spawn(move || {
        if let Some(mut stream) = accept(&peer, &peer_evidence)? {
            serve_metadata(&mut stream, hash, &info, &peer_evidence, true)?;
        }
        if let Some(mut stream) = accept(&peer, &peer_evidence)? {
            serve_payload(&mut stream, hash, &peer_evidence)?;
        }
        Ok(())
    }));
    let fixture = NativeOwner::new_with_options(
        vec![],
        Some(librqbit::SessionOptions {
            dht: None,
            listen: None,
            disable_local_service_discovery: true,
            trackers: [
                format!("http://{}/unexpected", sentinel.local_addr().unwrap())
                    .parse()
                    .unwrap(),
            ]
            .into(),
            ..Default::default()
        }),
    );
    let root = PathBuf::from(&fixture.settings.download.download_directory);
    let engine = fixture.owner.lock().unwrap().backend.test_engine().unwrap();
    let client = Client::connect(fixture.profile.path()).unwrap();
    fixture.wait_for(|_| {
        let _ = client.sync();
        client.connection().status == ConnectionStatus::Connected
    });
    let task = MagnetReviewTask::start(&client, magnet.to_string()).unwrap();
    let mut resolved = None;
    fixture.wait_for(|_| {
        resolved = task.poll();
        resolved.is_some()
    });
    let mut review = resolved.unwrap().unwrap_or_else(|error| {
        panic!(
            "{error}; tracker requests: {:?}; metadata requests: {}; preview piece requests: {}",
            evidence.announces.lock().unwrap(),
            evidence.metadata_requests.load(Ordering::Acquire),
            evidence.preview_piece_requests.load(Ordering::Acquire),
        )
    });
    assert!(review.metadata.is_private);
    assert_eq!(review.metadata.name, "payload");
    assert_eq!(review.metadata.files.len(), 2);
    assert!(fixture.owner.lock().unwrap().torrents.is_empty());
    assert!(engine.with_torrents(|torrents| torrents.count() == 0));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert_eq!(evidence.metadata_requests.load(Ordering::Acquire), 1);
    assert_eq!(evidence.preview_piece_requests.load(Ordering::Acquire), 0);
    assert!(evidence.payload_requests.lock().unwrap().is_empty());
    let announces = evidence.announces.lock().unwrap().clone();
    assert_eq!(announces.len(), 1);
    assert!(!announces[0].contains("event="));
    assert!(announces[0].contains("uploaded=0") && announces[0].contains("downloaded=0"));
    assert!(matches!(sentinel.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));

    let mut files = FileSelectionModel::from_metainfo_files(review.metadata.files.clone());
    files.select_all(false);
    files.set_selected(0, true);
    let job = review
        .token
        .confirm(MagnetSelection {
            files: &files,
            peer_id: crate::client::tests::torrent().peer_id,
            overrides: Default::default(),
            start: true,
            audit: None,
        })
        .unwrap();
    let mut receipt = None;
    fixture.wait_for(|_| {
        receipt = client
            .take_receipts()
            .into_iter()
            .find(|receipt| receipt.job_id == job && !receipt.queued);
        receipt.is_some()
    });
    assert_eq!(receipt.unwrap().error, None);
    review.token.confirmed();
    fixture.wait_for(|owner| {
        owner.torrents.first().is_some_and(|item| {
            item.completion == crate::completion::CompletionStatus::Verified
                && item.downloaded_bytes == PIECE as u64
        })
    });
    assert_eq!(fs::read(root.join("a.bin")).unwrap(), vec![17; PIECE]);
    assert!(fs::read(root.join("b.bin")).unwrap_or_default().is_empty());
    assert_eq!(evidence.payload_requests.lock().unwrap().as_slice(), &[0]);
    assert!(matches!(sentinel.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    let record = fixture
        .owner
        .lock()
        .unwrap()
        .backend
        .load_torrents()
        .unwrap()
        .pop()
        .unwrap();
    let cache = Path::new(&record.source);
    assert!(cache.starts_with(fixture.profile.path()) && cache.is_file());
    assert_eq!(
        TorrentMetainfo::load(cache)
            .unwrap()
            .info_hash_v1_hex
            .as_deref(),
        Some(hash_text.as_str())
    );
    assert_eq!(record.files.iter().filter(|file| file.selected).count(), 1);
    assert!(
        matches!(fixture.rpc(Action::BeginMagnetReview { magnet: magnet.to_string() }), Err(Error::Failed(message)) if message.contains("already added"))
    );
    drop((engine, client, review, task));
    fixture.stop_engine();
    drop(network);
}

#[test]
fn native_magnet_cancel_closes_pending_metadata_connection() {
    let info = b"d6:lengthi1e4:name1:x12:piece lengthi16384e6:pieces20:00000000000000000000e";
    let hash: [u8; 20] = Sha1::digest(info).into();
    let peer = bind();
    let tracker = bind();
    let port = peer.local_addr().unwrap().port();
    let tracker_url = format!("http://{}/announce", tracker.local_addr().unwrap());
    let mut magnet =
        url::Url::parse(&format!("magnet:?xt=urn:btih:{:x}", Sha1::digest(info))).unwrap();
    magnet.query_pairs_mut().append_pair("tr", &tracker_url);
    let magnet = magnet.to_string();
    let evidence = Arc::new(Evidence::default());
    let tracker_evidence = evidence.clone();
    let peer_evidence = evidence.clone();
    let network = Network {
        evidence: evidence.clone(),
        workers: vec![
            thread::spawn(move || serve_tracker(tracker, port, tracker_evidence)),
            thread::spawn(move || {
                if let Some(mut stream) = accept(&peer, &peer_evidence)? {
                    serve_metadata(&mut stream, hash, info, &peer_evidence, false)?;
                    peer_evidence.preview_closed.store(true, Ordering::Release);
                }
                Ok(())
            }),
        ],
    };
    let fixture = NativeOwner::new_with_options(
        vec![],
        Some(librqbit::SessionOptions {
            dht: None,
            listen: None,
            disable_local_service_discovery: true,
            ..Default::default()
        }),
    );
    let Reply::MagnetReview(page) = fixture
        .rpc(Action::BeginMagnetReview {
            magnet: magnet.clone(),
        })
        .unwrap()
    else {
        panic!("review ID required");
    };
    assert_eq!(page.state, MagnetReviewState::Resolving);
    fixture.wait_for(|_| evidence.metadata_requests.load(Ordering::Acquire) > 0);
    assert!(matches!(
        fixture.rpc(Action::BeginMagnetReview { magnet }),
        Err(Error::Failed(message)) if message.contains("already open")
    ));
    let mut unauthorized_client = fixture.request(Action::MagnetReview {
        id: page.id.clone(),
        offset: 0,
    });
    unauthorized_client.client_id = "c".repeat(64);
    let result = transport::call_blocking(fixture.profile.path().into(), unauthorized_client)
        .unwrap()
        .result;
    assert!(matches!(result, Err(Error::Stale)));
    assert!(matches!(
        fixture.rpc(Action::CancelMagnetReview {
            id: page.id.clone()
        }),
        Ok(Reply::Done(_))
    ));
    fixture.wait_for(|_| evidence.preview_closed.load(Ordering::Acquire));
    let Reply::MagnetReview(cancelled) = fixture
        .rpc(Action::MagnetReview {
            id: page.id,
            offset: 0,
        })
        .unwrap()
    else {
        panic!("cancelled review required");
    };
    assert_eq!(cancelled.state, MagnetReviewState::Cancelled);
    assert_eq!(evidence.preview_piece_requests.load(Ordering::Acquire), 0);
    assert!(fixture.owner.lock().unwrap().torrents.is_empty());
    assert_eq!(
        fs::read_dir(&fixture.settings.download.download_directory)
            .unwrap()
            .count(),
        0
    );
    fixture.stop_engine();
    drop(network);
}
