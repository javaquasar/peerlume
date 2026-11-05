//! Independent bounded diagnostic history; native readers never consume owner events.
use super::{Controller, SessionNotice};
use std::collections::VecDeque;

const CAPACITY: usize = 256;

#[derive(Default)]
pub(super) struct Tail {
    sequence: u64,
    entries: VecDeque<(u64, SessionNotice)>,
}
impl Tail {
    fn record(&mut self, notice: &SessionNotice) -> bool {
        if matches!(notice, SessionNotice::None)
            || self
                .entries
                .back()
                .is_some_and(|(_, previous)| previous == notice)
        {
            return false;
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("Native diagnostic sequence exhausted");
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((self.sequence, notice.clone()));
        true
    }

    fn read(&self, after: u64, limit: usize) -> (u64, bool, Vec<SessionNotice>) {
        let first = self
            .entries
            .front()
            .map_or(self.sequence.saturating_add(1), |(sequence, _)| *sequence);
        let resync = after > self.sequence || after.saturating_add(1) < first;
        let start = if resync {
            first.saturating_sub(1)
        } else {
            after
        };
        let mut cursor = if resync {
            start.min(self.sequence)
        } else {
            after
        };
        let entries = self
            .entries
            .iter()
            .filter(|(sequence, _)| *sequence > start)
            .take(limit.min(CAPACITY))
            .map(|(sequence, notice)| {
                cursor = *sequence;
                notice.clone()
            })
            .collect();
        (cursor, resync, entries)
    }
}

impl Controller {
    pub(super) fn record_native_notice(&mut self, notice: &SessionNotice) -> bool {
        let changed = self.diagnostic_tail.record(notice);
        if changed {
            self.diagnostic_wake.notify_one();
        }
        changed
    }

    /// A distinct wake token for the daemon's diagnostics monitor. Waiting here
    /// cannot consume the transfer-owner Host's event notification permit.
    pub fn owner_diagnostic_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.diagnostic_wake.clone()
    }

    /// Cursor advances only through returned entries. Resync explicitly reports
    /// an evicted range or a cursor from a different owner lifetime.
    pub(crate) fn native_notices(
        &self,
        after: u64,
        limit: usize,
    ) -> (u64, bool, Vec<SessionNotice>) {
        self.diagnostic_tail.read(after, limit)
    }

    pub(crate) fn detachment_generation(&self, id: &str) -> u64 {
        self.detachment_acks
            .get(id)
            .copied()
            .or_else(|| {
                self.torrent_indices
                    .get(id)
                    .and_then(|index| self.canonical_ids.get(*index))
                    .and_then(|id| self.detachment_acks.get(id))
                    .copied()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client::{ClientBackend, tests::torrent, torrent_to_record},
        session::SessionEvent,
        state::StateStore,
    };

    #[test]
    fn native_tail_has_independent_cursors_dedup_and_explicit_eviction_resync() {
        let mut control = Controller::new(ClientBackend::offline(), vec![]);
        for index in 0..300 {
            let event = SessionEvent::Error {
                id: "synthetic".into(),
                message: format!("diagnostic-{index}"),
            };
            control.apply(event.clone());
            control.apply(event); // Consecutive repeated status must not flood history.
        }
        let (cursor, resync, entries) = control.native_notices(0, 1);
        assert!(resync);
        assert_eq!(cursor, 45);
        assert_eq!(
            entries,
            vec![SessionNotice::Error {
                id: "synthetic".into(),
                message: "diagnostic-44".into()
            }]
        );
        let (last, resync, entries) = control.native_notices(cursor, 256);
        assert!(!resync);
        assert_eq!((last, entries.len()), (300, 255));
        assert_eq!(control.native_notices(last, 128), (last, false, vec![]));
        // A second frontend reads independently; the first did not consume history.
        assert_eq!(control.native_notices(0, 1).0, 45);
        assert!(control.native_notices(u64::MAX, 1).1);
        assert_eq!(control.diagnostic_tail.entries.len(), CAPACITY);
    }

    #[test]
    fn failed_ack_persistence_keeps_precise_error_and_pending_intent_without_retry_spam() {
        let item = torrent();
        let mut backend = ClientBackend::with_state_store(StateStore::in_memory().unwrap());
        backend
            .save_torrent(&torrent_to_record(&item, None))
            .unwrap();
        let mut control = Controller::new(backend, vec![item]);
        control.apply(SessionEvent::StateChanged {
            id: "v1: abc".into(),
            state: "paused",
        });
        control.torrents[0].uploaded_bytes = u64::MAX;
        let first = control.persist_preserving_audit("v1: abc").unwrap_err();
        assert!(control.persist_preserving_audit("v1: abc").is_err());
        let (_, _, notices) = control.native_notices(0, 256);
        let errors: Vec<_> = notices
            .iter()
            .filter_map(|notice| match notice {
                SessionNotice::Error { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(&first));
        assert!(control.has_confirmed_update("v1: abc"));
        control.torrents[0].uploaded_bytes = 0;
        control.persist_preserving_audit("v1: abc").unwrap();
        assert!(!control.has_confirmed_update("v1: abc"));
    }

    #[test]
    fn only_matching_fresh_remove_events_advance_detachment_generation() {
        let mut control = Controller::new(ClientBackend::offline(), vec![torrent()]);
        assert_eq!(control.detachment_generation("v1: abc"), 0);
        control.apply(SessionEvent::Removed {
            id: "unrelated".into(),
        });
        assert_eq!(control.detachment_generation("v1: abc"), 0);
        control.apply(SessionEvent::Removed {
            id: "v1: abc".into(),
        });
        let first = control.detachment_generation("v1: abc");
        assert!(first > 0);
        control.apply(SessionEvent::Removed {
            id: "v1: abc".into(),
        });
        assert!(control.detachment_generation("v1: abc") > first);
        control.torrents.clear();
        control.publish();
        assert!(control.detachment_acks.is_empty());
    }
}
