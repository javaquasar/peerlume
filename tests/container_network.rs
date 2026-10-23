use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{SystemTime, UNIX_EPOCH},
};

use peerlume::{
    protocol::{HANDSHAKE_LENGTH, Handshake},
    storage::PositionedFile,
    tracker::{AnnounceEvent, AnnounceRequest, announce_http},
    transfer::download_single_v1_piece,
};
use sha1::{Digest, Sha1};
use testcontainers::{
    ContainerRequest, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::SyncRunner,
};

const SERVICE_PORT: u16 = 18_080;
const PAYLOAD: &[u8] = b"verified piece";

const TRACKER_SCRIPT: &str = r#"
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlsplit, parse_qs

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        values = parse_qs(urlsplit(self.path).query, keep_blank_values=True)
        expected = {
            'port': ['51413'], 'uploaded': ['7'], 'downloaded': ['11'],
            'left': ['13'], 'compact': ['1'], 'event': ['started']
        }
        if not self.path.startswith('/announce?') or any(values.get(k) != v for k, v in expected.items()):
            self.send_response(400)
            self.end_headers()
            return
        if len(values.get('info_hash', [])) != 1 or len(values.get('peer_id', [])) != 1:
            self.send_response(400)
            self.end_headers()
            return
        body = b'd8:intervali60e5:peers6:\x7f\x00\x00\x01\xc8\xd5e'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass

with HTTPServer(('0.0.0.0', 18080), Handler) as server:
    # Report readiness only after the socket has been bound and is listening.
    print('READY', flush=True)
    server.serve_forever()
"#;

const PEER_SCRIPT: &str = r#"
import socket, struct, sys

mode = sys.argv[1]
payload = bytes.fromhex(sys.argv[2])
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(('0.0.0.0', 18080))
server.listen(1)
print('READY', flush=True)
connection, _ = server.accept()

def read_exact(length):
    data = b''
    while len(data) < length:
        block = connection.recv(length - len(data))
        if not block:
            raise RuntimeError('unexpected EOF')
        data += block
    return data

handshake = read_exact(68)
if handshake[0:20] != b'\x13BitTorrent protocol':
    raise RuntimeError('invalid handshake')
connection.sendall(handshake[:48] + b'-PLTEST-container001')

if mode == 'handshake':
    connection.close()
    server.close()
    sys.exit(0)

length = struct.unpack('>I', read_exact(4))[0]
message = read_exact(length)
if message != b'\x02':
    raise RuntimeError('expected interested')
connection.sendall(struct.pack('>IB', 1, 1))

length = struct.unpack('>I', read_exact(4))[0]
message = read_exact(length)
if len(message) != 13 or message[0] != 6:
    raise RuntimeError('expected request')
index, begin, requested = struct.unpack('>III', message[1:])
if requested != len(payload):
    raise RuntimeError('unexpected request length')
if mode == 'corrupt':
    payload = bytes([payload[0] ^ 0xff]) + payload[1:]
piece = struct.pack('>IBII', 9 + len(payload), 7, index, begin) + payload
connection.sendall(piece)
connection.close()
server.close()
"#;

fn tracker_request() -> AnnounceRequest {
    AnnounceRequest {
        info_hash: [1; 20],
        peer_id: *b"-PL0001-123456789012",
        port: 51_413,
        uploaded: 7,
        downloaded: 11,
        left: 13,
        event: Some(AnnounceEvent::Started),
    }
}

fn peer_image(mode: &str) -> ContainerRequest<GenericImage> {
    GenericImage::new("python", "3.13-alpine")
        .with_exposed_port(SERVICE_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stdout("READY"))
        .with_cmd([
            "python".to_owned(),
            "-u".to_owned(),
            "-c".to_owned(),
            PEER_SCRIPT.to_owned(),
            mode.to_owned(),
            hex(PAYLOAD),
        ])
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("peerlume-{label}-{nonce}"))
}

#[test]
#[ignore = "requires a running Docker daemon"]
fn container_tracker_receives_exact_accounting_and_returns_compact_peer() {
    let container = GenericImage::new("python", "3.13-alpine")
        .with_exposed_port(SERVICE_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stdout("READY"))
        .with_cmd(["python", "-u", "-c", TRACKER_SCRIPT])
        .start()
        .expect("start tracker fixture container");
    let port = container
        .get_host_port_ipv4(SERVICE_PORT.tcp())
        .expect("mapped tracker port");

    let response = announce_http(
        &format!("http://127.0.0.1:{port}/announce"),
        &tracker_request(),
    )
    .expect("tracker announce must succeed");

    assert_eq!(response.interval_seconds, 60);
    assert_eq!(response.compact_peers, [127, 0, 0, 1, 200, 213]);
}

#[test]
#[ignore = "requires a running Docker daemon"]
fn container_peer_exchanges_a_bittorrent_handshake() {
    let container = peer_image("handshake")
        .start()
        .expect("start peer fixture container");
    let port = container
        .get_host_port_ipv4(SERVICE_PORT.tcp())
        .expect("mapped peer port");
    let outgoing = Handshake {
        reserved: [0; 8],
        info_hash: [3; 20],
        peer_id: *b"-PL0001-client000003",
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to peer fixture");
    stream
        .write_all(&outgoing.encode())
        .expect("write handshake");
    let mut encoded = [0; HANDSHAKE_LENGTH];
    stream.read_exact(&mut encoded).expect("read handshake");
    let incoming = Handshake::decode(&encoded).expect("decode handshake");

    assert_eq!(incoming.info_hash, outgoing.info_hash);
    assert_eq!(incoming.peer_id, *b"-PLTEST-container001");
}

#[test]
#[ignore = "requires a running Docker daemon"]
fn container_peer_piece_is_verified_and_committed() {
    let container = peer_image("valid")
        .start()
        .expect("start peer fixture container");
    let port = container
        .get_host_port_ipv4(SERVICE_PORT.tcp())
        .expect("mapped peer port");
    let root = temp_path("container-valid-piece");
    let destination = PositionedFile::create(&root, "piece.bin", PAYLOAD.len() as u64)
        .expect("create destination");
    let hash: [u8; 20] = Sha1::digest(PAYLOAD).into();
    let handshake = Handshake {
        reserved: [0; 8],
        info_hash: [9; 20],
        peer_id: *b"-PL0001-client000004",
    };
    let peer: SocketAddr = ([127, 0, 0, 1], port).into();

    download_single_v1_piece(
        peer,
        &handshake,
        0,
        &hash,
        PAYLOAD.len() as u32,
        &destination,
        0,
    )
    .expect("download valid piece");
    let mut actual = vec![0; PAYLOAD.len()];
    destination
        .read_exact_at(0, &mut actual)
        .expect("read downloaded piece");
    assert_eq!(actual, PAYLOAD);

    drop(destination);
    fs::remove_dir_all(root).expect("remove temporary download directory");
}

#[test]
#[ignore = "requires a running Docker daemon"]
fn container_peer_corrupt_piece_is_not_committed() {
    let container = peer_image("corrupt")
        .start()
        .expect("start peer fixture container");
    let port = container
        .get_host_port_ipv4(SERVICE_PORT.tcp())
        .expect("mapped peer port");
    let root = temp_path("container-corrupt-piece");
    let destination = PositionedFile::create(&root, "piece.bin", PAYLOAD.len() as u64)
        .expect("create destination");
    let hash: [u8; 20] = Sha1::digest(PAYLOAD).into();
    let handshake = Handshake {
        reserved: [0; 8],
        info_hash: [8; 20],
        peer_id: *b"-PL0001-client000005",
    };
    let peer: SocketAddr = ([127, 0, 0, 1], port).into();

    let error = download_single_v1_piece(
        peer,
        &handshake,
        0,
        &hash,
        PAYLOAD.len() as u32,
        &destination,
        0,
    )
    .expect_err("corrupt piece must fail verification");
    assert_eq!(error, "piece hash verification failed");
    let mut actual = vec![1; PAYLOAD.len()];
    destination
        .read_exact_at(0, &mut actual)
        .expect("read destination after rejected piece");
    assert_eq!(actual, vec![0; PAYLOAD.len()]);

    drop(destination);
    fs::remove_dir_all(root).expect("remove temporary download directory");
}
