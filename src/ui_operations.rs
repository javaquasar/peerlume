//! Bounded recent UI operations. This is evidence presentation, not an audit log.
use std::{
    collections::VecDeque,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_HISTORY_ROWS: usize = 256;
pub const MAX_HISTORY_BYTES: usize = 512 * 1024;
const MAX_MESSAGE_BYTES: usize = 2048;
const MAX_NAME_BYTES: usize = 512;
const MAX_TARGET_BYTES: usize = 256;
pub(crate) const MAX_OWNER_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(u64);
impl OperationId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Add,
    Pause,
    Resume,
    Recheck,
    Repair,
    Relocate,
    Copy,
    Remove,
    Recycle,
    Preferences,
    ViewSettings,
    FileSelection,
    Priorities,
    SetMode,
    RepairPolicy,
    Export,
    TrackerAudit,
    Reveal,
    Other,
}
impl OperationKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "Add torrent",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::Recheck => "Integrity check",
            Self::Repair => "Check and repair",
            Self::Relocate => "Move data",
            Self::Copy => "Verify and copy",
            Self::Remove => "Remove from list",
            Self::Recycle => "Recycle confirmed data",
            Self::Preferences => "Save preferences",
            Self::ViewSettings => "Save view",
            Self::FileSelection => "Select files",
            Self::Priorities => "File priorities",
            Self::SetMode => "Change transfer mode",
            Self::RepairPolicy => "Change repair policy",
            Self::Export => "Export log",
            Self::TrackerAudit => "Controlled tracker audit",
            Self::Reveal => "Show location",
            Self::Other => "Operation",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionRule {
    /// Both the matching engine/effect completion and later saved-state proof.
    EngineAndDurable,
    /// A correlated owner reply explicitly establishes a completed durable job.
    OwnerDurableReceipt,
    /// Only the local API result; never a transfer or durability claim.
    LocalReturn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationPhase {
    Queued,
    Accepted,
    Running,
    Persisting,
    Completed,
    LocalCompleted,
    Failed,
    Cancelled,
    NeedsReview,
}
impl OperationPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Accepted => "Accepted · waiting for confirmation",
            Self::Running => "In progress",
            Self::Persisting => "Confirmed · saving",
            Self::Completed => "Completed and saved",
            Self::LocalCompleted => "Local action completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled before dispatch",
            Self::NeedsReview => "Outcome unknown · inspect current state",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::LocalCompleted | Self::Failed | Self::Cancelled
        )
    }
    pub fn is_pending(self) -> bool {
        !self.is_terminal()
    }
    pub fn tone(self) -> crate::appearance::Tone {
        use crate::appearance::Tone;
        match self {
            Self::Completed => Tone::Success,
            Self::Failed => Tone::Danger,
            Self::NeedsReview | Self::Persisting => Tone::Warning,
            Self::Accepted | Self::Running | Self::LocalCompleted => Tone::Info,
            Self::Queued | Self::Cancelled => Tone::Neutral,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correlation {
    pub owner_instance: String,
    pub revision: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
}
impl Progress {
    pub fn fraction(self) -> Option<f32> {
        self.total
            .filter(|total| *total > 0)
            .map(|total| (self.done as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}
#[derive(Clone, Debug)]
pub struct OperationEntry {
    pub id: OperationId,
    pub kind: OperationKind,
    pub target_id: Option<String>,
    pub target_name: String,
    pub phase: OperationPhase,
    pub rule: CompletionRule,
    pub correlation: Option<Correlation>,
    pub progress: Option<Progress>,
    pub message: String,
    pub display_truncated: bool,
    pub created_unix_ms: u64,
    pub started_at: Instant,
    dispatched: bool,
    reserved_bytes: usize,
}
impl OperationEntry {
    pub fn error(&self) -> Option<&str> {
        matches!(
            self.phase,
            OperationPhase::Failed | OperationPhase::NeedsReview
        )
        .then_some(self.message.as_str())
    }
    fn actual_string_bytes(&self) -> usize {
        self.target_name.capacity()
            + self.target_id.as_ref().map_or(0, String::capacity)
            + self.message.capacity()
            + self
                .correlation
                .as_ref()
                .map_or(0, |proof| proof.owner_instance.capacity())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    InvalidLimits,
    InvalidIdentity,
    HistoryFull,
    IdExhausted,
    UnknownOperation,
    EvidenceMismatch,
    WrongPhase,
    WrongCompletionRule,
    AmbiguousOutcome,
    ProgressTotalChanged,
    AlreadyDispatched,
}
impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidLimits=>"Invalid operation history limits",Self::InvalidIdentity=>"Invalid operation identity",
            Self::HistoryFull=>"Operation history is full of unconfirmed work; inspect current state before starting more",
            Self::IdExhausted=>"Operation ID space is exhausted",Self::UnknownOperation=>"Operation is no longer in recent history",
            Self::EvidenceMismatch=>"Owner or command revision does not match this operation",Self::WrongPhase=>"Operation evidence arrived in the wrong phase",
            Self::WrongCompletionRule=>"This response cannot prove completion for this operation",Self::AmbiguousOutcome=>"Reconnect and inspect current state; this operation was not replayed",
            Self::ProgressTotalChanged=>"Operation progress total changed; previous progress retained",Self::AlreadyDispatched=>"The operation was already dispatched and cannot be cancelled locally",
        })
    }
}
impl std::error::Error for HistoryError {}

pub struct OperationHistory {
    entries: VecDeque<OperationEntry>,
    row_limit: usize,
    byte_limit: usize,
    fixed_bytes: usize,
    reserved_bytes: usize,
    next_id: u64,
    revision: u64,
    evicted: u64,
    rejected: u64,
}
impl Default for OperationHistory {
    fn default() -> Self {
        Self::with_limits(MAX_HISTORY_ROWS, MAX_HISTORY_BYTES)
            .expect("default history bounds are valid")
    }
}
impl OperationHistory {
    pub fn with_limits(rows: usize, bytes: usize) -> Result<Self, HistoryError> {
        if rows == 0 || rows > MAX_HISTORY_ROWS || bytes > MAX_HISTORY_BYTES {
            return Err(HistoryError::InvalidLimits);
        }
        let entries = VecDeque::with_capacity(rows);
        let fixed = std::mem::size_of::<Self>()
            + entries.capacity() * std::mem::size_of::<OperationEntry>();
        if bytes < fixed + MAX_MESSAGE_BYTES + MAX_OWNER_BYTES + 1 {
            return Err(HistoryError::InvalidLimits);
        }
        Ok(Self {
            entries,
            row_limit: rows,
            byte_limit: bytes,
            fixed_bytes: fixed,
            reserved_bytes: fixed,
            next_id: 1,
            revision: 0,
            evicted: 0,
            rejected: 0,
        })
    }
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &OperationEntry> {
        self.entries.iter()
    }
    pub fn get(&self, id: OperationId) -> Option<&OperationEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.phase.is_pending())
            .count()
    }
    pub fn failed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.phase == OperationPhase::Failed)
            .count()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn evicted_count(&self) -> u64 {
        self.evicted
    }
    pub fn rejected_count(&self) -> u64 {
        self.rejected
    }
    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }
    pub fn retained_bytes(&self) -> usize {
        self.fixed_bytes
            + self
                .entries
                .iter()
                .map(OperationEntry::actual_string_bytes)
                .sum::<usize>()
    }

    pub fn begin(
        &mut self,
        kind: OperationKind,
        target_id: Option<&str>,
        name: &str,
        rule: CompletionRule,
    ) -> Result<OperationId, HistoryError> {
        if target_id.is_some_and(|id| !valid_identity(id, MAX_TARGET_BYTES)) {
            return self.reject(HistoryError::InvalidIdentity);
        }
        let Some(next) = self.next_id.checked_add(1) else {
            return self.reject(HistoryError::IdExhausted);
        };
        let (target_name, truncated) = bounded_text(name, MAX_NAME_BYTES, false);
        let target_id = target_id.map(str::to_owned);
        let reserve = target_name.capacity()
            + target_id.as_ref().map_or(0, String::capacity)
            + MAX_MESSAGE_BYTES
            + MAX_OWNER_BYTES;
        while self.entries.len() >= self.row_limit
            || self.reserved_bytes.saturating_add(reserve) > self.byte_limit
        {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.phase.is_terminal())
            else {
                return self.reject(HistoryError::HistoryFull);
            };
            let removed = self.entries.remove(index).expect("existing terminal row");
            self.reserved_bytes -= removed.reserved_bytes;
            self.evicted = self.evicted.saturating_add(1);
        }
        let id = OperationId(self.next_id);
        self.next_id = next;
        self.entries.push_back(OperationEntry {
            id,
            kind,
            target_id,
            target_name,
            phase: OperationPhase::Queued,
            rule,
            correlation: None,
            progress: None,
            message: String::new(),
            display_truncated: truncated,
            created_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            started_at: Instant::now(),
            dispatched: false,
            reserved_bytes: reserve,
        });
        self.reserved_bytes += reserve;
        self.changed();
        Ok(id)
    }
    fn reject<T>(&mut self, error: HistoryError) -> Result<T, HistoryError> {
        self.rejected = self.rejected.saturating_add(1);
        self.changed();
        Err(error)
    }
    fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
    fn entry_mut(&mut self, id: OperationId) -> Result<&mut OperationEntry, HistoryError> {
        self.entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(HistoryError::UnknownOperation)
    }
    pub fn dispatched(&mut self, id: OperationId) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        ensure_mutable(entry)?;
        if entry.phase != OperationPhase::Queued {
            return Err(HistoryError::WrongPhase);
        }
        if !entry.dispatched {
            entry.dispatched = true;
            self.changed();
        }
        Ok(())
    }
    pub fn accepted(
        &mut self,
        id: OperationId,
        owner: &str,
        revision: Option<u64>,
    ) -> Result<(), HistoryError> {
        if !valid_identity(owner, MAX_OWNER_BYTES) {
            return Err(HistoryError::InvalidIdentity);
        }
        let entry = self.entry_mut(id)?;
        if entry.rule == CompletionRule::LocalReturn {
            return Err(HistoryError::WrongCompletionRule);
        }
        if entry.rule == CompletionRule::EngineAndDurable && revision.is_none() {
            return Err(HistoryError::EvidenceMismatch);
        }
        if entry
            .correlation
            .as_ref()
            .is_some_and(|proof| proof.owner_instance == owner && proof.revision == revision)
        {
            if entry.phase == OperationPhase::NeedsReview {
                return Err(HistoryError::AmbiguousOutcome);
            }
            return Ok(());
        }
        ensure_mutable(entry)?;
        if entry.phase != OperationPhase::Queued {
            return Err(HistoryError::EvidenceMismatch);
        }
        entry.correlation = Some(Correlation {
            owner_instance: owner.to_owned(),
            revision,
        });
        entry.dispatched = true;
        entry.phase = OperationPhase::Accepted;
        set_message(entry, "Waiting for confirmation", false);
        self.changed();
        Ok(())
    }
    pub fn running(&mut self, id: OperationId, message: &str) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        ensure_mutable(entry)?;
        if !(matches!(
            entry.phase,
            OperationPhase::Accepted | OperationPhase::Running
        ) || (entry.phase == OperationPhase::Queued
            && entry.rule == CompletionRule::LocalReturn))
        {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Running;
        set_message(entry, message, false);
        self.changed();
        Ok(())
    }
    pub fn progress(
        &mut self,
        id: OperationId,
        done: u64,
        total: Option<u64>,
        message: &str,
    ) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        ensure_mutable(entry)?;
        if entry.phase == OperationPhase::Persisting {
            return Err(HistoryError::WrongPhase);
        }
        let total = total.filter(|total| *total > 0);
        let previous = entry.progress;
        let total = match (previous.and_then(|old| old.total), total) {
            (Some(old), Some(new)) if old != new => return Err(HistoryError::ProgressTotalChanged),
            (Some(old), _) => Some(old),
            (_, new) => new,
        };
        let done = previous.map_or(done, |old| old.done.max(done));
        if previous.is_some_and(|old| old.total.is_none())
            && total.is_some_and(|total| done > total)
        {
            return Err(HistoryError::ProgressTotalChanged);
        }
        let done = total.map_or(done, |total| done.min(total));
        entry.progress = Some(Progress { done, total });
        if entry.phase == OperationPhase::Accepted {
            entry.phase = OperationPhase::Running;
        }
        set_message(entry, message, false);
        self.changed();
        Ok(())
    }
    pub fn effect_completed(
        &mut self,
        id: OperationId,
        owner: &str,
        revision: Option<u64>,
    ) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.rule != CompletionRule::EngineAndDurable {
            return Err(HistoryError::WrongCompletionRule);
        }
        check_proof(entry, owner, revision)?;
        if matches!(
            entry.phase,
            OperationPhase::Persisting | OperationPhase::Completed
        ) {
            return Ok(());
        }
        ensure_mutable(entry)?;
        if !matches!(
            entry.phase,
            OperationPhase::Accepted | OperationPhase::Running
        ) {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Persisting;
        set_message(entry, "Effect confirmed; waiting for saved state", false);
        self.changed();
        Ok(())
    }
    pub fn durable_completed(
        &mut self,
        id: OperationId,
        owner: &str,
        revision: Option<u64>,
    ) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.rule != CompletionRule::EngineAndDurable {
            return Err(HistoryError::WrongCompletionRule);
        }
        check_proof(entry, owner, revision)?;
        if entry.phase == OperationPhase::Completed {
            return Ok(());
        }
        ensure_mutable(entry)?;
        if entry.phase != OperationPhase::Persisting {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Completed;
        set_message(entry, "Completed and saved", false);
        self.changed();
        Ok(())
    }
    pub fn owner_completed(
        &mut self,
        id: OperationId,
        owner: &str,
        message: &str,
    ) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.rule != CompletionRule::OwnerDurableReceipt {
            return Err(HistoryError::WrongCompletionRule);
        }
        if entry
            .correlation
            .as_ref()
            .is_none_or(|proof| proof.owner_instance != owner)
        {
            return Err(HistoryError::EvidenceMismatch);
        }
        if entry.phase == OperationPhase::Completed {
            return Ok(());
        }
        ensure_mutable(entry)?;
        if !matches!(
            entry.phase,
            OperationPhase::Accepted | OperationPhase::Running
        ) {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Completed;
        set_message(entry, message, false);
        self.changed();
        Ok(())
    }
    pub fn local_completed(&mut self, id: OperationId, message: &str) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.rule != CompletionRule::LocalReturn {
            return Err(HistoryError::WrongCompletionRule);
        }
        if entry.phase == OperationPhase::LocalCompleted {
            return Ok(());
        }
        ensure_mutable(entry)?;
        if !matches!(
            entry.phase,
            OperationPhase::Queued | OperationPhase::Running
        ) {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::LocalCompleted;
        set_message(entry, message, false);
        self.changed();
        Ok(())
    }
    /// Use for a known rejection/error. Ambiguous transport failures need needs_review.
    pub fn fail(&mut self, id: OperationId, error: &str) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.phase == OperationPhase::Failed {
            return Ok(());
        }
        if entry.phase.is_terminal() {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Failed;
        set_message(entry, error, true);
        self.changed();
        Ok(())
    }
    pub fn needs_review(&mut self, id: OperationId, reason: &str) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        if entry.phase.is_terminal() || entry.phase == OperationPhase::NeedsReview {
            return Ok(());
        }
        entry.phase = OperationPhase::NeedsReview;
        set_message(entry, reason, true);
        self.changed();
        Ok(())
    }
    pub fn owner_disconnected(&mut self, owner: &str) {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                !entry.phase.is_terminal()
                    && entry
                        .correlation
                        .as_ref()
                        .is_none_or(|proof| proof.owner_instance == owner)
            })
            .map(|entry| entry.id)
            .collect();
        for id in ids {
            let _=self.needs_review(id,"Connection lost. Inspect current state before deciding what to do; this operation was not replayed.");
        }
    }
    pub fn owner_changed(&mut self, new_owner: &str) {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                !entry.phase.is_terminal()
                    && entry
                        .correlation
                        .as_ref()
                        .is_none_or(|proof| proof.owner_instance != new_owner)
            })
            .map(|entry| entry.id)
            .collect();
        for id in ids {
            let _=self.needs_review(id,"Background client changed. Earlier receipts cannot confirm this operation; inspect current state.");
        }
    }
    pub fn cancel_before_dispatch(&mut self, id: OperationId) -> Result<(), HistoryError> {
        let entry = self.entry_mut(id)?;
        ensure_mutable(entry)?;
        if entry.dispatched {
            return Err(HistoryError::AlreadyDispatched);
        }
        if entry.phase != OperationPhase::Queued {
            return Err(HistoryError::WrongPhase);
        }
        entry.phase = OperationPhase::Cancelled;
        set_message(entry, "Cancelled before dispatch", false);
        self.changed();
        Ok(())
    }
    pub fn dismiss(&mut self, id: OperationId) -> Result<(), HistoryError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(HistoryError::UnknownOperation)?;
        if !self.entries[index].phase.is_terminal() {
            return Err(HistoryError::WrongPhase);
        }
        let entry = self.entries.remove(index).expect("existing finished row");
        self.reserved_bytes -= entry.reserved_bytes;
        self.changed();
        Ok(())
    }
    /// Drop only the local display row once a correlated owner operation exists.
    /// The authoritative owner retains and resolves the operation independently.
    #[cfg(feature = "gui")]
    pub(crate) fn handoff_to_owner(&mut self, id: OperationId) -> Result<(), HistoryError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(HistoryError::UnknownOperation)?;
        let entry = self.entries.remove(index).expect("existing local row");
        self.reserved_bytes -= entry.reserved_bytes;
        self.changed();
        Ok(())
    }
    /// Explicitly forget an uncertain display record without claiming success,
    /// cancelling work, or sending/retrying anything to the owner.
    pub fn acknowledge_review(&mut self, id: OperationId) -> Result<(), HistoryError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(HistoryError::UnknownOperation)?;
        if self.entries[index].phase != OperationPhase::NeedsReview {
            return Err(HistoryError::WrongPhase);
        }
        let entry = self.entries.remove(index).expect("existing uncertain row");
        self.reserved_bytes -= entry.reserved_bytes;
        self.changed();
        Ok(())
    }
    pub fn clear_finished(&mut self) -> usize {
        let before = self.len();
        self.entries.retain(|entry| !entry.phase.is_terminal());
        self.reserved_bytes = self.fixed_bytes
            + self
                .entries
                .iter()
                .map(|entry| entry.reserved_bytes)
                .sum::<usize>();
        let removed = before - self.len();
        if removed > 0 {
            self.changed();
        }
        removed
    }
}
fn valid_identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn ensure_mutable(entry: &OperationEntry) -> Result<(), HistoryError> {
    if entry.phase == OperationPhase::NeedsReview {
        Err(HistoryError::AmbiguousOutcome)
    } else if entry.phase.is_terminal() {
        Err(HistoryError::WrongPhase)
    } else {
        Ok(())
    }
}
fn check_proof(
    entry: &OperationEntry,
    owner: &str,
    revision: Option<u64>,
) -> Result<(), HistoryError> {
    if entry
        .correlation
        .as_ref()
        .is_some_and(|proof| proof.owner_instance == owner && proof.revision == revision)
    {
        Ok(())
    } else {
        Err(HistoryError::EvidenceMismatch)
    }
}
fn bounded_text(text: &str, max: usize, redact: bool) -> (String, bool) {
    let cleaned = if redact {
        crate::runtime_status::display_error(text)
    } else {
        text.chars()
            .filter(|value| !value.is_control())
            .take(8192)
            .collect()
    };
    if cleaned.len() <= max {
        return (cleaned.into_boxed_str().into_string(), false);
    }
    let mut boundary = max.saturating_sub(3);
    while !cleaned.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut result = cleaned[..boundary].to_owned();
    result.push_str("...");
    result.shrink_to_fit();
    (result, true)
}
fn set_message(entry: &mut OperationEntry, message: &str, redact: bool) {
    let (mut value, truncated) = bounded_text(message, MAX_MESSAGE_BYTES, redact);
    if value.is_empty()
        && matches!(
            entry.phase,
            OperationPhase::Failed | OperationPhase::NeedsReview
        )
    {
        value = "No confirmed outcome was supplied; inspect current state.".into();
    }
    entry.message = value;
    entry.display_truncated |= truncated;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryIntent {
    FocusTorrent(String),
    CopyError(String),
    Dismiss(OperationId),
    AcknowledgeReview(OperationId),
    DismissFinished,
}

#[cfg(feature = "gui")]
pub mod gui {
    use super::*;
    use eframe::egui::{self, RichText};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Control {
        Clear,
        Focus(OperationId),
        Copy(OperationId),
        Dismiss(OperationId),
        AcknowledgeReview(OperationId),
    }
    pub fn panel(
        ui: &mut egui::Ui,
        history: &OperationHistory,
        max_height: f32,
    ) -> Vec<HistoryIntent> {
        panel_inner(ui, history, max_height, |_, _| {})
    }
    fn panel_inner(
        ui: &mut egui::Ui,
        history: &OperationHistory,
        max_height: f32,
        mut trace: impl FnMut(Control, egui::Rect),
    ) -> Vec<HistoryIntent> {
        let mut intents = Vec::new();
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("Recent operations ({})", history.len()));
            ui.label(format!("{} unconfirmed", history.pending_count()));
            let clear = ui.add_enabled(
                history.entries().any(|entry| entry.phase.is_terminal()),
                egui::Button::new("Dismiss finished"),
            );
            trace(Control::Clear, clear.rect);
            if clear.clicked() {
                intents.push(HistoryIntent::DismissFinished);
            }
        });
        if history.evicted_count() > 0 {
            ui.small(format!(
                "{} older finished records removed by history limits",
                history.evicted_count()
            ));
        }
        if history.rejected_count() > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} requests could not enter the bounded history",
                    history.rejected_count()
                ),
            );
        }
        if history.is_empty() {
            ui.label("No operations requested in this window.");
            return intents;
        }
        let height = max_height.max(0.0).min(ui.available_height());
        egui::ScrollArea::vertical()
            .id_salt("recent-operations")
            .max_height(height)
            .show(ui, |ui| {
                for entry in history.entries().rev() {
                    ui.push_id(entry.id.get(), |ui| {
                        let colors = crate::appearance::tone_colors(
                            entry.phase.tone(),
                            ui.visuals().dark_mode,
                            false,
                            false,
                        );
                        egui::Frame::new()
                            .fill(colors.background.color32())
                            .stroke(egui::Stroke::new(1.0, colors.accent.color32()))
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "#{} · {} · {}",
                                            entry.id.get(),
                                            entry.kind.label(),
                                            entry.target_name
                                        ))
                                        .strong()
                                        .color(colors.text.color32()),
                                    )
                                    .wrap(),
                                );
                                ui.horizontal_wrapped(|ui| {
                                    if matches!(
                                        entry.phase,
                                        OperationPhase::Queued
                                            | OperationPhase::Accepted
                                            | OperationPhase::Running
                                            | OperationPhase::Persisting
                                    ) {
                                        ui.spinner();
                                    }
                                    ui.label(
                                        RichText::new(entry.phase.label())
                                            .color(colors.text.color32()),
                                    );
                                    ui.small(format!(
                                        "{} s ago",
                                        entry.started_at.elapsed().as_secs()
                                    ));
                                });
                                if let Some(progress) = entry.progress {
                                    if let Some(fraction) = progress.fraction() {
                                        ui.add(egui::ProgressBar::new(fraction).show_percentage());
                                    } else {
                                        ui.small(format!(
                                            "{} reported; total not established",
                                            progress.done
                                        ));
                                    }
                                }
                                if !entry.message.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&entry.message)
                                                .color(colors.text.color32()),
                                        )
                                        .wrap(),
                                    );
                                }
                                if entry.display_truncated {
                                    ui.small("Display text shortened to keep history bounded.");
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if let Some(target) = &entry.target_id {
                                        let response = ui.button("Focus torrent");
                                        trace(Control::Focus(entry.id), response.rect);
                                        if response.clicked() {
                                            intents
                                                .push(HistoryIntent::FocusTorrent(target.clone()));
                                        }
                                    }
                                    if let Some(error) = entry.error() {
                                        let response = ui.button("Copy error");
                                        trace(Control::Copy(entry.id), response.rect);
                                        if response.clicked() {
                                            intents
                                                .push(HistoryIntent::CopyError(error.to_owned()));
                                        }
                                    }
                                    if entry.phase == OperationPhase::NeedsReview {
                                        let response = ui.button("Dismiss unconfirmed").on_hover_text("Remove only this display record after inspecting current state. This does not confirm success, cancel work, or retry the request.");
                                        trace(Control::AcknowledgeReview(entry.id), response.rect);
                                        if response.clicked() {
                                            intents.push(HistoryIntent::AcknowledgeReview(entry.id));
                                        }
                                    }
                                    let response = ui.add_enabled(
                                        entry.phase.is_terminal(),
                                        egui::Button::new("Dismiss"),
                                    );
                                    trace(Control::Dismiss(entry.id), response.rect);
                                    if response.clicked() {
                                        intents.push(HistoryIntent::Dismiss(entry.id));
                                    }
                                });
                            });
                        ui.add_space(ui.spacing().item_spacing.y);
                    });
                }
            });
        intents
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
mod tests;
