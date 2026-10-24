use std::collections::HashSet;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::SocketAddrV6;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::StreamExt;
use futures::stream::BoxStream;
use tracing::debug;
use tracing::trace;
use url::Url;

use crate::tracker_comms_http;
use crate::tracker_comms_udp;
mod lifecycle;
pub use lifecycle::{forget_torrent_lifecycle, stop_session_announces, stop_torrent_announces};

/// Cap decoded HTTP bodies before bencode parsing or peer-list allocation.
pub const MAX_TRACKER_RESPONSE_BYTES: usize = 1024 * 1024;

async fn bounded_response_body(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > MAX_TRACKER_RESPONSE_BYTES as u64) { anyhow::bail!("Tracker response exceeds the 1 MiB limit"); }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_TRACKER_RESPONSE_BYTES.saturating_sub(body.len()) { anyhow::bail!("Tracker response exceeds the 1 MiB limit"); }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod response_limit_tests {
    use super::*;
    use std::{io::{BufRead, BufReader, Write}, net::TcpListener};
    #[test]
    fn http_body_limit_covers_advertised_chunked_and_exact_size_responses() {
        for (length, chunked, advertised, accepted) in [
            (0, false, Some(MAX_TRACKER_RESPONSE_BYTES + 1), false),
            (MAX_TRACKER_RESPONSE_BYTES + 1, false, None, false),
            (MAX_TRACKER_RESPONSE_BYTES + 1, true, None, false),
            (MAX_TRACKER_RESPONSE_BYTES, false, Some(MAX_TRACKER_RESPONSE_BYTES), true),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/announce", listener.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(&stream);
                loop { let mut line = String::new(); if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" { break; } }
                write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n").unwrap();
                if let Some(n) = advertised { write!(stream, "Content-Length: {n}\r\n").unwrap(); }
                if chunked { write!(stream, "Transfer-Encoding: chunked\r\n").unwrap(); }
                write!(stream, "\r\n").unwrap();
                if chunked { let _ = write!(stream, "{length:x}\r\n"); }
                let _ = stream.write_all(&vec![b'x'; length]);
                if chunked { let _ = stream.write_all(b"\r\n0\r\n\r\n"); }
            });
            let result = tokio::runtime::Runtime::new().unwrap().block_on(async {
                let response = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap().get(url).send().await.unwrap();
                bounded_response_body(response).await
            });
            worker.join().unwrap();
            assert_eq!(result.is_ok(), accepted, "length={length}, chunked={chunked}");
            if let Ok(bytes) = result { assert_eq!(bytes.len(), MAX_TRACKER_RESPONSE_BYTES); }
        }
    }
}
use crate::tracker_comms_udp::UdpTrackerClient;
use librqbit_core::hash_id::Id20;

pub struct TrackerComms {
    info_hash: Id20,
    peer_id: Id20,
    stats: Box<dyn TorrentStatsProvider>,
    force_tracker_interval: Option<Duration>,
    tx: Sender,
    // This MUST be set as trackers don't work with 0 port.
    announce_port: u16,
    reqwest_client: reqwest::Client,
    key: u32,
    reporting: Arc<crate::ReportingPolicy>,
    hooks: Option<crate::observation::Hooks>,
}

#[derive(Default)]
pub enum TrackerCommsStatsState {
    #[default]
    None,
    Initializing,
    Paused,
    Live,
}

#[derive(Default)]
pub struct TrackerCommsStats {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub torrent_state: TrackerCommsStatsState,
}

impl TrackerCommsStats {
    pub fn get_left_to_download_bytes(&self) -> u64 {
        let total = self.total_bytes;
        let down = self.downloaded_bytes;
        if total >= down {
            return total - down;
        }
        0
    }

    pub fn is_completed(&self) -> bool {
        self.downloaded_bytes >= self.total_bytes
    }
}

pub trait TorrentStatsProvider: Send + Sync {
    fn get(&self) -> TrackerCommsStats;
}

impl TorrentStatsProvider for () {
    fn get(&self) -> TrackerCommsStats {
        Default::default()
    }
}

type Sender = tokio::sync::mpsc::Sender<SocketAddr>;

#[derive(Clone)]
enum SupportedTracker {
    Udp(Url),
    Http(Url),
}

impl std::fmt::Debug for SupportedTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SupportedTracker::Udp(u) => std::fmt::Display::fmt(u, f),
            SupportedTracker::Http(u) => std::fmt::Display::fmt(u, f),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum UdpTrackerResolveResult {
    One(SocketAddr),
    Two(SocketAddrV4, SocketAddrV6),
}

async fn udp_tracker_to_socket_addrs(
    host: url::Host<&str>,
    port: u16,
) -> anyhow::Result<UdpTrackerResolveResult> {
    let res = match host {
        url::Host::Domain(name) => {
            // Use the first IPv4 and the first IPv6 addresses only.

            let mut v4: Option<SocketAddrV4> = None;
            let mut v6: Option<SocketAddrV6> = None;
            for addr in tokio::net::lookup_host((name, port))
                .await
                .with_context(|| format!("error looking up hostname {name}"))?
            {
                match (v4, v6, addr) {
                    (None, _, SocketAddr::V4(addr)) => v4 = Some(addr),
                    (_, None, SocketAddr::V6(addr)) => v6 = Some(addr),
                    _ => continue,
                }
            }
            let res = match (v4, v6) {
                (Some(v4), Some(v6)) => UdpTrackerResolveResult::Two(v4, v6),
                (Some(v4), None) => UdpTrackerResolveResult::One(v4.into()),
                (None, Some(v6)) => UdpTrackerResolveResult::One(v6.into()),
                _ => anyhow::bail!("zero addresses returned looking up {name}"),
            };
            trace!(?res, "resolved");
            res
        }
        url::Host::Ipv4(addr) => UdpTrackerResolveResult::One((addr, port).into()),
        url::Host::Ipv6(addr) => UdpTrackerResolveResult::One((addr, port).into()),
    };
    Ok(res)
}

impl TrackerComms {
    fn observe(
        &self,
        tracker: String,
        transport: &str,
        stats: &TrackerCommsStats,
        event: Option<&str>,
    ) -> crate::observation::Attempt {
        let snapshot = self
            .hooks
            .as_ref()
            .map(|h| (h.transfers)(self.info_hash.0))
            .unwrap_or(crate::TransferSnapshot {
                downloaded: stats.downloaded_bytes,
                uploaded: stats.uploaded_bytes,
                ..Default::default()
            });
        let verified_bytes = snapshot.verified_bytes.unwrap_or(stats.downloaded_bytes);
        let full_size = snapshot.full_size.unwrap_or(stats.total_bytes);
        let actual = crate::AnnounceMetrics {
            downloaded: snapshot.downloaded,
            uploaded: snapshot.uploaded,
            left: full_size.saturating_sub(verified_bytes),
        };
        crate::observation::Attempt::new(
            crate::AnnounceObservation {
                started_unix_ms: crate::observation::unix_ms(),
                elapsed_ms: 0,
                info_hash: crate::observation::hex(&self.info_hash.0),
                peer_id_hex: crate::observation::hex(&self.peer_id.0),
                tracker,
                transport: transport.into(),
                mode: self.reporting.mode_id().into(),
                event: event.map(str::to_owned),
                port: self.announce_port,
                actual,
                verified_bytes,
                full_size,
                selected_bytes: snapshot.selected_bytes,
                request_transmitted: None,
                sent: self.reporting.apply_metrics(actual),
                outcome: "cancelled".into(),
                reply: Default::default(),
            },
            self.hooks.as_ref().map(|h| h.observer.clone()),
        )
    }
    // TODO: fix too many args
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        info_hash: Id20,
        peer_id: Id20,
        trackers: HashSet<Url>,
        stats: Box<dyn TorrentStatsProvider>,
        force_interval: Option<Duration>,
        announce_port: u16,
        reqwest_client: reqwest::Client,
        udp_client: UdpTrackerClient,
    ) -> Option<BoxStream<'static, SocketAddr>> {
        let trackers = trackers
            .into_iter()
            .filter_map(|t| match t.scheme() {
                "http" | "https" => Some(SupportedTracker::Http(t)),
                "udp" => Some(SupportedTracker::Udp(t)),
                _ => {
                    debug!("unsupported tracker URL: {}", t);
                    None
                }
            })
            .collect::<Vec<_>>();
        if trackers.is_empty() {
            debug!(?info_hash, "trackers list is empty");
            return None;
        }

        tracing::trace!(?trackers);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<SocketAddr>(16);

        let reporting = crate::reporting::policy_for(info_hash.0, peer_id.0);
        let hooks = crate::observation::hooks_for(peer_id.0);
        let comms = Arc::new(Self {
                info_hash,
                peer_id,
                stats,
                force_tracker_interval: force_interval,
                tx,
                announce_port,
                reqwest_client,
                key: rand::random(),
                reporting,
                hooks,
            });
        let monitors: Vec<_> = trackers.into_iter().map(|tracker|
            lifecycle::Monitor::new(comms.clone(), tracker, udp_client.clone())).collect();
        let s = async_stream::stream! {
            // JoinSet aborts workers when the engine drops this stream. Workers
            // keep lifecycle timers alive even while the peer consumer is not
            // polling a yielded address.
            let mut tasks = tokio::task::JoinSet::new();
            for monitor in monitors {
                tasks.spawn(async move { monitor.run().await });
            }
            while !tasks.is_empty() {
                tokio::select! {
                    addr = rx.recv() => {
                        if let Some(addr) = addr {
                            yield addr;
                        }
                    }
                    e = tasks.join_next(), if !tasks.is_empty() => {
                        if let Some(Ok(Err(e))) = e {
                            debug!("error: {e}");
                        }
                    }
                }
            }
        };

        Some(s.boxed())
    }

    async fn tracker_one_request_http(
        &self,
        tracker_url: &Url,
        event: Option<tracker_comms_http::TrackerRequestEvent>,
    ) -> anyhow::Result<(Duration, Vec<SocketAddr>)> {
        let stats = self.stats.get();
        let event_name = event.map(|e| match e {
            tracker_comms_http::TrackerRequestEvent::Started => "started",
            tracker_comms_http::TrackerRequestEvent::Stopped => "stopped",
            tracker_comms_http::TrackerRequestEvent::Completed => "completed",
        });
        let mut log = self.observe(
            crate::observation::redacted_tracker(tracker_url),
            tracker_url.scheme(),
            &stats,
            event_name,
        );
        let sent = log.record.sent;
        let request = tracker_comms_http::TrackerRequest {
            info_hash: &self.info_hash,
            peer_id: &self.peer_id,
            port: self.announce_port,
            uploaded: sent.uploaded,
            downloaded: sent.downloaded,
            left: sent.left,
            compact: true,
            no_peer_id: false,
            event,
            ip: None,
            numwant: None,
            key: Some(self.key),
            trackerid: None,
        };

        let mut url = tracker_url.clone();

        let mut queries = request.as_querystring();
        if let Some(url_query) = url.query() {
            queries.push_str(&format!("&{}", url_query));
        }
        url.set_query(Some(&queries));

        let result = async {
            let response: reqwest::Response =
                self.reqwest_client.get(url).send().await.map_err(|e| {
                    log.record.outcome = if e.is_timeout() {
                        "timeout"
                    } else {
                        "transport_error"
                    }
                    .into();
                    e
                })?;
            log.record.reply.http_status = Some(response.status().as_u16());
            log.record.request_transmitted = Some(true);
            if !response.status().is_success() {
                log.record.outcome = "http_error".into();
                anyhow::bail!("tracker responded with {:?}", response.status());
            }
            log.record.outcome = "invalid_response".into();
            let bytes = match bounded_response_body(response).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    log.record.outcome = "response_read_error".into();
                    // Fixed wording: never propagate an HTTP error containing a credential URL.
                    log.record.reply.failure = Some("Tracker response could not be read within the 1 MiB limit".into());
                    return Err(error);
                }
            };
            if let Ok((error, _)) =
                bencode::from_bytes_with_rest::<tracker_comms_http::TrackerError>(&bytes)
            {
                log.record.outcome = "tracker_failure".into();
                log.record.reply.failure = Some(crate::observation::safe_message(
                    &error.failure_reason.to_string(),
                    tracker_url,
                ));
                anyhow::bail!(
                    "tracker returned failure. Failure reason: {}",
                    error.failure_reason
                )
            };
            let response =
                bencode::from_bytes_with_rest::<tracker_comms_http::TrackerResponse>(&bytes)
                    .map_err(|e| {
                        tracing::trace!("error deserializing TrackerResponse: {e:#}");
                        e.into_kind()
                    })?
                    .0;
            log.record.outcome = "ok".into();
            log.record.reply.interval = Some(response.interval);
            log.record.reply.min_interval = response.min_interval;
            log.record.reply.seeders = response.complete;
            log.record.reply.leechers = response.incomplete;
            log.record.reply.warning = response
                .warning_message
                .as_ref()
                .map(|v| crate::observation::safe_message(&v.to_string(), tracker_url));
            log.record.reply.peers = response.iter_peers().collect();
            Ok::<_, anyhow::Error>(Duration::from_secs(
                response.min_interval.unwrap_or(response.interval),
            ))
        }
        .await;
        let peers = log.record.reply.peers.clone();
        log.finish();
        let interval = result?;
        Ok((interval, peers))
    }

    async fn tracker_one_request_udp(
        &self,
        tracker_url: &Url,
        addr: SocketAddr,
        client: &UdpTrackerClient,
        event: Option<tracker_comms_http::TrackerRequestEvent>,
    ) -> anyhow::Result<(Duration, Vec<SocketAddr>)> {
        use tracker_comms_udp::*;

        let stats = self.stats.get();
        let mut log = self.observe(
            crate::observation::redacted_tracker(tracker_url),
            "udp",
            &stats,
            None,
        );
        log.record.request_transmitted = Some(false);
        let sent = log.record.sent;
        let request = AnnounceFields {
            info_hash: self.info_hash,
            peer_id: self.peer_id,
            downloaded: sent.downloaded,
            left: sent.left,
            uploaded: sent.uploaded,
            event: match event {
                None => EVENT_NONE,
                Some(tracker_comms_http::TrackerRequestEvent::Started) => EVENT_STARTED,
                Some(tracker_comms_http::TrackerRequestEvent::Completed) => EVENT_COMPLETED,
                Some(tracker_comms_http::TrackerRequestEvent::Stopped) => EVENT_STOPPED,
            },
            key: self.key,
            port: self.announce_port,
        };
        log.record.event = match request.event {
            EVENT_STARTED => Some("started"),
            EVENT_COMPLETED => Some("completed"),
            EVENT_STOPPED => Some("stopped"),
            _ => None,
        }
        .map(str::to_owned);

        match client
            .announce_with_send_hook(addr, request, || {
                log.record.request_transmitted = Some(true)
            })
            .await
        {
            Ok(response) => {
                log.record.outcome = "ok".into();
                log.record.reply.interval = Some(u64::from(response.interval));
                log.record.reply.seeders = Some(u64::from(response.seeders));
                log.record.reply.leechers = Some(u64::from(response.leechers));
                log.record.reply.peers = response.addrs.clone();
                log.finish();
                trace!(len = response.addrs.len(), "received announce response");
                let sleep = response.interval.max(5);
                let sleep = Duration::from_secs(sleep as u64);
                Ok((sleep, response.addrs))
            }
            Err(e) => {
                log.record.outcome = "udp_error".into();
                log.record.reply.failure = Some(crate::observation::safe_message(
                    &format!("{e:#}"),
                    tracker_url,
                ));
                log.finish();
                debug!(?addr, "error reading announce response: {e:#}");
                Err(e)
            }
        }
    }
}
