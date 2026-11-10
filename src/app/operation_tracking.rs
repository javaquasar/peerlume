//! Bridge real native receipts and authoritative owner state to recent operations.
//! Missing proof remains unconfirmed; this module never sends a command or retries.
use crate::{
    completion::CompletionStatus,
    control::{
        TorrentCommand,
        frontend::{ConnectionStatus, ConnectionView, Receipt},
    },
    runtime_status::{TransferAction, TransferFeedback},
    torrent::TorrentItem,
    ui_operations::{
        CompletionRule, HistoryError, MAX_HISTORY_ROWS, MAX_OWNER_BYTES, OperationHistory,
        OperationId, OperationKind, OperationPhase,
    },
};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone, Copy, Debug)]
pub(crate) struct CommandEvidence {
    pub revision: u64,
    pub durable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Pause,
    Resume,
    Recheck,
    Repair,
    Remove,
    Other,
}
impl Effect {
    fn from_command(command: Option<&TorrentCommand>) -> Self {
        match command {
            Some(TorrentCommand::Pause) => Self::Pause,
            Some(TorrentCommand::Resume) => Self::Resume,
            Some(TorrentCommand::Recheck) => Self::Recheck,
            Some(TorrentCommand::RecheckAndRepair) => Self::Repair,
            Some(TorrentCommand::Remove) => Self::Remove,
            _ => Self::Other,
        }
    }
    fn action(self) -> Option<TransferAction> {
        match self {
            Self::Pause => Some(TransferAction::Pause),
            Self::Resume => Some(TransferAction::Resume),
            Self::Recheck | Self::Repair => Some(TransferAction::Recheck),
            Self::Remove | Self::Other => None,
        }
    }
}

struct Job {
    operation_id: OperationId,
    owner: String,
    target: String,
    effect: Effect,
    accepted_revision: Option<u64>,
    snapshot_after: u64,
    final_receipt: bool,
}

#[derive(Default)]
pub(crate) struct Tracker {
    pub history: OperationHistory,
    jobs: BTreeMap<u64, Job>,
    retired: VecDeque<u64>,
    expired_retired_through: u64,
    owner: Option<String>,
    snapshot_serial: u64,
}

impl Tracker {
    fn retire(&mut self, id: u64) {
        if !self.retired.contains(&id) {
            self.retired.push_back(id);
            if self.retired.len() > MAX_HISTORY_ROWS {
                let expired = self.retired.pop_front().expect("bounded tombstone queue");
                self.expired_retired_through = self.expired_retired_through.max(expired);
            }
        }
    }
    fn prune(&mut self) {
        let obsolete: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| self.history.get(job.operation_id).is_none())
            .map(|(&id, _)| id)
            .collect();
        for id in obsolete {
            self.jobs.remove(&id);
            self.retire(id);
        }
        while self.jobs.len() >= MAX_HISTORY_ROWS {
            let oldest = self
                .jobs
                .iter()
                .find(|(_, job)| {
                    self.history
                        .get(job.operation_id)
                        .is_some_and(|entry| entry.phase.is_terminal())
                })
                .map(|(&id, _)| id);
            let Some(id) = oldest else { break };
            self.jobs.remove(&id);
            self.retire(id);
        }
    }

    pub fn record_error(
        &mut self,
        kind: &str,
        target_id: Option<&str>,
        name: &str,
        message: &str,
    ) -> Result<OperationId, HistoryError> {
        let id = self.history.begin(
            kind_from_label(kind),
            target_id,
            name,
            CompletionRule::LocalReturn,
        )?;
        self.history.fail(id, &format!("{kind}: {message}"))?;
        Ok(id)
    }

    pub fn receipt(
        &mut self,
        receipt: &Receipt,
        torrents: &[TorrentItem],
    ) -> Result<Option<OperationId>, HistoryError> {
        // Quiet polls and old post-dismissal duplicates do not create new operations.
        if receipt.job_id == 0
            || receipt.operation.is_empty()
            || receipt.operation == "Agent view"
            || receipt.label == "Agent view"
        {
            return Ok(None);
        }
        if receipt.owner_instance.is_empty()
            || receipt.owner_instance.len() > MAX_OWNER_BYTES
            || receipt.owner_instance.chars().any(char::is_control)
        {
            return Err(HistoryError::InvalidIdentity);
        }
        self.prune();
        if !receipt.queued
            && !receipt.ambiguous_error
            && receipt.operation_id.as_deref().is_some_and(|id| {
                !id.is_empty() && id.len() <= MAX_OWNER_BYTES && !id.chars().any(char::is_control)
            })
            && self
                .jobs
                .get(&receipt.job_id)
                .is_none_or(|job| job.owner == receipt.owner_instance)
        {
            if let Some(job) = self.jobs.remove(&receipt.job_id) {
                self.history.handoff_to_owner(job.operation_id)?;
            }
            self.retire(receipt.job_id);
            return Ok(None);
        }
        if !self.jobs.contains_key(&receipt.job_id) {
            if self.retired.contains(&receipt.job_id) {
                return Ok(None);
            }
            let torrent = torrents.iter().find(|item| {
                item.info_hashes.iter().any(|hash| hash == &receipt.id)
                    || item.info_hashes.join(";") == receipt.id
            });
            let canonical = torrent
                .map(|item| item.info_hashes.join(";"))
                .unwrap_or_else(|| receipt.id.clone());
            let name = torrent.map_or(receipt.operation, |item| item.name.as_str());
            let rule = if receipt.completed_by_reply {
                CompletionRule::OwnerDurableReceipt
            } else {
                CompletionRule::EngineAndDurable
            };
            let id = self.history.begin(
                kind_from_receipt(receipt),
                (!canonical.is_empty()).then_some(canonical.as_str()),
                name,
                rule,
            )?;
            self.history.dispatched(id)?;
            self.jobs.insert(
                receipt.job_id,
                Job {
                    operation_id: id,
                    owner: receipt.owner_instance.clone(),
                    target: canonical,
                    effect: Effect::from_command(receipt.command.as_ref()),
                    accepted_revision: None,
                    snapshot_after: 0,
                    final_receipt: false,
                },
            );
            if receipt.job_id <= self.expired_retired_through {
                self.history.needs_review(id, "A late receipt is outside retained operation history. Its earlier outcome cannot be reconstructed; inspect current state before retrying.")?;
            }
            debug_assert!(self.jobs.len() <= MAX_HISTORY_ROWS);
        }
        let job = self
            .jobs
            .get_mut(&receipt.job_id)
            .expect("new or retained job");
        let id = job.operation_id;
        if job.owner != receipt.owner_instance {
            self.history.needs_review(
                id,
                "A receipt belongs to a different background owner; no operation was replayed.",
            )?;
            return Ok(Some(id));
        }
        if self.history.get(id).is_some_and(|entry| {
            entry.phase.is_terminal() || entry.phase == OperationPhase::NeedsReview
        }) {
            return Ok(Some(id));
        }
        if receipt.queued {
            return Ok(Some(id));
        }
        job.final_receipt = true;
        job.snapshot_after = receipt.snapshot_after;
        if let Some(error) = &receipt.error {
            if receipt.ambiguous_error {
                self.history.needs_review(id, error)?;
            } else {
                self.history.fail(id, error)?;
            }
            return Ok(Some(id));
        }
        if let Some(accepted) = &receipt.accepted {
            if accepted.torrent_id != job.target || accepted.command_revision == 0 {
                self.history.needs_review(
                    id,
                    "The owner response did not identify the same torrent and command revision.",
                )?;
            } else if job
                .accepted_revision
                .is_some_and(|old| old != accepted.command_revision)
            {
                self.history.needs_review(
                    id,
                    "A later command response cannot replace this operation's accepted revision.",
                )?;
            } else {
                self.history
                    .accepted(id, &job.owner, Some(accepted.command_revision))?;
                job.accepted_revision = Some(accepted.command_revision);
                if job.effect == Effect::Other {
                    self.history.needs_review(id, "The owner accepted this change, but correlated final completion proof is unavailable. Inspect current state; dismissing this record will not retry or cancel the request.")?;
                }
            }
        } else if receipt.completed_by_reply {
            self.history.accepted(id, &job.owner, None)?;
            self.history
                .owner_completed(id, &job.owner, &receipt.message)?;
        } else {
            self.history.needs_review(id, "The background owner replied without correlated completion evidence. Inspect current state before retrying.")?;
        }
        Ok(Some(id))
    }

    /// Call only after importing the native snapshot into the controller mirror.
    pub fn reconcile(
        &mut self,
        connection: &ConnectionView,
        torrents: &[TorrentItem],
        mut evidence: impl FnMut(&str) -> Option<CommandEvidence>,
    ) {
        self.prune();
        if let Some(owner) = &connection.owner_instance {
            if self
                .owner
                .as_ref()
                .is_some_and(|previous| previous != owner)
            {
                self.history.owner_changed(owner);
                self.snapshot_serial = 0;
            }
            self.owner = Some(owner.clone());
        }
        if connection.status != ConnectionStatus::Connected {
            if let Some(owner) = &self.owner {
                self.history.owner_disconnected(owner);
            }
            return;
        }
        let Some(owner) = connection.owner_instance.as_deref() else {
            return;
        };
        if connection.snapshot_serial < self.snapshot_serial {
            self.history.owner_disconnected(owner);
            return;
        }
        if connection.snapshot_serial == self.snapshot_serial {
            return;
        }
        self.snapshot_serial = connection.snapshot_serial;
        if !self.jobs.values().any(|job| {
            job.final_receipt
                && job.accepted_revision.is_some()
                && job.effect != Effect::Other
                && job.snapshot_after > 0
                && connection.snapshot_serial >= job.snapshot_after
                && self.history.get(job.operation_id).is_some_and(|entry| {
                    !entry.phase.is_terminal() && entry.phase != OperationPhase::NeedsReview
                })
        }) {
            return;
        }
        let mut indexed = HashMap::new();
        for torrent in torrents {
            for hash in &torrent.info_hashes {
                indexed.insert(hash.clone(), torrent);
            }
            indexed.insert(torrent.info_hashes.join(";"), torrent);
        }
        for job in self.jobs.values() {
            let Some(entry) = self.history.get(job.operation_id) else {
                continue;
            };
            if entry.phase.is_terminal()
                || entry.phase == OperationPhase::NeedsReview
                || !job.final_receipt
            {
                continue;
            }
            if job.owner != owner {
                let _ = self.history.needs_review(
                    job.operation_id,
                    "The background owner changed before this operation was confirmed.",
                );
                continue;
            }
            let Some(revision) = job.accepted_revision else {
                continue;
            };
            if job.snapshot_after == 0 || connection.snapshot_serial < job.snapshot_after {
                continue;
            }
            let item = indexed.get(&job.target).copied();
            if job.effect == Effect::Remove && item.is_none() {
                // Owner snapshots detach the canonical item only after durable removal.
                let _ = self
                    .history
                    .effect_completed(job.operation_id, owner, Some(revision));
                let _ = self
                    .history
                    .durable_completed(job.operation_id, owner, Some(revision));
                continue;
            }
            let Some(item) = item else {
                let _ = self.history.needs_review(job.operation_id, "The torrent is absent from the owner's current snapshot. Its earlier operation cannot be confirmed; inspect current state.");
                continue;
            };
            let Some(proof) = evidence(&job.target) else {
                let _ = self.history.needs_review(job.operation_id, "The current owner snapshot has no command revision for this torrent. Completion cannot be confirmed; inspect current state.");
                continue;
            };
            if proof.revision != revision {
                if proof.revision > revision {
                    let _ = self.history.needs_review(
                        job.operation_id,
                        "A superseding command prevents confirmation of this earlier operation.",
                    );
                }
                continue;
            }
            let Some(action) = job.effect.action() else {
                continue;
            };
            match &item.runtime.command {
                TransferFeedback::Failed {
                    action: failed,
                    message,
                } if *failed == action => {
                    let _ = self.history.fail(job.operation_id, message);
                    continue;
                }
                TransferFeedback::Confirmed { action: confirmed } if *confirmed == action => {}
                _ => continue,
            }
            if matches!(job.effect, Effect::Recheck | Effect::Repair) {
                match &item.completion {
                    CompletionStatus::Checking {
                        checked_bytes,
                        total_bytes,
                    } => {
                        let _ = self.history.progress(
                            job.operation_id,
                            *checked_bytes,
                            Some(*total_bytes),
                            "Integrity check in progress",
                        );
                        continue;
                    }
                    CompletionStatus::Failed { message } => {
                        let _ = self.history.fail(job.operation_id, message);
                        continue;
                    }
                    CompletionStatus::Verified => {}
                    CompletionStatus::Pending => continue,
                }
            }
            let _ = self
                .history
                .effect_completed(job.operation_id, owner, Some(revision));
            if proof.durable {
                let _ = self
                    .history
                    .durable_completed(job.operation_id, owner, Some(revision));
            }
        }
    }
}

fn kind_from_receipt(receipt: &Receipt) -> OperationKind {
    match receipt.command.as_ref() {
        Some(TorrentCommand::Pause) => OperationKind::Pause,
        Some(TorrentCommand::Resume) => OperationKind::Resume,
        Some(TorrentCommand::Recheck) => OperationKind::Recheck,
        Some(TorrentCommand::RecheckAndRepair) => OperationKind::Repair,
        Some(TorrentCommand::Remove) => OperationKind::Remove,
        Some(TorrentCommand::SetFiles(_)) => OperationKind::FileSelection,
        Some(TorrentCommand::SetMode(_)) => OperationKind::SetMode,
        Some(TorrentCommand::SetRepairPolicy(_)) => OperationKind::RepairPolicy,
        _ => kind_from_label(receipt.operation),
    }
}
fn kind_from_label(label: &str) -> OperationKind {
    let label = label.to_ascii_lowercase();
    if label.contains("recycle") {
        OperationKind::Recycle
    } else if label.contains("export") {
        OperationKind::Export
    } else if label.contains("copy") {
        OperationKind::Copy
    } else if label.contains("move") || label.contains("existing data") {
        OperationKind::Relocate
    } else if label.contains("priorit") {
        OperationKind::Priorities
    } else if label.contains("files") || label.contains("selection") {
        OperationKind::FileSelection
    } else if label.contains("view") || label.contains("appearance") {
        OperationKind::ViewSettings
    } else if label.contains("settings") || label.contains("preferences") {
        OperationKind::Preferences
    } else if label.contains("add") || label.contains("import") {
        OperationKind::Add
    } else if label.contains("pause") {
        OperationKind::Pause
    } else if label.contains("resume") {
        OperationKind::Resume
    } else if label.contains("recheck") || label.contains("integrity") {
        OperationKind::Recheck
    } else if label.contains("reveal") || label.contains("location") {
        OperationKind::Reveal
    } else {
        OperationKind::Other
    }
}

#[cfg(test)]
mod tests;
