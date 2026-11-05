//! Durable acknowledgement deltas never read paths, piece maps, or unchanged audits.
use super::*;
use crate::file_selection::CompactFileUpdate;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersistenceIntent {
    pub selection: bool,
    pub priorities: bool,
    pub storage: bool,
}
impl PersistenceIntent {
    pub const SCALAR: Self = Self {
        selection: false,
        priorities: false,
        storage: false,
    };
    pub const SELECTION: Self = Self {
        selection: true,
        ..Self::SCALAR
    };
    pub const PRIORITIES: Self = Self {
        priorities: true,
        ..Self::SCALAR
    };
    pub const STORAGE: Self = Self {
        storage: true,
        ..Self::SCALAR
    };
    pub fn merge(&mut self, other: Self) {
        self.selection |= other.selection;
        self.priorities |= other.priorities;
        self.storage |= other.storage;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScalarUpdate {
    pub id: String,
    pub state: String,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub resume: crate::resume::ResumeCounters,
    pub download_mode: crate::download_mode::DownloadMode,
    pub freeze_after_bytes: u64,
    pub upload_ratio: crate::download_mode::UploadRatioRange,
    pub repair_policy: crate::runtime_status::RepairPolicy,
    pub verify_after_download: bool,
    pub transfer_settings: Option<crate::transfer_settings::TransferSettings>,
    pub download_directory: Option<String>,
}
impl ScalarUpdate {
    pub fn from_torrent(torrent: &crate::torrent::TorrentItem) -> Self {
        Self {
            id: torrent.info_hashes.join(";"),
            state: torrent.state.label().to_ascii_lowercase(),
            downloaded_bytes: torrent.downloaded_bytes,
            uploaded_bytes: torrent.uploaded_bytes,
            resume: crate::resume::ResumeCounters {
                downloaded: torrent.runtime.network_downloaded,
                uploaded: torrent.runtime.network_uploaded,
                seeded_seconds: torrent.runtime.seeded_seconds,
            },
            download_mode: torrent.overrides.download_mode,
            freeze_after_bytes: torrent.overrides.freeze_after_bytes,
            upload_ratio: torrent.overrides.upload_ratio,
            repair_policy: torrent.overrides.repair_policy,
            verify_after_download: torrent.overrides.verify_after_download
                || torrent.overrides.repair_policy.enabled,
            transfer_settings: torrent.overrides.transfer_settings,
            download_directory: torrent.overrides.download_directory.clone(),
        }
    }
    pub fn from_record(record: &TorrentRecord) -> Self {
        Self {
            id: record.id.clone(),
            state: record.state.clone(),
            downloaded_bytes: record.downloaded_bytes,
            uploaded_bytes: record.uploaded_bytes,
            resume: record.resume,
            download_mode: record.download_mode,
            freeze_after_bytes: record.freeze_after_bytes,
            upload_ratio: record.upload_ratio,
            repair_policy: record.repair_policy,
            verify_after_download: record.verify_after_download,
            transfer_settings: record.transfer_settings,
            download_directory: record.download_directory.clone(),
        }
    }
}

impl StateStore {
    /// Explicit trusted tracker/audit edit. `None` clears the audit; scalar ACKs
    /// use persist_acknowledged_with_audit instead, where omission preserves it.
    pub fn save_tracker_configuration(
        &mut self,
        id: &str,
        trackers: &[String],
        audit: Option<&StoredTrackerAuditProfile>,
    ) -> rusqlite::Result<bool> {
        if trackers.len() > crate::limits::ResourceLimits::default().max_trackers {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        let desired_trackers = tracker_fingerprints(trackers);
        let known_trackers = self.tracker_fingerprints.borrow().get(id).cloned();
        let trackers_changed = match known_trackers {
            Some(known) => known != desired_trackers,
            None => self
                .load_trackers(id)
                .map_or(true, |known| known != trackers),
        };
        let desired_audit = audit.map(audit_fingerprint).transpose()?;
        let audit_changed = self.audit_fingerprints.borrow().get(id) != Some(&desired_audit);
        let prepared = if trackers_changed {
            trackers
                .iter()
                .map(|tracker| {
                    let redacted = crate::identity::redact_tracker_url(tracker);
                    let reference = if redacted == *tracker {
                        None
                    } else {
                        Some(self.secrets.protect(tracker).map_err(|error| {
                            rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                        })?)
                    };
                    Ok((redacted, reference))
                })
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let tx = self.connection.transaction()?;
        if trackers_changed {
            tx.execute("DELETE FROM trackers WHERE torrent_id=?1", [id])?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO trackers(torrent_id,position,url,secret_ref) VALUES(?1,?2,?3,?4)",
            )?;
            for (position, (url, reference)) in prepared.iter().enumerate() {
                insert.execute(params![
                    id,
                    i64::try_from(position).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    url,
                    reference
                ])?;
            }
        }
        if audit_changed {
            tx.execute("DELETE FROM tracker_audit_steps WHERE torrent_id=?1", [id])?;
            tx.execute(
                "DELETE FROM tracker_audit_profiles WHERE torrent_id=?1",
                [id],
            )?;
            if let Some(audit) = audit {
                insert_tracker_audit(&tx, id, audit)?;
            }
        }
        tx.commit()?;
        self.tracker_fingerprints
            .borrow_mut()
            .insert(id.to_owned(), desired_trackers);
        self.audit_fingerprints
            .borrow_mut()
            .insert(id.to_owned(), desired_audit);
        Ok(true)
    }

    pub fn persist_acknowledged(
        &mut self,
        update: &ScalarUpdate,
        intent: PersistenceIntent,
        files: Option<&CompactFileUpdate>,
    ) -> rusqlite::Result<()> {
        self.persist_acknowledged_with_audit(update, intent, files, None)
    }
    pub fn persist_acknowledged_with_audit(
        &mut self,
        update: &ScalarUpdate,
        intent: PersistenceIntent,
        files: Option<&CompactFileUpdate>,
        audit: Option<&StoredTrackerAuditProfile>,
    ) -> rusqlite::Result<()> {
        update
            .upload_ratio
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        update
            .repair_policy
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if let Some(transfer) = update.transfer_settings {
            transfer
                .validate()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        }
        if (intent.selection || intent.priorities)
            && files.is_none_or(|files| !files.valid(intent.selection))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let desired_audit = audit.map(audit_fingerprint).transpose()?;
        let audit_changed = desired_audit.is_some_and(|desired| {
            self.audit_fingerprints.borrow().get(&update.id) != Some(&Some(desired))
        });
        let tx = self.connection.transaction()?;
        let changed = tx.execute("UPDATE torrents SET state=?2,downloaded_bytes=?3,uploaded_bytes=?4,
            download_mode=?5,freeze_after_bytes=?6,verify_after_download=?7,repair_automatically=?8,
            repair_max_attempts=?9,upload_ratio_min_milli=?10,upload_ratio_max_milli=?11 WHERE id=?1",
            params![update.id, update.state, to_sql_u64(update.downloaded_bytes)?, to_sql_u64(update.uploaded_bytes)?,
                update.download_mode.id(), to_sql_u64(update.freeze_after_bytes)?, update.verify_after_download,
                update.repair_policy.enabled, update.repair_policy.max_attempts, update.upload_ratio.min_milli, update.upload_ratio.max_milli])?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("INSERT INTO torrent_resume(torrent_id,snapshot) VALUES(?1,?2) ON CONFLICT(torrent_id) DO UPDATE SET snapshot=excluded.snapshot WHERE torrent_resume.snapshot IS NOT excluded.snapshot",
            params![update.id, serde_json::to_string(&update.resume).map_err(|_|rusqlite::Error::InvalidQuery)?])?;
        if let Some(transfer) = update.transfer_settings {
            let json =
                serde_json::to_string(&transfer).map_err(|_| rusqlite::Error::InvalidQuery)?;
            tx.execute("INSERT INTO torrent_transfer_settings(torrent_id,settings) VALUES(?1,?2) ON CONFLICT(torrent_id) DO UPDATE SET settings=excluded.settings WHERE torrent_transfer_settings.settings IS NOT excluded.settings", params![update.id,json])?;
        } else {
            tx.execute(
                "DELETE FROM torrent_transfer_settings WHERE torrent_id=?1",
                [&update.id],
            )?;
        }
        if intent.storage {
            tx.execute(
                "UPDATE torrents SET download_directory=?2 WHERE id=?1",
                params![update.id, update.download_directory],
            )?;
        }
        if intent.selection || intent.priorities {
            let files = files.ok_or(rusqlite::Error::InvalidQuery)?;
            let (count, end): (i64, i64) = tx.query_row(
                "SELECT COUNT(*),COALESCE(MAX(file_index)+1,0) FROM files WHERE torrent_id=?1",
                [&update.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let count = usize::try_from(count).map_err(|_| rusqlite::Error::InvalidQuery)?;
            let end = usize::try_from(end).map_err(|_| rusqlite::Error::InvalidQuery)?;
            if count != files.file_count || end != count {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if intent.selection {
                let mut write = tx.prepare_cached("UPDATE files SET selected=?3,downloaded_bytes=MIN(size_bytes,?4) WHERE torrent_id=?1 AND file_index=?2 AND (selected IS NOT ?3 OR downloaded_bytes IS NOT MIN(size_bytes,?4))")?;
                for index in 0..count {
                    write.execute(params![
                        update.id,
                        i64::try_from(index).map_err(|_| rusqlite::Error::InvalidQuery)?,
                        files.selected(index),
                        to_sql_u64(files.downloaded[index])?
                    ])?;
                }
            }
            if intent.priorities {
                let mut write = tx.prepare_cached("UPDATE files SET priority=?3 WHERE torrent_id=?1 AND file_index=?2 AND priority IS NOT ?3")?;
                for index in 0..count {
                    write.execute(params![
                        update.id,
                        i64::try_from(index).map_err(|_| rusqlite::Error::InvalidQuery)?,
                        i32::from(files.high_priority(index))
                    ])?;
                }
            }
        }
        if let Some(audit) = audit.filter(|_| audit_changed) {
            tx.execute(
                "DELETE FROM tracker_audit_steps WHERE torrent_id=?1",
                [&update.id],
            )?;
            tx.execute(
                "DELETE FROM tracker_audit_profiles WHERE torrent_id=?1",
                [&update.id],
            )?;
            insert_tracker_audit(&tx, &update.id, audit)?;
        }
        tx.commit()?;
        self.resume_checkpoints
            .borrow_mut()
            .insert(update.id.clone(), (update.resume, update.uploaded_bytes));
        if let Some(fingerprint) = desired_audit {
            self.audit_fingerprints
                .borrow_mut()
                .insert(update.id.clone(), Some(fingerprint));
        }
        if intent.storage {
            self.acknowledge_storage_location(&update.id, update.download_directory.as_deref())
                .map_err(|message| {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                        message,
                    )))
                })?;
        }
        Ok(())
    }

    pub fn clear_tracker_audit(&mut self, id: &str) -> rusqlite::Result<bool> {
        let tx = self.connection.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        tx.execute("DELETE FROM tracker_audit_steps WHERE torrent_id=?1", [id])?;
        tx.execute(
            "DELETE FROM tracker_audit_profiles WHERE torrent_id=?1",
            [id],
        )?;
        tx.commit()?;
        self.audit_fingerprints
            .borrow_mut()
            .insert(id.to_owned(), None);
        Ok(true)
    }
}
