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
pub struct TorrentRecord {
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
}

pub struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Self::from_connection(connection)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "foreign_keys", true)?;
        let mut store = Self { connection };
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
                PRAGMA user_version = 2;
                ",
            )?;
            transaction.commit()?;
        } else if version == 1 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                "ALTER TABLE torrents ADD COLUMN download_directory TEXT;
                 PRAGMA user_version = 2;",
            )?;
            transaction.commit()?;
        } else if version != 2 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }

    pub fn save_torrent(&mut self, torrent: &TorrentRecord) -> rusqlite::Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO torrents (
                id, name, source, info_hash_v1, info_hash_v2, is_private, state,
                queue_position, download_directory, downloaded_bytes, uploaded_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, source=excluded.source,
                info_hash_v1=excluded.info_hash_v1, info_hash_v2=excluded.info_hash_v2,
                is_private=excluded.is_private, state=excluded.state,
                queue_position=excluded.queue_position,
                download_directory=excluded.download_directory,
                downloaded_bytes=excluded.downloaded_bytes,
                uploaded_bytes=excluded.uploaded_bytes",
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
            ],
        )?;
        transaction.execute("DELETE FROM trackers WHERE torrent_id=?1", [&torrent.id])?;
        transaction.execute("DELETE FROM files WHERE torrent_id=?1", [&torrent.id])?;
        transaction.execute(
            "DELETE FROM verified_pieces WHERE torrent_id=?1",
            [&torrent.id],
        )?;
        for (position, tracker) in torrent.trackers.iter().enumerate() {
            let persisted_url = crate::identity::redact_tracker_url(tracker);
            transaction.execute(
                "INSERT INTO trackers(torrent_id, position, url) VALUES (?1, ?2, ?3)",
                params![torrent.id, position as i64, persisted_url],
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
        transaction.commit()
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
                        queue_position, download_directory, downloaded_bytes, uploaded_bytes
                 FROM torrents WHERE id=?1",
                [id],
                |row| {
                    Ok(TorrentRecord {
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
                    })
                },
            )
            .optional()?;
        let Some(torrent) = torrent.as_mut() else {
            return Ok(None);
        };
        let mut tracker_statement = self
            .connection
            .prepare("SELECT url FROM trackers WHERE torrent_id=?1 ORDER BY position")?;
        torrent.trackers = tracker_statement
            .query_map([id], |row| row.get(0))?
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
        Ok(Some(torrent.clone()))
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

    fn record() -> TorrentRecord {
        TorrentRecord {
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
        }
    }

    #[test]
    fn sqlite_round_trip_restores_full_torrent_state() {
        let mut store = StateStore::in_memory().unwrap();
        let expected = record();
        store.save_torrent(&expected).unwrap();
        assert_eq!(store.load_torrent(&expected.id).unwrap(), Some(expected));
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
    }

    #[test]
    fn sqlite_never_persists_tracker_secrets() {
        let mut store = StateStore::in_memory().unwrap();
        let mut record = record();
        record.trackers =
            vec!["https://tracker.test/0123456789abcdef01234567/announce?token=secret".into()];
        store.save_torrent(&record).unwrap();
        let stored = store.load_torrent(&record.id).unwrap().unwrap();
        assert!(!stored.trackers[0].contains("0123456789abcdef01234567"));
        assert!(!stored.trackers[0].contains("token=secret"));
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
        assert_eq!(version, 2);
        assert_eq!(column_count, 1);
    }
}
