//! Restored registration is admitted in bounded batches after the owner starts.
use super::Controller;
use crate::{session::SessionSendError, settings::ClientSettings};
use std::collections::VecDeque;

impl Controller {
    /// Call once before exposing native mutations. Saved rows already belong to
    /// this controller; pending registration is busy until an engine ACK/error.
    pub fn begin_restores(&mut self) -> VecDeque<String> {
        self.reconcile();
        self.pending_restore
            .extend(self.canonical_ids.iter().cloned());
        self.canonical_ids.iter().cloned().collect()
    }

    /// `false` means temporary queue saturation and retains this ID for retry.
    /// Acceptance does not clear the busy marker: only Added/Error may do that.
    pub fn admit_restored(&mut self, id: &str, settings: &ClientSettings) -> Result<bool, String> {
        if !self.pending_restore.contains(id) {
            return Ok(true);
        }
        let Some(index) = self.torrent_index(id) else {
            self.pending_restore.remove(id);
            return Ok(true);
        };
        let command = crate::client::add_torrent_command(&self.torrents[index], settings);
        match self.backend.send_checked(command) {
            Ok(()) => {
                self.owner_wake.notify_one();
                Ok(true)
            }
            Err(SessionSendError::Full) => Ok(false),
            Err(error) => Err(format!(
                "Saved transfer registration failed: {}",
                super::CommandError::from(error)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientBackend, tests::torrent};

    #[test]
    fn saved_membership_beyond_command_capacity_remains_busy_until_actual_ack() {
        let torrents = (0..5000)
            .map(|index| {
                let mut item = torrent();
                item.info_hashes = vec![format!("v1: {index:040x}")];
                item
            })
            .collect();
        let mut owner = Controller::new(ClientBackend::offline(), torrents);
        let queue = owner.begin_restores();
        assert_eq!(queue.len(), 5000);
        assert!(queue.len() > 256);
        assert!(queue.iter().all(|id| owner.is_busy(id)));
        let id = queue.front().unwrap();
        assert!(
            owner
                .admit_restored(id, &ClientSettings::default())
                .is_err()
        );
        assert!(owner.pending_restore.contains(id));
        assert_eq!(owner.pending_restore.len(), 5000);
    }
}
