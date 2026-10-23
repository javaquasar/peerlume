use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddrV4, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use url::{Host, Url};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnounceEvent {
    Started,
    Completed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnounceRequest {
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Option<AnnounceEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnounceResponse {
    pub interval_seconds: u64,
    pub compact_peers: Vec<u8>,
}

pub fn announce_for_audit(
    base: &str,
    request: &AnnounceRequest,
    allow_hosts: &[String],
) -> Result<AnnounceResponse, String> {
    let parsed = validate_audit_target(base, allow_hosts)?;
    match parsed.scheme() {
        "http" | "https" => announce_http(base, request),
        "udp" => announce_udp(base, request),
        _ => unreachable!("validated tracker scheme"),
    }
}

pub fn validate_audit_target(tracker: &str, allow_hosts: &[String]) -> Result<Url, String> {
    let parsed = Url::parse(tracker).map_err(|_| "invalid tracker URL".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https" | "udp") {
        return Err("tracker URL must use HTTP, HTTPS, or UDP".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "tracker URL has no hostname".to_owned())?;
    let loopback = match parsed.host() {
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    let explicitly_allowed = allow_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host));
    if !loopback && !explicitly_allowed {
        return Err(format!(
            "refusing non-loopback tracker host {host}; enter its exact staging hostname in the allowlist"
        ));
    }
    Ok(parsed)
}

pub fn parse_info_hash_hex(value: &str) -> Result<[u8; 20], String> {
    if value.len() != 40 {
        return Err("info-hash must contain exactly 40 hexadecimal characters".into());
    }
    let mut output = [0_u8; 20];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(output)
}

pub fn compact_ipv4_peers(bytes: &[u8]) -> Result<Vec<SocketAddrV4>, String> {
    if !bytes.len().is_multiple_of(6) {
        return Err("compact IPv4 peers length must be divisible by six".into());
    }
    Ok(bytes
        .as_chunks::<6>()
        .0
        .iter()
        .map(|peer| {
            SocketAddrV4::new(
                Ipv4Addr::new(peer[0], peer[1], peer[2], peer[3]),
                u16::from_be_bytes([peer[4], peer[5]]),
            )
        })
        .collect())
}

struct TrackerEndpoint {
    url: String,
    tier: usize,
    failures: u8,
    next_announce_unix: u64,
}

pub struct TrackerPool {
    endpoints: Vec<TrackerEndpoint>,
}

impl TrackerPool {
    pub fn new(tiers: Vec<Vec<String>>) -> Result<Self, String> {
        let mut endpoints = Vec::new();
        for (tier, urls) in tiers.into_iter().enumerate() {
            for url in urls {
                let parsed = Url::parse(&url).map_err(|_| "invalid tracker URL".to_owned())?;
                if !matches!(parsed.scheme(), "http" | "https" | "udp") {
                    return Err("tracker must use HTTP, HTTPS, or UDP".into());
                }
                endpoints.push(TrackerEndpoint {
                    url,
                    tier,
                    failures: 0,
                    next_announce_unix: 0,
                });
            }
        }
        if endpoints.is_empty() {
            return Err("tracker pool cannot be empty".into());
        }
        Ok(Self { endpoints })
    }

    pub fn next_ready(&self, now_unix: u64) -> Option<(usize, &str)> {
        self.endpoints
            .iter()
            .enumerate()
            .filter(|(_, endpoint)| endpoint.next_announce_unix <= now_unix)
            .min_by_key(|(_, endpoint)| (endpoint.tier, endpoint.failures))
            .map(|(index, endpoint)| (index, endpoint.url.as_str()))
    }

    pub fn record_success(
        &mut self,
        index: usize,
        now_unix: u64,
        interval_seconds: u64,
    ) -> Result<(), String> {
        let endpoint = self
            .endpoints
            .get_mut(index)
            .ok_or_else(|| "tracker index is out of range".to_owned())?;
        endpoint.failures = 0;
        endpoint.next_announce_unix = now_unix.saturating_add(interval_seconds.max(30));
        Ok(())
    }

    pub fn record_failure(&mut self, index: usize, now_unix: u64) -> Result<u64, String> {
        let endpoint = self
            .endpoints
            .get_mut(index)
            .ok_or_else(|| "tracker index is out of range".to_owned())?;
        endpoint.failures = endpoint.failures.saturating_add(1);
        let exponent = endpoint.failures.saturating_sub(1).min(7);
        let delay = (15_u64 << exponent).min(1_800);
        endpoint.next_announce_unix = now_unix.saturating_add(delay);
        Ok(delay)
    }

    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }
}

pub fn build_announce_url(base: &str, request: &AnnounceRequest) -> Result<String, String> {
    let parsed = Url::parse(base).map_err(|_| "invalid tracker URL".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("tracker must use HTTP or HTTPS".into());
    }
    let separator = if parsed.query().is_some() { '&' } else { '?' };
    let mut output = parsed.to_string();
    output.push(separator);
    output.push_str("info_hash=");
    output.push_str(&percent_encode(&request.info_hash));
    output.push_str("&peer_id=");
    output.push_str(&percent_encode(&request.peer_id));
    output.push_str(&format!(
        "&port={}&uploaded={}&downloaded={}&left={}&compact=1",
        request.port, request.uploaded, request.downloaded, request.left
    ));
    if let Some(event) = request.event {
        output.push_str("&event=");
        output.push_str(match event {
            AnnounceEvent::Started => "started",
            AnnounceEvent::Completed => "completed",
            AnnounceEvent::Stopped => "stopped",
        });
    }
    Ok(output)
}

pub fn announce_http(base: &str, request: &AnnounceRequest) -> Result<AnnounceResponse, String> {
    let url = build_announce_url(base, request)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("PeerLume/0.2")
        .build()
        .map_err(|_| "could not create tracker client".to_owned())?;
    let response = client
        .get(url)
        .send()
        .map_err(|_| "tracker request failed".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "tracker returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let limit = librqbit_tracker_comms::MAX_TRACKER_RESPONSE_BYTES;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("tracker response exceeds the 1 MiB limit".into());
    }
    let mut body = Vec::new();
    let mut reader = response;
    let mut chunk = [0; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .map_err(|_| "tracker response read failed".to_owned())?;
        if count == 0 {
            break;
        }
        if count > limit.saturating_sub(body.len()) {
            return Err("tracker response exceeds the 1 MiB limit".into());
        }
        body.extend_from_slice(&chunk[..count]);
    }
    parse_announce_response(&body)
}

pub fn announce_udp(base: &str, request: &AnnounceRequest) -> Result<AnnounceResponse, String> {
    announce_udp_with_timeout(base, request, Duration::from_secs(15))
}

fn announce_udp_with_timeout(
    base: &str,
    request: &AnnounceRequest,
    timeout: Duration,
) -> Result<AnnounceResponse, String> {
    const PROTOCOL_ID: u64 = 0x0417_2710_1980;
    let parsed = Url::parse(base).map_err(|_| "invalid tracker URL".to_owned())?;
    if parsed.scheme() != "udp" {
        return Err("tracker must use UDP".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "UDP tracker has no host".to_owned())?;
    let port = parsed
        .port()
        .ok_or_else(|| "UDP tracker has no port".to_owned())?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|_| "could not resolve UDP tracker".to_owned())?
        .next()
        .ok_or_else(|| "UDP tracker resolved to no addresses".to_owned())?;
    let bind_address = if address.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let socket = UdpSocket::bind(bind_address).map_err(|error| error.to_string())?;
    socket.connect(address).map_err(|error| error.to_string())?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    socket
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;

    let connect_transaction = random_u32()?;
    let mut connect = [0_u8; 16];
    connect[..8].copy_from_slice(&PROTOCOL_ID.to_be_bytes());
    connect[8..12].copy_from_slice(&0_u32.to_be_bytes());
    connect[12..].copy_from_slice(&connect_transaction.to_be_bytes());
    socket.send(&connect).map_err(|error| error.to_string())?;
    let mut response = [0_u8; 65_536];
    let count = socket
        .recv(&mut response)
        .map_err(|error| error.to_string())?;
    if count < 16
        || u32::from_be_bytes(response[..4].try_into().unwrap()) != 0
        || u32::from_be_bytes(response[4..8].try_into().unwrap()) != connect_transaction
    {
        return Err("invalid UDP tracker connect response".into());
    }
    let connection_id = u64::from_be_bytes(response[8..16].try_into().unwrap());

    let announce_transaction = random_u32()?;
    let key = random_u32()?;
    let mut announce = Vec::with_capacity(98);
    announce.extend(connection_id.to_be_bytes());
    announce.extend(1_u32.to_be_bytes());
    announce.extend(announce_transaction.to_be_bytes());
    announce.extend(request.info_hash);
    announce.extend(request.peer_id);
    announce.extend(request.downloaded.to_be_bytes());
    announce.extend(request.left.to_be_bytes());
    announce.extend(request.uploaded.to_be_bytes());
    announce.extend(
        match request.event {
            None => 0_u32,
            Some(AnnounceEvent::Completed) => 1,
            Some(AnnounceEvent::Started) => 2,
            Some(AnnounceEvent::Stopped) => 3,
        }
        .to_be_bytes(),
    );
    announce.extend(0_u32.to_be_bytes());
    announce.extend(key.to_be_bytes());
    announce.extend((-1_i32).to_be_bytes());
    announce.extend(request.port.to_be_bytes());
    socket.send(&announce).map_err(|error| error.to_string())?;
    let count = socket
        .recv(&mut response)
        .map_err(|error| error.to_string())?;
    if count < 20
        || u32::from_be_bytes(response[..4].try_into().unwrap()) != 1
        || u32::from_be_bytes(response[4..8].try_into().unwrap()) != announce_transaction
    {
        return Err("invalid UDP tracker announce response".into());
    }
    let peers = &response[20..count];
    if !peers.len().is_multiple_of(6) {
        return Err("invalid UDP compact IPv4 peers".into());
    }
    Ok(AnnounceResponse {
        interval_seconds: u32::from_be_bytes(response[8..12].try_into().unwrap()).into(),
        compact_peers: peers.to_vec(),
    })
}

fn random_u32() -> Result<u32, String> {
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes)
        .map_err(|_| "operating-system randomness unavailable".to_owned())?;
    Ok(u32::from_ne_bytes(bytes))
}

pub fn parse_announce_response(input: &[u8]) -> Result<AnnounceResponse, String> {
    let interval_marker = b"8:intervali";
    let start = find_bytes(input, interval_marker).ok_or("tracker response has no interval")?
        + interval_marker.len();
    let end = input[start..]
        .iter()
        .position(|byte| *byte == b'e')
        .ok_or("invalid tracker interval")?
        + start;
    let interval_seconds = std::str::from_utf8(&input[start..end])
        .map_err(|_| "invalid tracker interval")?
        .parse()
        .map_err(|_| "invalid tracker interval")?;
    let peers_marker = b"5:peers";
    let peers_start = find_bytes(input, peers_marker).ok_or("tracker response has no peers")?
        + peers_marker.len();
    let colon = input[peers_start..]
        .iter()
        .position(|byte| *byte == b':')
        .ok_or("invalid compact peers")?
        + peers_start;
    let length: usize = std::str::from_utf8(&input[peers_start..colon])
        .map_err(|_| "invalid compact peers")?
        .parse()
        .map_err(|_| "invalid compact peers")?;
    let data_start = colon + 1;
    let data_end = data_start
        .checked_add(length)
        .filter(|end| *end <= input.len())
        .ok_or("truncated compact peers")?;
    if !length.is_multiple_of(6) {
        return Err("compact IPv4 peers length must be divisible by six".into());
    }
    Ok(AnnounceResponse {
        interval_seconds,
        compact_peers: input[data_start..data_end].to_vec(),
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 3);
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push('%');
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 15) as usize] as char);
        }
    }
    output
}

fn hex_nibble(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("info-hash contains a non-hexadecimal character".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, UdpSocket},
    };

    fn request() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: [1; 20],
            peer_id: *b"-PL0001-123456789012",
            port: 51413,
            uploaded: 7,
            downloaded: 11,
            left: 13,
            event: Some(AnnounceEvent::Started),
        }
    }

    #[test]
    fn announce_url_contains_exact_accounting_values() {
        let url =
            build_announce_url("https://tracker.test/announce?passkey=secret", &request()).unwrap();
        assert!(url.contains("uploaded=7"));
        assert!(url.contains("downloaded=11"));
        assert!(url.contains("left=13"));
        assert!(url.contains("event=started"));
        let mut completed = request();
        completed.event = Some(AnnounceEvent::Completed);
        assert!(
            build_announce_url("https://tracker.test/announce", &completed)
                .unwrap()
                .contains("event=completed")
        );
        completed.event = Some(AnnounceEvent::Stopped);
        assert!(
            build_announce_url("https://tracker.test/announce", &completed)
                .unwrap()
                .contains("event=stopped")
        );
    }

    #[test]
    fn audit_http_rejects_large_advertised_and_unframed_bodies() {
        use std::io::{BufRead, BufReader};
        for advertised in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/announce", listener.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n").unwrap();
                let length = librqbit_tracker_comms::MAX_TRACKER_RESPONSE_BYTES + 1;
                if advertised {
                    write!(stream, "Content-Length: {length}\r\n").unwrap();
                }
                write!(stream, "\r\n").unwrap();
                if !advertised {
                    let _ = stream.write_all(&vec![b'x'; length]);
                }
            });
            let error = announce_http(&url, &request()).unwrap_err();
            worker.join().unwrap();
            assert!(error.contains("1 MiB"), "{error}");
        }
    }

    #[test]
    fn audit_helpers_restrict_targets_and_decode_identifiers_and_peers() {
        assert!(validate_audit_target("http://localhost:8080/announce", &[]).is_ok());
        assert!(validate_audit_target("udp://127.0.0.1:8080/announce", &[]).is_ok());
        assert!(validate_audit_target("http://[::1]:8080/announce", &[]).is_ok());
        assert!(validate_audit_target("https://tracker.example/announce", &[]).is_err());
        assert!(
            validate_audit_target(
                "https://tracker.example/announce",
                &["tracker.example".into()]
            )
            .is_ok()
        );
        assert!(parse_info_hash_hex("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(parse_info_hash_hex("abcd").is_err());
        assert_eq!(
            compact_ipv4_peers(&[127, 0, 0, 1, 0xc8, 0xd5]).unwrap(),
            ["127.0.0.1:51413".parse().unwrap()]
        );
    }

    #[test]
    fn info_hash_chunks_preserve_all_bytes_and_reject_invalid_hex() {
        let value = "000102030405060708090a0b0c0d0e0f10111213";
        let expected = std::array::from_fn(|index| index as u8);
        assert_eq!(parse_info_hash_hex(value).unwrap(), expected);
        assert_eq!(
            parse_info_hash_hex(&value.to_uppercase()).unwrap(),
            expected
        );
        assert!(parse_info_hash_hex(&"zz".repeat(20)).is_err());
        assert!(parse_info_hash_hex(&"0".repeat(39)).is_err());
        assert!(parse_info_hash_hex(&"0".repeat(41)).is_err());
    }

    #[test]
    fn compact_peers_reject_partial_chunks_and_preserve_each_address() {
        assert!(compact_ipv4_peers(&[]).unwrap().is_empty());
        for length in [1, 2, 3, 4, 5, 7, 11, 13] {
            assert!(compact_ipv4_peers(&vec![0; length]).is_err());
        }
        assert_eq!(
            compact_ipv4_peers(&[127, 0, 0, 1, 0xc8, 0xd5, 192, 0, 2, 1, 0, 80]).unwrap(),
            [
                "127.0.0.1:51413".parse().unwrap(),
                "192.0.2.1:80".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn local_http_tracker_receives_announce_and_returns_peers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_bytes = [0; 4096];
            let count = stream.read(&mut request_bytes).unwrap();
            let text = String::from_utf8_lossy(&request_bytes[..count]);
            assert!(text.contains("downloaded=11"));
            let body = b"d8:intervali60e5:peers6:\x7f\x00\x00\x01\xc8\xd5e";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let response = announce_http(&format!("http://{address}/announce"), &request()).unwrap();
        assert_eq!(response.interval_seconds, 60);
        assert_eq!(response.compact_peers.len(), 6);
        server.join().unwrap();
    }

    #[test]
    fn tracker_pool_uses_tiers_and_exponential_retry() {
        let mut pool = TrackerPool::new(vec![
            vec!["https://primary.test/announce".into()],
            vec!["udp://backup.test:80/announce".into()],
        ])
        .unwrap();
        let (primary, _) = pool.next_ready(100).unwrap();
        assert_eq!(primary, 0);
        assert_eq!(pool.record_failure(primary, 100).unwrap(), 15);
        let (backup, url) = pool.next_ready(100).unwrap();
        assert_eq!(backup, 1);
        assert!(url.starts_with("udp://"));
        pool.record_success(backup, 100, 60).unwrap();
        assert!(pool.next_ready(114).is_none());
        assert_eq!(pool.next_ready(115).unwrap().0, primary);
        assert_eq!(pool.record_failure(primary, 115).unwrap(), 30);
    }

    #[test]
    fn tracker_pool_keeps_credential_url_only_in_memory() {
        let pool = TrackerPool::new(vec![vec![
            "https://tracker.test/private-passkey/announce?token=secret".into(),
        ]])
        .unwrap();
        let (_, url) = pool.next_ready(0).unwrap();
        assert!(url.contains("private-passkey"));
        assert!(url.contains("token=secret"));
        assert!(!crate::identity::redact_tracker_url(url).contains("token=secret"));
    }

    #[test]
    fn local_udp_tracker_validates_transactions_and_exact_accounting() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut request_bytes = [0_u8; 256];
            let (count, source) = socket.recv_from(&mut request_bytes).unwrap();
            assert_eq!(count, 16);
            assert_eq!(
                u64::from_be_bytes(request_bytes[..8].try_into().unwrap()),
                0x0417_2710_1980
            );
            let transaction = &request_bytes[12..16];
            let connection_id = 42_u64;
            let mut response = Vec::new();
            response.extend(0_u32.to_be_bytes());
            response.extend(transaction);
            response.extend(connection_id.to_be_bytes());
            socket.send_to(&response, source).unwrap();

            let (count, source) = socket.recv_from(&mut request_bytes).unwrap();
            assert_eq!(count, 98);
            assert_eq!(
                u64::from_be_bytes(request_bytes[..8].try_into().unwrap()),
                connection_id
            );
            assert_eq!(
                u64::from_be_bytes(request_bytes[56..64].try_into().unwrap()),
                11
            );
            assert_eq!(
                u64::from_be_bytes(request_bytes[64..72].try_into().unwrap()),
                13
            );
            assert_eq!(
                u64::from_be_bytes(request_bytes[72..80].try_into().unwrap()),
                7
            );
            assert_eq!(
                u32::from_be_bytes(request_bytes[80..84].try_into().unwrap()),
                2
            );
            let transaction = &request_bytes[12..16];
            let mut response = Vec::new();
            response.extend(1_u32.to_be_bytes());
            response.extend(transaction);
            response.extend(60_u32.to_be_bytes());
            response.extend(2_u32.to_be_bytes());
            response.extend(3_u32.to_be_bytes());
            response.extend([127, 0, 0, 1, 200, 213]);
            socket.send_to(&response, source).unwrap();
        });

        let response = announce_udp_with_timeout(
            &format!("udp://{address}/announce"),
            &request(),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(response.interval_seconds, 60);
        assert_eq!(response.compact_peers, [127, 0, 0, 1, 200, 213]);
        server.join().unwrap();
    }
}
