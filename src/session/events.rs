//! Latest telemetry per torrent, with ordered, non-dropping control delivery.
use super::{CompletionStatus, SessionEvent};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Kind {
    Storage,
    Peers,
    Stats,
    Network,
    Checking,
}
struct Stamped {
    sequence: u64,
    event: SessionEvent,
}
#[derive(Default)]
struct Pending {
    sequence: u64,
    controls: VecDeque<Stamped>,
    telemetry: HashMap<(String, Kind), Stamped>,
    sender_closed: bool,
    receiver_closed: bool,
    control_capacity: usize,
}
#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
}
pub(super) struct Sender(Arc<Shared>);
pub(super) struct Receiver(Arc<Shared>);

pub(super) fn channel(control_capacity: usize) -> (Sender, Receiver) {
    assert!(control_capacity > 0);
    let shared = Arc::new(Shared::default());
    shared.pending.lock().unwrap().control_capacity = control_capacity;
    (Sender(shared.clone()), Receiver(shared))
}
impl Sender {
    /// Telemetry replaces old snapshots; controls apply bounded backpressure.
    pub fn send(&self, event: SessionEvent) -> Result<(), String> {
        let key = match &event {
            SessionEvent::Storage {
                id,
                status: crate::storage::relocation::StorageStatus::Copying { .. },
            } => Some((id.clone(), Kind::Storage)),
            SessionEvent::Peers { id, .. } => Some((id.clone(), Kind::Peers)),
            SessionEvent::Stats { id, .. } => Some((id.clone(), Kind::Stats)),
            SessionEvent::NetworkTotals { id, .. } => Some((id.clone(), Kind::Network)),
            SessionEvent::Verification {
                id,
                status: CompletionStatus::Checking { .. },
            } => Some((id.clone(), Kind::Checking)),
            _ => None,
        };
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.receiver_closed {
            return Err("Session event receiver closed".into());
        }
        // A persistent engine error is polled every tick. Retain one pending copy,
        // rather than turning it into an unbounded periodic event stream.
        if let SessionEvent::Error { .. } = &event
            && pending.controls.iter().any(|old| old.event == event)
        {
            return Ok(());
        }
        if key.is_none() {
            while pending.controls.len() >= pending.control_capacity && !pending.receiver_closed {
                pending = self
                    .0
                    .ready
                    .wait(pending)
                    .unwrap_or_else(|e| e.into_inner());
            }
            if pending.receiver_closed {
                return Err("Session event receiver closed".into());
            }
        }
        if let SessionEvent::Removed { id } = &event {
            pending.telemetry.retain(|(torrent, _), _| torrent != id);
            if pending.telemetry.is_empty() {
                pending.telemetry.shrink_to_fit();
            }
        }
        if event == SessionEvent::Stopped {
            // Final real counters are a shutdown checkpoint barrier. Other stale
            // telemetry is disposable, but these must precede Stopped.
            pending
                .telemetry
                .retain(|(_, kind), _| matches!(kind, Kind::Network));
        }
        pending.sequence += 1;
        let stamped = Stamped {
            sequence: pending.sequence,
            event,
        };
        if let Some(key) = key {
            pending.telemetry.insert(key, stamped);
        } else {
            pending.controls.push_back(stamped);
        }
        self.0.ready.notify_one();
        Ok(())
    }
}
impl Pending {
    fn pop(&mut self) -> Option<SessionEvent> {
        let first = self
            .telemetry
            .iter()
            .min_by_key(|(_, value)| value.sequence);
        if let Some((key, value)) = first
            && self
                .controls
                .front()
                .is_none_or(|c| value.sequence < c.sequence)
        {
            let key = key.clone();
            return self.telemetry.remove(&key).map(|item| item.event);
        }
        self.controls.pop_front().map(|item| item.event)
    }
}
impl Receiver {
    #[cfg(test)]
    pub fn pending_controls(&self) -> usize {
        self.0.pending.lock().unwrap().controls.len()
    }
    pub fn try_recv(&self) -> Result<SessionEvent, String> {
        let event = self
            .0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        self.0.ready.notify_all();
        event.ok_or_else(|| "No session event available".into())
    }
    pub fn close(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.receiver_closed = true;
        pending.controls.clear();
        pending.telemetry.clear();
        self.0.ready.notify_all();
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<SessionEvent, String> {
        let deadline = Instant::now() + timeout;
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(event) = pending.pop() {
                self.0.ready.notify_all();
                return Ok(event);
            }
            if pending.sender_closed || pending.receiver_closed {
                return Err("Session event sender closed".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Timed out waiting for session event".into());
            }
            pending = self
                .0
                .ready
                .wait_timeout(pending, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sender_closed = true;
        self.0.ready.notify_all();
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_network_totals_are_delivered_before_shutdown_acknowledgement() {
        let (sender, receiver) = channel(8);
        sender
            .send(SessionEvent::NetworkTotals {
                id: "a".into(),
                uploaded: 31,
                downloaded: 47,
                seeded_seconds: 59,
            })
            .unwrap();
        sender.send(SessionEvent::Stopped).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::NetworkTotals {
                uploaded: 31,
                downloaded: 47,
                seeded_seconds: 59,
                ..
            }
        ));
        assert_eq!(receiver.try_recv().unwrap(), SessionEvent::Stopped);
    }
    fn stats(id: &str, value: u64, files: usize) -> SessionEvent {
        SessionEvent::Stats {
            id: id.into(),
            downloaded_bytes: value,
            uploaded_bytes: 0,
            download_rate: 0,
            upload_rate: 0,
            file_progress: vec![value; files],
            peers: 0,
            finished: false,
            paused: false,
        }
    }
    #[test]
    fn slow_consumer_keeps_one_large_snapshot_per_torrent_and_all_controls() {
        let (sender, receiver) = channel(512);
        for tick in 0..300 {
            sender.send(stats("a", tick, 100_000)).unwrap();
            sender.send(stats("b", tick, 100_000)).unwrap();
            sender
                .send(SessionEvent::NetworkTotals {
                    seeded_seconds: 0,
                    id: "a".into(),
                    uploaded: 0,
                    downloaded: tick,
                })
                .unwrap();
            sender
                .send(SessionEvent::Mode {
                    id: "a".into(),
                    application: crate::runtime_status::ModeApplication::AwaitingEngine,
                })
                .unwrap();
        }
        {
            let pending = sender.0.pending.lock().unwrap();
            assert_eq!(pending.telemetry.len(), 3);
            assert_eq!(pending.controls.len(), 300);
            assert_eq!(
                pending
                    .telemetry
                    .values()
                    .map(|e| match &e.event {
                        SessionEvent::Stats { file_progress, .. } => file_progress.len(),
                        _ => 0,
                    })
                    .sum::<usize>(),
                200_000
            );
        }
        let mut controls = 0;
        let mut snapshots = 0;
        while let Ok(event) = receiver.try_recv() {
            match event {
                SessionEvent::Mode { .. } => controls += 1,
                SessionEvent::Stats {
                    downloaded_bytes, ..
                } => {
                    assert_eq!(downloaded_bytes, 299);
                    snapshots += 1;
                }
                _ => {}
            }
        }
        assert_eq!((controls, snapshots), (300, 2));
    }
    #[test]
    fn verification_barriers_remove_and_shutdown_preserve_causal_order() {
        let (sender, receiver) = channel(8);
        sender.send(stats("a", 0, 1)).unwrap();
        sender
            .send(SessionEvent::Verification {
                id: "a".into(),
                status: CompletionStatus::Checking {
                    checked_bytes: 0,
                    total_bytes: 1,
                },
            })
            .unwrap();
        sender
            .send(SessionEvent::Verification {
                id: "a".into(),
                status: CompletionStatus::Verified,
            })
            .unwrap();
        sender.send(stats("a", 1, 1)).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Verification {
                status: CompletionStatus::Checking { .. },
                ..
            }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Verification {
                status: CompletionStatus::Verified,
                ..
            }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Stats {
                downloaded_bytes: 1,
                ..
            }
        ));
        for _ in 0..100 {
            sender
                .send(SessionEvent::Error {
                    id: "a".into(),
                    message: "same failure".into(),
                })
                .unwrap();
        }
        assert_eq!(sender.0.pending.lock().unwrap().controls.len(), 1);
        sender.send(stats("a", 0, 100_000)).unwrap();
        sender
            .send(SessionEvent::Removed { id: "a".into() })
            .unwrap();
        assert!(sender.0.pending.lock().unwrap().telemetry.is_empty());
        sender.send(SessionEvent::Stopped).unwrap();
        drop(sender);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Error { .. }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            SessionEvent::Removed { .. }
        ));
        assert_eq!(receiver.try_recv().unwrap(), SessionEvent::Stopped);
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_err());
    }
    #[test]
    fn receiver_drop_releases_snapshots_and_sender_drop_wakes_waiters() {
        let (sender, receiver) = channel(8);
        sender.send(stats("a", 0, 100_000)).unwrap();
        drop(receiver);
        assert!(sender.0.pending.lock().unwrap().telemetry.is_empty());
        assert!(sender.send(stats("a", 1, 1)).is_err());
        let (sender, receiver) = channel(8);
        let waiter = std::thread::spawn(move || receiver.recv_timeout(Duration::from_secs(5)));
        drop(sender);
        assert!(waiter.join().unwrap().is_err());
    }
    #[test]
    fn control_backpressure_is_bounded_and_receiver_close_unblocks_producers() {
        let (sender, receiver) = channel(1);
        sender
            .send(SessionEvent::Added { id: "first".into() })
            .unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = sender.send(SessionEvent::Added {
                id: "second".into(),
            });
            done_tx.send(result).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(25)).is_err());
        assert_eq!(receiver.0.pending.lock().unwrap().controls.len(), 1);
        assert!(
            matches!(receiver.try_recv().unwrap(), SessionEvent::Added { id } if id == "first")
        );
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        worker.join().unwrap();
        assert!(
            matches!(receiver.try_recv().unwrap(), SessionEvent::Added { id } if id == "second")
        );
        let (sender, receiver) = channel(1);
        sender.send(SessionEvent::Stopped).unwrap();
        let worker = std::thread::spawn(move || sender.send(SessionEvent::Stopped));
        receiver.close();
        assert!(worker.join().unwrap().is_err());
    }
}
