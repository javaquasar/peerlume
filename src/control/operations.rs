//! Bounded durable operation records. No replay after a crash or ambiguous timeout.
use super::{
    access::random_id,
    wire::{ErrorCode, MAX_OPERATIONS},
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    ReconciliationRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub client_id: String,
    pub torrent_id: String,
    pub command: String,
    pub created_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub state: OperationState,
    pub approval_required: bool,
    pub error: Option<ErrorCode>,
    pub(crate) key: String,
    pub(crate) fingerprint: String,
}

pub struct ControlStore {
    pub(crate) connection: Connection,
}

/// Full results and keys remain durable; only the hot working set is compacted.
pub const MAX_ARCHIVED_OPERATIONS: usize = 100_000;

impl ControlStore {
    pub fn open(path: &std::path::Path) -> Result<Self, ErrorCode> {
        let connection = Connection::open(path).map_err(|_| ErrorCode::PersistenceUnavailable)?;
        connection
            .busy_timeout(std::time::Duration::from_millis(100))
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        connection.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS agent_operations (id TEXT PRIMARY KEY, client TEXT NOT NULL,
              key TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(client,key));
            CREATE TABLE IF NOT EXISTS agent_clients (id TEXT PRIMARY KEY, record TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_operation_archive (id TEXT PRIMARY KEY, client TEXT NOT NULL,
              key TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(client,key));
            CREATE TABLE IF NOT EXISTS agent_audit (sequence INTEGER PRIMARY KEY AUTOINCREMENT,
              stamp INTEGER NOT NULL, client TEXT NOT NULL, request TEXT NOT NULL, decision TEXT NOT NULL);")
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let store = Self { connection };
        for mut op in store.list()? {
            if matches!(op.state, OperationState::Queued | OperationState::Running) {
                op.state = OperationState::ReconciliationRequired;
                op.error = Some(ErrorCode::ReconciliationRequired);
                store.save(&op)?;
            }
        }
        Ok(store)
    }
    pub fn list(&self) -> Result<Vec<Operation>, ErrorCode> {
        let mut query = self
            .connection
            .prepare("SELECT record FROM agent_operations ORDER BY rowid DESC LIMIT 513")
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let result: Result<Vec<_>, _> = rows
            .map(|row| {
                row.map_err(|_| ErrorCode::PersistenceUnavailable)
                    .and_then(|json| {
                        serde_json::from_str(&json).map_err(|_| ErrorCode::PersistenceUnavailable)
                    })
            })
            .collect();
        let result = result?;
        if result.len() > MAX_OPERATIONS {
            return Err(ErrorCode::PersistenceUnavailable);
        }
        Ok(result)
    }
    pub fn lookup(&self, client: &str, key: &str) -> Result<Option<Operation>, ErrorCode> {
        use rusqlite::OptionalExtension;
        self.connection
            .query_row(
                "SELECT record FROM agent_operations WHERE client=?1 AND key=?2
                 UNION ALL SELECT record FROM agent_operation_archive WHERE client=?1 AND key=?2",
                params![client, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| ErrorCode::PersistenceUnavailable)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| ErrorCode::PersistenceUnavailable)
            })
            .transpose()
    }
    pub fn save(&self, operation: &Operation) -> Result<(), ErrorCode> {
        let value =
            serde_json::to_string(operation).map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let archived: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_operation_archive WHERE id=?1 OR (client=?2 AND key=?3))",
            params![operation.id, operation.client_id, operation.key], |row| row.get(0),
        ).map_err(|_| ErrorCode::PersistenceUnavailable)?;
        if archived {
            return Err(ErrorCode::Conflict);
        }
        transaction
            .execute(
                "INSERT INTO agent_operations(id,client,key,record) VALUES(?,?,?,?)
            ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                params![operation.id, operation.client_id, operation.key, value],
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        transaction
            .execute(
                "INSERT INTO agent_audit(stamp,client,request,decision) VALUES(?,?,?,?)",
                params![
                    now_ms().min(i64::MAX as u64) as i64,
                    operation.client_id,
                    operation.id,
                    format!("{:?}", operation.state)
                ],
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        transaction.execute("DELETE FROM agent_audit WHERE sequence <= (SELECT MAX(sequence)-4096 FROM agent_audit)",[])
            .map_err(|_|ErrorCode::PersistenceUnavailable)?;
        transaction
            .commit()
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        Ok(())
    }
    pub fn create(
        &self,
        scope: (&str, &str, &str),
        key: &str,
        fingerprint: &str,
        deadline: u64,
        approval: bool,
    ) -> Result<Operation, ErrorCode> {
        let (client, torrent, command) = scope;
        if self.lookup(client, key)?.is_some() {
            return Err(ErrorCode::IdempotencyConflict);
        }
        self.archive_completed()?;
        if self.list()?.len() >= MAX_OPERATIONS {
            return Err(ErrorCode::Busy);
        }
        let op = Operation {
            id: random_id().map_err(|_| ErrorCode::PersistenceUnavailable)?,
            client_id: client.into(),
            torrent_id: torrent.into(),
            command: command.into(),
            created_unix_ms: now_ms(),
            deadline_unix_ms: deadline,
            state: OperationState::Queued,
            approval_required: approval,
            error: None,
            key: key.into(),
            fingerprint: fingerprint.into(),
        };
        self.save(&op)?;
        Ok(op)
    }

    fn archive_completed(&self) -> Result<(), ErrorCode> {
        let current = self.list()?;
        // Keep recent results hot until the working set reaches its bound.
        if current.len() < MAX_OPERATIONS {
            return Ok(());
        }
        let terminal: Vec<_> = current
            .iter()
            .filter(|op| {
                matches!(
                    op.state,
                    OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
                )
            })
            .collect();
        if terminal.is_empty() {
            return Err(ErrorCode::Busy);
        }
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let count: i64 = tx
            .query_row("SELECT count(*) FROM agent_operation_archive", [], |r| {
                r.get(0)
            })
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        if count.saturating_add(terminal.len() as i64) > MAX_ARCHIVED_OPERATIONS as i64 {
            return Err(ErrorCode::Busy);
        }
        for op in terminal {
            tx.execute(
                "INSERT INTO agent_operation_archive(id,client,key,record)
                SELECT id,client,key,record FROM agent_operations WHERE id=?",
                [&op.id],
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
            tx.execute("DELETE FROM agent_operations WHERE id=?", [&op.id])
                .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        }
        tx.commit().map_err(|_| ErrorCode::PersistenceUnavailable)
    }

    pub fn operation(&self, client: &str, id: &str) -> Result<Option<Operation>, ErrorCode> {
        use rusqlite::OptionalExtension;
        self.connection
            .query_row(
                "SELECT record FROM agent_operations WHERE client=?1 AND id=?2
            UNION ALL SELECT record FROM agent_operation_archive WHERE client=?1 AND id=?2",
                params![client, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| ErrorCode::PersistenceUnavailable)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| ErrorCode::PersistenceUnavailable)
            })
            .transpose()
    }

    pub fn page(
        &self,
        client: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<Operation>, usize), ErrorCode> {
        if limit == 0
            || limit > super::MAX_PAGE_SIZE
            || offset > MAX_ARCHIVED_OPERATIONS + MAX_OPERATIONS
        {
            return Err(ErrorCode::InvalidPage);
        }
        let total: usize = self
            .connection
            .query_row(
                "SELECT
            (SELECT count(*) FROM agent_operations WHERE client=?1)+
            (SELECT count(*) FROM agent_operation_archive WHERE client=?1)",
                [client],
                |r| r.get::<_, i64>(0).map(|value| value as usize),
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        if offset > total {
            return Err(ErrorCode::InvalidPage);
        }
        let mut query = self.connection.prepare("SELECT record FROM (
            SELECT record,id,json_extract(record,'$.created_unix_ms') AS stamp FROM agent_operations WHERE client=?1
            UNION ALL SELECT record,id,json_extract(record,'$.created_unix_ms') AS stamp FROM agent_operation_archive WHERE client=?1)
            ORDER BY stamp DESC,id ASC LIMIT ?2 OFFSET ?3").map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let rows = query
            .query_map(params![client, limit as i64, offset as i64], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let mut result = Vec::with_capacity(limit);
        for row in rows {
            result.push(
                serde_json::from_str(&row.map_err(|_| ErrorCode::PersistenceUnavailable)?)
                    .map_err(|_| ErrorCode::PersistenceUnavailable)?,
            );
        }
        Ok((result, total))
    }
    pub fn audit(&self, client: &str, request: &str, decision: &str) -> Result<(), ErrorCode> {
        self.connection
            .execute(
                "INSERT INTO agent_audit(stamp,client,request,decision) VALUES(?,?,?,?)",
                params![
                    now_ms().min(i64::MAX as u64) as i64,
                    client,
                    request,
                    decision
                ],
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        self.connection.execute("DELETE FROM agent_audit WHERE sequence <= (SELECT MAX(sequence)-4096 FROM agent_audit)", [])
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        Ok(())
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests;
