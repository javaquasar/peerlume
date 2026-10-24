use super::*;
use crate::{AnnounceObservation, ReportingMode, ReportingPolicy, TransferSnapshot};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

struct Tracker {
    url: Url,
    worker: tokio::task::JoinHandle<()>,
    fail_completion: Arc<AtomicUsize>,
    wire_events: Arc<Mutex<Vec<Option<String>>>>,
}
impl Drop for Tracker {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
impl Tracker {
    async fn new(udp: bool, peers: usize, hang_stop: bool) -> Self {
        let fail_completion = Arc::new(AtomicUsize::new(0));
        let fail = fail_completion.clone();
        let wire_events = Arc::new(Mutex::new(Vec::new()));
        let received = wire_events.clone();
        if udp {
            let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let url =
                Url::parse(&format!("udp://{}/announce", socket.local_addr().unwrap())).unwrap();
            let worker = tokio::spawn(async move {
                let mut buffer = [0; 1024];
                loop {
                    let (len, addr) = socket.recv_from(&mut buffer).await.unwrap();
                    let action = u32::from_be_bytes(buffer[8..12].try_into().unwrap());
                    let mut reply = Vec::from(&buffer[8..16]);
                    if action == 0 {
                        reply.extend(42u64.to_be_bytes());
                    } else {
                        assert_eq!(len, 98);
                        let event = u32::from_be_bytes(buffer[80..84].try_into().unwrap());
                        received.lock().unwrap().push(match event {
                            1 => Some("completed".into()),
                            2 => Some("started".into()),
                            3 => Some("stopped".into()),
                            0 => None,
                            _ => panic!("invalid wire event"),
                        });
                        if hang_stop && event == 3 {
                            continue;
                        }
                        if event == 1
                            && fail
                                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                    n.checked_sub(1)
                                })
                                .is_ok()
                        {
                            reply[..4].copy_from_slice(&3u32.to_be_bytes());
                            reply.extend(b"intentional rejection");
                        } else {
                            reply.extend(3600u32.to_be_bytes());
                            reply.extend([0; 8]);
                            for _ in 0..peers {
                                reply.extend([127, 0, 0, 1, 0x1a, 0xe1]);
                            }
                        }
                    }
                    socket.send_to(&reply, addr).await.unwrap();
                }
            });
            Self {
                url,
                worker,
                fail_completion,
                wire_events,
            }
        } else {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = Url::parse(&format!(
                "http://{}/secret/announce?passkey=hidden",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let worker = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    while !bytes.ends_with(b"\r\n\r\n") {
                        let mut b = [0];
                        if stream.read_exact(&mut b).await.is_err() {
                            break;
                        }
                        bytes.push(b[0]);
                    }
                    let text = String::from_utf8(bytes).unwrap();
                    let path = text.split_whitespace().nth(1).unwrap();
                    let url = Url::parse(&format!("http://localhost{path}")).unwrap();
                    let event = url
                        .query_pairs()
                        .find(|(k, _)| k == "event")
                        .map(|(_, v)| v.into_owned());
                    received.lock().unwrap().push(event.clone());
                    if hang_stop && event.as_deref() == Some("stopped") {
                        std::future::pending::<()>().await;
                    }
                    let failed = event.as_deref() == Some("completed")
                        && fail
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                n.checked_sub(1)
                            })
                            .is_ok();
                    let mut body = format!("d8:intervali3600e5:peers{}:", peers * 6).into_bytes();
                    for _ in 0..peers {
                        body.extend([127, 0, 0, 1, 0x1a, 0xe1]);
                    }
                    body.push(b'e');
                    let status = if failed { 500 } else { 200 };
                    let header = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                }
            });
            Self {
                url,
                worker,
                fail_completion,
                wire_events,
            }
        }
    }
}

struct Stats(Arc<Mutex<TransferSnapshot>>);
impl TorrentStatsProvider for Stats {
    fn get(&self) -> TrackerCommsStats {
        let s = *self.0.lock().unwrap();
        TrackerCommsStats {
            uploaded_bytes: s.uploaded,
            downloaded_bytes: s.verified_bytes.unwrap_or(0),
            total_bytes: s.full_size.unwrap_or(0),
            torrent_state: TrackerCommsStatsState::Live,
        }
    }
}
struct Harness {
    monitor: Arc<Monitor>,
    snapshot: Arc<Mutex<TransferSnapshot>>,
    observations: Arc<Mutex<Vec<AnnounceObservation>>>,
    _registration: crate::ObservationRegistration,
    _rx: tokio::sync::mpsc::Receiver<SocketAddr>,
    worker: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.worker.abort();
        self.cancel.cancel();
    }
}
impl Harness {
    async fn new(tracker: &Tracker, mode: ReportingMode, verified: u64, downloaded: u64) -> Self {
        static ID: AtomicU64 = AtomicU64::new(1);
        let mut peer = [0; 20];
        peer[..8].copy_from_slice(&ID.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let hash = [91; 20];
        let snapshot = Arc::new(Mutex::new(TransferSnapshot {
            downloaded,
            uploaded: 13,
            verified_bytes: Some(verified),
            full_size: Some(32768),
            selected_bytes: Some(32768),
        }));
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = observations.clone();
        let transfers = snapshot.clone();
        let registration = crate::register_observer(
            peer,
            Arc::new(move |r| observed.lock().unwrap().push(r)),
            Arc::new(move |_| *transfers.lock().unwrap()),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let udp = UdpTrackerClient::new(cancel.clone(), None).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let comms = Arc::new(TrackerComms {
            info_hash: Id20::new(hash),
            peer_id: Id20::new(peer),
            stats: Box::new(Stats(snapshot.clone())),
            force_tracker_interval: None,
            tx,
            announce_port: 6881,
            reqwest_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            key: 17,
            reporting: Arc::new(ReportingPolicy::new(mode, 32768, 8192)),
            hooks: crate::observation::hooks_for(peer),
        });
        let url = if tracker.url.scheme() == "udp" {
            SupportedTracker::Udp(tracker.url.clone())
        } else {
            SupportedTracker::Http(tracker.url.clone())
        };
        let monitor = Monitor::new(comms, url, udp);
        assert!(
            observations.lock().unwrap().is_empty(),
            "lifecycle probes must not create fake journal attempts"
        );
        let running = monitor.clone();
        let worker = tokio::spawn(async move {
            let _ = running.run().await;
        });
        let harness = Self {
            monitor,
            snapshot,
            observations,
            _registration: registration,
            _rx: rx,
            worker,
            cancel,
        };
        harness.wait_for(Some("started"), "ok").await;
        harness
    }
    fn records(&self) -> Vec<AnnounceObservation> {
        self.observations.lock().unwrap().clone()
    }
    async fn wait_for(&self, event: Option<&str>, outcome: &str) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if self
                    .records()
                    .iter()
                    .any(|r| r.event.as_deref() == event && r.outcome == outcome)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {event:?}/{outcome}: {:?}", self.records()));
    }
    fn complete(&self) {
        let mut s = self.snapshot.lock().unwrap();
        s.downloaded += 32768 - s.verified_bytes.unwrap();
        s.verified_bytes = Some(32768);
    }
    async fn stop(&self) {
        stop_torrent_announces(self.monitor.key().0, self.monitor.key().1).await;
    }
    fn successful(&self, event: &str) -> usize {
        self.records()
            .iter()
            .filter(|r| r.event.as_deref() == Some(event) && r.outcome == "ok")
            .count()
    }
    async fn restart(&mut self) {
        self.worker.abort();
        self.monitor = Monitor::new(
            self.monitor.comms.clone(),
            self.monitor.tracker.clone(),
            self.monitor.udp.clone(),
        );
        let running = self.monitor.clone();
        self.worker = tokio::spawn(async move {
            let _ = running.run().await;
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.successful("started") < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

#[test]
fn http_and_udp_complete_promptly_once_then_stop_before_a_long_interval() {
    run(async {
        for udp in [false, true] {
            let tracker = Tracker::new(udp, 0, false).await;
            let h = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
            h.complete();
            h.wait_for(Some("completed"), "ok").await;
            h.stop().await;
            h.stop().await;
            assert_eq!(h.successful("completed"), 1);
            assert_eq!(h.successful("stopped"), 1);
            let entries = h.records();
            assert_eq!(
                *tracker.wire_events.lock().unwrap(),
                entries.iter().map(|r| r.event.clone()).collect::<Vec<_>>(),
                "journal lifecycle events must match the actual HTTP/UDP request bytes"
            );
            assert_eq!(
                entries
                    .iter()
                    .map(|r| r.event.as_deref())
                    .collect::<Vec<_>>(),
                vec![Some("started"), Some("completed"), Some("stopped")]
            );
            for r in entries {
                assert_eq!(r.actual, r.sent);
                assert!(!r.tracker.contains("hidden"));
                assert!(!r.tracker.contains("secret"));
            }
        }
    });
}

#[test]
fn completion_and_stop_do_not_wait_for_a_blocked_peer_receiver() {
    run(async {
        for udp in [false, true] {
            let tracker = Tracker::new(udp, 3, false).await;
            let h = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
            h.complete();
            h.wait_for(Some("completed"), "ok").await;
            h.stop().await;
            assert_eq!(h.successful("completed"), 1);
            assert_eq!(h.successful("stopped"), 1);
        }
    });
}

#[test]
fn preload_partial_selection_and_zero_size_do_not_report_a_new_completion() {
    run(async {
        for udp in [false, true] {
            for (verified, downloaded, full) in
                [(32768, 0, 32768), (16384, 16384, 32768), (0, 0, 0)]
            {
                let tracker = Tracker::new(udp, 0, false).await;
                let h = Harness::new(&tracker, ReportingMode::Accurate, verified, downloaded).await;
                h.snapshot.lock().unwrap().full_size = Some(full);
                h.stop().await;
                assert_eq!(h.successful("completed"), 0);
                assert_eq!(h.successful("stopped"), 1);
            }
        }
    });
}

#[test]
fn reporting_modes_preserve_completion_suppression_and_wire_metrics() {
    run(async {
        for udp in [false, true] {
            for mode in [
                ReportingMode::FrozenProgress,
                ReportingMode::HideDownloaded,
                ReportingMode::UploadRatio15,
                ReportingMode::FreezeAfter,
            ] {
                let tracker = Tracker::new(udp, 0, false).await;
                let h = Harness::new(&tracker, mode, 16384, 16384).await;
                h.complete();
                h.stop().await;
                let allowed = matches!(
                    mode,
                    ReportingMode::HideDownloaded | ReportingMode::UploadRatio15
                );
                assert_eq!(h.successful("completed"), usize::from(allowed), "{mode:?}");
                assert_eq!(h.successful("stopped"), 1);
                for r in h.records() {
                    match mode {
                        ReportingMode::FrozenProgress => {
                            assert_eq!(r.sent.downloaded, 0);
                            assert_eq!(r.sent.uploaded, 0);
                            assert_eq!(r.sent.left, 32768);
                        }
                        ReportingMode::HideDownloaded => assert_eq!(r.sent.downloaded, 0),
                        ReportingMode::UploadRatio15 => {
                            assert_eq!(r.sent.uploaded, r.actual.downloaded * 3 / 2)
                        }
                        ReportingMode::FreezeAfter => assert_eq!(r.sent.left, 16384),
                        _ => unreachable!(),
                    }
                }
            }
        }
    });
}

#[test]
fn repairs_and_resume_keep_completion_history_but_new_add_resets_it() {
    run(async {
        for udp in [false, true] {
            let tracker = Tracker::new(udp, 0, false).await;
            let mut h = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
            h.complete();
            h.stop().await;
            h.snapshot.lock().unwrap().verified_bytes = Some(0);
            h.restart().await;
            h.complete();
            h.stop().await;
            assert_eq!(h.successful("completed"), 1);
            assert_eq!(h.successful("stopped"), 2);
            forget_torrent_lifecycle(h.monitor.key().0, h.monitor.key().1);
            h.snapshot.lock().unwrap().verified_bytes = Some(0);
            h.restart().await;
            // Wait for the third started reply before simulating the new download.
            tokio::time::timeout(Duration::from_secs(3), async {
                while h.successful("started") < 3 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            h.complete();
            h.stop().await;
            assert_eq!(h.successful("completed"), 2);
        }
    });
}

#[test]
fn rejected_completions_are_retried_not_acknowledged_and_still_stop() {
    run(async {
        for udp in [false, true] {
            let tracker = Tracker::new(udp, 0, false).await;
            tracker.fail_completion.store(1, Ordering::Relaxed);
            let h = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
            h.complete();
            h.wait_for(
                Some("completed"),
                if udp { "udp_error" } else { "http_error" },
            )
            .await;
            h.stop().await;
            assert_eq!(h.successful("completed"), 1);
            assert_eq!(h.successful("stopped"), 1);
        }
    });
}

#[test]
fn stopping_one_session_does_not_stop_another_with_the_same_hash() {
    run(async {
        let tracker = Tracker::new(false, 0, false).await;
        let a = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
        let b = Harness::new(&tracker, ReportingMode::Accurate, 0, 0).await;
        stop_session_announces(a.monitor.key().1).await;
        assert_eq!(a.successful("stopped"), 1);
        assert_eq!(b.successful("stopped"), 0);
        b.complete();
        b.stop().await;
        assert_eq!(b.successful("completed"), 1);
    });
}

#[test]
fn unreachable_stop_is_bounded_and_never_logged_as_success() {
    run(async {
        for udp in [false, true] {
            let tracker = Tracker::new(udp, 0, true).await;
            let h = Harness::new(&tracker, ReportingMode::Accurate, 32768, 0).await;
            tokio::time::timeout(Duration::from_secs(4), h.stop())
                .await
                .unwrap();
            assert_eq!(h.successful("stopped"), 0);
            assert!(
                h.records()
                    .iter()
                    .any(|r| r.event.as_deref() == Some("stopped") && r.outcome == "cancelled")
            );
        }
    });
}
