use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFile {
    pub index: u32,
    pub path: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub selected: bool,
    pub priority: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTrackerAuditStep {
    pub delay_seconds: u64,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTrackerAuditProfile {
    pub allowed_host: String,
    pub port: u16,
    pub started_only: bool,
    pub observation_seconds: u64,
    pub steps: Vec<StoredTrackerAuditStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentRecord {
    pub transfer_settings: Option<crate::transfer_settings::TransferSettings>,
    pub repair_policy: crate::runtime_status::RepairPolicy,
    pub verify_after_download: bool,
    pub download_mode: crate::download_mode::DownloadMode,
    pub freeze_after_bytes: u64,
    pub upload_ratio: crate::download_mode::UploadRatioRange,
    pub id: String,
    pub name: String,
    pub source: String,
    pub info_hash_v1: Option<String>,
    pub info_hash_v2: Option<String>,
    pub is_private: bool,
    pub state: String,
    pub queue_position: i64,
    pub download_directory: Option<String>,
    pub trackers: Vec<String>,
    pub files: Vec<StoredFile>,
    pub verified_pieces: Vec<u32>,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub tracker_audit: Option<StoredTrackerAuditProfile>,
}

pub struct StateStore {
    connection: Connection,
    secrets: std::sync::Arc<dyn crate::secrets::TrackerSecrets>,
}

impl StateStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let mut store = Self::from_connection(connection)?;
        store.secrets = std::sync::Arc::new(crate::secrets::PlatformSecrets);
        Ok(store)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn open_with_secrets(
        path: &Path,
        secrets: std::sync::Arc<dyn crate::secrets::TrackerSecrets>,
    ) -> rusqlite::Result<Self> {
        let mut store = Self::open(path)?;
        store.secrets = secrets;
        Ok(store)
    }

    pub fn in_memory_with_secrets(
        secrets: std::sync::Arc<dyn crate::secrets::TrackerSecrets>,
    ) -> rusqlite::Result<Self> {
        let mut store = Self::in_memory()?;
        store.secrets = secrets;
        Ok(store)
    }

    fn from_connection(connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "foreign_keys", true)?;
        #[cfg(windows)]
        let secrets: std::sync::Arc<dyn crate::secrets::TrackerSecrets> =
            std::sync::Arc::new(crate::secrets::PlatformSecrets);
        #[cfg(not(windows))]
        let secrets: std::sync::Arc<dyn crate::secrets::TrackerSecrets> =
            std::sync::Arc::new(crate::secrets::MemorySecrets::default());
        let mut store = Self {
            connection,
            secrets,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> rusqlite::Result<()> {
        let version: u32 = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "
                CREATE TABLE torrents (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    source TEXT NOT NULL,
                    info_hash_v1 TEXT,
                    info_hash_v2 TEXT,
                    is_private INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    queue_position INTEGER NOT NULL,
                    download_directory TEXT,
                    downloaded_bytes INTEGER NOT NULL,
                    uploaded_bytes INTEGER NOT NULL
                );
                CREATE TABLE trackers (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    url TEXT NOT NULL,
                    secret_ref TEXT,
                    PRIMARY KEY (torrent_id, position)
                );
                CREATE TABLE files (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    file_index INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    downloaded_bytes INTEGER NOT NULL,
                    selected INTEGER NOT NULL,
                    priority INTEGER NOT NULL,
                    PRIMARY KEY (torrent_id, file_index)
                );
                CREATE TABLE verified_pieces (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    piece_index INTEGER NOT NULL,
                    PRIMARY KEY (torrent_id, piece_index)
                );
                CREATE TABLE blocklist_metadata (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    source_url TEXT NOT NULL,
                    etag TEXT,
                    last_modified TEXT,
                    updated_unix INTEGER NOT NULL
                );
                CREATE TABLE tracker_audit_profiles (
                    torrent_id TEXT PRIMARY KEY REFERENCES torrents(id) ON DELETE CASCADE,
                    allowed_host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    started_only INTEGER NOT NULL,
                    observation_seconds INTEGER NOT NULL
                );
                CREATE TABLE tracker_audit_steps (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    delay_seconds INTEGER NOT NULL,
                    uploaded INTEGER NOT NULL,
                    downloaded INTEGER NOT NULL,
                    left_bytes INTEGER NOT NULL,
                    event TEXT,
                    PRIMARY KEY (torrent_id, position)
                );
                PRAGMA user_version = 4;
                ",
            )?;
            transaction.commit()?;
        } else if version == 1 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "ALTER TABLE torrents ADD COLUMN download_directory TEXT;
                 CREATE TABLE tracker_audit_profiles (
                    torrent_id TEXT PRIMARY KEY REFERENCES torrents(id) ON DELETE CASCADE,
                    allowed_host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    started_only INTEGER NOT NULL,
                    observation_seconds INTEGER NOT NULL
                 );
                 CREATE TABLE tracker_audit_steps (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    delay_seconds INTEGER NOT NULL,
                    uploaded INTEGER NOT NULL,
                    downloaded INTEGER NOT NULL,
                    left_bytes INTEGER NOT NULL,
                    event TEXT,
                    PRIMARY KEY (torrent_id, position)
                 );
                 PRAGMA user_version = 4;",
            )?;
            transaction.commit()?;
        } else if version == 2 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE tracker_audit_profiles (
                    torrent_id TEXT PRIMARY KEY REFERENCES torrents(id) ON DELETE CASCADE,
                    allowed_host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    started_only INTEGER NOT NULL,
                    observation_seconds INTEGER NOT NULL
                 );
                 CREATE TABLE tracker_audit_steps (
                    torrent_id TEXT NOT NULL REFERENCES torrents(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    delay_seconds INTEGER NOT NULL,
                    uploaded INTEGER NOT NULL,
                    downloaded INTEGER NOT NULL,
                    left_bytes INTEGER NOT NULL,
                    event TEXT,
                    PRIMARY KEY (torrent_id, position)
                 );
                 PRAGMA user_version = 4;",
            )?;
            transaction.commit()?;
        } else if version == 3 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "ALTER TABLE tracker_audit_profiles
                    ADD COLUMN started_only INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE tracker_audit_profiles
                    ADD COLUMN observation_seconds INTEGER NOT NULL DEFAULT 14400;
                 PRAGMA user_version = 4;",
            )?;
            transaction.commit()?;
        } else if !matches!(version, 4..=10) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if version < 5 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "ALTER TABLE torrents ADD COLUMN download_mode TEXT NOT NULL DEFAULT 'normal';
                 PRAGMA user_version = 5;",
            )?;
            transaction.commit()?;
        }
        if version < 6 {
            self.connection.execute_batch("ALTER TABLE torrents ADD COLUMN freeze_after_bytes INTEGER NOT NULL DEFAULT 0; PRAGMA user_version = 6;")?;
        }
        if version < 7 {
            self.connection.execute_batch("ALTER TABLE torrents ADD COLUMN verify_after_download INTEGER NOT NULL DEFAULT 0; PRAGMA user_version = 7;")?;
        }
        if version < 8 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch("ALTER TABLE torrents ADD COLUMN repair_automatically INTEGER NOT NULL DEFAULT 0; ALTER TABLE torrents ADD COLUMN repair_max_attempts INTEGER NOT NULL DEFAULT 3; PRAGMA user_version = 8;")?;
            transaction.commit()?;
        }
        if version < 9 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch("ALTER TABLE torrents ADD COLUMN upload_ratio_min_milli INTEGER NOT NULL DEFAULT 1500; ALTER TABLE torrents ADD COLUMN upload_ratio_max_milli INTEGER NOT NULL DEFAULT 2500; PRAGMA user_version = 9;")?;
            transaction.commit()?;
        }
        if version < 10 {
            self.connection.execute_batch("CREATE TABLE IF NOT EXISTS torrent_transfer_settings (torrent_id TEXT PRIMARY KEY REFERENCES torrents(id) ON DELETE CASCADE, settings TEXT NOT NULL); PRAGMA user_version = 10;")?;
        }
        Ok(())
    }

    pub fn save_torrent(&mut self, torrent: &TorrentRecord) -> rusqlite::Result<()> {
        if let Some(settings) = torrent.transfer_settings {
            settings
                .validate()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        }
        torrent
            .upload_ratio
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        torrent
            .repair_policy
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO torrents (
                id, name, source, info_hash_v1, info_hash_v2, is_private, state,
                queue_position, download_directory, downloaded_bytes, uploaded_bytes, download_mode, freeze_after_bytes, verify_after_download, repair_automatically, repair_max_attempts, upload_ratio_min_milli, upload_ratio_max_milli
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, source=excluded.source,
                info_hash_v1=excluded.info_hash_v1, info_hash_v2=excluded.info_hash_v2,
                is_private=excluded.is_private, state=excluded.state,
                queue_position=excluded.queue_position,
                download_directory=excluded.download_directory,
                downloaded_bytes=excluded.downloaded_bytes,
                uploaded_bytes=excluded.uploaded_bytes,
                download_mode=excluded.download_mode, freeze_after_bytes=excluded.freeze_after_bytes,
                verify_after_download=excluded.verify_after_download,
                repair_automatically=excluded.repair_automatically, repair_max_attempts=excluded.repair_max_attempts,
                upload_ratio_min_milli=excluded.upload_ratio_min_milli, upload_ratio_max_milli=excluded.upload_ratio_max_milli",
            params![
                torrent.id,
                torrent.name,
                torrent.source,
                torrent.info_hash_v1,
                torrent.info_hash_v2,
                torrent.is_private,
                torrent.state,
                torrent.queue_position,
                torrent.download_directory,
                to_sql_u64(torrent.downloaded_bytes)?,
                to_sql_u64(torrent.uploaded_bytes)?,
                torrent.download_mode.id(),
                to_sql_u64(torrent.freeze_after_bytes)?,
                torrent.verify_after_download,
                torrent.repair_policy.enabled,
                torrent.repair_policy.max_attempts,
                torrent.upload_ratio.min_milli,
                torrent.upload_ratio.max_milli,
            ],
        )?;
        transaction.execute(
            "DELETE FROM torrent_transfer_settings WHERE torrent_id=?1",
            [&torrent.id],
        )?;
        if let Some(settings) = torrent.transfer_settings {
            let json =
                serde_json::to_string(&settings).map_err(|_| rusqlite::Error::InvalidQuery)?;
            transaction.execute(
                "INSERT INTO torrent_transfer_settings(torrent_id, settings) VALUES (?1, ?2)",
                params![torrent.id, json],
            )?;
        }
        transaction.execute("DELETE FROM trackers WHERE torrent_id=?1", [&torrent.id])?;
        transaction.execute("DELETE FROM files WHERE torrent_id=?1", [&torrent.id])?;
        transaction.execute(
            "DELETE FROM verified_pieces WHERE torrent_id=?1",
            [&torrent.id],
        )?;
        transaction.execute(
            "DELETE FROM tracker_audit_steps WHERE torrent_id=?1",
            [&torrent.id],
        )?;
        transaction.execute(
            "DELETE FROM tracker_audit_profiles WHERE torrent_id=?1",
            [&torrent.id],
        )?;
        for (position, tracker) in torrent.trackers.iter().enumerate() {
            let persisted_url = crate::identity::redact_tracker_url(tracker);
            let protected_url =
                if persisted_url == *tracker {
                    None
                } else {
                    Some(self.secrets.protect(tracker).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?)
                };
            transaction.execute(
                "INSERT INTO trackers(torrent_id, position, url, secret_ref)
                 VALUES (?1, ?2, ?3, ?4)",
                params![torrent.id, position as i64, persisted_url, protected_url],
            )?;
        }
        for file in &torrent.files {
            transaction.execute(
                "INSERT INTO files(
                    torrent_id, file_index, path, size_bytes, downloaded_bytes, selected, priority
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    torrent.id,
                    file.index,
                    file.path,
                    to_sql_u64(file.size_bytes)?,
                    to_sql_u64(file.downloaded_bytes)?,
                    file.selected,
                    file.priority,
                ],
            )?;
        }
        for piece in &torrent.verified_pieces {
            transaction.execute(
                "INSERT INTO verified_pieces(torrent_id, piece_index) VALUES (?1, ?2)",
                params![torrent.id, piece],
            )?;
        }
        if let Some(audit) = &torrent.tracker_audit {
            insert_tracker_audit(&transaction, &torrent.id, audit)?;
        }
        transaction.commit()
    }

    pub fn save_tracker_audit(
        &mut self,
        torrent_id: &str,
        audit: &StoredTrackerAuditProfile,
    ) -> rusqlite::Result<bool> {
        let transaction = self.connection.transaction()?;
        let torrent_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE id=?1)",
            [torrent_id],
            |row| row.get(0),
        )?;
        if !torrent_exists {
            return Ok(false);
        }
        transaction.execute(
            "DELETE FROM tracker_audit_steps WHERE torrent_id=?1",
            [torrent_id],
        )?;
        transaction.execute(
            "DELETE FROM tracker_audit_profiles WHERE torrent_id=?1",
            [torrent_id],
        )?;
        insert_tracker_audit(&transaction, torrent_id, audit)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn checkpoint_torrent(
        &mut self,
        id: &str,
        state: &str,
        downloaded_bytes: u64,
        uploaded_bytes: u64,
        verified_pieces: &[u32],
    ) -> rusqlite::Result<bool> {
        let downloaded_bytes = to_sql_u64(downloaded_bytes)?;
        let uploaded_bytes = to_sql_u64(uploaded_bytes)?;
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE torrents
             SET state=?2, downloaded_bytes=?3, uploaded_bytes=?4
             WHERE id=?1",
            params![id, state, downloaded_bytes, uploaded_bytes],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        transaction.execute("DELETE FROM verified_pieces WHERE torrent_id=?1", [id])?;
        for piece in verified_pieces {
            transaction.execute(
                "INSERT INTO verified_pieces(torrent_id, piece_index) VALUES (?1, ?2)",
                params![id, piece],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn load_torrent(&self, id: &str) -> rusqlite::Result<Option<TorrentRecord>> {
        let mut torrent = self
            .connection
            .query_row(
                "SELECT name, source, info_hash_v1, info_hash_v2, is_private, state,
                        queue_position, download_directory, downloaded_bytes, uploaded_bytes, download_mode, freeze_after_bytes, verify_after_download, repair_automatically, repair_max_attempts, upload_ratio_min_milli, upload_ratio_max_milli
                 FROM torrents WHERE id=?1",
                [id],
                |row| {
                    Ok(TorrentRecord {
                        transfer_settings: None,
                        upload_ratio: crate::download_mode::UploadRatioRange { min_milli: row.get(15)?, max_milli: row.get(16)? },
                        verify_after_download: row.get(12)?,
                        repair_policy: crate::runtime_status::RepairPolicy { enabled: row.get(13)?, max_attempts: row.get(14)? },
                        freeze_after_bytes: from_sql_u64(row.get(11)?)?,
                        download_mode: crate::download_mode::DownloadMode::from_id(&row.get::<_, String>(10)?)
                            .map_err(|error| rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, std::io::Error::new(std::io::ErrorKind::InvalidData, error).into()))?,
                        id: id.to_owned(),
                        name: row.get(0)?,
                        source: row.get(1)?,
                        info_hash_v1: row.get(2)?,
                        info_hash_v2: row.get(3)?,
                        is_private: row.get(4)?,
                        state: row.get(5)?,
                        queue_position: row.get(6)?,
                        download_directory: row.get(7)?,
                        trackers: Vec::new(),
                        files: Vec::new(),
                        verified_pieces: Vec::new(),
                        downloaded_bytes: from_sql_u64(row.get(8)?)?,
                        uploaded_bytes: from_sql_u64(row.get(9)?)?,
                        tracker_audit: None,
                    })
                },
            )
            .optional()?;
        let Some(torrent) = torrent.as_mut() else {
            return Ok(None);
        };
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT settings FROM torrent_transfer_settings WHERE torrent_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        torrent.transfer_settings = json
            .map(|text| {
                let settings: crate::transfer_settings::TransferSettings =
                    serde_json::from_str(&text).map_err(|_| rusqlite::Error::InvalidQuery)?;
                settings
                    .validate()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok::<_, rusqlite::Error>(settings)
            })
            .transpose()?;
        torrent
            .upload_ratio
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        torrent
            .repair_policy
            .validate()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let mut tracker_statement = self.connection.prepare(
            "SELECT url, secret_ref FROM trackers WHERE torrent_id=?1 ORDER BY position",
        )?;
        let stored_trackers: Vec<(String, Option<Vec<u8>>)> = tracker_statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        torrent.trackers = stored_trackers
            .into_iter()
            .map(|(redacted_url, protected_url)| {
                if let Some(reference) = protected_url {
                    self.secrets.restore(&reference).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Blob,
                            Box::new(error),
                        )
                    })
                } else if redacted_url.contains("REDACTED") {
                    Err(rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Null,
                        Box::new(crate::secrets::SecretUnavailable),
                    ))
                } else {
                    Ok(redacted_url)
                }
            })
            .collect::<rusqlite::Result<_>>()?;
        let mut file_statement = self.connection.prepare(
            "SELECT file_index, path, size_bytes, downloaded_bytes, selected, priority
             FROM files WHERE torrent_id=?1 ORDER BY file_index",
        )?;
        torrent.files = file_statement
            .query_map([id], |row| {
                Ok(StoredFile {
                    index: row.get(0)?,
                    path: row.get(1)?,
                    size_bytes: from_sql_u64(row.get(2)?)?,
                    downloaded_bytes: from_sql_u64(row.get(3)?)?,
                    selected: row.get(4)?,
                    priority: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut piece_statement = self.connection.prepare(
            "SELECT piece_index FROM verified_pieces WHERE torrent_id=?1 ORDER BY piece_index",
        )?;
        torrent.verified_pieces = piece_statement
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        torrent.tracker_audit = self.load_tracker_audit(id)?;
        Ok(Some(torrent.clone()))
    }

    fn load_tracker_audit(
        &self,
        torrent_id: &str,
    ) -> rusqlite::Result<Option<StoredTrackerAuditProfile>> {
        let mut profile = self
            .connection
            .query_row(
                "SELECT allowed_host, port, started_only, observation_seconds
                 FROM tracker_audit_profiles WHERE torrent_id=?1",
                [torrent_id],
                |row| {
                    let port: i64 = row.get(1)?;
                    let port = u16::try_from(port).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?;
                    Ok(StoredTrackerAuditProfile {
                        allowed_host: row.get(0)?,
                        port,
                        started_only: row.get(2)?,
                        observation_seconds: from_sql_u64(row.get(3)?)?,
                        steps: Vec::new(),
                    })
                },
            )
            .optional()?;
        let Some(profile) = profile.as_mut() else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT delay_seconds, uploaded, downloaded, left_bytes, event
             FROM tracker_audit_steps WHERE torrent_id=?1 ORDER BY position",
        )?;
        profile.steps = statement
            .query_map([torrent_id], |row| {
                Ok(StoredTrackerAuditStep {
                    delay_seconds: from_sql_u64(row.get(0)?)?,
                    uploaded: from_sql_u64(row.get(1)?)?,
                    downloaded: from_sql_u64(row.get(2)?)?,
                    left: from_sql_u64(row.get(3)?)?,
                    event: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(profile.clone()))
    }

    pub fn remove_torrent(&self, id: &str) -> rusqlite::Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM torrents WHERE id=?1", [id])?
            > 0)
    }

    pub fn load_all_torrents(&self) -> rusqlite::Result<Vec<TorrentRecord>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM torrents ORDER BY queue_position, rowid")?;
        let ids: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(statement);
        ids.into_iter()
            .filter_map(|id| match self.load_torrent(&id) {
                Ok(Some(torrent)) => Some(Ok(torrent)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    #[cfg(test)]
    fn child_row_count(&self, table: &str) -> rusqlite::Result<u64> {
        self.connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                let count: i64 = row.get(0)?;
                from_sql_u64(count)
            })
    }
}

fn insert_tracker_audit(
    transaction: &rusqlite::Transaction<'_>,
    torrent_id: &str,
    audit: &StoredTrackerAuditProfile,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO tracker_audit_profiles(
            torrent_id, allowed_host, port, started_only, observation_seconds
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            torrent_id,
            audit.allowed_host,
            i64::from(audit.port),
            audit.started_only,
            to_sql_u64(audit.observation_seconds)?,
        ],
    )?;
    for (position, step) in audit.steps.iter().enumerate() {
        transaction.execute(
            "INSERT INTO tracker_audit_steps(
                torrent_id, position, delay_seconds, uploaded, downloaded, left_bytes, event
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                torrent_id,
                position as i64,
                to_sql_u64(step.delay_seconds)?,
                to_sql_u64(step.uploaded)?,
                to_sql_u64(step.downloaded)?,
                to_sql_u64(step.left)?,
                step.event,
            ],
        )?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };

    let input_length = u32::try_from(plaintext.len())
        .map_err(|_| "tracker URL is too large for Windows DPAPI".to_owned())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: plaintext.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(format!(
            "Windows DPAPI encryption failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let encrypted = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(encrypted)
}

#[cfg(windows)]
pub(crate) fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    let input_length = u32::try_from(ciphertext.len())
        .map_err(|_| "encrypted tracker URL is too large for Windows DPAPI".to_owned())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: ciphertext.as_ptr().cast_mut(),
    };
    let mut description = ptr::null_mut();
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptUnprotectData(
            &input,
            &mut description,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(format!(
            "Windows DPAPI decryption failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let plaintext = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        if !description.is_null() {
            LocalFree(description.cast());
        }
        bytes
    };
    Ok(plaintext)
}

fn to_sql_u64(value: u64) -> rusqlite::Result<i64> {
    value
        .try_into()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn from_sql_u64(value: i64) -> rusqlite::Result<u64> {
    value.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_range_survives_reopen_and_v8_migration_preserves_the_torrent() {
        let mut store = StateStore::in_memory().unwrap();
        let mut item = record();
        item.download_mode = crate::download_mode::DownloadMode::UploadRatio15;
        item.upload_ratio = crate::download_mode::UploadRatioRange {
            min_milli: 1250,
            max_milli: 3750,
        };
        store.save_torrent(&item).unwrap();
        let mut store = StateStore::from_connection(store.connection).unwrap();
        assert_eq!(store.load_torrent(&item.id).unwrap().unwrap(), item);
        item.upload_ratio.min_milli = 0;
        assert!(store.save_torrent(&item).is_err());
        assert_eq!(
            store
                .load_torrent(&item.id)
                .unwrap()
                .unwrap()
                .upload_ratio
                .min_milli,
            1250
        );
        store.connection.execute_batch("ALTER TABLE torrents DROP COLUMN upload_ratio_min_milli; ALTER TABLE torrents DROP COLUMN upload_ratio_max_milli; PRAGMA user_version=8;").unwrap();
        let store = StateStore::from_connection(store.connection).unwrap();
        let restored = store.load_torrent(&item.id).unwrap().unwrap();
        item.upload_ratio = Default::default();
        assert_eq!(restored, item);
        assert_eq!(restored.upload_ratio.min_milli, 1500);
        assert_eq!(restored.upload_ratio.max_milli, 2500);
    }

    fn record() -> TorrentRecord {
        TorrentRecord {
            transfer_settings: None,
            repair_policy: Default::default(),
            verify_after_download: false,
            download_mode: Default::default(),
            freeze_after_bytes: 0,
            upload_ratio: Default::default(),
            id: "v1:abc".into(),
            name: "example".into(),
            source: "example.torrent".into(),
            info_hash_v1: Some("abc".into()),
            info_hash_v2: None,
            is_private: true,
            state: "paused".into(),
            queue_position: 2,
            download_directory: Some("downloads/example".into()),
            trackers: vec!["https://tracker/announce".into()],
            files: vec![StoredFile {
                index: 0,
                path: "file.bin".into(),
                size_bytes: 100,
                downloaded_bytes: 50,
                selected: true,
                priority: 1,
            }],
            verified_pieces: vec![0, 2],
            downloaded_bytes: 50,
            uploaded_bytes: 5,
            tracker_audit: Some(StoredTrackerAuditProfile {
                allowed_host: "staging.tracker.test".into(),
                port: 51_413,
                started_only: true,
                observation_seconds: 14_400,
                steps: vec![StoredTrackerAuditStep {
                    delay_seconds: 30,
                    uploaded: 5,
                    downloaded: 50,
                    left: 50,
                    event: Some("started".into()),
                }],
            }),
        }
    }

    #[test]
    fn confirmed_transfer_settings_round_trip_and_invalid_updates_roll_back() {
        let mut store = StateStore::in_memory().unwrap();
        let mut record = record();
        record.transfer_settings = Some(crate::transfer_settings::TransferSettings {
            download_kib: 1,
            sequential: true,
            ..Default::default()
        });
        store.save_torrent(&record).unwrap();
        assert_eq!(store.load_torrent(&record.id).unwrap().unwrap(), record);
        record.transfer_settings.as_mut().unwrap().max_peers = 0;
        assert!(store.save_torrent(&record).is_err());
        assert_eq!(
            store
                .load_torrent(&record.id)
                .unwrap()
                .unwrap()
                .transfer_settings
                .unwrap()
                .max_peers,
            128
        );
        assert!(store.remove_torrent(&record.id).is_ok());
    }

    #[test]
    fn verification_option_is_independent_and_survives_updates() {
        let mut store = StateStore::in_memory().unwrap();
        let mut first = record();
        let mut second = record();
        second.id = "second".into();
        second.verify_after_download = true;
        store.save_torrent(&first).unwrap();
        store.save_torrent(&second).unwrap();
        assert!(
            !store
                .load_torrent(&first.id)
                .unwrap()
                .unwrap()
                .verify_after_download
        );
        assert!(
            store
                .load_torrent(&second.id)
                .unwrap()
                .unwrap()
                .verify_after_download
        );
        first.verify_after_download = true;
        store.save_torrent(&first).unwrap();
        assert!(
            store
                .load_torrent(&first.id)
                .unwrap()
                .unwrap()
                .verify_after_download
        );
    }

    #[test]
    fn repair_policy_round_trip_migration_and_validation() {
        let mut store = StateStore::in_memory().unwrap();
        let mut record = record();
        record.download_mode = crate::download_mode::DownloadMode::NoProgress;
        record.verify_after_download = true;
        record.repair_policy = crate::runtime_status::RepairPolicy {
            enabled: true,
            max_attempts: 4,
        };
        store.save_torrent(&record).unwrap();
        assert_eq!(store.load_torrent(&record.id).unwrap().unwrap(), record);
        record.repair_policy.max_attempts = 0;
        assert!(store.save_torrent(&record).is_err());
        assert_eq!(
            store
                .load_torrent(&record.id)
                .unwrap()
                .unwrap()
                .repair_policy
                .max_attempts,
            4
        );
        store.connection.execute_batch("ALTER TABLE torrents DROP COLUMN upload_ratio_min_milli; ALTER TABLE torrents DROP COLUMN upload_ratio_max_milli; ALTER TABLE torrents DROP COLUMN repair_automatically; ALTER TABLE torrents DROP COLUMN repair_max_attempts; PRAGMA user_version=7;").unwrap();
        let store = StateStore::from_connection(store.connection).unwrap();
        let restored = store.load_torrent(&record.id).unwrap().unwrap();
        assert_eq!(restored.repair_policy, Default::default());
        assert_eq!(restored.download_mode, record.download_mode);
        assert!(restored.verify_after_download);
        assert_eq!(restored.files, record.files);
        store
            .connection
            .execute("UPDATE torrents SET repair_max_attempts=11", [])
            .unwrap();
        assert!(store.load_torrent(&record.id).is_err());
    }

    #[test]
    fn v6_migration_defaults_verification_to_disabled_and_preserves_payload_state() {
        let mut store = StateStore::in_memory().unwrap();
        let torrent = record();
        store.save_torrent(&torrent).unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE torrents DROP COLUMN upload_ratio_min_milli; ALTER TABLE torrents DROP COLUMN upload_ratio_max_milli; ALTER TABLE torrents DROP COLUMN repair_automatically; ALTER TABLE torrents DROP COLUMN repair_max_attempts; ALTER TABLE torrents DROP COLUMN verify_after_download; PRAGMA user_version=6;",
            )
            .unwrap();
        store.migrate().unwrap();
        let restored = store.load_torrent(&torrent.id).unwrap().unwrap();
        assert!(!restored.verify_after_download);
        assert_eq!(restored.files, torrent.files);
        assert_eq!(restored.downloaded_bytes, torrent.downloaded_bytes);
    }

    #[test]
    fn sqlite_round_trip_restores_full_torrent_state() {
        let mut store = StateStore::in_memory().unwrap();
        let expected = record();
        store.save_torrent(&expected).unwrap();
        assert_eq!(store.load_torrent(&expected.id).unwrap(), Some(expected));
    }

    #[test]
    fn download_modes_persist_independently_for_each_torrent() {
        let mut store = StateStore::in_memory().unwrap();
        for (index, mode) in crate::download_mode::DownloadMode::ALL
            .into_iter()
            .enumerate()
        {
            let mut torrent = record();
            torrent.id = format!("mode-{index}");
            torrent.download_mode = mode;
            torrent.freeze_after_bytes = 123_456 + index as u64;
            store.save_torrent(&torrent).unwrap();
            assert_eq!(
                store
                    .load_torrent(&torrent.id)
                    .unwrap()
                    .unwrap()
                    .download_mode,
                mode
            );
            assert_eq!(
                store
                    .load_torrent(&torrent.id)
                    .unwrap()
                    .unwrap()
                    .freeze_after_bytes,
                torrent.freeze_after_bytes
            );
        }
    }

    #[test]
    fn v4_torrents_migrate_to_normal_without_losing_counters() {
        let mut store = StateStore::in_memory().unwrap();
        let torrent = record();
        store.save_torrent(&torrent).unwrap();
        store
            .connection
            .execute_batch("ALTER TABLE torrents DROP COLUMN upload_ratio_min_milli; ALTER TABLE torrents DROP COLUMN upload_ratio_max_milli; ALTER TABLE torrents DROP COLUMN repair_automatically; ALTER TABLE torrents DROP COLUMN repair_max_attempts; ALTER TABLE torrents DROP COLUMN verify_after_download; ALTER TABLE torrents DROP COLUMN download_mode; ALTER TABLE torrents DROP COLUMN freeze_after_bytes; PRAGMA user_version=4;")
            .unwrap();
        let migrated = StateStore::from_connection(store.connection).unwrap();
        let restored = migrated.load_torrent(&torrent.id).unwrap().unwrap();
        assert_eq!(
            restored.download_mode,
            crate::download_mode::DownloadMode::Normal
        );
        assert_eq!(restored.downloaded_bytes, torrent.downloaded_bytes);
        assert_eq!(restored.uploaded_bytes, torrent.uploaded_bytes);
    }

    #[test]
    fn v5_mode_is_preserved_when_freeze_threshold_column_is_added() {
        let mut store = StateStore::in_memory().unwrap();
        let mut torrent = record();
        torrent.download_mode = crate::download_mode::DownloadMode::HideDownloaded;
        store.save_torrent(&torrent).unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE torrents DROP COLUMN upload_ratio_min_milli; ALTER TABLE torrents DROP COLUMN upload_ratio_max_milli; ALTER TABLE torrents DROP COLUMN repair_automatically; ALTER TABLE torrents DROP COLUMN repair_max_attempts; ALTER TABLE torrents DROP COLUMN verify_after_download; ALTER TABLE torrents DROP COLUMN freeze_after_bytes; PRAGMA user_version=5;",
            )
            .unwrap();
        store.migrate().unwrap();
        let restored = store.load_torrent(&torrent.id).unwrap().unwrap();
        assert_eq!(restored.download_mode, torrent.download_mode);
        assert_eq!(restored.freeze_after_bytes, 0);
    }

    #[test]
    fn delete_cascades_to_resume_children() {
        let mut store = StateStore::in_memory().unwrap();
        let record = record();
        store.save_torrent(&record).unwrap();
        assert!(store.remove_torrent(&record.id).unwrap());
        assert_eq!(store.child_row_count("files").unwrap(), 0);
        assert_eq!(store.child_row_count("trackers").unwrap(), 0);
        assert_eq!(store.child_row_count("verified_pieces").unwrap(), 0);
        assert_eq!(store.child_row_count("tracker_audit_profiles").unwrap(), 0);
        assert_eq!(store.child_row_count("tracker_audit_steps").unwrap(), 0);
    }

    #[test]
    fn sqlite_persists_tracker_secrets_only_as_dpapi_ciphertext() {
        let mut store = StateStore::in_memory().unwrap();
        let mut record = record();
        record.trackers =
            vec!["https://tracker.test/0123456789abcdef01234567/announce?token=secret".into()];
        store.save_torrent(&record).unwrap();
        let (redacted_url, protected_url): (String, Option<Vec<u8>>) = store
            .connection
            .query_row(
                "SELECT url, secret_ref FROM trackers WHERE torrent_id=?1",
                [&record.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(!redacted_url.contains("0123456789abcdef01234567"));
        assert!(!redacted_url.contains("token=secret"));
        assert!(protected_url.is_some());
        let stored = store.load_torrent(&record.id).unwrap().unwrap();
        assert_eq!(stored.trackers, record.trackers);
    }

    #[test]
    fn updates_one_torrents_audit_profile_independently() {
        let mut store = StateStore::in_memory().unwrap();
        let expected = record();
        store.save_torrent(&expected).unwrap();
        let replacement = StoredTrackerAuditProfile {
            allowed_host: "localhost".into(),
            port: 6_881,
            started_only: false,
            observation_seconds: 7_200,
            steps: vec![StoredTrackerAuditStep {
                delay_seconds: 2,
                uploaded: 100,
                downloaded: 200,
                left: 300,
                event: Some("stopped".into()),
            }],
        };

        assert!(
            store
                .save_tracker_audit(&expected.id, &replacement)
                .unwrap()
        );
        assert_eq!(
            store
                .load_torrent(&expected.id)
                .unwrap()
                .unwrap()
                .tracker_audit,
            Some(replacement)
        );
        assert!(
            !store
                .save_tracker_audit("missing", expected.tracker_audit.as_ref().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn loads_torrents_in_queue_order() {
        let mut store = StateStore::in_memory().unwrap();
        let mut later = record();
        later.id = "later".into();
        later.queue_position = 2;
        let mut first = record();
        first.id = "first".into();
        first.queue_position = 1;
        store.save_torrent(&later).unwrap();
        store.save_torrent(&first).unwrap();
        let loaded = store.load_all_torrents().unwrap();
        assert_eq!(loaded[0].id, "first");
        assert_eq!(loaded[1].id, "later");
    }

    #[test]
    fn checkpoint_updates_counters_and_verified_pieces_atomically() {
        let mut store = StateStore::in_memory().unwrap();
        let record = record();
        store.save_torrent(&record).unwrap();
        assert!(
            store
                .checkpoint_torrent(&record.id, "downloading", 80, 9, &[1, 3])
                .unwrap()
        );
        let loaded = store.load_torrent(&record.id).unwrap().unwrap();
        assert_eq!(loaded.state, "downloading");
        assert_eq!(loaded.downloaded_bytes, 80);
        assert_eq!(loaded.uploaded_bytes, 9);
        assert_eq!(loaded.verified_pieces, [1, 3]);
    }

    #[test]
    fn failed_checkpoint_rolls_back_counters_and_piece_set() {
        let mut store = StateStore::in_memory().unwrap();
        let record = record();
        store.save_torrent(&record).unwrap();
        assert!(
            store
                .checkpoint_torrent(&record.id, "downloading", 99, 99, &[1, 1])
                .is_err()
        );
        let loaded = store.load_torrent(&record.id).unwrap().unwrap();
        assert_eq!(loaded.state, record.state);
        assert_eq!(loaded.downloaded_bytes, record.downloaded_bytes);
        assert_eq!(loaded.verified_pieces, record.verified_pieces);
    }

    #[test]
    fn migrates_v1_database_to_download_directory_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE torrents (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    source TEXT NOT NULL,
                    info_hash_v1 TEXT,
                    info_hash_v2 TEXT,
                    is_private INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    queue_position INTEGER NOT NULL,
                    downloaded_bytes INTEGER NOT NULL,
                    uploaded_bytes INTEGER NOT NULL
                );
                PRAGMA user_version = 1;",
            )
            .unwrap();
        let store = StateStore::from_connection(connection).unwrap();
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let column_count: u32 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('torrents')
                 WHERE name='download_directory'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 10);
        assert_eq!(column_count, 1);
    }

    #[test]
    fn migrates_v2_database_to_tracker_audit_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE torrents (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    source TEXT NOT NULL,
                    info_hash_v1 TEXT,
                    info_hash_v2 TEXT,
                    is_private INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    queue_position INTEGER NOT NULL,
                    download_directory TEXT,
                    downloaded_bytes INTEGER NOT NULL,
                    uploaded_bytes INTEGER NOT NULL
                );
                PRAGMA user_version = 2;",
            )
            .unwrap();
        let store = StateStore::from_connection(connection).unwrap();
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        assert_eq!(store.child_row_count("tracker_audit_profiles").unwrap(), 0);
        assert_eq!(store.child_row_count("tracker_audit_steps").unwrap(), 0);
    }

    #[test]
    fn migrates_v3_database_to_started_only_observation_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE torrents (id TEXT PRIMARY KEY);
                 CREATE TABLE tracker_audit_profiles (
                    torrent_id TEXT PRIMARY KEY REFERENCES torrents(id) ON DELETE CASCADE,
                    allowed_host TEXT NOT NULL,
                    port INTEGER NOT NULL
                 );
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        let store = StateStore::from_connection(connection).unwrap();
        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let new_column_count: u32 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tracker_audit_profiles')
                 WHERE name IN ('started_only', 'observation_seconds')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 10);
        assert_eq!(new_column_count, 2);
    }
}
