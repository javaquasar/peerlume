use super::*;
fn begin(history: &mut OperationHistory) -> OperationId {
    history
        .begin(
            OperationKind::Recheck,
            Some("hash-a"),
            "Synthetic",
            CompletionRule::EngineAndDurable,
        )
        .unwrap()
}
#[test]
fn owner_acceptance_and_recheck_start_do_not_complete_or_cache_early_durability() {
    let mut history = OperationHistory::default();
    let id = begin(&mut history);
    assert_eq!(
        history.durable_completed(id, "owner", Some(7)),
        Err(HistoryError::EvidenceMismatch)
    );
    history.accepted(id, "owner", Some(7)).unwrap();
    history.accepted(id, "owner", Some(7)).unwrap();
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Accepted);
    assert_eq!(
        history.durable_completed(id, "owner", Some(7)),
        Err(HistoryError::WrongPhase)
    );
    history.running(id, "Checking started").unwrap();
    history.progress(id, 1, Some(10), "Checking").unwrap();
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Running);
    history.effect_completed(id, "owner", Some(7)).unwrap();
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Persisting);
    history.durable_completed(id, "owner", Some(7)).unwrap();
    history.durable_completed(id, "owner", Some(7)).unwrap();
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Completed);
}
#[test]
fn stale_superseding_owner_and_missing_revision_proofs_cannot_turn_green() {
    let mut history = OperationHistory::default();
    let id = begin(&mut history);
    assert_eq!(
        history.accepted(id, "owner", None),
        Err(HistoryError::EvidenceMismatch)
    );
    history.accepted(id, "owner", Some(7)).unwrap();
    for (owner, revision) in [
        ("old", Some(7)),
        ("owner", Some(6)),
        ("owner", Some(8)),
        ("owner", None),
    ] {
        assert_eq!(
            history.effect_completed(id, owner, revision),
            Err(HistoryError::EvidenceMismatch)
        );
        assert_eq!(
            history.durable_completed(id, owner, revision),
            Err(HistoryError::EvidenceMismatch)
        );
    }
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Accepted);
    history.owner_disconnected("owner");
    assert_eq!(
        history.durable_completed(id, "owner", Some(7)),
        Err(HistoryError::AmbiguousOutcome)
    );
    assert_eq!(
        history.effect_completed(id, "owner", Some(7)),
        Err(HistoryError::AmbiguousOutcome)
    );
    history.owner_changed("replacement");
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::NeedsReview);
}
#[test]
fn repeated_operations_keep_independent_ids_and_errors_survive_later_navigation() {
    let mut history = OperationHistory::default();
    let first = begin(&mut history);
    let second = begin(&mut history);
    assert_ne!(first, second);
    history.fail(first, "first failure").unwrap();
    history.fail(first, "replacement failure").unwrap();
    history.fail(second, "second failure").unwrap();
    let reveal = history
        .begin(
            OperationKind::Reveal,
            Some("hash-a"),
            "Synthetic",
            CompletionRule::LocalReturn,
        )
        .unwrap();
    history
        .local_completed(reveal, "Location request accepted")
        .unwrap();
    assert_eq!(history.get(first).unwrap().message, "first failure");
    assert_eq!(history.get(second).unwrap().message, "second failure");
    assert_eq!(history.failed_count(), 2);
    assert_eq!(history.len(), 3);
}
#[test]
fn progress_is_bounded_monotonic_and_unknown_totals_do_not_fabricate_percentages() {
    let mut history = OperationHistory::default();
    let id = begin(&mut history);
    history.accepted(id, "owner", Some(1)).unwrap();
    history.progress(id, 10, None, "Beginning").unwrap();
    assert_eq!(history.get(id).unwrap().progress.unwrap().fraction(), None);
    assert_eq!(
        history.progress(id, 10, Some(5), "invalid total"),
        Err(HistoryError::ProgressTotalChanged)
    );
    history.progress(id, 20, Some(100), "Checking").unwrap();
    history.progress(id, 5, Some(100), "Late sample").unwrap();
    assert_eq!(history.get(id).unwrap().progress.unwrap().done, 20);
    assert_eq!(
        history.progress(id, 30, Some(200), "Changed total"),
        Err(HistoryError::ProgressTotalChanged)
    );
    history
        .progress(id, u64::MAX, None, "Reported complete")
        .unwrap();
    assert_eq!(
        history.get(id).unwrap().progress.unwrap().fraction(),
        Some(1.0)
    );
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Running);
}
#[test]
fn completion_rules_and_cancellation_require_their_actual_boundary() {
    let mut history = OperationHistory::default();
    let engine = begin(&mut history);
    assert_eq!(
        history.local_completed(engine, "done"),
        Err(HistoryError::WrongCompletionRule)
    );
    assert_eq!(
        history.owner_completed(engine, "owner", "done"),
        Err(HistoryError::WrongCompletionRule)
    );
    history.cancel_before_dispatch(engine).unwrap();
    let queued = begin(&mut history);
    history.dispatched(queued).unwrap();
    assert_eq!(
        history.cancel_before_dispatch(queued),
        Err(HistoryError::AlreadyDispatched)
    );
    let saved = history
        .begin(
            OperationKind::ViewSettings,
            None,
            "View",
            CompletionRule::OwnerDurableReceipt,
        )
        .unwrap();
    assert_eq!(
        history.owner_completed(saved, "owner", "Saved"),
        Err(HistoryError::EvidenceMismatch)
    );
    history.accepted(saved, "owner", None).unwrap();
    history.owner_completed(saved, "owner", "Saved").unwrap();
    assert_eq!(history.get(saved).unwrap().phase, OperationPhase::Completed);
}
#[test]
fn row_and_byte_limits_evict_finished_only_and_reject_unconfirmed_overflow() {
    let mut history = OperationHistory::with_limits(2, 8192).unwrap();
    let first = begin(&mut history);
    let second = begin(&mut history);
    assert_eq!(
        history.begin(
            OperationKind::Pause,
            Some("hash"),
            "another",
            CompletionRule::EngineAndDurable
        ),
        Err(HistoryError::HistoryFull)
    );
    assert_eq!(history.rejected_count(), 1);
    assert!(history.get(first).is_some() && history.get(second).is_some());
    history.fail(first, "known failure").unwrap();
    let third = begin(&mut history);
    assert!(third.get() > second.get());
    assert!(history.get(first).is_none());
    assert_eq!(history.evicted_count(), 1);
    assert_eq!(history.dismiss(second), Err(HistoryError::WrongPhase));
    assert_eq!(history.clear_finished(), 0);
    assert!(
        history.retained_bytes() <= history.reserved_bytes() && history.reserved_bytes() <= 8192
    );
    for limits in [
        (0, 8192),
        (MAX_HISTORY_ROWS + 1, MAX_HISTORY_BYTES),
        (2, 0),
        (1, MAX_HISTORY_BYTES + 1),
    ] {
        assert!(OperationHistory::with_limits(limits.0, limits.1).is_err());
    }
}
#[test]
fn unicode_text_is_bounded_and_redacted_without_truncating_identity() {
    let mut history = OperationHistory::default();
    let name = "界".repeat(1000);
    let id = history
        .begin(
            OperationKind::Pause,
            Some("canonical-hash"),
            &name,
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    history
        .fail(
            id,
            &format!("{} https://tracker.test/?secret=private", "é".repeat(4000)),
        )
        .unwrap();
    let entry = history.get(id).unwrap();
    assert_eq!(entry.target_id.as_deref(), Some("canonical-hash"));
    assert!(
        entry.target_name.len() <= MAX_NAME_BYTES
            && entry.message.len() <= MAX_MESSAGE_BYTES
            && entry.display_truncated
    );
    assert!(!entry.message.contains("private"));
    assert!(history.retained_bytes() <= history.reserved_bytes());
    assert_eq!(
        history.begin(
            OperationKind::Pause,
            Some(&"x".repeat(MAX_TARGET_BYTES + 1)),
            "bad",
            CompletionRule::EngineAndDurable
        ),
        Err(HistoryError::InvalidIdentity)
    );
}
#[test]
fn missing_ids_id_exhaustion_and_finished_dismissal_are_explicit() {
    let mut history = OperationHistory::default();
    assert!(history.is_empty());
    assert_eq!(
        history.dismiss(OperationId(1)),
        Err(HistoryError::UnknownOperation)
    );
    let id = begin(&mut history);
    history.fail(id, "failure").unwrap();
    history.dismiss(id).unwrap();
    assert!(history.is_empty());
    history.next_id = u64::MAX;
    assert_eq!(
        history.begin(
            OperationKind::Other,
            None,
            "new",
            CompletionRule::LocalReturn
        ),
        Err(HistoryError::IdExhausted)
    );
}
#[test]
fn byte_admission_reserves_future_errors_and_owner_proofs_before_dispatch() {
    let fixed = OperationHistory::with_limits(4, MAX_HISTORY_BYTES)
        .unwrap()
        .reserved_bytes();
    let budget = fixed + MAX_MESSAGE_BYTES + MAX_OWNER_BYTES + 16;
    let mut history = OperationHistory::with_limits(4, budget).unwrap();
    let id = history
        .begin(
            OperationKind::Pause,
            Some("hash"),
            "short",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        history.begin(
            OperationKind::Resume,
            Some("hash"),
            "later",
            CompletionRule::EngineAndDurable
        ),
        Err(HistoryError::HistoryFull)
    );
    assert_eq!(history.len(), 1); // The four-row cap was not the limiting budget.
    let reserved = history.reserved_bytes();
    history
        .accepted(id, &"o".repeat(MAX_OWNER_BYTES), Some(1))
        .unwrap();
    history.fail(id, &"é".repeat(MAX_MESSAGE_BYTES)).unwrap();
    assert_eq!(history.reserved_bytes(), reserved);
    assert!(history.retained_bytes() <= reserved && reserved <= budget);
    let next = history
        .begin(
            OperationKind::Resume,
            Some("hash"),
            "later",
            CompletionRule::EngineAndDurable,
        )
        .unwrap();
    assert!(next.get() > id.get());
    assert!(history.get(id).is_none());
    assert_eq!(history.evicted_count(), 1);
}
#[test]
fn clear_finished_keeps_ambiguous_work_and_ids_never_recur() {
    let mut history = OperationHistory::default();
    let failed = begin(&mut history);
    history.fail(failed, "").unwrap();
    assert!(!history.get(failed).unwrap().error().unwrap().is_empty());
    let cancelled = begin(&mut history);
    history.cancel_before_dispatch(cancelled).unwrap();
    let ambiguous = begin(&mut history);
    history.needs_review(ambiguous, "").unwrap();
    assert!(!history.get(ambiguous).unwrap().error().unwrap().is_empty());
    let revision = history.revision();
    history
        .needs_review(ambiguous, "do not overwrite original outcome")
        .unwrap();
    assert_eq!(history.revision(), revision);
    assert_eq!(history.clear_finished(), 2);
    assert!(history.get(ambiguous).is_some());
    assert_eq!(history.pending_count(), 1);
    assert_eq!(history.failed_count(), 0);
    assert_eq!(history.dismiss(ambiguous), Err(HistoryError::WrongPhase));
    let next = begin(&mut history);
    assert!(next.get() > ambiguous.get());
    assert_eq!(history.len(), 2);
}

#[test]
fn only_explicit_review_acknowledgement_forgets_uncertainty_without_completing_it() {
    let mut history = OperationHistory::default();
    let active = begin(&mut history);
    assert_eq!(
        history.acknowledge_review(active),
        Err(HistoryError::WrongPhase)
    );
    history
        .needs_review(active, "No correlated result")
        .unwrap();
    assert_eq!(history.clear_finished(), 0);
    assert_eq!(history.dismiss(active), Err(HistoryError::WrongPhase));
    assert_eq!(
        history.get(active).unwrap().phase,
        OperationPhase::NeedsReview
    );
    history.acknowledge_review(active).unwrap();
    assert!(history.get(active).is_none());
    assert_eq!(
        history.acknowledge_review(active),
        Err(HistoryError::UnknownOperation)
    );
    let finished = begin(&mut history);
    history.fail(finished, "Known failure").unwrap();
    assert_eq!(
        history.acknowledge_review(finished),
        Err(HistoryError::WrongPhase)
    );
    assert_eq!(history.get(finished).unwrap().phase, OperationPhase::Failed);
}
#[test]
fn local_and_owner_receipts_do_not_cross_completion_rules_or_overwrite_final_outcomes() {
    let mut history = OperationHistory::default();
    let local = history
        .begin(
            OperationKind::Export,
            None,
            "Export",
            CompletionRule::LocalReturn,
        )
        .unwrap();
    assert_eq!(
        history.accepted(local, "owner", None),
        Err(HistoryError::WrongCompletionRule)
    );
    history.running(local, "Writing export").unwrap();
    history
        .progress(local, 2, Some(3), "Writing export")
        .unwrap();
    history
        .local_completed(local, "Export returned successfully")
        .unwrap();
    let revision = history.revision();
    history.local_completed(local, "late replacement").unwrap();
    assert_eq!(history.revision(), revision);
    assert_eq!(
        history.get(local).unwrap().phase,
        OperationPhase::LocalCompleted
    );
    assert_eq!(
        history.fail(local, "late error"),
        Err(HistoryError::WrongPhase)
    );
    assert_eq!(
        history.running(local, "late progress"),
        Err(HistoryError::WrongPhase)
    );
    let saved = history
        .begin(
            OperationKind::Preferences,
            None,
            "View",
            CompletionRule::OwnerDurableReceipt,
        )
        .unwrap();
    history.accepted(saved, "owner", None).unwrap();
    history.running(saved, "Owner save").unwrap();
    assert_eq!(
        history.effect_completed(saved, "owner", None),
        Err(HistoryError::WrongCompletionRule)
    );
    assert_eq!(
        history.durable_completed(saved, "owner", None),
        Err(HistoryError::WrongCompletionRule)
    );
    assert_eq!(
        history.owner_completed(saved, "replacement", "Saved"),
        Err(HistoryError::EvidenceMismatch)
    );
    history.owner_completed(saved, "owner", "Saved").unwrap();
    history
        .owner_completed(saved, "owner", "late duplicate")
        .unwrap();
    assert_eq!(history.get(saved).unwrap().message, "Saved");
    history.accepted(saved, "owner", None).unwrap();
    assert_eq!(history.get(saved).unwrap().phase, OperationPhase::Completed);
}
#[test]
fn invalid_and_out_of_order_identity_inputs_leave_existing_operation_unchanged() {
    let mut history = OperationHistory::default();
    let id = begin(&mut history);
    for owner in ["", "bad\nowner", &"x".repeat(MAX_OWNER_BYTES + 1)] {
        assert_eq!(
            history.accepted(id, owner, Some(1)),
            Err(HistoryError::InvalidIdentity)
        );
    }
    assert_eq!(
        history.running(id, "out of order"),
        Err(HistoryError::WrongPhase)
    );
    history.dispatched(id).unwrap();
    let revision = history.revision();
    history.dispatched(id).unwrap();
    assert_eq!(history.revision(), revision);
    history.accepted(id, "owner", Some(1)).unwrap();
    assert_eq!(
        history.accepted(id, "other", Some(1)),
        Err(HistoryError::EvidenceMismatch)
    );
    assert_eq!(history.dispatched(id), Err(HistoryError::WrongPhase));
    history.effect_completed(id, "owner", Some(1)).unwrap();
    assert_eq!(
        history.progress(id, 1, Some(1), "early finished"),
        Err(HistoryError::WrongPhase)
    );
    history.durable_completed(id, "owner", Some(1)).unwrap();
    history.effect_completed(id, "owner", Some(1)).unwrap();
    history.needs_review(id, "late lost connection").unwrap();
    assert_eq!(history.get(id).unwrap().phase, OperationPhase::Completed);
    assert_eq!(history.failed_count(), 0);
    assert_eq!(
        history.running(OperationId(1000), "missing"),
        Err(HistoryError::UnknownOperation)
    );
}
