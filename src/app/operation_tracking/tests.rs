use super::*;
use crate::control::AcceptedCommand;

fn torrent() -> TorrentItem {
    let mut item = crate::client::tests::torrent();
    item.info_hashes = vec!["hash-a".into()];
    item.name = "Synthetic operation target".into();
    item
}
fn queued(job: u64, command: TorrentCommand) -> Receipt {
    Receipt {
        job_id: job,
        owner_instance: "owner-a".into(),
        queued: true,
        operation: "Synthetic command",
        command: Some(command),
        id: "hash-a".into(),
        ..Default::default()
    }
}
fn accepted(job: u64, command: TorrentCommand) -> Receipt {
    Receipt {
        queued: false,
        snapshot_after: 2,
        accepted: Some(AcceptedCommand {
            torrent_id: "hash-a".into(),
            command_revision: 7,
        }),
        ..queued(job, command)
    }
}
fn connected(serial: u64) -> ConnectionView {
    ConnectionView {
        status: ConnectionStatus::Connected,
        owner_instance: Some("owner-a".into()),
        snapshot_serial: serial,
    }
}
fn phase(tracker: &Tracker, id: OperationId) -> OperationPhase {
    tracker.history.get(id).unwrap().phase
}

#[test]
fn only_matching_owner_receipt_hands_off_local_placeholder() {
    let mut tracker = Tracker::default();
    let items = [torrent()];
    let local = tracker
        .record_error("Show location", None, "fixture", "Unavailable")
        .unwrap();
    let placeholder = tracker
        .receipt(&queued(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    let mut receipt = accepted(1, TorrentCommand::Pause);
    receipt.operation_id = Some("owner-operation-1".into());
    receipt.owner_instance = "different-owner".into();
    tracker.receipt(&receipt, &items).unwrap();
    assert_eq!(phase(&tracker, placeholder), OperationPhase::NeedsReview);

    receipt.owner_instance = "owner-a".into();
    assert!(tracker.receipt(&receipt, &items).unwrap().is_none());
    assert!(tracker.history.get(placeholder).is_none());
    assert_eq!(phase(&tracker, local), OperationPhase::Failed);
    assert!(
        tracker
            .receipt(&queued(1, TorrentCommand::Pause), &items)
            .unwrap()
            .is_none()
    );
}

#[test]
fn pause_receipts_need_new_snapshot_matching_engine_action_and_saved_revision() {
    let mut tracker = Tracker::default();
    let mut items = vec![torrent()];
    let id = tracker
        .receipt(&queued(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    tracker
        .receipt(&accepted(1, TorrentCommand::Pause), &items)
        .unwrap();
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Pause,
    };
    tracker.reconcile(&connected(1), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Resume,
    };
    tracker.reconcile(&connected(2), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Pause,
    };
    tracker.reconcile(&connected(3), &items, |_| {
        Some(CommandEvidence {
            revision: 6,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    tracker.reconcile(&connected(4), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: false,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Persisting);
    tracker.reconcile(&connected(4), &items, |_| {
        panic!("unchanged snapshot must not be re-read as new durability")
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Persisting);
    tracker.reconcile(&connected(5), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Completed);
    tracker
        .receipt(&accepted(1, TorrentCommand::Pause), &items)
        .unwrap();
    assert_eq!(tracker.history.len(), 1);
}

#[test]
fn recheck_start_and_full_progress_are_not_integrity_success() {
    let mut tracker = Tracker::default();
    let mut items = vec![torrent()];
    let id = tracker
        .receipt(&accepted(1, TorrentCommand::Recheck), &items)
        .unwrap()
        .unwrap();
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Recheck,
    };
    items[0].completion = CompletionStatus::Checking {
        checked_bytes: 40,
        total_bytes: 100,
    };
    tracker.reconcile(&connected(2), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Running);
    assert_eq!(tracker.history.get(id).unwrap().progress.unwrap().done, 40);
    items[0].completion = CompletionStatus::Checking {
        checked_bytes: 100,
        total_bytes: 100,
    };
    tracker.reconcile(&connected(3), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Running);
    items[0].completion = CompletionStatus::Verified;
    tracker.reconcile(&connected(4), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: false,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Persisting);
    tracker.reconcile(&connected(5), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Completed);
}

#[test]
fn known_rejections_transport_ambiguity_and_separate_errors_retain_their_causes() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    let rejected = Receipt {
        queued: false,
        error: Some("Queue is full".into()),
        ..queued(1, TorrentCommand::Pause)
    };
    let failed = tracker.receipt(&rejected, &items).unwrap().unwrap();
    let uncertain = Receipt {
        queued: false,
        error: Some("Response timed out".into()),
        ambiguous_error: true,
        ..queued(2, TorrentCommand::Pause)
    };
    let unknown = tracker.receipt(&uncertain, &items).unwrap().unwrap();
    let local = tracker
        .record_error("Import", None, "Synthetic metadata", "Invalid torrent")
        .unwrap();
    assert_eq!(phase(&tracker, failed), OperationPhase::Failed);
    assert_eq!(phase(&tracker, unknown), OperationPhase::NeedsReview);
    assert_eq!(phase(&tracker, local), OperationPhase::Failed);
    assert!(
        tracker
            .history
            .get(failed)
            .unwrap()
            .message
            .contains("Queue is full")
    );
    tracker
        .receipt(&accepted(2, TorrentCommand::Pause), &items)
        .unwrap();
    assert_eq!(phase(&tracker, unknown), OperationPhase::NeedsReview);
    assert_eq!(tracker.history.failed_count(), 2);
}

#[test]
fn superseding_revisions_owner_changes_and_snapshot_regressions_cannot_turn_green() {
    let items = vec![torrent()];
    for mode in 0..3 {
        let mut tracker = Tracker::default();
        let id = tracker
            .receipt(&accepted(1, TorrentCommand::Pause), &items)
            .unwrap()
            .unwrap();
        tracker.reconcile(&connected(2), &items, |_| {
            Some(CommandEvidence {
                revision: 7,
                durable: false,
            })
        });
        let mut connection = connected(3);
        if mode == 0 {
            connection.owner_instance = Some("owner-b".into());
        }
        if mode == 1 {
            connection.snapshot_serial = 1;
        }
        let revision = if mode == 2 { 8 } else { 7 };
        tracker.reconcile(&connection, &items, |_| {
            Some(CommandEvidence {
                revision,
                durable: true,
            })
        });
        assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
        tracker.reconcile(&connected(4), &items, |_| {
            Some(CommandEvidence {
                revision: 7,
                durable: true,
            })
        });
        assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    }
}

#[test]
fn removal_requires_acknowledged_command_then_post_reply_authoritative_absence() {
    let mut tracker = Tracker::default();
    let id = tracker
        .receipt(&queued(1, TorrentCommand::Remove), &[torrent()])
        .unwrap()
        .unwrap();
    tracker.reconcile(&connected(0), &[], |_| None);
    assert_eq!(phase(&tracker, id), OperationPhase::Queued);
    tracker
        .receipt(&accepted(1, TorrentCommand::Remove), &[torrent()])
        .unwrap();
    tracker.reconcile(&connected(1), &[], |_| None);
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    tracker.reconcile(&connected(2), &[torrent()], |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    tracker.reconcile(&connected(3), &[], |_| None);
    assert_eq!(phase(&tracker, id), OperationPhase::Completed);
}

#[test]
fn missing_revisions_or_final_snapshots_and_async_storage_do_not_fabricate_success() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    let missing_serial = Receipt {
        snapshot_after: 0,
        ..accepted(1, TorrentCommand::Remove)
    };
    let first = tracker.receipt(&missing_serial, &items).unwrap().unwrap();
    tracker.reconcile(&connected(100), &[], |_| None);
    assert_eq!(phase(&tracker, first), OperationPhase::Accepted);
    let asynchronous = Receipt {
        queued: false,
        operation: "Move data directory",
        command: None,
        ..queued(2, TorrentCommand::Pause)
    };
    let second = tracker.receipt(&asynchronous, &items).unwrap().unwrap();
    assert_eq!(phase(&tracker, second), OperationPhase::NeedsReview);
    let wrong_target = Receipt {
        accepted: Some(AcceptedCommand {
            torrent_id: "another".into(),
            command_revision: 7,
        }),
        ..accepted(3, TorrentCommand::Resume)
    };
    let third = tracker.receipt(&wrong_target, &items).unwrap().unwrap();
    assert_eq!(phase(&tracker, third), OperationPhase::NeedsReview);
}

#[test]
fn ui_save_completes_only_on_the_correlated_owner_final_reply_and_duplicates_do_not_reappear() {
    let mut tracker = Tracker::default();
    let queue = Receipt {
        job_id: 1,
        owner_instance: "owner-a".into(),
        operation: "Save view",
        completed_by_reply: true,
        queued: true,
        ..Default::default()
    };
    let id = tracker.receipt(&queue, &[]).unwrap().unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::Queued);
    let reply = Receipt {
        queued: false,
        message: "View saved".into(),
        ..queue
    };
    tracker.receipt(&reply, &[]).unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::Completed);
    tracker.history.dismiss(id).unwrap();
    assert_eq!(tracker.receipt(&reply, &[]).unwrap(), None);
    assert!(tracker.history.is_empty());
    assert!(tracker.receipt(&Receipt::default(), &[]).unwrap().is_none());
}

#[test]
fn failed_engine_confirmation_and_failed_integrity_check_never_complete() {
    let mut tracker = Tracker::default();
    let mut items = vec![torrent()];
    let pause = tracker
        .receipt(&accepted(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    items[0].runtime.command = TransferFeedback::Failed {
        action: TransferAction::Pause,
        message: "Engine rejected pause".into(),
    };
    tracker.reconcile(&connected(2), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, pause), OperationPhase::Failed);
    let repair = tracker
        .receipt(&accepted(2, TorrentCommand::RecheckAndRepair), &items)
        .unwrap()
        .unwrap();
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Recheck,
    };
    items[0].completion = CompletionStatus::Failed {
        message: "Bad piece remains".into(),
    };
    tracker.reconcile(&connected(3), &items, |_| {
        Some(CommandEvidence {
            revision: 7,
            durable: true,
        })
    });
    assert_eq!(phase(&tracker, repair), OperationPhase::Failed);
    assert!(
        tracker
            .history
            .get(repair)
            .unwrap()
            .message
            .contains("Bad piece")
    );
}

#[test]
fn bookkeeping_is_bounded_and_oversized_owner_identity_is_rejected_before_retention() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    for index in 1..=1000 {
        let receipt = Receipt {
            queued: false,
            error: Some("Known error".into()),
            ..queued(index, TorrentCommand::Pause)
        };
        tracker.receipt(&receipt, &items).unwrap();
        assert!(tracker.jobs.len() <= MAX_HISTORY_ROWS);
        assert!(tracker.history.reserved_bytes() <= crate::ui_operations::MAX_HISTORY_BYTES);
    }
    let invalid = Receipt {
        owner_instance: "x".repeat(MAX_OWNER_BYTES + 1),
        ..queued(1001, TorrentCommand::Pause)
    };
    assert_eq!(
        tracker.receipt(&invalid, &items),
        Err(HistoryError::InvalidIdentity)
    );
    assert!(tracker.jobs.len() <= MAX_HISTORY_ROWS);
    assert!(tracker.retired.len() <= MAX_HISTORY_ROWS);
    let expired = tracker
        .receipt(&accepted(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    assert_eq!(phase(&tracker, expired), OperationPhase::NeedsReview);
}

#[test]
fn same_job_from_another_owner_is_an_ambiguous_separate_cause_not_a_success() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    let id = tracker
        .receipt(&queued(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    let wrong_owner = Receipt {
        owner_instance: "owner-b".into(),
        ..accepted(1, TorrentCommand::Pause)
    };
    tracker.receipt(&wrong_owner, &items).unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    assert!(
        tracker
            .history
            .get(id)
            .unwrap()
            .message
            .contains("different background owner")
    );
    assert_eq!(tracker.history.len(), 1);
    for owner in ["", "owner\ninvalid"] {
        let invalid = Receipt {
            owner_instance: owner.into(),
            ..queued(2, TorrentCommand::Pause)
        };
        assert_eq!(
            tracker.receipt(&invalid, &items),
            Err(HistoryError::InvalidIdentity)
        );
    }
    assert_eq!(tracker.jobs.len(), 1);
}

#[test]
fn an_unseen_lower_job_is_not_silently_dropped_by_a_higher_retired_job() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    let failure = Receipt {
        queued: false,
        error: Some("Known rejection".into()),
        ..queued(10, TorrentCommand::Pause)
    };
    let old = tracker.receipt(&failure, &items).unwrap().unwrap();
    tracker.history.dismiss(old).unwrap();
    tracker
        .receipt(&queued(12, TorrentCommand::Pause), &items)
        .unwrap();
    assert!(tracker.retired.contains(&10));
    let reordered = tracker
        .receipt(&queued(2, TorrentCommand::Resume), &items)
        .unwrap()
        .unwrap();
    assert_eq!(phase(&tracker, reordered), OperationPhase::Queued);
    tracker
        .receipt(&accepted(2, TorrentCommand::Resume), &items)
        .unwrap();
    assert_eq!(phase(&tracker, reordered), OperationPhase::Accepted);
    assert!(tracker.receipt(&failure, &items).unwrap().is_none());
}

#[test]
fn quiet_agent_polls_do_not_fill_history_and_unprovable_setting_changes_remain_reviewable() {
    let mut tracker = Tracker::default();
    let quiet = Receipt {
        job_id: 1,
        operation: "Agent view",
        label: "Agent view",
        owner_instance: "owner-a".into(),
        ..Default::default()
    };
    for _ in 0..1000 {
        assert!(tracker.receipt(&quiet, &[]).unwrap().is_none());
    }
    assert!(tracker.history.is_empty() && tracker.jobs.is_empty());
    let command = TorrentCommand::SetVerifyAfterDownload(true);
    let id = tracker
        .receipt(&accepted(2, command), &[torrent()])
        .unwrap()
        .unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    tracker.history.acknowledge_review(id).unwrap();
    assert!(tracker.history.is_empty());
}

#[test]
fn fresh_snapshot_missing_target_or_revision_becomes_explicit_review_without_green() {
    for missing_target in [false, true] {
        let mut tracker = Tracker::default();
        let items = vec![torrent()];
        let id = tracker
            .receipt(&accepted(1, TorrentCommand::Pause), &items)
            .unwrap()
            .unwrap();
        let current = if missing_target { &[][..] } else { &items[..] };
        tracker.reconcile(&connected(1), current, |_| None);
        assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
        tracker.reconcile(&connected(2), current, |_| None);
        assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
        assert!(
            tracker
                .history
                .get(id)
                .unwrap()
                .message
                .contains(if missing_target {
                    "absent"
                } else {
                    "no command revision"
                })
        );
        tracker.history.acknowledge_review(id).unwrap();
        assert!(tracker.history.get(id).is_none());
    }
}

fn saved_revision_seven(_: &str) -> Option<CommandEvidence> {
    Some(CommandEvidence {
        revision: 7,
        durable: true,
    })
}

#[test]
fn disconnect_and_stop_states_require_review_even_if_a_later_snapshot_looks_complete() {
    for status in [
        ConnectionStatus::Connecting,
        ConnectionStatus::Reconnecting("Connection lost".into()),
        ConnectionStatus::StopRequested,
        ConnectionStatus::Stopped,
    ] {
        let mut tracker = Tracker::default();
        let mut items = vec![torrent()];
        items[0].runtime.command = TransferFeedback::Confirmed {
            action: TransferAction::Pause,
        };
        let id = tracker
            .receipt(&accepted(1, TorrentCommand::Pause), &items)
            .unwrap()
            .unwrap();
        tracker.reconcile(&connected(1), &items, saved_revision_seven);
        let lost = ConnectionView {
            status,
            ..connected(2)
        };
        tracker.reconcile(&lost, &items, saved_revision_seven);
        assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
        tracker.reconcile(&connected(3), &items, saved_revision_seven);
        assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    }
}

#[test]
fn first_owner_snapshot_cannot_confirm_a_job_accepted_by_another_owner() {
    let mut tracker = Tracker::default();
    let mut items = vec![torrent()];
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Pause,
    };
    let id = tracker
        .receipt(&accepted(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    assert!(tracker.owner.is_none());
    let wrong_owner = ConnectionView {
        owner_instance: Some("owner-b".into()),
        ..connected(2)
    };
    tracker.reconcile(&wrong_owner, &items, saved_revision_seven);
    assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    assert!(
        tracker
            .history
            .get(id)
            .unwrap()
            .message
            .contains("owner changed")
    );
}

#[test]
fn a_second_acceptance_cannot_replace_the_original_job_revision() {
    let mut tracker = Tracker::default();
    let items = vec![torrent()];
    let id = tracker
        .receipt(&accepted(1, TorrentCommand::Resume), &items)
        .unwrap()
        .unwrap();
    let mut superseding_reply = accepted(1, TorrentCommand::Resume);
    superseding_reply
        .accepted
        .as_mut()
        .unwrap()
        .command_revision = 8;
    tracker.receipt(&superseding_reply, &items).unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    assert_eq!(tracker.jobs[&1].accepted_revision, Some(7));
    tracker
        .receipt(&accepted(1, TorrentCommand::Resume), &items)
        .unwrap();
    assert_eq!(phase(&tracker, id), OperationPhase::NeedsReview);
    assert_eq!(tracker.history.len(), 1);
}

#[test]
fn confirmed_recheck_without_verified_completion_never_turns_green() {
    let mut tracker = Tracker::default();
    let mut items = vec![torrent()];
    items[0].runtime.command = TransferFeedback::Confirmed {
        action: TransferAction::Recheck,
    };
    items[0].completion = CompletionStatus::Pending;
    let id = tracker
        .receipt(&accepted(1, TorrentCommand::Recheck), &items)
        .unwrap()
        .unwrap();
    tracker.reconcile(&connected(2), &items, saved_revision_seven);
    assert_eq!(phase(&tracker, id), OperationPhase::Accepted);
    items[0].completion = CompletionStatus::Verified;
    tracker.reconcile(&connected(3), &items, saved_revision_seven);
    assert_eq!(phase(&tracker, id), OperationPhase::Completed);
}

#[test]
fn full_history_rejects_tracking_admission_without_an_orphan_job_or_lost_active_record() {
    let mut tracker = Tracker {
        history: OperationHistory::with_limits(1, 8192).unwrap(),
        ..Default::default()
    };
    let items = vec![torrent()];
    let first = tracker
        .receipt(&queued(1, TorrentCommand::Pause), &items)
        .unwrap()
        .unwrap();
    assert_eq!(
        tracker.receipt(&queued(2, TorrentCommand::Resume), &items),
        Err(HistoryError::HistoryFull)
    );
    assert_eq!(
        tracker.record_error("Import", None, "Synthetic source", "Invalid metadata"),
        Err(HistoryError::HistoryFull)
    );
    assert_eq!(tracker.jobs.len(), 1);
    assert!(!tracker.jobs.contains_key(&2));
    assert_eq!(tracker.history.len(), 1);
    assert_eq!(phase(&tracker, first), OperationPhase::Queued);
    assert_eq!(tracker.history.rejected_count(), 2);
}
