//! Trusted owner operation records. A restart preserves uncertainty and never replays work.
use super::wire::{
    Action, Error, NativeOperation, NativeOperationPage, NativeOperationProgress,
    NativeOperationState as State, Reply, StorageAction,
};
use crate::{
    completion::CompletionStatus,
    control::{TorrentCommand, access::random_id, operations::now_ms},
    runtime_status::TransferAction,
    session::SessionEvent,
    storage::relocation::StorageStatus,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CAPACITY: usize = 256;
const MAX_PAGE: usize = 32;
const PROOF_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const UNPROVEN: &str = "The owner accepted the request; its final outcome requires review";
const REPLACED: &str = "The previous owner ended before this operation was confirmed";
const SUPERSEDED: &str = "Overlapping commands prevent a unique engine acknowledgement";
const PERSISTING: &str = "Engine effect confirmed; waiting for durable state";

pub(crate) type SharedHistory = Arc<Mutex<History>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Immediate,
    Recycle,
    Unproven,
    Pause,
    Resume,
    Start,
    Add,
    Remove,
    Check,
    Storage,
    Cleanup,
}

pub(super) struct Intent {
    command: &'static str,
    target: Option<String>,
    revision: Option<u64>,
    effect: Effect,
}

impl Intent {
    pub(super) fn from_action(action: &Action) -> Option<Self> {
        let (command, target, revision, effect) = match action {
            Action::Mutate {
                id,
                command,
                revision,
            } => {
                let (command, effect) = match command {
                    TorrentCommand::Pause => ("Pause", Effect::Pause),
                    TorrentCommand::Resume => ("Resume", Effect::Resume),
                    TorrentCommand::Recheck => ("Recheck", Effect::Check),
                    TorrentCommand::RecheckAndRepair => ("Recheck and repair", Effect::Check),
                    TorrentCommand::Remove => ("Remove torrent", Effect::Remove),
                    TorrentCommand::SetFiles(_) => ("File selection", Effect::Unproven),
                    TorrentCommand::SetMode(_) => ("Download mode", Effect::Unproven),
                    TorrentCommand::SetRepairPolicy(_) => ("Repair policy", Effect::Unproven),
                    TorrentCommand::SetVerifyAfterDownload(_) => {
                        ("Verify after download", Effect::Unproven)
                    }
                    TorrentCommand::SetTransferSettings(_) => {
                        ("Torrent transfer settings", Effect::Unproven)
                    }
                };
                (command, safe_target(id), *revision, effect)
            }
            Action::Selection { id, revision, .. } => (
                "File selection",
                safe_target(id),
                Some(*revision),
                Effect::Unproven,
            ),
            Action::Priorities { id, revision, .. } => (
                "File priorities",
                safe_target(id),
                Some(*revision),
                Effect::Unproven,
            ),
            Action::Retry { id, revision } => (
                "Retry start",
                safe_target(id),
                Some(*revision),
                Effect::Start,
            ),
            Action::Add(request)
            | Action::ConfirmMagnetReview {
                torrent: request, ..
            } => (
                "Register torrent",
                safe_target(&request.hashes.join(";")),
                None,
                Effect::Add,
            ),
            Action::Storage {
                id,
                revision,
                action,
            } => {
                let (command, effect) = match action {
                    StorageAction::Select { copy: true, .. } => ("Copy data", Effect::Storage),
                    StorageAction::Select { copy: false, .. } => {
                        ("Use existing data", Effect::Storage)
                    }
                    StorageAction::Move { .. } => ("Move data", Effect::Storage),
                    StorageAction::Cancel => ("Cancel storage operation", Effect::Unproven),
                    StorageAction::Cleanup => ("Remove duplicate originals", Effect::Cleanup),
                    StorageAction::Priorities { .. } => ("File priorities", Effect::Unproven),
                };
                (command, safe_target(id), Some(*revision), effect)
            }
            Action::SaveSettings(_) => ("Save preferences", None, None, Effect::Immediate),
            Action::SaveView(_) => ("Save view", None, None, Effect::Immediate),
            Action::ApplySettings(_) => ("Apply preferences", None, None, Effect::Unproven),
            Action::SetSpeedPolicy(_) => ("Save speed profile", None, None, Effect::Immediate),
            Action::SaveEditable { id, revision, .. } => (
                "Save tracker settings",
                safe_target(id),
                Some(*revision),
                Effect::Immediate,
            ),
            Action::SaveAudit { id, revision, .. } => (
                "Save tracker audit",
                safe_target(id),
                Some(*revision),
                Effect::Immediate,
            ),
            Action::Export { id, .. } => (
                "Export announce journal",
                id.as_deref().and_then(safe_target),
                None,
                Effect::Immediate,
            ),
            Action::Trash { .. } => ("Recycle files", None, None, Effect::Recycle),
            // Reads and bootstrap do not fill history. Graceful Stop remains
            // available independently of operation history capacity or health.
            _ => return None,
        };
        Some(Self {
            command,
            target,
            revision,
            effect,
        })
    }
}

fn safe_target(id: &str) -> Option<String> {
    let valid_hash = |part: &str| {
        let hex = |value: &str, len| {
            value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        };
        part.strip_prefix("v1: ")
            .is_some_and(|value| hex(value, 40))
            || part
                .strip_prefix("v2: ")
                .is_some_and(|value| hex(value, 64))
            || part.strip_prefix("urn:btih:").is_some_and(|value| {
                hex(value, 40)
                    || (value.len() == 32
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'2'..=b'7')))
            })
            || part
                .strip_prefix("urn:btmh:1220")
                .is_some_and(|value| hex(value, 64))
    };
    let mut parts = id.split(';');
    (parts.next().is_some_and(valid_hash)
        && parts.next().is_none_or(valid_hash)
        && parts.next().is_none())
    .then(|| id.to_owned())
}

#[derive(Clone, Copy)]
pub(super) struct Ticket {
    sequence: i64,
    effect: Effect,
}

struct Pending {
    ticket: Ticket,
    revision: u64,
    started: bool,
    manual_recheck: bool,
    durable: bool,
    last_progress: Instant,
    sampled_progress: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Feedback {
    Paused,
    Resumed,
    Added,
    Removed,
    Checking(NativeOperationProgress),
    Verified,
    VerificationPending,
    VerificationFailed,
    Copying(u64),
    CheckingDestination,
    OriginalsRemoved,
    StorageFailed,
    UncertainError,
}

impl Feedback {
    pub(crate) fn from_event(
        event: &SessionEvent,
        pending: Option<TransferAction>,
    ) -> Option<Self> {
        Some(match event {
            SessionEvent::StateChanged {
                state: "paused", ..
            } if pending == Some(TransferAction::Pause) => Self::Paused,
            SessionEvent::StateChanged {
                state: "downloading",
                ..
            } if pending == Some(TransferAction::Resume) => Self::Resumed,
            SessionEvent::Added { .. } => Self::Added,
            SessionEvent::Removed { .. } => Self::Removed,
            SessionEvent::Verification { status, .. } => match status {
                CompletionStatus::Checking {
                    checked_bytes,
                    total_bytes,
                } => Self::Checking(NativeOperationProgress {
                    bytes: *checked_bytes,
                    total: Some(*total_bytes),
                }),
                CompletionStatus::Verified => Self::Verified,
                CompletionStatus::Pending => Self::VerificationPending,
                CompletionStatus::Failed { .. } => Self::VerificationFailed,
            },
            SessionEvent::Storage { status, .. } => match status {
                StorageStatus::Copying { bytes, .. } => Self::Copying(*bytes),
                StorageStatus::Checking { .. } => Self::CheckingDestination,
                StorageStatus::OriginalsRemoved { .. } => Self::OriginalsRemoved,
                StorageStatus::Failed { .. } => Self::StorageFailed,
                _ => return None,
            },
            SessionEvent::Error { .. } => Self::UncertainError,
            _ => return None,
        })
    }
}

pub(crate) struct History {
    connection: Connection,
    healthy: bool,
    pending: HashMap<String, Pending>,
    // Older events carry no command revision. Once same-target operations
    // overlap, later state alone cannot uniquely identify their engine effects.
    ambiguous: HashSet<String>,
    proof_disabled: bool,
}

impl History {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        for file in [
            path.to_owned(),
            path.with_extension("sqlite3-wal"),
            path.with_extension("sqlite3-shm"),
            path.with_extension("sqlite3-journal"),
        ] {
            match std::fs::symlink_metadata(file) {
                Ok(metadata) if regular(&metadata) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(io::Error::other("Unsafe operation history file")),
            }
        }
        Self::initialize(Connection::open(path).map_err(io::Error::other)?)
    }

    fn initialize(connection: Connection) -> io::Result<Self> {
        connection
            .busy_timeout(Duration::from_millis(50))
            .map_err(io::Error::other)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=128;
             CREATE TABLE IF NOT EXISTS native_operations (
               sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               terminal INTEGER NOT NULL,
               record TEXT NOT NULL
             );",
            )
            .map_err(io::Error::other)?;
        let mut history = Self {
            connection,
            healthy: true,
            pending: HashMap::new(),
            ambiguous: HashSet::new(),
            proof_disabled: false,
        };
        history.recover().map_err(io::Error::other)?;
        Ok(history)
    }

    #[cfg(test)]
    pub(super) fn memory() -> SharedHistory {
        Arc::new(Mutex::new(
            Self::initialize(Connection::open_in_memory().unwrap()).unwrap(),
        ))
    }

    fn recover(&mut self) -> Result<(), Error> {
        let tx = self.connection.transaction().map_err(history_error)?;
        let count: i64 = tx
            .query_row("SELECT count(*) FROM native_operations", [], |row| {
                row.get(0)
            })
            .map_err(history_error)?;
        if count > CAPACITY as i64 {
            return Err(unavailable());
        }
        let pending = {
            let mut rows = tx.prepare("SELECT sequence,record FROM native_operations WHERE terminal=0 ORDER BY sequence LIMIT 257").map_err(history_error)?;
            let decoded = rows
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(history_error)?;
            decoded
                .collect::<Result<Vec<_>, _>>()
                .map_err(history_error)?
        };
        if pending.len() > CAPACITY {
            return Err(unavailable());
        }
        for (sequence, json) in pending {
            let mut record = serde_json::from_str(&json).map_err(history_error)?;
            set_state(&mut record, State::NeedsReview, Some(REPLACED));
            write(&tx, sequence, &record)?;
        }
        tx.commit().map_err(history_error)
    }

    pub(super) fn begin(
        &mut self,
        owner: &str,
        request: &str,
        intent: Intent,
    ) -> Result<(Ticket, String), Error> {
        if !self.healthy {
            return Err(unavailable());
        }
        self.expire();
        let id = random_id().map_err(history_error)?;
        let tx = self.connection.transaction().map_err(history_error)?;
        let count: i64 = tx
            .query_row("SELECT count(*) FROM native_operations", [], |row| {
                row.get(0)
            })
            .map_err(history_error)?;
        if count >= CAPACITY as i64
            && tx.execute("DELETE FROM native_operations WHERE sequence=(SELECT MIN(sequence) FROM native_operations WHERE terminal=1)", []).map_err(history_error)? == 0
        {
            return Err(Error::Busy);
        }
        tx.execute(
            "INSERT INTO native_operations(terminal,record) VALUES(0,'')",
            [],
        )
        .map_err(history_error)?;
        let sequence = tx.last_insert_rowid();
        let record = NativeOperation {
            id: id.clone(),
            sequence: u64::try_from(sequence).map_err(history_error)?,
            owner_instance: owner.to_owned(),
            request_id: if request.len() == 64
                && request.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                request.to_owned()
            } else {
                String::new()
            },
            target: intent.target,
            command: intent.command.to_owned(),
            revision: intent.revision,
            state: State::Queued,
            progress: None,
            message: None,
            created_unix_ms: now_ms(),
        };
        write(&tx, sequence, &record)?;
        tx.commit().map_err(history_error)?;
        Ok((
            Ticket {
                sequence,
                effect: intent.effect,
            },
            id,
        ))
    }

    pub(super) fn page(
        &mut self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<NativeOperationPage, Error> {
        if !self.healthy {
            return Err(unavailable());
        }
        if !(1..=MAX_PAGE).contains(&limit) || before == Some(0) {
            return Err(Error::TooLarge);
        }
        self.expire();
        let mut rows = self.connection.prepare(
            "SELECT record FROM native_operations WHERE (?1 IS NULL OR sequence < ?1) ORDER BY sequence DESC LIMIT ?2"
        ).map_err(history_error)?;
        let before = before
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::TooLarge)?;
        let records = rows
            .query_map(params![before, (limit + 1) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(history_error)?;
        let mut entries = records
            .map(|row| {
                serde_json::from_str::<NativeOperation>(&row.map_err(history_error)?)
                    .map_err(history_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_before = if entries.len() > limit {
            entries.pop();
            entries.last().map(|row| row.sequence)
        } else {
            None
        };
        Ok(NativeOperationPage {
            entries,
            next_before,
        })
    }

    pub(super) fn accepted(&mut self, ticket: Ticket, id: &str, revision: u64, fresh: bool) {
        let overlap = self.pending.remove(id);
        if let Some(earlier) = overlap {
            self.mark_ambiguous(id);
            let _ = self.update(earlier.ticket, |row| {
                set_state(row, State::NeedsReview, Some(SUPERSEDED))
            });
        }
        let target = safe_target(id);
        let provable = fresh
            && target.is_some()
            && !self.proof_disabled
            && !self.ambiguous.contains(id)
            && !matches!(ticket.effect, Effect::Unproven | Effect::Immediate);
        if self
            .update(ticket, |row| {
                row.target = target;
                row.revision = Some(revision);
                if provable {
                    set_state(row, State::Accepted, None);
                } else {
                    set_state(
                        row,
                        State::NeedsReview,
                        Some(if self.ambiguous.contains(id) {
                            SUPERSEDED
                        } else {
                            UNPROVEN
                        }),
                    );
                }
            })
            .is_ok()
            && provable
        {
            self.pending.insert(
                id.to_owned(),
                Pending {
                    ticket,
                    revision,
                    started: false,
                    manual_recheck: false,
                    durable: false,
                    last_progress: Instant::now(),
                    sampled_progress: false,
                },
            );
        }
    }

    pub(super) fn finish_rpc(&mut self, ticket: Ticket, result: &Result<Reply, Error>) -> bool {
        if self.update(ticket, |row| {
            if row.state != State::Queued {
                return;
            }
            match result {
                Ok(_) if matches!(ticket.effect, Effect::Immediate | Effect::Recycle) => set_state(row, State::Succeeded, None),
                Ok(_) => set_state(row, State::NeedsReview, Some(UNPROVEN)),
                Err(Error::Failed(_) | Error::Transport(_)) => set_state(row, State::NeedsReview, Some("The operation did not complete; inspect the current owner state before retrying")),
                Err(Error::Stale) if ticket.effect == Effect::Recycle => set_state(row, State::NeedsReview, Some("The Recycle Bin plan changed; inspect the torrent and files before retrying")),
                Err(_) => set_state(row, State::Failed, Some("The owner rejected the request")),
            }
        }).is_err() {
            self.healthy = false;
        }
        self.healthy
    }

    pub(super) fn degrade(&mut self) {
        self.healthy = false;
    }

    pub(crate) fn observe(&mut self, id: &str, revision: u64, feedback: Feedback) {
        let Some(mut pending) = self.pending.remove(id) else {
            return;
        };
        let ticket = pending.ticket;
        if pending.revision != revision {
            self.mark_ambiguous(id);
            let _ = self.update(ticket, |row| {
                set_state(row, State::NeedsReview, Some(SUPERSEDED))
            });
            return;
        }
        let sampling = matches!(
            (ticket.effect, feedback),
            (Effect::Check, Feedback::Checking(_)) | (Effect::Storage, Feedback::Copying(_))
        );
        if sampling
            && pending.sampled_progress
            && pending.last_progress.elapsed() < PROGRESS_INTERVAL
        {
            self.pending.insert(id.to_owned(), pending);
            return;
        }
        let state = self.update(ticket, |row| match (ticket.effect, feedback) {
            (Effect::Pause, Feedback::Paused)
            | (Effect::Resume, Feedback::Resumed)
            | (Effect::Start, Feedback::Added)
            | (Effect::Remove, Feedback::Removed) => {
                set_state(row, State::EffectConfirmed, Some(PERSISTING));
            }
            (Effect::Add, Feedback::Added) | (Effect::Cleanup, Feedback::OriginalsRemoved) => {
                set_state(row, State::Succeeded, None)
            }
            (Effect::Check, Feedback::Checking(progress)) => {
                pending.started = true;
                row.progress = Some(progress);
                set_state(row, State::Running, Some("Integrity check in progress"));
            }
            (Effect::Check, Feedback::Added) if pending.started => {
                // Explicit recheck queues Checking then emits Added after
                // its own restart_for_check succeeds. An automatic check
                // emits Checking and Verified without this Added marker.
                pending.manual_recheck = true;
            }
            (Effect::Check, Feedback::Verified) if pending.manual_recheck => {
                set_state(row, State::Succeeded, None)
            }
            (Effect::Check, Feedback::VerificationFailed) if pending.manual_recheck => {
                set_state(row, State::Failed, Some("Engine verification failed"))
            }
            (Effect::Check, Feedback::VerificationFailed) => set_state(
                row,
                State::NeedsReview,
                Some("Verification failed before the requested restart was confirmed"),
            ),
            (Effect::Storage, Feedback::Copying(bytes)) => {
                row.progress = Some(NativeOperationProgress { bytes, total: None });
                set_state(row, State::Running, Some("Copying data"));
            }
            (Effect::Storage, Feedback::CheckingDestination) => {
                pending.started = true;
                set_state(row, State::Running, Some("Checking the destination"));
            }
            (Effect::Storage, Feedback::Verified) if pending.started => {
                if pending.durable {
                    set_state(row, State::Succeeded, None);
                } else {
                    set_state(row, State::EffectConfirmed, Some(PERSISTING));
                }
            }
            (Effect::Storage, Feedback::VerificationFailed | Feedback::VerificationPending)
                if pending.started =>
            {
                set_state(
                    row,
                    State::NeedsReview,
                    Some("Destination verification did not confirm complete data"),
                )
            }
            (Effect::Storage | Effect::Cleanup, Feedback::StorageFailed)
            | (_, Feedback::UncertainError) => set_state(
                row,
                State::NeedsReview,
                Some("The engine reported an error; inspect the current state before retrying"),
            ),
            _ => {}
        });
        if state.as_ref().is_ok_and(|(_, changed)| *changed) {
            pending.last_progress = Instant::now();
            pending.sampled_progress |= sampling;
        }
        self.retain(id, pending, state);
    }

    pub(crate) fn persisted(&mut self, id: &str, revision: u64, removal: bool) {
        let Some(mut pending) = self.pending.remove(id) else {
            return;
        };
        if pending.revision != revision {
            self.mark_ambiguous(id);
            let _ = self.update(pending.ticket, |row| {
                set_state(row, State::NeedsReview, Some(SUPERSEDED))
            });
            return;
        }
        if (pending.ticket.effect == Effect::Remove) != removal {
            self.pending.insert(id.to_owned(), pending);
            return;
        }
        if pending.ticket.effect == Effect::Storage && pending.started {
            pending.durable = true;
        }
        let state = self.update(pending.ticket, |row| {
            if row.state == State::EffectConfirmed {
                set_state(row, State::Succeeded, None);
            }
        });
        self.retain(id, pending, state);
    }

    fn retain(&mut self, id: &str, pending: Pending, state: Result<(State, bool), Error>) {
        match state {
            Ok((State::NeedsReview, _)) => self.mark_ambiguous(id),
            Ok((state, _)) if terminal(state) => {}
            Err(_) => self.healthy = false,
            _ => {
                self.pending.insert(id.to_owned(), pending);
            }
        }
    }

    fn mark_ambiguous(&mut self, id: &str) {
        if self.ambiguous.len() >= CAPACITY {
            self.proof_disabled = true;
        } else {
            self.ambiguous.insert(id.to_owned());
        }
    }

    fn expire(&mut self) {
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.last_progress.elapsed() >= PROOF_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let pending = &self.pending[&id];
            if self.update(pending.ticket, |row| set_state(row, State::NeedsReview, Some("No new confirmation arrived; inspect the current state before retrying"))).is_ok() {
                self.pending.remove(&id);
                self.mark_ambiguous(&id);
            }
        }
    }

    fn update(
        &self,
        ticket: Ticket,
        modify: impl FnOnce(&mut NativeOperation),
    ) -> Result<(State, bool), Error> {
        let stored = self
            .connection
            .query_row(
                "SELECT record FROM native_operations WHERE sequence=?1",
                [ticket.sequence],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(history_error)?
            .ok_or_else(unavailable)?;
        let mut record: NativeOperation = serde_json::from_str(&stored).map_err(history_error)?;
        let previous = record.clone();
        modify(&mut record);
        let changed = previous != record;
        if changed {
            write(&self.connection, ticket.sequence, &record)?;
        }
        Ok((record.state, changed))
    }
}

fn terminal(state: State) -> bool {
    matches!(state, State::Succeeded | State::Failed | State::NeedsReview)
}

fn set_state(record: &mut NativeOperation, state: State, message: Option<&str>) {
    record.state = state;
    record.message = message.map(str::to_owned);
}

fn write(connection: &Connection, sequence: i64, record: &NativeOperation) -> Result<(), Error> {
    let json = serde_json::to_string(record).map_err(history_error)?;
    connection
        .execute(
            "UPDATE native_operations SET terminal=?1,record=?2 WHERE sequence=?3",
            params![terminal(record.state), json, sequence],
        )
        .map_err(history_error)?;
    Ok(())
}

pub(super) fn unavailable() -> Error {
    Error::Failed("Owner operation history is unavailable".into())
}

fn regular(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}

fn history_error(_: impl std::fmt::Display) -> Error {
    unavailable()
}

#[cfg(test)]
mod tests;
