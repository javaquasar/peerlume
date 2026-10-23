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
    midpoint_requested: Vec<bool>,
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
        self.wait_at_stage(peer, false)
    }

    fn wait_at_stage(&self, peer: usize, at_midpoint: bool) -> Result<(), String> {
        let deadline = Instant::now() + DEADLINE;
        let mut proof = self.proof.lock().unwrap();
        if at_midpoint {
            proof.midpoint_requested[peer] = true;
        } else {
            proof.requested[peer] = true;
        }
        self.changed.notify_all();
        loop {
            let requested = if at_midpoint {
                &proof.midpoint_requested
            } else {
                &proof.requested
            };
            if requested.iter().all(|requested| *requested) {
                return Ok(());
            }
            if self.stop.load(Ordering::Relaxed) {
                return Err("parallel-download fixture was stopped".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "client did not request blocks concurrently from every peer (midpoint={at_midpoint}): {requested:?}"
                ));
            }
            proof = self.changed.wait_timeout(proof, remaining).unwrap().0;
        }
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

pub(super) fn accept_before_deadline(
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

pub(super) fn serve_tracker(
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
    gate_slot: usize,
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
        peer_id: format!("-PLTEST-multi{gate_slot:07}")
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
    let mut passed_midpoint = false;
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
            gate.wait_for_every_peer(gate_slot)?;
        }
        if !passed_midpoint && unique_bytes >= PIECES_PER_PEER * PIECE_LENGTH / 2 {
            // A fast torrent cannot finish while another torrent has not started.
            // Every peer must reach half of its payload before anyone continues.
            gate.wait_at_stage(gate_slot, true)?;
            passed_midpoint = true;
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
        gate.record_response(gate_slot, block_length);
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
    download_torrents_concurrently(&[(peer_count, mode)]);
}

struct TorrentFixture {
    id: String,
    source: std::path::PathBuf,
    output: std::path::PathBuf,
    tracker_url: String,
    payload: Arc<Vec<u8>>,
    peer_count: usize,
    mode: DownloadMode,
}

fn download_torrents_concurrently(specifications: &[(usize, DownloadMode)]) {
    download_torrents_with_verification(specifications, false);
}

fn download_torrents_with_verification(
    specifications: &[(usize, DownloadMode)],
    verify_after_download: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let total_peers: usize = specifications.iter().map(|(peers, _)| peers).sum();
    let gate = Arc::new(RequestGate {
        proof: Mutex::new(TransferProof {
            requested: vec![false; total_peers],
            midpoint_requested: vec![false; total_peers],
            sent_bytes: vec![0; total_peers],
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
    let mut torrents = Vec::new();
    let mut first_gate_slot = 0;
    for (torrent_index, &(peer_count, mode)) in specifications.iter().enumerate() {
        let payload = Arc::new(
            (0..peer_count * PIECES_PER_PEER * PIECE_LENGTH)
                .map(|offset| {
                    ((offset * 31 + offset / PIECE_LENGTH * 17 + torrent_index * 53) % 251) as u8
                })
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
        let mut metainfo =
            format!("d8:announce{}:{tracker_url}4:info", tracker_url.len()).into_bytes();
        metainfo.extend(info);
        metainfo.push(b'e');
        let source = fixture
            .root
            .path()
            .join(format!("torrent-{torrent_index}.torrent"));
        fs::write(&source, metainfo).unwrap();
        let output = fixture
            .root
            .path()
            .join(format!("download-{torrent_index}"));
        let tracker_stop = fixture.gate.stop.clone();
        fixture.workers.push(thread::spawn(move || {
            serve_tracker(tracker_listener, addresses, tracker_stop)
        }));
        for (peer, listener) in listeners.into_iter().enumerate() {
            let data = payload.clone();
            let peer_gate = fixture.gate.clone();
            let gate_slot = first_gate_slot + peer;
            fixture.workers.push(thread::spawn(move || {
                serve_peer(
                    listener, peer, peer_count, gate_slot, info_hash, data, peer_gate,
                )
            }));
        }
        first_gate_slot += peer_count;
        torrents.push(TorrentFixture {
            id,
            source,
            output,
            tracker_url,
            payload,
            peer_count,
            mode,
        });
    }
    let distinct_ids: HashSet<_> = torrents.iter().map(|torrent| &torrent.id).collect();
    assert_eq!(
        distinct_ids.len(),
        specifications.len(),
        "fixtures must have distinct info hashes"
    );
    fixture.runtime = Some(
        SessionRuntime::start_with_options(
            256,
            fixture.root.path().join("default-download"),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..super::session_options(false)
            },
        )
        .unwrap(),
    );
    let runtime = fixture.runtime.as_ref().unwrap();
    for torrent in &torrents {
        runtime
            .try_send(SessionCommand::AddTorrent {
                repair_policy: Default::default(),
                verify_after_download,
                download_mode: torrent.mode,
                freeze_after_bytes: 16384,
                full_size: torrent.payload.len() as u64,
                id: torrent.id.clone(),
                source: torrent.source.display().to_string(),
                output_folder: torrent.output.display().to_string(),
                only_files: None,
                trackers: vec![torrent.tracker_url.clone()],
                paused: false,
                peer_limit: Some(torrent.peer_count),
            })
            .unwrap();
    }
    let deadline = Instant::now() + DEADLINE;
    let mut finished = HashSet::new();
    let mut checking_seen = HashSet::new();
    let mut verified_seen = HashSet::new();
    let mut last_stats = String::new();
    while Instant::now() < deadline {
        match runtime.recv_timeout(Duration::from_millis(200)) {
            Ok(SessionEvent::Stats {
                id: torrent_id,
                downloaded_bytes,
                finished: true,
                ..
            }) => {
                let torrent = torrents
                    .iter()
                    .find(|torrent| torrent.id == torrent_id)
                    .expect("stats must belong to an added torrent");
                if verify_after_download {
                    assert!(
                        checking_seen.contains(&torrent_id),
                        "Done must follow a real disk check"
                    );
                    assert!(
                        verified_seen.contains(&torrent_id),
                        "Done must wait for successful verification"
                    );
                }
                assert_eq!(downloaded_bytes, torrent.payload.len() as u64);
                finished.insert(torrent_id);
                if finished.len() == torrents.len() {
                    break;
                }
            }
            Ok(SessionEvent::Verification { id, status }) => match status {
                crate::completion::CompletionStatus::Checking {
                    checked_bytes,
                    total_bytes,
                } => {
                    assert!(checked_bytes <= total_bytes);
                    checking_seen.insert(id);
                }
                crate::completion::CompletionStatus::Verified => {
                    verified_seen.insert(id);
                }
                crate::completion::CompletionStatus::Failed { message } => {
                    panic!("verification: {message}")
                }
                _ => {}
            },
            Ok(SessionEvent::Error { message, .. }) => panic!("transfer engine: {message}"),
            Ok(event @ SessionEvent::Stats { .. }) => last_stats = format!("{event:?}"),
            _ => {}
        }
    }
    if finished.len() != torrents.len() {
        let requested = fixture.gate.proof.lock().unwrap().requested.clone();
        let midpoint = fixture
            .gate
            .proof
            .lock()
            .unwrap()
            .midpoint_requested
            .clone();
        let sent_bytes = fixture.gate.proof.lock().unwrap().sent_bytes.clone();
        let errors = fixture.shutdown();
        panic!(
            "parallel downloads did not finish; finished={finished:?}; requests={requested:?}; midpoint={midpoint:?}; bytes={sent_bytes:?}; last={last_stats}; fixture={errors:?}"
        );
    }
    fixture.shutdown().unwrap();
    let proof = fixture.gate.proof.lock().unwrap();
    assert_eq!(proof.requests_before_first_response, total_peers);
    assert!(proof.midpoint_requested.iter().all(|requested| *requested));
    assert!(
        proof
            .sent_bytes
            .iter()
            .all(|sent| *sent >= PIECES_PER_PEER * PIECE_LENGTH)
    );
    for torrent in &torrents {
        let actual = fs::read(torrent.output.join("payload.bin")).unwrap();
        assert_eq!(actual, *torrent.payload, "wrong payload for {}", torrent.id);
        for (piece, expected) in actual
            .chunks(PIECE_LENGTH)
            .zip(torrent.payload.chunks(PIECE_LENGTH))
        {
            assert_eq!(Sha1::digest(piece), Sha1::digest(expected));
        }
    }
}

#[test]
fn request_gate_does_not_allow_a_serial_peer_to_send_data() {
    verify_gate_blocks_incomplete_group(3, 1, false);
}

#[test]
fn midpoint_gate_blocks_one_torrent_until_the_other_makes_progress() {
    verify_gate_blocks_incomplete_group(4, 2, true);
}

fn verify_gate_blocks_incomplete_group(peer_count: usize, active_peers: usize, at_midpoint: bool) {
    let gate = Arc::new(RequestGate {
        proof: Mutex::new(TransferProof {
            requested: vec![at_midpoint; peer_count],
            midpoint_requested: vec![false; peer_count],
            sent_bytes: vec![0; peer_count],
            ..Default::default()
        }),
        changed: Condvar::new(),
        stop: Arc::new(AtomicBool::new(false)),
    });
    let workers: Vec<_> = (0..active_peers)
        .map(|peer| {
            let peer_gate = gate.clone();
            thread::spawn(move || peer_gate.wait_at_stage(peer, at_midpoint))
        })
        .collect();
    let proof = gate.proof.lock().unwrap();
    let (proof, _) = gate
        .changed
        .wait_timeout_while(proof, Duration::from_secs(5), |proof| {
            let requested = if at_midpoint {
                &proof.midpoint_requested
            } else {
                &proof.requested
            };
            !requested[..active_peers].iter().all(|requested| *requested)
        })
        .unwrap();
    let requested = if at_midpoint {
        &proof.midpoint_requested
    } else {
        &proof.requested
    };
    let stayed_closed = requested[..active_peers].iter().all(|requested| *requested)
        && requested[active_peers..]
            .iter()
            .all(|requested| !*requested)
        && workers.iter().all(|worker| !worker.is_finished());
    gate.stop.store(true, Ordering::Relaxed);
    gate.changed.notify_all();
    drop(proof);
    let canceled = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().is_err())
        .collect::<Vec<_>>();
    assert!(
        stayed_closed,
        "an incomplete peer/torrent group must not pass the concurrency gate"
    );
    assert!(
        canceled.iter().all(|canceled| *canceled),
        "cancellation must release a blocked fixture"
    );
    assert_eq!(gate.proof.lock().unwrap().sent_bytes, vec![0; peer_count]);
}

#[test]
fn three_peers_download_distinct_pieces_concurrently() {
    download_from_concurrent_peers(3, DownloadMode::Normal);
}

#[test]
fn two_peers_download_multiblock_pieces_concurrently_without_reporting_progress() {
    download_from_concurrent_peers(2, DownloadMode::NoProgress);
}

#[test]
fn different_torrents_download_concurrently_in_one_session() {
    // Separate metainfo, hashes, trackers, peer sets, output directories, and modes.
    // The shared first-block and midpoint gates span all four peer connections.
    download_torrents_concurrently(&[(2, DownloadMode::Normal), (2, DownloadMode::NoProgress)]);
}

#[test]
fn different_torrents_are_verified_after_parallel_download_before_done() {
    download_torrents_with_verification(
        &[(2, DownloadMode::Normal), (2, DownloadMode::NoProgress)],
        true,
    );
}
