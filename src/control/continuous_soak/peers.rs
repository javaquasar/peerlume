use crate::protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) const PIECE: usize = 32 * 1024;
pub(super) const BLOCK: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct Data {
    pub hash: [u8; 20],
    pub payload: Arc<Vec<u8>>,
}

pub(super) struct Proof {
    pub downloaded: AtomicU64,
    pub uploaded: AtomicU64,
    pub anchor_uploaded: AtomicU64,
    pub anchor_connections: AtomicU64,
    pub source_connections: AtomicU64,
    pub tracker_requests: AtomicU64,
    pub pieces: Vec<AtomicU64>,
    pub client_id: Mutex<Option<[u8; 20]>>,
    pub failure: Mutex<Option<String>>,
}
impl Proof {
    pub fn new(pieces: usize) -> Self {
        Self {
            downloaded: AtomicU64::new(0),
            uploaded: AtomicU64::new(0),
            anchor_uploaded: AtomicU64::new(0),
            anchor_connections: AtomicU64::new(0),
            source_connections: AtomicU64::new(0),
            tracker_requests: AtomicU64::new(0),
            pieces: (0..pieces).map(|_| AtomicU64::new(0)).collect(),
            client_id: Mutex::new(None),
            failure: Mutex::new(None),
        }
    }
    fn identity(&self, id: [u8; 20]) -> io::Result<()> {
        let mut previous = self.client_id.lock().unwrap();
        if previous.is_some_and(|value| value != id) {
            return Err(io::Error::other("session peer identity changed"));
        }
        *previous = Some(id);
        Ok(())
    }
    pub fn healthy(&self) {
        let failure = self.failure.lock().unwrap().clone();
        assert!(failure.is_none(), "fixture failure: {:?}", failure);
    }
    fn failed(&self, message: String) {
        let mut first = self.failure.lock().unwrap();
        if first.is_none() {
            *first = Some(message);
        }
    }
    pub fn counts(&self) -> Vec<u64> {
        self.pieces
            .iter()
            .map(|v| v.load(Ordering::Relaxed))
            .collect()
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"wire_downloaded_bytes": self.downloaded.load(Ordering::Relaxed),
            "verified_target_upload_bytes": self.uploaded.load(Ordering::Relaxed),
            "verified_anchor_upload_bytes": self.anchor_uploaded.load(Ordering::Relaxed),
            "anchor_connections": self.anchor_connections.load(Ordering::Relaxed),
            "source_connections": self.source_connections.load(Ordering::Relaxed),
            "tracker_requests": self.tracker_requests.load(Ordering::Relaxed)})
    }
}

fn message(stream: &mut TcpStream) -> io::Result<PeerMessage> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    if size > 65536 {
        return Err(io::Error::other("oversize fixture peer frame"));
    }
    let mut bytes = prefix.to_vec();
    bytes.resize(size + 4, 0);
    stream.read_exact(&mut bytes[4..])?;
    PeerMessage::decode(&bytes, 65536).map_err(io::Error::other)
}

fn source(
    mut stream: TcpStream,
    data: &[Data],
    proof: &Proof,
    stop: &AtomicBool,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut encoded = [0; HANDSHAKE_LENGTH];
    stream.read_exact(&mut encoded)?;
    let incoming = Handshake::decode(&encoded).map_err(io::Error::other)?;
    proof.identity(incoming.peer_id)?;
    let torrent = data
        .iter()
        .find(|d| d.hash == incoming.info_hash)
        .ok_or_else(|| io::Error::other("unknown fixture hash"))?;
    stream.write_all(
        &Handshake {
            reserved: [0; 8],
            info_hash: torrent.hash,
            peer_id: *b"-PLSOAK-source000001",
        }
        .encode(),
    )?;
    let count = torrent.payload.len() / PIECE;
    let mut bits = vec![0; count.div_ceil(8)];
    for i in 0..count {
        bits[i / 8] |= 0x80 >> (i % 8);
    }
    stream.write_all(&PeerMessage::Bitfield(bits).encode())?;
    stream.write_all(&PeerMessage::Unchoke.encode())?;
    while !stop.load(Ordering::Relaxed) {
        if let PeerMessage::Request {
            index,
            begin,
            length,
        } = message(&mut stream)?
        {
            if index as usize >= count
                || length == 0
                || length as usize > BLOCK
                || u64::from(begin) + u64::from(length) > PIECE as u64
            {
                return Err(io::Error::other("invalid fixture block request"));
            }
            let start = index as usize * PIECE + begin as usize;
            thread::sleep(Duration::from_millis(3));
            stream.write_all(
                &PeerMessage::Piece {
                    index,
                    begin,
                    block: torrent.payload[start..start + length as usize].to_vec(),
                }
                .encode(),
            )?;
            if torrent.hash == data[0].hash {
                proof.pieces[index as usize].fetch_add(u64::from(length), Ordering::Relaxed);
                proof
                    .downloaded
                    .fetch_add(u64::from(length), Ordering::Relaxed);
            }
        }
    }
    Ok(())
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

pub(super) struct Servers {
    pub tracker_url: String,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}
impl Servers {
    pub fn start(data: Vec<Data>, proof: Arc<Proof>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let peer = listener.local_addr().unwrap();
        let tracker = TcpListener::bind("127.0.0.1:0").unwrap();
        tracker.set_nonblocking(true).unwrap();
        let tracker_url = format!("http://{}/announce", tracker.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let source_stop = stop.clone();
        let source_proof = proof.clone();
        let source_worker = thread::spawn(move || {
            let data = Arc::new(data);
            let mut clients: Vec<JoinHandle<()>> = Vec::new();
            while !source_stop.load(Ordering::Relaxed) {
                let mut i = 0;
                while i < clients.len() {
                    if clients[i].is_finished() {
                        if clients.swap_remove(i).join().is_err() {
                            source_proof.failed("source worker panicked".into());
                        }
                    } else {
                        i += 1;
                    }
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if clients.len() >= 8 {
                            source_proof.failed("too many source connections".into());
                            break;
                        }
                        source_proof
                            .source_connections
                            .fetch_add(1, Ordering::Relaxed);
                        let (data, proof, stop) =
                            (data.clone(), source_proof.clone(), source_stop.clone());
                        clients.push(thread::spawn(move || {
                            if let Err(error) = source(stream, &data, &proof, &stop)
                                && !disconnected(&error)
                                && !stop.load(Ordering::Relaxed)
                            {
                                proof.failed(format!("source: {error}"));
                            }
                        }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => {
                        source_proof.failed(format!("accept: {e}"));
                        break;
                    }
                }
            }
            for client in clients {
                if client.join().is_err() {
                    source_proof.failed("source worker panicked".into());
                }
            }
        });
        let tracker_stop = stop.clone();
        let tracker_worker = thread::spawn(move || {
            while !tracker_stop.load(Ordering::Relaxed) {
                match tracker.accept() {
                    Ok((mut stream, _)) => {
                        let reply = (|| -> io::Result<()> {
                            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                            stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                            let mut reader = BufReader::new(&stream);
                            let mut total = 0;
                            loop {
                                let mut line = String::new();
                                let count = reader.read_line(&mut line)?;
                                total += count;
                                if total > 16384 {
                                    return Err(io::Error::other("oversize tracker request"));
                                }
                                if count == 0 || line == "\r\n" {
                                    break;
                                }
                            }
                            proof.tracker_requests.fetch_add(1, Ordering::Relaxed);
                            let mut body = b"d8:intervali1e5:peers6:".to_vec();
                            body.extend_from_slice(&[127, 0, 0, 1]);
                            body.extend_from_slice(&peer.port().to_be_bytes());
                            body.push(b'e');
                            write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )?;
                            stream.write_all(&body)
                        })();
                        if let Err(e) = reply
                            && !disconnected(&e)
                        {
                            proof.failed(format!("tracker: {e}"));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => {
                        proof.failed(format!("tracker accept: {e}"));
                        break;
                    }
                }
            }
        });
        Self {
            tracker_url,
            stop,
            workers: vec![source_worker, tracker_worker],
        }
    }
}
impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub(super) struct Leecher {
    stream: TcpStream,
    data: Data,
    anchor: bool,
    cursor: usize,
    proof: Arc<Proof>,
}
impl Leecher {
    pub fn connect(
        address: SocketAddr,
        data: Data,
        proof: Arc<Proof>,
        anchor: bool,
    ) -> io::Result<Self> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let incoming = Handshake::exchange(
            &mut stream,
            &Handshake {
                reserved: [0; 8],
                info_hash: data.hash,
                peer_id: if anchor {
                    *b"-PLSOAK-anchor000001"
                } else {
                    *b"-PLSOAK-target000001"
                },
            },
        )?;
        if incoming.info_hash != data.hash {
            return Err(io::Error::other("wrong seeding torrent"));
        }
        proof.identity(incoming.peer_id)?;
        stream.write_all(
            &PeerMessage::Bitfield(vec![0; (data.payload.len() / PIECE).div_ceil(8)]).encode(),
        )?;
        stream.write_all(&PeerMessage::Interested.encode())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::other("seeding peer did not unchoke"));
            }
            if matches!(message(&mut stream)?, PeerMessage::Unchoke) {
                break;
            }
        }
        if anchor {
            proof.anchor_connections.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Self {
            stream,
            data,
            anchor,
            cursor: 0,
            proof,
        })
    }
    pub fn block(&mut self) -> io::Result<()> {
        let index = (self.cursor / PIECE) as u32;
        let begin = (self.cursor % PIECE) as u32;
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
                return Err(io::Error::other("seeding block timed out"));
            }
            if let PeerMessage::Piece {
                index: actual_index,
                begin: actual_begin,
                block,
            } = message(&mut self.stream)?
            {
                if actual_index != index
                    || actual_begin != begin
                    || block != self.data.payload[self.cursor..self.cursor + BLOCK]
                {
                    return Err(io::Error::other("seeding returned incorrect payload"));
                }
                let counter = if self.anchor {
                    &self.proof.anchor_uploaded
                } else {
                    &self.proof.uploaded
                };
                counter.fetch_add(BLOCK as u64, Ordering::Relaxed);
                self.cursor = (self.cursor + BLOCK) % self.data.payload.len();
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
    pub fn start(address: SocketAddr, data: Data, proof: Arc<Proof>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let observed = stop.clone();
        let worker = thread::spawn(move || {
            // Exactly one connection: a reconnect would hide a session restart.
            let mut client = Leecher::connect(address, data, proof, true)?;
            while !observed.load(Ordering::Relaxed) {
                client.block()?;
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
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("anchor panicked")
            .expect("anchor connection failed");
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
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let reader = listener.accept().unwrap().0;
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (writer, reader)
    }
    #[test]
    fn frame_bounds_are_checked_before_reading_the_body() {
        let (mut writer, mut reader) = pair();
        writer.write_all(&65537u32.to_be_bytes()).unwrap();
        assert!(message(&mut reader).is_err());
        let (mut writer, mut reader) = pair();
        writer.write_all(&PeerMessage::KeepAlive.encode()).unwrap();
        assert_eq!(message(&mut reader).unwrap(), PeerMessage::KeepAlive);
    }
    #[test]
    fn identity_is_stable_and_never_exposed_in_activity_snapshots() {
        let proof = Proof::new(2);
        proof.identity([1; 20]).unwrap();
        proof.identity([1; 20]).unwrap();
        assert!(proof.identity([2; 20]).is_err());
        assert!(proof.snapshot().get("client_id").is_none());
        proof.failed("first".into());
        proof.failed("second".into());
        assert_eq!(proof.failure.lock().unwrap().as_deref(), Some("first"));
    }
    #[test]
    fn uploaded_bytes_are_counted_only_after_payload_validation() {
        for correct in [true, false] {
            let (mut server, client) = pair();
            let worker = thread::spawn(move || {
                assert!(matches!(
                    message(&mut server).unwrap(),
                    PeerMessage::Request {
                        index: 0,
                        begin: 0,
                        ..
                    }
                ));
                server
                    .write_all(
                        &PeerMessage::Piece {
                            index: 0,
                            begin: 0,
                            block: vec![if correct { 7 } else { 8 }; BLOCK],
                        }
                        .encode(),
                    )
                    .unwrap();
            });
            let proof = Arc::new(Proof::new(1));
            let mut leecher = Leecher {
                stream: client,
                data: Data {
                    hash: [0; 20],
                    payload: Arc::new(vec![7; PIECE]),
                },
                anchor: false,
                cursor: 0,
                proof: proof.clone(),
            };
            assert_eq!(leecher.block().is_ok(), correct);
            assert_eq!(
                proof.uploaded.load(Ordering::Relaxed),
                if correct { BLOCK as u64 } else { 0 }
            );
            worker.join().unwrap();
        }
    }
}
