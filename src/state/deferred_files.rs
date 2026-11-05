//! Database-backed cold restoration and bounded path pages.
use super::*;
use crate::file_selection::{FileSelectionModel, FileState};

#[derive(Clone, Debug)]
pub struct DeferredRecord {
    pub record: TorrentRecord,
    pub files: FileSelectionModel,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilePage {
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub files: Vec<StoredFile>,
}
pub(super) const FILE_PAGE_ROWS: usize = 128;
pub(super) const FILE_PAGE_PATH_BYTES: usize = 64 * 1024;

impl StateStore {
    pub fn database_path(&self) -> Option<std::path::PathBuf> {
        self.connection
            .path()
            .filter(|path| !path.is_empty() && *path != ":memory:")
            .map(std::path::PathBuf::from)
    }
    pub fn load_deferred_torrents(&self) -> rusqlite::Result<Vec<DeferredRecord>> {
        let ids = {
            let mut statement = self
                .connection
                .prepare("SELECT id FROM torrents ORDER BY queue_position,rowid")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut restored = Vec::with_capacity(ids.len());
        for id in ids {
            let record = self
                .load_torrent_parts(&id, true, false)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let count: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM files WHERE torrent_id=?1",
                [&id],
                |row| row.get(0),
            )?;
            let count = usize::try_from(count).map_err(|_| rusqlite::Error::InvalidQuery)?;
            if count > crate::limits::ResourceLimits::default().max_files {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let mut statement = self.connection.prepare("SELECT file_index,size_bytes,downloaded_bytes,selected,priority FROM files WHERE torrent_id=?1 ORDER BY file_index")?;
            let mut expected = 0usize;
            let states = statement.query_map([&id], |row| {
                let index = usize::try_from(row.get::<_, i64>(0)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                if index != expected {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                expected += 1;
                Ok(FileState {
                    size_bytes: from_sql_u64(row.get(1)?)?,
                    downloaded_bytes: from_sql_u64(row.get(2)?)?,
                    selected: row.get(3)?,
                    high_priority: row.get::<_, i32>(4)? > 0,
                })
            })?;
            let mut files = FileSelectionModel::try_from_compact_states(states)?;
            if files.checked_total_size().is_none() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            files.set_selected_downloaded_summary(record.downloaded_bytes);
            restored.push(DeferredRecord { record, files });
        }
        Ok(restored)
    }
    pub fn load_file_page(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> rusqlite::Result<FilePage> {
        read_page(&self.connection, id, offset, limit)
    }
}

pub(super) fn read_page(
    connection: &Connection,
    id: &str,
    offset: usize,
    limit: usize,
) -> rusqlite::Result<FilePage> {
    if limit == 0 || limit > FILE_PAGE_ROWS {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM torrents WHERE id=?1)",
        [id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    // This uses the (torrent_id,file_index) index, not COUNT/OFFSET for every page.
    let total: i64 = connection.query_row(
        "SELECT COALESCE(MAX(file_index)+1,0) FROM files WHERE torrent_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let total = usize::try_from(total).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if offset > total || total > crate::limits::ResourceLimits::default().max_files {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut statement = connection.prepare_cached("SELECT file_index,path,size_bytes,downloaded_bytes,selected,priority FROM files WHERE torrent_id=?1 AND file_index>=?2 ORDER BY file_index LIMIT ?3")?;
    let mut rows = statement.query(params![
        id,
        i64::try_from(offset).map_err(|_| rusqlite::Error::InvalidQuery)?,
        i64::try_from(limit).map_err(|_| rusqlite::Error::InvalidQuery)?
    ])?;
    let mut files = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let path: String = row.get(1)?;
        if path.len() > crate::limits::ResourceLimits::default().max_path_bytes {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !files.is_empty() && bytes + path.len() > FILE_PAGE_PATH_BYTES {
            break;
        }
        let index =
            usize::try_from(row.get::<_, i64>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
        if index != offset + files.len() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        bytes += path.len();
        files.push(StoredFile {
            index: index as u32,
            path,
            size_bytes: from_sql_u64(row.get(2)?)?,
            downloaded_bytes: from_sql_u64(row.get(3)?)?,
            selected: row.get(4)?,
            priority: row.get(5)?,
        });
    }
    let next = offset + files.len();
    if next < total && files.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(FilePage {
        offset,
        total,
        next_offset: (next < total).then_some(next),
        files,
    })
}
