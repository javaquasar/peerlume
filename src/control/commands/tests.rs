use super::*;
use crate::{
    client::{SessionNotice, tests::torrent},
    completion::CompletionStatus,
    control::{ObservedModeState, ObservedState, test_support::LiveFixture},
    download_mode::DownloadMode,
    session::SessionEvent,
    state::StateStore,
};

fn controller() -> Controller {
    Controller::new(ClientBackend::offline(), vec![torrent()])
}

fn accept(control: &mut Controller, command: TorrentCommand) -> SessionCommand {
    let mut sent = None;
    control
        .submit_via("v1: abc", command, None, |_, command| {
            sent = Some(command);
            Ok(())
        })
        .unwrap();
    sent.unwrap()
}

#[test]
fn every_existing_command_uses_canonical_identity_and_does_not_claim_completion() {
    let commands = [
        TorrentCommand::Pause,
        TorrentCommand::Resume,
        TorrentCommand::Recheck,
        TorrentCommand::SetMode(ModeConfig::default()),
        TorrentCommand::SetRepairPolicy(RepairPolicy::default()),
        TorrentCommand::SetVerifyAfterDownload(true),
        TorrentCommand::SetFiles(vec![0]),
    ];
    for command in commands {
        let mut control = controller();
        control.torrents[0].info_hashes.push("v2: def".into());
        control.publish();
        let before = control.torrents[0].state;
        let revision = control.command_revision("v2: def").unwrap();
        let mut sent = None;
        let accepted = control
            .submit_via("v2: def", command.clone(), Some(revision), |_, command| {
                sent = Some(command);
                Ok(())
            })
            .unwrap();
        assert_eq!(accepted.torrent_id, "v1: abc;v2: def");
        assert!(accepted.command_revision > revision);
        assert_eq!(
            sent,
            Some(
                command
                    .prepare(&control.torrents[0], accepted.torrent_id.clone())
                    .unwrap()
            )
        );
        assert_eq!(control.torrents[0].state, before);
        assert!(!control.torrents[0].overrides.verify_after_download);
        assert_eq!(
            control.torrents[0].overrides.download_mode,
            DownloadMode::Normal
        );
        let row = control
            .read_handle()
            .page(0, 1, None, None)
            .unwrap()
            .torrents
            .remove(0);
        assert_eq!(row.command_revision, accepted.command_revision);
        assert_eq!(row.state, ObservedState::AwaitingEngine);
        assert_eq!(row.mode_state, ObservedModeState::AwaitingEngine);
    }
}

#[test]
fn conflicts_unknown_ids_and_exhausted_revisions_send_nothing() {
    let mut control = controller();
    let initial = control.command_revision("v1: abc").unwrap();
    accept(&mut control, TorrentCommand::Pause);
    let current = control.command_revision("v1: abc").unwrap();
    let fail = |_: &ClientBackend, _: SessionCommand| -> Result<(), CommandError> {
        panic!("must not enqueue")
    };
    assert_eq!(
        control.submit_via("v1: abc", TorrentCommand::Resume, Some(initial), fail),
        Err(CommandError::RevisionConflict {
            current_revision: current
        })
    );
    assert_eq!(
        control.submit_via("unknown", TorrentCommand::Resume, None, fail),
        Err(CommandError::UnknownTorrent)
    );
    assert_eq!(
        control.command_revision("unknown"),
        Err(CommandError::UnknownTorrent)
    );
    control.command_clock = u64::MAX;
    assert_eq!(
        control.submit_via("v1: abc", TorrentCommand::Pause, None, fail),
        Err(CommandError::RevisionExhausted)
    );
    assert_eq!(control.command_revision("v1: abc").unwrap(), current);
}

#[test]
fn per_torrent_tokens_ignore_telemetry_and_unrelated_commands_but_not_re_registration() {
    let mut control = controller();
    let mut other = torrent();
    other.info_hashes = vec!["v1: xyz".into()];
    control.torrents.push(other);
    control.publish();
    let first = control.command_revision("v1: abc").unwrap();
    control
        .submit_via("v1: xyz", TorrentCommand::Pause, None, |_, _| Ok(()))
        .unwrap();
    control.apply(SessionEvent::NetworkTotals {
        id: "v1: abc".into(),
        uploaded: 1,
        downloaded: 2,
    });
    assert_eq!(control.command_revision("v1: abc").unwrap(), first);
    control
        .submit_via("v1: abc", TorrentCommand::Pause, Some(first), |_, _| Ok(()))
        .unwrap();
    control.torrents.remove(0);
    control.publish();
    control.torrents.push(torrent());
    control.publish();
    let new_revision = control.command_revision("v1: abc").unwrap();
    assert!(new_revision > first);
    assert_eq!(
        control.submit("v1: abc", TorrentCommand::Pause, Some(first)),
        Err(CommandError::RevisionConflict {
            current_revision: new_revision
        })
    );
}

#[test]
fn invalid_modes_policies_and_file_indices_have_no_effects() {
    let cases = [
        (
            TorrentCommand::SetMode(ModeConfig {
                mode: DownloadMode::FreezeAfter,
                freeze_after_bytes: 0,
            }),
            CommandError::InvalidMode,
        ),
        (
            TorrentCommand::SetRepairPolicy(RepairPolicy {
                enabled: true,
                max_attempts: 0,
            }),
            CommandError::InvalidRepairPolicy,
        ),
        (
            TorrentCommand::SetRepairPolicy(RepairPolicy {
                enabled: false,
                max_attempts: 11,
            }),
            CommandError::InvalidRepairPolicy,
        ),
        (
            TorrentCommand::SetFiles(vec![usize::MAX]),
            CommandError::InvalidFileSelection,
        ),
        (
            TorrentCommand::SetFiles(vec![0, 0]),
            CommandError::InvalidFileSelection,
        ),
    ];
    for (command, expected) in cases {
        let mut control = controller();
        let revision = control.command_revision("v1: abc").unwrap();
        assert_eq!(
            control.submit_via("v1: abc", command, None, |_, _| panic!(
                "invalid request sent"
            )),
            Err(expected)
        );
        assert_eq!(control.command_revision("v1: abc").unwrap(), revision);
        assert!(matches!(
            control.torrents[0].runtime.mode,
            ModeApplication::AwaitingEngine
        ));
    }
    let mut control = controller();
    control.torrents[0].info_hashes = vec!["v2: def".into()];
    assert_eq!(
        control.submit(
            "v2: def",
            TorrentCommand::SetMode(ModeConfig {
                mode: DownloadMode::NoProgress,
                ..Default::default()
            }),
            None
        ),
        Err(CommandError::InvalidMode)
    );
    control.torrents[0].info_hashes = vec!["v1: abc".into()];
    control.torrents[0].files = Default::default();
    assert_eq!(
        control.submit(
            "v1: abc",
            TorrentCommand::SetMode(ModeConfig {
                mode: DownloadMode::NoProgress,
                ..Default::default()
            }),
            None
        ),
        Err(CommandError::InvalidMode)
    );
}

#[test]
fn valid_file_selections_are_sorted_empty_is_allowed_and_duplicates_are_rejected() {
    let mut control = controller();
    control.torrents[0].files =
        crate::file_selection::FileSelectionModel::from_files([("a".into(), 1), ("b".into(), 1)]);
    assert_eq!(
        accept(&mut control, TorrentCommand::SetFiles(vec![1, 0])),
        SessionCommand::SetFiles {
            id: "v1: abc".into(),
            only_files: vec![0, 1]
        }
    );
    assert_eq!(
        accept(&mut control, TorrentCommand::SetFiles(vec![])),
        SessionCommand::SetFiles {
            id: "v1: abc".into(),
            only_files: vec![]
        }
    );
    assert_eq!(
        control.submit("v1: abc", TorrentCommand::SetFiles(vec![0, 0]), None),
        Err(CommandError::InvalidFileSelection)
    );
    assert_eq!(control.torrents[0].files.selected_count(), 2);
}

#[test]
fn overflowed_metadata_is_rejected_instead_of_panicking() {
    let mut control = controller();
    control.torrents[0].files = crate::file_selection::FileSelectionModel::from_files([
        ("a".into(), u64::MAX),
        ("b".into(), 1),
    ]);
    assert_eq!(
        control.submit(
            "v1: abc",
            TorrentCommand::SetMode(ModeConfig::default()),
            None
        ),
        Err(CommandError::InvalidMode)
    );
}

#[test]
fn transport_errors_are_typed_and_do_not_advance_tokens_or_settings() {
    for error in [
        CommandError::EngineUnavailable,
        CommandError::QueueFull,
        CommandError::EngineClosed,
    ] {
        let mut control = controller();
        let revision = control.command_revision("v1: abc").unwrap();
        assert_eq!(
            control.submit_via("v1: abc", TorrentCommand::Pause, None, |_, _| Err(error)),
            Err(error)
        );
        assert_eq!(control.command_revision("v1: abc").unwrap(), revision);
        assert_eq!(control.torrents[0].state, TorrentState::WaitingForEngine);
        assert_eq!(
            control.submit_via(
                "v1: abc",
                TorrentCommand::SetMode(ModeConfig {
                    mode: DownloadMode::NoProgress,
                    ..Default::default()
                }),
                None,
                |_, _| Err(error)
            ),
            Err(error)
        );
        assert!(matches!(
            control.torrents[0].runtime.mode,
            ModeApplication::Failed { .. }
        ));
        assert_eq!(
            control.torrents[0].overrides.download_mode,
            DownloadMode::Normal
        );
        assert_eq!(control.command_revision("v1: abc").unwrap(), revision);
    }
    assert_eq!(
        controller().submit("v1: abc", TorrentCommand::Pause, None),
        Err(CommandError::EngineUnavailable)
    );
    for (error, mapped) in [
        (
            SessionSendError::Unavailable,
            CommandError::EngineUnavailable,
        ),
        (SessionSendError::Full, CommandError::QueueFull),
        (SessionSendError::Closed, CommandError::EngineClosed),
    ] {
        assert_eq!(CommandError::from(error), mapped);
        let json = serde_json::to_string(&mapped).unwrap();
        assert_eq!(serde_json::from_str::<CommandError>(&json).unwrap(), mapped);
        assert!(!mapped.to_string().is_empty());
    }
}

#[test]
fn batches_report_rejections_and_do_not_mutate_offline_models() {
    let states = [
        TorrentState::Downloading,
        TorrentState::WaitingForEngine,
        TorrentState::Seeding,
        TorrentState::Checking,
        TorrentState::Paused,
        TorrentState::Error,
        TorrentState::VerificationFailed,
    ];
    let items = states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let mut item = torrent();
            item.info_hashes = vec![format!("v1: {index}")];
            item.state = *state;
            item
        })
        .collect();
    let mut control = Controller::new(ClientBackend::offline(), items);
    assert!(
        control
            .submit_batch(false)
            .contains("queued: 0; rejected: 4")
    );
    assert!(
        control
            .submit_batch(true)
            .contains("queued: 0; rejected: 3")
    );
    assert_eq!(
        control
            .torrents
            .iter()
            .map(|item| item.state)
            .collect::<Vec<_>>(),
        states
    );
    assert!(
        Controller::new(ClientBackend::offline(), vec![])
            .submit_batch(false)
            .contains("queued: 0; rejected: 0")
    );
}

#[test]
fn partial_batches_publish_once_and_do_not_hide_a_full_queue() {
    let items = (0..100)
        .map(|index| {
            let mut item = torrent();
            item.info_hashes = vec![format!("v1: {index}")];
            item.state = TorrentState::Downloading;
            item
        })
        .collect();
    let mut control = Controller::new(ClientBackend::offline(), items);
    let reader = control.read_handle();
    let before = reader.page(0, 1, None, None).unwrap().revision;
    let mut sent = 0;
    let result = control.submit_batch_via(false, |_, _| {
        sent += 1;
        if sent > 3 {
            Err(CommandError::QueueFull)
        } else {
            Ok(())
        }
    });
    assert!(result.contains("queued: 3; rejected: 97"));
    assert_eq!(reader.page(0, 1, None, None).unwrap().revision, before + 1);
    assert!(
        control
            .torrents
            .iter()
            .all(|item| item.state == TorrentState::Downloading)
    );
}

#[test]
fn a_missing_state_store_is_not_reported_as_successful_persistence() {
    let mut control = controller();
    control.apply(SessionEvent::VerifyAfterDownload {
        id: "v1: abc".into(),
        enabled: true,
    });
    assert!(
        control
            .persist_confirmed_settings("v1: abc", None)
            .unwrap_err()
            .contains("unavailable")
    );
    assert!(control.unpersisted_settings.contains("v1: abc"));
}

#[test]
fn real_engine_rejects_unknown_and_invalid_setting_targets_explicitly() {
    use std::time::{Duration, Instant};
    let mut fixture = LiveFixture::new();
    for command in [
        SessionCommand::SetVerifyAfterDownload {
            id: "unknown".into(),
            enabled: true,
        },
        SessionCommand::SetFiles {
            id: "unknown".into(),
            only_files: vec![],
        },
        SessionCommand::SetFiles {
            id: fixture.id.clone(),
            only_files: vec![usize::MAX],
        },
    ] {
        fixture.controller.backend.send(command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(SessionNotice::Error { .. }) = fixture.controller.poll_next() {
                break;
            }
            assert!(Instant::now() < deadline, "missing engine rejection");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn accepted_commands_do_not_accumulate_operation_history() {
    let mut control = controller();
    for _ in 0..10_000 {
        accept(&mut control, TorrentCommand::Pause);
    }
    assert_eq!(control.observed.len(), 1);
    assert_eq!(control.unpersisted_settings.len(), 0);
    let receipt = control
        .submit_via("v1: abc", TorrentCommand::Pause, None, |_, _| Ok(()))
        .unwrap();
    let json = serde_json::to_string(&receipt).unwrap();
    assert_eq!(
        serde_json::from_str::<AcceptedCommand>(&json).unwrap(),
        receipt
    );
    assert!(!json.contains("applied"));
}

#[test]
fn persistence_requires_confirmation_preserves_audit_and_retains_retry_on_failure() {
    let store = StateStore::in_memory().unwrap();
    let mut control = Controller::new(ClientBackend::with_state_store(store), vec![torrent()]);
    assert!(control.persist_confirmed_settings("unknown", None).is_err());
    assert!(control.persist_confirmed_settings("v1: abc", None).is_err());
    accept(&mut control, TorrentCommand::SetVerifyAfterDownload(true));
    assert!(control.persist_confirmed_settings("v1: abc", None).is_err());
    assert!(control.backend.load_torrents().unwrap().is_empty());
    assert_eq!(
        control.apply(SessionEvent::VerifyAfterDownload {
            id: "v1: abc".into(),
            enabled: true
        }),
        SessionNotice::SettingsApplied {
            id: "v1: abc".into()
        }
    );
    let audit = StoredTrackerAuditProfile {
        allowed_host: "localhost".into(),
        port: 1234,
        started_only: false,
        observation_seconds: 0,
        steps: vec![],
    };
    control
        .persist_confirmed_settings("v1: abc", Some(audit.clone()))
        .unwrap();
    let record = control.backend.load_torrents().unwrap().remove(0);
    assert!(record.verify_after_download);
    assert_eq!(record.tracker_audit, Some(audit));
    assert!(control.persist_confirmed_settings("v1: abc", None).is_err());
    control.apply(SessionEvent::VerifyAfterDownload {
        id: "v1: abc".into(),
        enabled: false,
    });
    control.torrents[0].uploaded_bytes = u64::MAX;
    assert!(control.persist_confirmed_settings("v1: abc", None).is_err());
    assert!(control.unpersisted_settings.contains("v1: abc"));
    assert!(control.backend.load_torrents().unwrap()[0].verify_after_download);
    control.torrents[0].uploaded_bytes = 0;
    control.persist_confirmed_settings("v1: abc", None).unwrap();
    assert!(!control.backend.load_torrents().unwrap()[0].verify_after_download);
}

#[test]
fn file_confirmation_updates_payload_accounting_and_invalidates_stale_done() {
    let mut control = controller();
    control.torrents[0].files.set_total_downloaded(100);
    control.torrents[0].downloaded_bytes = 100;
    control.torrents[0].completion = CompletionStatus::Verified;
    control.torrents[0].state = TorrentState::Seeding;
    accept(&mut control, TorrentCommand::SetFiles(vec![]));
    assert!(control.torrents[0].completed_successfully());
    control.apply(SessionEvent::FilesApplied {
        id: "v1: abc".into(),
        only_files: vec![],
    });
    control.publish();
    assert_eq!(control.torrents[0].files.selected_count(), 0);
    assert_eq!(control.torrents[0].downloaded_bytes, 0);
    assert!(!control.torrents[0].completed_successfully());
    assert_eq!(control.torrents[0].completion, CompletionStatus::Pending);
    assert_eq!(
        control
            .read_handle()
            .page(0, 1, None, None)
            .unwrap()
            .torrents[0]
            .downloaded_bytes,
        0
    );
}

#[test]
fn real_pause_resume_and_batches_wait_for_engine_state() {
    let mut fixture = LiveFixture::new();
    assert!(
        fixture
            .controller
            .submit_batch(true)
            .contains("queued: 1; rejected: 0")
    );
    assert_eq!(fixture.controller.torrents[0].state, TorrentState::Paused);
    fixture.wait_for(|fixture| fixture.controller.torrents[0].state == TorrentState::Seeding);
    assert!(
        fixture
            .controller
            .submit_batch(false)
            .contains("queued: 1; rejected: 0")
    );
    assert_eq!(fixture.controller.torrents[0].state, TorrentState::Seeding);
    fixture.wait_for(|fixture| fixture.controller.torrents[0].state == TorrentState::Paused);
}

#[test]
fn real_mode_and_verification_settings_are_saved_only_after_acknowledgement() {
    let mut fixture = LiveFixture::new();
    fixture
        .controller
        .submit(
            &fixture.id,
            TorrentCommand::SetMode(ModeConfig {
                mode: DownloadMode::NoProgress,
                ..Default::default()
            }),
            None,
        )
        .unwrap();
    assert_eq!(
        fixture.controller.torrents[0].overrides.download_mode,
        DownloadMode::Normal
    );
    assert_eq!(
        fixture.controller.backend.load_torrents().unwrap()[0].download_mode,
        DownloadMode::Normal
    );
    fixture.wait_for(|fixture| {
        fixture.controller.torrents[0].overrides.download_mode == DownloadMode::NoProgress
    });
    assert_eq!(
        fixture.controller.backend.load_torrents().unwrap()[0].download_mode,
        DownloadMode::NoProgress
    );
    let before = fixture.settings_applied;
    fixture
        .controller
        .submit(
            &fixture.id,
            TorrentCommand::SetVerifyAfterDownload(true),
            None,
        )
        .unwrap();
    assert!(
        !fixture.controller.torrents[0]
            .overrides
            .verify_after_download
    );
    fixture.wait_for(|fixture| fixture.settings_applied > before);
    assert!(fixture.controller.backend.load_torrents().unwrap()[0].verify_after_download);
}

#[test]
fn real_repair_policy_cannot_disable_its_required_verification() {
    let mut fixture = LiveFixture::new();
    let before = fixture.settings_applied;
    fixture
        .controller
        .submit(
            &fixture.id,
            TorrentCommand::SetRepairPolicy(RepairPolicy {
                enabled: true,
                max_attempts: 2,
            }),
            None,
        )
        .unwrap();
    assert!(
        !fixture.controller.torrents[0]
            .overrides
            .repair_policy
            .enabled
    );
    fixture.wait_for(|fixture| fixture.settings_applied > before);
    assert!(
        fixture.controller.torrents[0]
            .overrides
            .verify_after_download
    );
    let before = fixture.settings_applied;
    fixture
        .controller
        .submit(
            &fixture.id,
            TorrentCommand::SetVerifyAfterDownload(false),
            None,
        )
        .unwrap();
    fixture.wait_for(|fixture| fixture.settings_applied > before);
    assert!(fixture.controller.backend.load_torrents().unwrap()[0].verify_after_download);
}

#[test]
fn real_file_selection_waits_for_ack_before_changing_the_model_and_database() {
    let mut fixture = LiveFixture::new();
    let before = fixture.settings_applied;
    fixture
        .controller
        .submit(&fixture.id, TorrentCommand::SetFiles(vec![]), None)
        .unwrap();
    assert!(fixture.controller.torrents[0].files.files()[0].selected);
    assert!(fixture.controller.backend.load_torrents().unwrap()[0].files[0].selected);
    fixture.wait_for(|fixture| fixture.settings_applied > before);
    assert!(!fixture.controller.torrents[0].files.files()[0].selected);
    assert!(!fixture.controller.backend.load_torrents().unwrap()[0].files[0].selected);
}

#[test]
fn real_recheck_of_corruption_does_not_change_mode_or_fake_new_downloads() {
    let mut fixture = LiveFixture::new();
    std::fs::write(
        fixture.root.path().join("payload.bin"),
        b"corrupt command bytes",
    )
    .unwrap();
    fixture
        .controller
        .submit(&fixture.id, TorrentCommand::Recheck, None)
        .unwrap();
    assert_eq!(
        fixture.controller.torrents[0].completion,
        CompletionStatus::Verified
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        while let Some(notice) = fixture.controller.poll_next() {
            if let SessionNotice::Error { .. } = notice {
                assert!(matches!(
                    fixture.controller.torrents[0].completion,
                    CompletionStatus::Failed { .. }
                ));
            }
        }
        if matches!(
            fixture.controller.torrents[0].completion,
            CompletionStatus::Failed { .. }
        ) {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fixture.controller.torrents[0].runtime.network_downloaded, 0);
    assert_eq!(
        fixture.controller.torrents[0].overrides.download_mode,
        DownloadMode::Normal
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_indices_never_panic_or_change_the_confirmed_selection(indices in proptest::collection::vec(0usize..8, 0..12)) {
        let mut control = controller();
        let before = control.torrents[0].files.selected_count();
        let result = control.submit_via("v1: abc", TorrentCommand::SetFiles(indices.clone()), None, |_, _| Ok(()));
        let valid = indices.is_empty() || indices == [0];
        proptest::prop_assert_eq!(result.is_ok(), valid);
        proptest::prop_assert_eq!(control.torrents[0].files.selected_count(), before);
    }
}
