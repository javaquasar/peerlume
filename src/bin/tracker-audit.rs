use std::{
    env,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use peerlume::tracker::{
    AnnounceEvent, AnnounceRequest, AnnounceResponse, announce_for_audit, compact_ipv4_peers,
    parse_info_hash_hex, validate_audit_target,
};
use serde::Serialize;

const DEFAULT_PEER_ID: [u8; 20] = *b"-PLAUD1-000000000001";
const TRACKER_URL_ENV: &str = "PEERLUME_AUDIT_TRACKER_URL";

#[derive(Debug, PartialEq, Eq)]
struct AuditArgs {
    tracker: String,
    allow_hosts: Vec<String>,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    port: u16,
    uploaded: u64,
    downloaded: u64,
    left: u64,
    event: Option<AnnounceEvent>,
    run_id: String,
}

#[derive(Serialize)]
struct AuditReport {
    run_id: String,
    transport: String,
    tracker_host: String,
    event: Option<&'static str>,
    uploaded: u64,
    downloaded: u64,
    left: u64,
    interval_seconds: u64,
    peers: Vec<String>,
}

fn main() -> ExitCode {
    let raw_args: Vec<_> = env::args().skip(1).collect();
    if raw_args
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }

    match run(raw_args, env::var(TRACKER_URL_ENV).ok()) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("tracker-audit: could not encode report: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("tracker-audit: {error}");
            eprintln!("Run with --help for usage.");
            ExitCode::from(2)
        }
    }
}

fn run(raw_args: Vec<String>, tracker_from_env: Option<String>) -> Result<AuditReport, String> {
    let args = parse_args(raw_args, tracker_from_env)?;
    let parsed = validate_audit_target(&args.tracker, &args.allow_hosts)?;
    let transport = parsed.scheme().to_owned();
    let tracker_host = parsed
        .host_str()
        .ok_or_else(|| "tracker URL has no hostname".to_owned())?
        .to_owned();
    let request = AnnounceRequest {
        info_hash: args.info_hash,
        peer_id: args.peer_id,
        port: args.port,
        uploaded: args.uploaded,
        downloaded: args.downloaded,
        left: args.left,
        event: args.event,
    };
    let response = announce_for_audit(&args.tracker, &request, &args.allow_hosts)?;

    Ok(report(args, response, transport, tracker_host))
}

fn parse_args(
    raw_args: Vec<String>,
    tracker_from_env: Option<String>,
) -> Result<AuditArgs, String> {
    let mut tracker = None;
    let mut allow_hosts = Vec::new();
    let mut info_hash = None;
    let mut peer_id = DEFAULT_PEER_ID;
    let mut port = 51_413;
    let mut uploaded = None;
    let mut downloaded = None;
    let mut left = None;
    let mut event = None;
    let mut run_id = None;
    let mut arguments = raw_args.into_iter();

    while let Some(flag) = arguments.next() {
        let value = |arguments: &mut std::vec::IntoIter<String>| {
            arguments
                .next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--tracker" => tracker = Some(value(&mut arguments)?),
            "--allow-host" => allow_hosts.push(value(&mut arguments)?),
            "--info-hash" => info_hash = Some(parse_info_hash_hex(&value(&mut arguments)?)?),
            "--peer-id" => peer_id = parse_peer_id(&value(&mut arguments)?)?,
            "--port" => port = parse_number("port", &value(&mut arguments)?)?,
            "--uploaded" => uploaded = Some(parse_number("uploaded", &value(&mut arguments)?)?),
            "--downloaded" => {
                downloaded = Some(parse_number("downloaded", &value(&mut arguments)?)?)
            }
            "--left" => left = Some(parse_number("left", &value(&mut arguments)?)?),
            "--event" => event = parse_event(&value(&mut arguments)?)?,
            "--run-id" => run_id = Some(value(&mut arguments)?),
            unknown => return Err(format!("unknown argument {unknown}")),
        }
    }

    let tracker = tracker
        .or(tracker_from_env)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!("set {TRACKER_URL_ENV} or pass --tracker with an audit tracker URL")
        })?;
    let run_id = run_id.unwrap_or_else(default_run_id);
    if run_id.trim().is_empty() {
        return Err("run-id cannot be empty".into());
    }

    Ok(AuditArgs {
        tracker,
        allow_hosts,
        info_hash: info_hash.ok_or_else(|| "--info-hash is required".to_owned())?,
        peer_id,
        port,
        uploaded: uploaded.ok_or_else(|| "--uploaded is required".to_owned())?,
        downloaded: downloaded.ok_or_else(|| "--downloaded is required".to_owned())?,
        left: left.ok_or_else(|| "--left is required".to_owned())?,
        event,
        run_id,
    })
}

fn report(
    args: AuditArgs,
    response: AnnounceResponse,
    transport: String,
    tracker_host: String,
) -> AuditReport {
    AuditReport {
        run_id: args.run_id,
        transport,
        tracker_host,
        event: args.event.map(event_name),
        uploaded: args.uploaded,
        downloaded: args.downloaded,
        left: args.left,
        interval_seconds: response.interval_seconds,
        peers: compact_ipv4_peers(&response.compact_peers)
            .unwrap_or_default()
            .into_iter()
            .map(|peer| peer.to_string())
            .collect(),
    }
}

fn parse_peer_id(value: &str) -> Result<[u8; 20], String> {
    if !value.is_ascii() || value.len() != 20 {
        return Err("peer-id must contain exactly 20 ASCII bytes".into());
    }
    value
        .as_bytes()
        .try_into()
        .map_err(|_| "peer-id must contain exactly 20 ASCII bytes".into())
}

fn parse_number<T>(name: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| format!("{name} must be a non-negative integer in range"))
}

fn parse_event(value: &str) -> Result<Option<AnnounceEvent>, String> {
    match value {
        "none" => Ok(None),
        "started" => Ok(Some(AnnounceEvent::Started)),
        "completed" => Ok(Some(AnnounceEvent::Completed)),
        "stopped" => Ok(Some(AnnounceEvent::Stopped)),
        _ => Err("event must be none, started, completed, or stopped".into()),
    }
}

fn event_name(event: AnnounceEvent) -> &'static str {
    match event {
        AnnounceEvent::Started => "started",
        AnnounceEvent::Completed => "completed",
        AnnounceEvent::Stopped => "stopped",
    }
}

fn default_run_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    format!("audit-{millis}")
}

fn usage() -> &'static str {
    r#"Usage:
  tracker-audit --info-hash <40 hex chars> --uploaded <bytes> \
    --downloaded <bytes> --left <bytes> [options]

Tracker URL:
  Set PEERLUME_AUDIT_TRACKER_URL (recommended for credential-bearing URLs),
  or pass --tracker <http(s)://...|udp://...>.

Options:
  --allow-host <hostname>  Explicitly allow one non-loopback staging host; repeatable
  --peer-id <20 ASCII>     Stable audit peer ID (default: -PLAUD1-000000000001)
  --port <1..65535>        Announced listening port (default: 51413)
  --event <value>          none, started, completed, or stopped (default: none)
  --run-id <value>         Audit run identifier included in the JSON report

The command sends exactly one announce, prints the returned compact IPv4 peer list
as JSON, and never prints the credential-bearing tracker URL."#
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn required_args() -> Vec<String> {
        [
            "--info-hash",
            "0123456789abcdef0123456789abcdef01234567",
            "--uploaded",
            "7",
            "--downloaded",
            "11",
            "--left",
            "13",
            "--event",
            "started",
            "--run-id",
            "test-run",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn parses_explicit_metrics_without_normalizing_them() {
        let args = parse_args(
            required_args(),
            Some("http://127.0.0.1:8080/announce".into()),
        )
        .unwrap();
        assert_eq!(args.uploaded, 7);
        assert_eq!(args.downloaded, 11);
        assert_eq!(args.left, 13);
        assert_eq!(args.event, Some(AnnounceEvent::Started));
        assert_eq!(args.peer_id, DEFAULT_PEER_ID);
    }

    #[test]
    fn rejects_external_tracker_without_exact_allowlist_entry() {
        let error = validate_audit_target("https://tracker.example/announce", &[]).unwrap_err();
        assert!(error.contains("allowlist"));
        assert!(
            validate_audit_target(
                "https://tracker.example/announce",
                &["tracker.example".into()]
            )
            .is_ok()
        );
    }

    #[test]
    fn loopback_targets_are_allowed_by_default() {
        assert!(validate_audit_target("http://localhost:8080/announce", &[]).is_ok());
        assert!(validate_audit_target("udp://127.0.0.1:8080/announce", &[]).is_ok());
        assert!(validate_audit_target("http://[::1]:8080/announce", &[]).is_ok());
    }

    #[test]
    fn compact_peer_list_is_rendered_as_addresses() {
        assert_eq!(
            compact_ipv4_peers(&[127, 0, 0, 1, 0xc8, 0xd5]).unwrap(),
            ["127.0.0.1:51413".parse().unwrap()]
        );
    }

    #[test]
    fn malformed_identifiers_are_rejected() {
        assert!(parse_info_hash_hex("abcd").is_err());
        assert!(parse_info_hash_hex("z123456789abcdef0123456789abcdef01234567").is_err());
        assert!(parse_peer_id("short").is_err());
    }

    #[test]
    fn sends_selected_metrics_and_returns_the_tracker_peer_list() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("uploaded=7"));
            assert!(request.contains("downloaded=11"));
            assert!(request.contains("left=13"));
            assert!(request.contains("event=started"));

            let body = b"d8:intervali60e5:peers6:\x7f\x00\x00\x01\xc8\xd5e";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let mut arguments = required_args();
        arguments.extend(["--tracker".into(), format!("http://{address}/announce")]);
        let report = run(arguments, None).unwrap();

        assert_eq!(report.interval_seconds, 60);
        assert_eq!(report.peers, ["127.0.0.1:51413"]);
        server.join().unwrap();
    }
}
