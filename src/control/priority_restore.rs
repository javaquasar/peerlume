//! Saved priorities have their own bounded admission/ACK phase after registration.
use super::{ClientBackend, Controller, SessionEvent, SessionNotice};
use crate::session::{SessionCommand, SessionSendError};

impl Controller {
    /// Return true only for an obsolete priority ACK that must not overwrite the
    /// latest canonical settings or be published as a successful acknowledgement.
    pub(super) fn observe_priority_restore(
        &mut self,
        event: &SessionEvent,
        index: Option<usize>,
    ) -> bool {
        if matches!(event, SessionEvent::Stopped) {
            self.pending_restore.clear();
            self.pending_priority_restore.clear();
            self.admitted_priority_restore.clear();
            return false;
        }
        let Some(index) = index else {
            return false;
        };
        let id = self.canonical_ids[index].clone();
        match event {
            SessionEvent::Added { .. } => {
                if self.torrents[index].files.high_priority().is_empty() {
                    self.pending_restore.remove(&id);
                    self.pending_priority_restore.remove(&id);
                    self.admitted_priority_restore.remove(&id);
                } else {
                    self.pending_restore.insert(id.clone());
                    if !self.admitted_priority_restore.contains(&id) {
                        self.pending_priority_restore.insert(id);
                        self.owner_wake.notify_one();
                    }
                }
            }
            SessionEvent::Error { .. } | SessionEvent::Removed { .. } => {
                self.pending_restore.remove(&id);
                self.pending_priority_restore.remove(&id);
                self.admitted_priority_restore.remove(&id);
            }
            SessionEvent::FilePrioritiesApplied { high, .. }
                if self.admitted_priority_restore.remove(&id) =>
            {
                if *high != self.torrents[index].files.high_priority() {
                    self.pending_priority_restore.insert(id);
                    self.owner_wake.notify_one();
                    return true;
                }
                self.pending_priority_restore.remove(&id);
                self.pending_restore.remove(&id);
            }
            _ => {}
        }
        false
    }

    pub(super) fn retry_priority_restores(&mut self) {
        self.retry_priority_restores_via(|backend, command| backend.send_checked(command));
    }

    fn retry_priority_restores_via(
        &mut self,
        mut send: impl FnMut(&ClientBackend, SessionCommand) -> Result<(), SessionSendError>,
    ) {
        for id in self
            .pending_priority_restore
            .iter()
            .take(16)
            .cloned()
            .collect::<Vec<_>>()
        {
            let Some(index) = self.torrent_index(&id) else {
                self.pending_priority_restore.remove(&id);
                self.admitted_priority_restore.remove(&id);
                self.pending_restore.remove(&id);
                continue;
            };
            // Construct immediately before admission. A saturated queue never
            // captures an obsolete copy of the canonical desired priorities.
            let high = self.torrents[index].files.high_priority();
            match send(
                &self.backend,
                SessionCommand::SetFilePriorities {
                    id: id.clone(),
                    high,
                },
            ) {
                Ok(()) => {
                    self.pending_priority_restore.remove(&id);
                    self.admitted_priority_restore.insert(id);
                }
                Err(SessionSendError::Full) => break,
                Err(error) => {
                    self.pending_priority_restore.remove(&id);
                    self.admitted_priority_restore.remove(&id);
                    self.pending_restore.remove(&id);
                    let message = format!(
                        "Saved file priorities were not admitted: session command channel {error:?}. Desired priorities retained; no engine acknowledgement received"
                    );
                    self.torrents[index].runtime.record_error(&message);
                    let notice = SessionNotice::Error { id, message };
                    if self.record_native_notice(&notice) {
                        if self.notices.len() == 256 {
                            self.notices.pop_front();
                        }
                        self.notices.push_back(notice);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client::{tests::torrent, torrent_to_record},
        file_selection::FileSelectionModel,
        state::StateStore,
    };

    fn owner() -> Controller {
        let mut item = torrent();
        item.files = FileSelectionModel::from_files([
            ("first".into(), 100),
            ("second".into(), 100),
            ("third".into(), 100),
        ]);
        item.files.set_high_priority(&[0]);
        item.files.defer_details();
        Controller::new(ClientBackend::offline(), vec![item])
    }

    #[test]
    fn full_then_admitted_uses_latest_priorities_and_stays_busy_until_matching_ack() {
        let mut owner = owner();
        owner.begin_restores();
        owner.apply(SessionEvent::Added {
            id: "v1: abc".into(),
        });
        assert!(owner.is_busy("v1: abc"));
        owner.retry_priority_restores_via(|_, command| {
            assert!(matches!(command,SessionCommand::SetFilePriorities{high,..} if high==vec![0]));
            Err(SessionSendError::Full)
        });
        assert!(owner.next_wake_delay() <= std::time::Duration::from_millis(100));
        owner.torrents[0].files.set_high_priority(&[1, 2]);
        let mut calls = 0;
        owner.retry_priority_restores_via(|_, command| {
            calls += 1;
            assert!(
                matches!(command,SessionCommand::SetFilePriorities{high,..} if high==vec![1,2])
            );
            Ok(())
        });
        assert_eq!(calls, 1);
        assert!(owner.is_busy("v1: abc"));
        owner.retry_priority_restores_via(|_, _| {
            panic!("do not duplicate a command awaiting its ACK")
        });
        owner.apply(SessionEvent::FilePrioritiesApplied {
            id: "unrelated".into(),
            high: vec![1, 2],
        });
        assert!(owner.is_busy("v1: abc"));
        assert_eq!(
            owner.apply(SessionEvent::FilePrioritiesApplied {
                id: "v1: abc".into(),
                high: vec![0]
            }),
            SessionNotice::None
        );
        assert_eq!(owner.torrents[0].files.high_priority(), vec![1, 2]);
        assert!(owner.is_busy("v1: abc"));
        owner.retry_priority_restores_via(|_, command| {
            assert!(
                matches!(command,SessionCommand::SetFilePriorities{high,..} if high==vec![1,2])
            );
            Ok(())
        });
        owner.apply(SessionEvent::FilePrioritiesApplied {
            id: "v1: abc".into(),
            high: vec![1, 2],
        });
        assert!(!owner.is_busy("v1: abc"));
        assert!(
            owner.pending_priority_restore.is_empty() && owner.admitted_priority_restore.is_empty()
        );
        assert!(
            !owner.backend.session_available(),
            "the fake admission path never starts an engine"
        );
    }

    #[test]
    fn permanent_admission_failure_reports_exact_cause_without_losing_desired_bits_or_busy_loop() {
        let mut owner = owner();
        owner.apply(SessionEvent::Added {
            id: "v1: abc".into(),
        });
        owner.retry_priority_restores_via(|_, _| Err(SessionSendError::Closed));
        assert!(!owner.is_busy("v1: abc"));
        assert_eq!(owner.torrents[0].files.high_priority(), vec![0]);
        assert!(owner.native_notices(0,256).2.iter().any(|notice|matches!(notice,SessionNotice::Error{message,..} if message.contains("Closed") && message.contains("no engine acknowledgement"))));
        owner.retry_priority_restores_via(|_, _| {
            panic!("a permanent failure is not an endless retry")
        });
        owner.apply(SessionEvent::Added {
            id: "v1: abc".into(),
        });
        assert!(owner.is_busy("v1: abc"));
        owner.apply(SessionEvent::Error {
            id: "v1: abc".into(),
            message: "synthetic engine failure".into(),
        });
        assert!(!owner.is_busy("v1: abc"));
        assert!(
            owner.pending_priority_restore.is_empty() && owner.admitted_priority_restore.is_empty()
        );
    }

    #[test]
    fn priority_admission_and_ack_do_not_write_canonical_file_children() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("state.sqlite3");
        let mut item = torrent();
        item.files.set_high_priority(&[0]);
        let record = torrent_to_record(&item, None);
        let mut backend = ClientBackend::with_state_store(StateStore::open(&database).unwrap());
        backend.save_torrent(&record).unwrap();
        let sql = rusqlite::Connection::open(&database).unwrap();
        sql.execute_batch("CREATE TABLE child_writes(value INTEGER); CREATE TRIGGER count_file_writes AFTER UPDATE ON files BEGIN INSERT INTO child_writes VALUES(1); END;").unwrap();
        item.files.defer_details();
        let mut owner = Controller::new(backend, vec![item]);
        owner.begin_restores();
        owner.apply(SessionEvent::Added {
            id: "v1: abc".into(),
        });
        owner.retry_priority_restores_via(|_, _| Err(SessionSendError::Full));
        owner.retry_priority_restores_via(|_, _| Ok(()));
        owner.apply(SessionEvent::FilePrioritiesApplied {
            id: "v1: abc".into(),
            high: vec![0],
        });
        assert!(!owner.is_busy("v1: abc"));
        let writes: i64 = sql
            .query_row("SELECT COUNT(*) FROM child_writes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            writes, 0,
            "engine admission and ACK do not themselves persist child rows"
        );
        assert_eq!(owner.backend.load_torrents().unwrap(), vec![record]);
    }

    #[test]
    fn normal_empty_priority_registration_releases_busy_without_an_extra_command() {
        let mut owner = owner();
        owner.torrents[0].files.set_high_priority(&[]);
        owner.begin_restores();
        owner.apply(SessionEvent::Added {
            id: "v1: abc".into(),
        });
        assert!(!owner.is_busy("v1: abc"));
        owner.retry_priority_restores_via(|_, _| {
            panic!("empty saved priorities need no restore command")
        });
    }
}
