//! Bounded loopback peers, rotating endpoints and explicit tracker fault injection.
use super::data::{BLOCK, Data, PIECE};
use crate::protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage};
use std::{
    collections::HashSet,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    Unavailable,
    DroppedReply,
    Timeout,
}
impl Fault {
    pub fn index(self) -> usize {
        match self {
            Self::Unavailable => 0,
            Self::DroppedReply => 1,
            Self::Timeout => 2,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "http_503",
            Self::DroppedReply => "dropped_reply",
            Self::Timeout => "reply_timeout",
        }
    }
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Unavailable => "http_error",
            Self::DroppedReply => "transport_error",
            Self::Timeout => "timeout",
        }
    }
}
pub(super) struct Proof {
    pub source_bytes: Vec<AtomicU64>,
    pub uploads: Vec<AtomicU64>,
    pub pieces: Vec<Vec<AtomicU64>>,
    pub source_connections: AtomicU64,
    pub leecher_connections: AtomicU64,
    pub anchor_connections: AtomicU64,
    pub source_disconnects: AtomicU64,
    pub truncated_frames: AtomicU64,
    pub tracker_faults: [AtomicU64; 3],
    pub tracker_replies: AtomicU64,
    pub epoch: AtomicU64,
    pub truncate_next: AtomicBool,
    pub overlapping: AtomicBool,
    pub failure: Mutex<Option<String>>,
    identity: Mutex<Option<[u8; 20]>>,
    first: Mutex<HashSet<usize>>,
    gate: Condvar,
    pub stop: AtomicBool,
    fault: Mutex<Option<(Fault, usize)>>,
    pub endpoints: Mutex<HashSet<(usize, usize)>>,
}
impl Proof {
    pub fn new(data: &[Data]) -> Self {
        Self {
            source_bytes: data.iter().map(|_| AtomicU64::new(0)).collect(),
            uploads: data.iter().map(|_| AtomicU64::new(0)).collect(),
            pieces: data
                .iter()
                .map(|d| (0..d.pieces()).map(|_| AtomicU64::new(0)).collect())
                .collect(),
            source_connections: AtomicU64::new(0),
            leecher_connections: AtomicU64::new(0),
            anchor_connections: AtomicU64::new(0),
            source_disconnects: AtomicU64::new(0),
            truncated_frames: AtomicU64::new(0),
            tracker_faults: Default::default(),
            tracker_replies: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            truncate_next: AtomicBool::new(false),
            overlapping: AtomicBool::new(false),
            failure: Default::default(),
            identity: Default::default(),
            first: Default::default(),
            gate: Condvar::new(),
            stop: AtomicBool::new(false),
            fault: Default::default(),
            endpoints: Default::default(),
        }
    }
    pub fn healthy(&self) {
        let error = self.failure.lock().unwrap().clone();
        assert!(error.is_none(), "network fixture failed: {error:?}");
    }
    fn fail(&self, message: String) {
        let mut error = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if error.is_none() {
            *error = Some(message);
        }
    }
    fn identity(&self, id: [u8; 20]) -> io::Result<()> {
        let mut previous = self.identity.lock().unwrap();
        if previous.is_some_and(|p| p != id) {
            return Err(io::Error::other("session peer identity changed"));
        }
        *previous = Some(id);
        Ok(())
    }
    fn parallel(&self, index: usize) -> io::Result<()> {
        if index > 1 || self.overlapping.load(Ordering::Relaxed) {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut first = self.first.lock().unwrap();
        first.insert(index);
        self.gate.notify_all();
        while first.len() < 2 {
            if self.stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return Err(io::Error::other(
                    "two torrents did not request data concurrently",
                ));
            }
            first = self
                .gate
                .wait_timeout(first, Duration::from_millis(100))
                .unwrap()
                .0;
        }
        self.overlapping.store(true, Ordering::Relaxed);
        Ok(())
    }
    pub fn arm(&self, fault: Fault, index: usize) {
        let mut next = self.fault.lock().unwrap();
        assert!(next.is_none());
        *next = Some((fault, index));
    }
    pub fn counts(&self, index: usize) -> Vec<u64> {
        self.pieces[index]
            .iter()
            .map(|v| v.load(Ordering::Relaxed))
            .collect()
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"source_written_payload_bytes":self.source_bytes.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>(),"verified_upload_bytes":self.uploads.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>(),"source_connections":self.source_connections.load(Ordering::Relaxed),"leecher_connections":self.leecher_connections.load(Ordering::Relaxed),"anchor_connections":self.anchor_connections.load(Ordering::Relaxed),"source_disconnects":self.source_disconnects.load(Ordering::Relaxed),"truncated_frames":self.truncated_frames.load(Ordering::Relaxed),"tracker_faults":self.tracker_faults.iter().map(|v|v.load(Ordering::Relaxed)).collect::<Vec<_>>(),"tracker_replies":self.tracker_replies.load(Ordering::Relaxed),"peer_epoch":self.epoch.load(Ordering::Relaxed),"parallel_download_proven":self.overlapping.load(Ordering::Relaxed)})
    }
}

fn disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}
fn message(stream: &mut TcpStream) -> io::Result<PeerMessage> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    if size > 65536 {
        return Err(io::Error::other("oversize peer frame"));
    }
    let mut bytes = prefix.to_vec();
    bytes.resize(size + 4, 0);
    stream.read_exact(&mut bytes[4..])?;
    PeerMessage::decode(&bytes, 65536).map_err(io::Error::other)
}
fn configure(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))
}

fn source(mut stream: TcpStream, data: &[Data], proof: &Proof, endpoint: usize) -> io::Result<()> {
    configure(&stream)?;
    let mut bytes = [0; HANDSHAKE_LENGTH];
    stream.read_exact(&mut bytes)?;
    let incoming = Handshake::decode(&bytes).map_err(io::Error::other)?;
    proof.identity(incoming.peer_id)?;
    let index = data
        .iter()
        .position(|d| d.hash == incoming.info_hash)
        .ok_or_else(|| io::Error::other("unknown torrent hash"))?;
    let torrent = &data[index];
    let mut peer_id = *b"-PLSWARM-source00000";
    peer_id[19] = b'0' + endpoint as u8;
    stream.write_all(
        &Handshake {
            reserved: [0; 8],
            info_hash: torrent.hash,
            peer_id,
        }
        .encode(),
    )?;
    let mut bits = vec![0; torrent.pieces().div_ceil(8)];
    for i in 0..torrent.pieces() {
        if i % 2 == endpoint % 2 {
            bits[i / 8] |= 0x80 >> (i % 8);
        }
    }
    stream.write_all(&PeerMessage::Bitfield(bits).encode())?;
    stream.write_all(&PeerMessage::Unchoke.encode())?;
    let epoch = proof.epoch.load(Ordering::Relaxed);
    while !proof.stop.load(Ordering::Relaxed) {
        if let PeerMessage::Request {
            index: piece,
            begin,
            length,
        } = message(&mut stream)?
        {
            if piece as usize >= torrent.pieces()
                || piece as usize % 2 != endpoint % 2
                || length == 0
                || length as usize > BLOCK
                || u64::from(begin) + u64::from(length) > PIECE as u64
            {
                return Err(io::Error::other(
                    "invalid block range or unadvertised piece",
                ));
            }
            proof.endpoints.lock().unwrap().insert((index, endpoint));
            proof.parallel(index)?;
            if epoch != proof.epoch.load(Ordering::Relaxed) {
                proof.source_disconnects.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            let mut block = vec![0; length as usize];
            torrent.fill(
                u64::from(piece) * PIECE as u64 + u64::from(begin),
                &mut block,
            );
            let encoded = PeerMessage::Piece {
                index: piece,
                begin,
                block,
            }
            .encode();
            if index < 2 && proof.truncate_next.swap(false, Ordering::Relaxed) {
                stream.write_all(&encoded[..encoded.len() / 2])?;
                proof.truncated_frames.fetch_add(1, Ordering::Relaxed);
                proof.source_disconnects.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            stream.write_all(&encoded)?;
            proof.source_bytes[index].fetch_add(u64::from(length), Ordering::Relaxed);
            proof.pieces[index][piece as usize].fetch_add(u64::from(length), Ordering::Relaxed);
        }
    }
    Ok(())
}

fn hash_from_path(path: &str) -> io::Result<[u8; 20]> {
    let raw = path
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("info_hash="))
        .ok_or_else(|| io::Error::other("missing info hash"))?;
    let mut bytes = Vec::new();
    let raw = raw.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return Err(io::Error::other("invalid escaped hash"));
            }
            let digit = |b: u8| {
                (b as char)
                    .to_digit(16)
                    .map(|v| v as u8)
                    .ok_or_else(|| io::Error::other("invalid hex"))
            };
            bytes.push(digit(raw[i + 1])? * 16 + digit(raw[i + 2])?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    bytes
        .try_into()
        .map_err(|_| io::Error::other("wrong hash length"))
}

fn tracker(
    mut stream: TcpStream,
    data: &[Data],
    peers: &[SocketAddr],
    proof: &Proof,
) -> io::Result<()> {
    configure(&stream)?;
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let path = request
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::other("invalid tracker request"))?;
    let hash = hash_from_path(path)?;
    let index = data
        .iter()
        .position(|d| d.hash == hash)
        .ok_or_else(|| io::Error::other("unknown announce hash"))?;
    let url = url::Url::parse(&format!("http://localhost{path}")).map_err(io::Error::other)?;
    let regular = !url.query_pairs().any(|(k, _)| k == "event");
    let mut total = request.len();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        total += n;
        if total > 16384 {
            return Err(io::Error::other("oversize tracker request"));
        }
        if n == 0 || line == "\r\n" {
            break;
        }
    }
    let fault = {
        let mut pending = proof.fault.lock().unwrap();
        if regular && pending.is_some_and(|(_, target)| target == index) {
            pending.take().map(|(f, _)| f)
        } else {
            None
        }
    };
    if let Some(fault) = fault {
        proof.tracker_faults[fault.index()].fetch_add(1, Ordering::Relaxed);
        match fault {
            Fault::Unavailable => {
                stream.write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )?;
                return Ok(());
            }
            Fault::DroppedReply => return Ok(()),
            Fault::Timeout => {
                let deadline = Instant::now() + Duration::from_secs(12);
                while Instant::now() < deadline && !proof.stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(50));
                }
                return Ok(());
            }
        }
    }
    let group = (proof.epoch.load(Ordering::Relaxed) % 2) as usize * 2;
    let mut body = b"d8:intervali1e5:peers12:".to_vec();
    for peer in &peers[group..group + 2] {
        body.extend([127, 0, 0, 1]);
        body.extend(peer.port().to_be_bytes());
    }
    body.push(b'e');
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    proof.tracker_replies.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub(super) struct Servers {
    pub tracker_url: String,
    proof: Arc<Proof>,
    workers: Vec<JoinHandle<()>>,
}
impl Servers {
    pub fn start(data: Vec<Data>, proof: Arc<Proof>) -> Self {
        let data = Arc::new(data);
        let listeners: Vec<_> = (0..4)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let peers: Vec<_> = listeners.iter().map(|l| l.local_addr().unwrap()).collect();
        let tracker_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tracker_url = format!("http://{}/announce", tracker_listener.local_addr().unwrap());
        let mut workers = Vec::new();
        for (endpoint, listener) in listeners.into_iter().enumerate() {
            workers.push(Self::accept(
                listener,
                data.clone(),
                proof.clone(),
                move |stream, data, proof| source(stream, data, proof, endpoint),
                true,
            ));
        }
        workers.push(Self::accept(
            tracker_listener,
            data,
            proof.clone(),
            move |stream, data, proof| tracker(stream, data, &peers, proof),
            false,
        ));
        Self {
            tracker_url,
            proof,
            workers,
        }
    }
    fn accept(
        listener: TcpListener,
        data: Arc<Vec<Data>>,
        proof: Arc<Proof>,
        serve: impl Fn(TcpStream, &[Data], &Proof) -> io::Result<()> + Send + Sync + 'static,
        source: bool,
    ) -> JoinHandle<()> {
        listener.set_nonblocking(true).unwrap();
        let serve = Arc::new(serve);
        thread::spawn(move || {
            let mut clients: Vec<JoinHandle<()>> = Vec::new();
            while !proof.stop.load(Ordering::Relaxed) {
                let mut i = 0;
                while i < clients.len() {
                    if clients[i].is_finished() {
                        if clients.swap_remove(i).join().is_err() {
                            proof.fail("network worker panicked".into());
                        }
                    } else {
                        i += 1;
                    }
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if clients.len() >= 16 {
                            proof.fail("bounded connection pool exceeded".into());
                            break;
                        }
                        if source {
                            proof.source_connections.fetch_add(1, Ordering::Relaxed);
                        }
                        let (data, proof, serve) = (data.clone(), proof.clone(), serve.clone());
                        clients.push(thread::spawn(move || {
                            if let Err(error) = serve(stream, &data, &proof)
                                && !disconnected(&error)
                                && !proof.stop.load(Ordering::Relaxed)
                            {
                                proof.fail(format!("network fixture: {error}"));
                            }
                        }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => {
                        proof.fail(format!("accept: {e}"));
                        break;
                    }
                }
            }
            for client in clients {
                if client.join().is_err() {
                    proof.fail("network worker panicked".into());
                }
            }
        })
    }
}
impl Drop for Servers {
    fn drop(&mut self) {
        self.proof.stop.store(true, Ordering::Relaxed);
        self.proof.gate.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub(super) struct Leecher {
    stream: TcpStream,
    data: Data,
    proof: Arc<Proof>,
    index: usize,
}
impl Leecher {
    pub fn connect(
        addr: SocketAddr,
        data: Data,
        proof: Arc<Proof>,
        index: usize,
    ) -> io::Result<Self> {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        configure(&stream)?;
        let mut peer_id = *b"-PLSWARM-leecher0000";
        peer_id[19] = b'0' + index as u8;
        let reply = Handshake::exchange(
            &mut stream,
            &Handshake {
                reserved: [0; 8],
                info_hash: data.hash,
                peer_id,
            },
        )?;
        proof.identity(reply.peer_id)?;
        stream.write_all(&PeerMessage::Interested.encode())?;
        if index == 2 {
            proof.anchor_connections.fetch_add(1, Ordering::Relaxed);
        } else {
            proof.leecher_connections.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Self {
            stream,
            data,
            proof,
            index,
        })
    }
    pub fn block(&mut self, offset: u64) -> io::Result<()> {
        if !offset.is_multiple_of(BLOCK as u64) || offset + BLOCK as u64 > self.data.bytes {
            return Err(io::Error::other("invalid leecher range"));
        }
        let index = (offset / PIECE as u64) as u32;
        let begin = (offset % PIECE as u64) as u32;
        self.stream.write_all(
            &PeerMessage::Request {
                index,
                begin,
                length: BLOCK as u32,
            }
            .encode(),
        )?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::other("seeding response timed out"));
            }
            if let PeerMessage::Piece {
                index: got,
                begin: at,
                block,
            } = message(&mut self.stream)?
            {
                let mut expected = vec![0; BLOCK];
                self.data.fill(offset, &mut expected);
                if got != index || at != begin || block != expected {
                    return Err(io::Error::other("seeding payload or position mismatch"));
                }
                self.proof.uploads[self.index].fetch_add(BLOCK as u64, Ordering::Relaxed);
                return Ok(());
            }
        }
    }
}
pub(super) struct Anchor {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}
impl Anchor {
    pub fn start(addr: SocketAddr, data: Data, proof: Arc<Proof>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let observed = stop.clone();
        let worker = thread::spawn(move || {
            let mut client = Leecher::connect(addr, data, proof, 2)?;
            while !observed.load(Ordering::Relaxed) {
                client.block(0)?;
                thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
    pub fn healthy(&self) {
        assert!(
            !self.worker.as_ref().unwrap().is_finished(),
            "permanent seeding connection ended"
        );
    }
    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for Anchor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_decoder_preserves_binary_bytes_and_rejects_bad_identifiers() {
        let expected: [u8; 20] = std::array::from_fn(|i| (i * 13) as u8);
        let encoded: String = expected.iter().map(|b| format!("%{b:02X}")).collect();
        assert_eq!(
            hash_from_path(&format!("/announce?info_hash={encoded}&event=started")).unwrap(),
            expected
        );
        for bad in [
            "/announce",
            "/announce?info_hash=%",
            "/announce?info_hash=%XY",
            "/announce?info_hash=a",
        ] {
            assert!(hash_from_path(bad).is_err());
        }
    }
    #[test]
    fn fault_plan_consumption_is_scoped_and_counters_start_empty() {
        let data = Data::new("test.bin", PIECE as u64, false, 1);
        let proof = Proof::new(&[data]);
        for fault in [Fault::Unavailable, Fault::DroppedReply, Fault::Timeout] {
            proof.arm(fault, 0);
            assert_eq!(proof.fault.lock().unwrap().take(), Some((fault, 0)));
            assert!(!fault.name().is_empty());
            assert!(!fault.outcome().is_empty());
        }
        assert!(
            !proof.snapshot()["parallel_download_proven"]
                .as_bool()
                .unwrap()
        );
        assert!(proof.identity([1; 20]).is_ok());
        assert!(proof.identity([2; 20]).is_err());
    }
}
