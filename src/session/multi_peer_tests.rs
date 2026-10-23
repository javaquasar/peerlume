use super::{DownloadMode, SessionCommand, SessionEvent, SessionOptions, SessionRuntime};
use crate::protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage};
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PIECE_LENGTH: usize = 32 * 1024;
const PIECES_PER_PEER: usize = 8;
const DEADLINE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct TransferProof {
    requested: Vec<bool>,
    sent_bytes: Vec<usize>,
    requests_before_first_response: usize,
}

struct RequestGate {
    proof: Mutex<TransferProof>,
    changed: Condvar,
    stop: Arc<AtomicBool>,
}

impl RequestGate {
    fn wait_for_every_peer(&self, peer: usize) -> Result<(), String> {
        let deadline = Instant::now() + DEADLINE;
        let mut proof = self.proof.lock().unwrap();
        proof.requested[peer] = true;
        self.changed.notify_all();
        while !proof.requested.iter().all(|requested| *requested) {
            if self.stop.load(Ordering::Relaxed) {
                return Err("parallel-download fixture was stopped".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "client did not request blocks concurrently from every peer: {:?}",
                    proof.requested
                ));
            }
            proof = self.changed.wait_timeout(proof, remaining).unwrap().0;
        }
        Ok(())
    }

    fn record_response(&self, peer: usize, bytes: usize) {
        let mut proof = self.proof.lock().unwrap();
        if proof.sent_bytes.iter().all(|sent| *sent == 0) {
            proof.requests_before_first_response = proof
                .requested
                .iter()
                .filter(|requested| **requested)
                .count();
        }
        proof.sent_bytes[peer] += bytes;
    }
}

// Always stop the engine and join fixture threads, including after an assertion fails.
struct Fixture {
    runtime: Option<SessionRuntime>,
    gate: Arc<RequestGate>,
    workers: Vec<JoinHandle<Result<(), String>>>,
    root: tempfile::TempDir,
}

impl Fixture {
    fn shutdown(&mut self) -> Result<(), String> {
        {
            let _proof = self
                .gate
                .proof
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.gate.stop.store(true, Ordering::Relaxed);
            self.gate.changed.notify_all();
        }
        let mut errors = Vec::new();
        if let Some(runtime) = self.runtime.take()
            && let Err(error) = runtime.shutdown()
        {
            errors.push(error);
        }
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error),
                Err(_) => errors.push("local tracker or peer thread panicked".into()),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn accept_before_deadline(
    listener: &TcpListener,
    stop: &AtomicBool,
) -> Result<Option<TcpStream>, String> {
    let deadline = Instant::now() + DEADLINE;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .map_err(|error| error.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .map_err(|error| error.to_string())?;
                return Ok(Some(stream));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("client never connected to a local fixture".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(None)
}

fn serve_tracker(
    listener: TcpListener,
    peers: Vec<SocketAddr>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    while let Some(mut stream) = accept_before_deadline(&listener, &stop)? {
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if !line.starts_with("GET /announce?") {
            return Err(format!("unexpected local tracker request: {line}"));
        }
        loop {
            line.clear();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 || line == "\r\n" {
                break;
            }
        }
        let mut body = format!("d8:intervali1e5:peers{}:", peers.len() * 6).into_bytes();
        for peer in &peers {
            body.extend_from_slice(&[127, 0, 0, 1]);
            body.extend_from_slice(&peer.port().to_be_bytes());
        }
        body.push(b'e');
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .map_err(|e| e.to_string())?;
        stream.write_all(&body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn serve_peer(
    listener: TcpListener,
    peer: usize,
    peer_count: usize,
    info_hash: [u8; 20],
    data: Arc<Vec<u8>>,
    gate: Arc<RequestGate>,
) -> Result<(), String> {
    let Some(mut stream) = accept_before_deadline(&listener, &gate.stop)? else {
        return Ok(());
    };
    let mut encoded = [0; HANDSHAKE_LENGTH];
    stream.read_exact(&mut encoded).map_err(|e| e.to_string())?;
    let incoming = Handshake::decode(&encoded)?;
    if incoming.info_hash != info_hash {
        return Err("client requested the wrong torrent from a local peer".into());
    }
    let outgoing = Handshake {
        reserved: [0; 8],
        info_hash,
        peer_id: format!("-PLTEST-multi{peer:07}")
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    stream
        .write_all(&outgoing.encode())
        .map_err(|e| e.to_string())?;
    let piece_count = data.len() / PIECE_LENGTH;
    let mut bitfield = vec![0; piece_count.div_ceil(8)];
    for index in (peer..piece_count).step_by(peer_count) {
        bitfield[index / 8] |= 0x80 >> (index % 8);
    }
    stream
        .write_all(&PeerMessage::Bitfield(bitfield).encode())
        .map_err(|e| e.to_string())?;
    stream
        .write_all(&PeerMessage::Unchoke.encode())
        .map_err(|e| e.to_string())?;
    let mut served = HashSet::new();
    let mut unique_bytes = 0;
    while unique_bytes < PIECES_PER_PEER * PIECE_LENGTH {
        let mut prefix = [0; 4];
        if let Err(error) = stream.read_exact(&mut prefix) {
            return if gate.stop.load(Ordering::Relaxed) {
                Ok(())
            } else {
                Err(format!("peer {peer}: {error}"))
            };
        }
        let length = u32::from_be_bytes(prefix) as usize;
        if length > 65536 {
            return Err("client sent an oversized peer message".into());
        }
        let mut message = prefix.to_vec();
        message.resize(length + 4, 0);
        stream
            .read_exact(&mut message[4..])
            .map_err(|e| e.to_string())?;
        let Ok(PeerMessage::Request {
            index,
            begin,
            length,
        }) = PeerMessage::decode(&message, 65536)
        else {
            continue;
        };
        let piece = index as usize;
        let begin_offset = begin as usize;
        let block_length = length as usize;
        if piece >= piece_count
            || piece % peer_count != peer
            || block_length == 0
            || block_length > 16384
            || begin_offset + block_length > PIECE_LENGTH
        {
            return Err(format!(
                "peer {peer}: invalid or unavailable block requested"
            ));
        }
        if unique_bytes == 0 {
            // No response is allowed until every peer has an outstanding request.
            // A client that downloads from one peer at a time cannot pass this gate.
            gate.wait_for_every_peer(peer)?;
        }
        let offset = piece * PIECE_LENGTH + begin_offset;
        stream
            .write_all(
                &PeerMessage::Piece {
                    index,
                    begin,
                    block: data[offset..offset + block_length].to_vec(),
                }
                .encode(),
            )
            .map_err(|e| e.to_string())?;
        gate.record_response(peer, block_length);
        if served.insert((index, begin, length)) {
            unique_bytes += block_length;
        }
    }
    // Keep the connection alive while the client's queued blocks are verified.
    let deadline = Instant::now() + DEADLINE;
    while !gate.stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn download_from_concurrent_peers(peer_count: usize, mode: DownloadMode) {
    let root = tempfile::tempdir().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(RequestGate {
        proof: Mutex::new(TransferProof {
            requested: vec![false; peer_count],
            sent_bytes: vec![0; peer_count],
            ..Default::default()
        }),
        changed: Condvar::new(),
        stop,
    });
    let mut fixture = Fixture {
        runtime: None,
        gate,
        workers: Vec::new(),
        root,
    };
    let payload = Arc::new(
        (0..peer_count * PIECES_PER_PEER * PIECE_LENGTH)
            .map(|offset| ((offset * 31 + offset / PIECE_LENGTH * 17) % 251) as u8)
            .collect::<Vec<_>>(),
    );
    let tracker_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    tracker_listener.set_nonblocking(true).unwrap();
    let tracker_url = format!("http://{}/announce", tracker_listener.local_addr().unwrap());
    let listeners: Vec<_> = (0..peer_count)
        .map(|_| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        })
        .collect();
    let addresses = listeners
        .iter()
        .map(|peer| peer.local_addr().unwrap())
        .collect();
    let mut info = format!(
        "d6:lengthi{}e4:name11:payload.bin12:piece lengthi{PIECE_LENGTH}e6:pieces{}:",
        payload.len(),
        payload.len() / PIECE_LENGTH * 20
    )
    .into_bytes();
    for piece in payload.chunks(PIECE_LENGTH) {
        info.extend_from_slice(&Sha1::digest(piece));
    }
    info.extend_from_slice(b"7:privatei1ee");
    let info_hash: [u8; 20] = Sha1::digest(&info).into();
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut metainfo = format!("d8:announce{}:{tracker_url}4:info", tracker_url.len()).into_bytes();
    metainfo.extend(info);
    metainfo.push(b'e');
    let source = fixture.root.path().join("fixture.torrent");
    fs::write(&source, metainfo).unwrap();
    let output = fixture.root.path().join("download");
    let tracker_stop = fixture.gate.stop.clone();
    fixture.workers.push(thread::spawn(move || {
        serve_tracker(tracker_listener, addresses, tracker_stop)
    }));
    for (peer, listener) in listeners.into_iter().enumerate() {
        let data = payload.clone();
        let peer_gate = fixture.gate.clone();
        fixture.workers.push(thread::spawn(move || {
            serve_peer(listener, peer, peer_count, info_hash, data, peer_gate)
        }));
    }
    fixture.runtime = Some(
        SessionRuntime::start_with_options(
            256,
            output.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..super::session_options(false)
            },
        )
        .unwrap(),
    );
    let runtime = fixture.runtime.as_ref().unwrap();
    runtime
        .try_send(SessionCommand::AddTorrent {
            download_mode: mode,
            freeze_after_bytes: 16384,
            full_size: payload.len() as u64,
            id: id.clone(),
            source: source.display().to_string(),
            output_folder: output.display().to_string(),
            only_files: None,
            trackers: vec![tracker_url],
            paused: false,
            peer_limit: Some(peer_count),
        })
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    let mut finished = false;
    let mut last_stats = String::new();
    while Instant::now() < deadline {
        match runtime.recv_timeout(Duration::from_millis(200)) {
            Ok(SessionEvent::Stats {
                id: torrent_id,
                downloaded_bytes,
                finished: true,
                ..
            }) if torrent_id == id => {
                assert_eq!(downloaded_bytes, payload.len() as u64);
                finished = true;
                break;
            }
            Ok(SessionEvent::Error { message, .. }) => panic!("transfer engine: {message}"),
            Ok(event @ SessionEvent::Stats { .. }) => last_stats = format!("{event:?}"),
            _ => {}
        }
    }
    if !finished {
        let requested = fixture.gate.proof.lock().unwrap().requested.clone();
        let sent_bytes = fixture.gate.proof.lock().unwrap().sent_bytes.clone();
        let errors = fixture.shutdown();
        panic!(
            "parallel download did not finish; requests={requested:?}; bytes={sent_bytes:?}; last={last_stats}; fixture={errors:?}"
        );
    }
    fixture.shutdown().unwrap();
    let proof = fixture.gate.proof.lock().unwrap();
    assert_eq!(proof.requests_before_first_response, peer_count);
    assert!(
        proof
            .sent_bytes
            .iter()
            .all(|sent| *sent >= PIECES_PER_PEER * PIECE_LENGTH)
    );
    let actual = fs::read(output.join("payload.bin")).unwrap();
    assert_eq!(actual, *payload);
    for (piece, expected) in actual
        .chunks(PIECE_LENGTH)
        .zip(payload.chunks(PIECE_LENGTH))
    {
        assert_eq!(Sha1::digest(piece), Sha1::digest(expected));
    }
}

#[test]
fn request_gate_does_not_allow_a_serial_peer_to_send_data() {
    let gate = Arc::new(RequestGate {
        proof: Mutex::new(TransferProof {
            requested: vec![false; 3],
            sent_bytes: vec![0; 3],
            ..Default::default()
        }),
        changed: Condvar::new(),
        stop: Arc::new(AtomicBool::new(false)),
    });
    let peer_gate = gate.clone();
    let worker = thread::spawn(move || peer_gate.wait_for_every_peer(0));
    let proof = gate.proof.lock().unwrap();
    let (proof, _) = gate
        .changed
        .wait_timeout_while(proof, Duration::from_secs(5), |proof| !proof.requested[0])
        .unwrap();
    let stayed_closed = proof.requested == [true, false, false] && !worker.is_finished();
    gate.stop.store(true, Ordering::Relaxed);
    gate.changed.notify_all();
    drop(proof);
    let result = worker.join().unwrap();
    assert!(
        stayed_closed,
        "a single peer must not pass the concurrency gate"
    );
    assert!(
        result.is_err(),
        "cancellation must release a blocked fixture"
    );
    assert_eq!(gate.proof.lock().unwrap().sent_bytes, [0, 0, 0]);
}

#[test]
fn three_peers_download_distinct_pieces_concurrently() {
    download_from_concurrent_peers(3, DownloadMode::Normal);
}

#[test]
fn two_peers_download_multiblock_pieces_concurrently_without_reporting_progress() {
    download_from_concurrent_peers(2, DownloadMode::NoProgress);
}
