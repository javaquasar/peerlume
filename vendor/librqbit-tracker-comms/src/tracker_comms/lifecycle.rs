//! Tracker lifecycle is independent of announce intervals and peer backpressure.
use super::*;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, Weak},
};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::Instant;
use tracker_comms_http::TrackerRequestEvent as Event;

type Key = ([u8; 20], [u8; 20]);
type Registry = HashMap<Key, Vec<Weak<Monitor>>>;
static MONITORS: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const STOP_BUDGET: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    attempted: bool,
    started: bool,
    stopped: bool,
}

pub(super) struct Monitor {
    comms: Arc<TrackerComms>,
    tracker: SupportedTracker,
    udp: UdpTrackerClient,
    state: AsyncMutex<State>,
    completed: Arc<Mutex<crate::observation::CompletionState>>,
}

impl Monitor {
    pub(super) fn new(
        comms: Arc<TrackerComms>,
        tracker: SupportedTracker,
        udp: UdpTrackerClient,
    ) -> Arc<Self> {
        let url = match &tracker {
            SupportedTracker::Http(url) | SupportedTracker::Udp(url) => url,
        };
        let fresh = || {
            Arc::new(Mutex::new(crate::observation::CompletionState {
                baseline: None,
                settled: false,
            }))
        };
        let completed = comms
            .hooks
            .as_ref()
            .map(|h| {
                h.completions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry((comms.info_hash.0, url.as_str().to_owned()))
                    .or_insert_with(fresh)
                    .clone()
            })
            .unwrap_or_else(fresh);
        let monitor = Arc::new(Self {
            comms,
            tracker,
            udp,
            state: Default::default(),
            completed,
        });
        MONITORS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(monitor.key())
            .or_default()
            .push(Arc::downgrade(&monitor));
        monitor
    }

    fn key(&self) -> Key {
        (self.comms.info_hash.0, self.comms.peer_id.0)
    }

    fn initialize_history(&self) {
        // TrackerComms::start is called inside the engine's add/unpause path.
        // Sample only when the stream is first polled, never synchronously while
        // its caller may still hold the session database lock.
        let downloaded = self
            .comms
            .hooks
            .as_ref()
            .map(|h| (h.transfers)(self.comms.info_hash.0).downloaded)
            .unwrap_or_else(|| self.comms.stats.get().downloaded_bytes);
        self.completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .baseline
            .get_or_insert(downloaded);
    }

    fn completion_due(&self) -> bool {
        // Do not create an observation for a probe: every journal entry must be
        // an actual transport attempt, not a lifecycle decision.
        let stats = self.comms.stats.get();
        let snapshot = self
            .comms
            .hooks
            .as_ref()
            .map(|h| (h.transfers)(self.comms.info_hash.0))
            .unwrap_or(crate::TransferSnapshot {
                downloaded: stats.downloaded_bytes,
                uploaded: stats.uploaded_bytes,
                ..Default::default()
            });
        let full = snapshot.full_size.unwrap_or(stats.total_bytes);
        let verified = snapshot.verified_bytes.unwrap_or(stats.downloaded_bytes);
        if full == 0 || verified < full {
            return false;
        }
        let mut completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.downloaded <= completed.baseline.unwrap_or(snapshot.downloaded) {
            // BEP 3: starting with complete files is not a new download.
            completed.settled = true;
        }
        !completed.settled
            && self
                .comms
                .reporting
                .apply_metrics(crate::AnnounceMetrics {
                    uploaded: snapshot.uploaded,
                    downloaded: snapshot.downloaded,
                    left: 0,
                })
                .left
                == 0
    }

    async fn request(&self, event: Option<Event>) -> anyhow::Result<(Duration, Vec<SocketAddr>)> {
        match &self.tracker {
            SupportedTracker::Http(url) => self.comms.tracker_one_request_http(url, event).await,
            SupportedTracker::Udp(url) => {
                let addrs = udp_tracker_to_socket_addrs(
                    url.host().context("missing host")?,
                    url.port().context("missing port")?,
                )
                .await?;
                match addrs {
                    UdpTrackerResolveResult::One(addr) => {
                        self.comms
                            .tracker_one_request_udp(url, addr, &self.udp, event)
                            .await
                    }
                    UdpTrackerResolveResult::Two(v4, v6) => {
                        let (r4, r6) = tokio::join!(
                            self.comms
                                .tracker_one_request_udp(url, v4.into(), &self.udp, event),
                            self.comms
                                .tracker_one_request_udp(url, v6.into(), &self.udp, event)
                        );
                        match (r4, r6) {
                            (Ok((a, mut p)), Ok((b, q))) => {
                                p.extend(q);
                                Ok((a.min(b), p))
                            }
                            (Ok(r), _) | (_, Ok(r)) => Ok(r),
                            (Err(e), _) => Err(e),
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn run(&self) -> anyhow::Result<()> {
        self.initialize_history();
        let mut due = Instant::now();
        let mut retry_completion = Instant::now();
        let mut retry_delay = Duration::from_secs(10);
        loop {
            let mut state = self.state.lock().await;
            if state.stopped {
                return Ok(());
            }
            let completion =
                state.started && Instant::now() >= retry_completion && self.completion_due();
            if Instant::now() < due && !completion {
                drop(state);
                tokio::time::sleep(POLL).await;
                continue;
            }
            let event = if !state.started {
                Some(Event::Started)
            } else if completion {
                Some(Event::Completed)
            } else {
                None
            };
            state.attempted = true;
            match self.request(event).await {
                Ok((interval, peers)) => {
                    state.started = true;
                    if matches!(event, Some(Event::Completed)) {
                        self.completed
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .settled = true;
                    }
                    retry_delay = Duration::from_secs(10);
                    due = Instant::now()
                        + self
                            .comms
                            .force_tracker_interval
                            .unwrap_or(interval)
                            .max(POLL);
                    drop(state);
                    self.deliver_peers(peers).await?;
                }
                Err(_) => {
                    due = Instant::now() + retry_delay;
                    retry_completion = due;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(600));
                }
            }
        }
    }

    async fn deliver_peers(&self, peers: Vec<SocketAddr>) -> anyhow::Result<()> {
        let mut retry = Instant::now();
        for peer in peers {
            let send = self.comms.tx.send(peer);
            tokio::pin!(send);
            loop {
                tokio::select! {
                    result = &mut send => { result.context("rx closed")?; break; }
                    _ = tokio::time::sleep(POLL) => {
                        let state = self.state.lock().await;
                        if state.stopped { return Ok(()); }
                        if state.started && Instant::now() >= retry && self.completion_due() {
                            if self.request(Some(Event::Completed)).await.is_ok() {
                                self.completed.lock().unwrap_or_else(|e| e.into_inner()).settled = true;
                            } else { retry = Instant::now() + Duration::from_secs(10); }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn stop(&self) {
        let mut state = self.state.lock().await;
        if state.stopped {
            return;
        }
        if !state.attempted {
            state.stopped = true;
            return;
        }
        // Flush an earned completion before cancellation/recheck can erase the
        // live state. Each completion is acknowledged at most once per tracker
        // over pause/resume and repair generations in the same session.
        if state.started && self.completion_due() {
            if tokio::time::timeout(Duration::from_secs(2), self.request(Some(Event::Completed)))
                .await
                .is_ok_and(|r| r.is_ok())
            {
                self.completed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .settled = true;
            }
        }
        let _ =
            tokio::time::timeout(Duration::from_secs(2), self.request(Some(Event::Stopped))).await;
        state.stopped = true;
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let mut registry = MONITORS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entries) = registry.get_mut(&self.key()) {
            entries.retain(|entry| entry.strong_count() > 0);
            if entries.is_empty() {
                registry.remove(&self.key());
            }
        }
    }
}

async fn stop_matching(matches: impl Fn(&Key) -> bool) {
    let monitors: Vec<_> = MONITORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(key, _)| matches(key))
        .flat_map(|(_, entries)| entries.iter().filter_map(Weak::upgrade))
        .collect();
    // One shared deadline, including lock contention and DNS. An unreachable
    // tracker must not hang pause, removal, or application shutdown.
    let _ = tokio::time::timeout(
        STOP_BUDGET,
        futures::future::join_all(monitors.iter().map(|m| m.stop())),
    )
    .await;
}

/// Best-effort real stopped announces before the engine cancels tracker streams.
pub async fn stop_torrent_announces(info_hash: [u8; 20], peer_id: [u8; 20]) {
    stop_matching(|key| *key == (info_hash, peer_id)).await;
}

/// Flush all session trackers concurrently under one five-second deadline.
pub async fn stop_session_announces(peer_id: [u8; 20]) {
    stop_matching(|key| key.1 == peer_id).await;
}

/// A removed download may be added anew; repair/pause must NOT reset history.
pub fn forget_torrent_lifecycle(info_hash: [u8; 20], peer_id: [u8; 20]) {
    if let Some(hooks) = crate::observation::hooks_for(peer_id) {
        hooks
            .completions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(hash, _), _| *hash != info_hash);
    }
}

#[cfg(test)]
mod tests;
