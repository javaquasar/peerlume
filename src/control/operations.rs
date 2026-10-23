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
                "SELECT record FROM agent_operations WHERE client=? AND key=?",
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
