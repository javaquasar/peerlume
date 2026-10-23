use super::*;
use crate::{
    client::{ClientBackend, torrent_to_record},
    state::StateStore,
};

struct Fixture {
    control: Controller,
    _root: tempfile::TempDir,
    token: String,
}
impl Fixture {
    fn new(grants: Grants) -> Self {
        let root = tempfile::tempdir().unwrap();
        let item = crate::client::tests::torrent();
        let mut backend = ClientBackend::with_state_store(StateStore::in_memory().unwrap());
        backend
            .save_torrent(&torrent_to_record(&item, None))
            .unwrap();
        let mut control = Controller::new(backend, vec![item]);
        control
            .configure_agents(root.path(), root.path().into())
            .unwrap();
        let agent = control.agent_access().unwrap();
        agent.enabled = true;
        let token = agent.grant(grants).unwrap();
        Self {
            control,
            _root: root,
            token,
        }
    }
    fn manager() -> Self {
        Self::new(Grants {
            manage: true,
            reporting: true,
            export: true,
            remove: true,
        })
    }
    fn call(&mut self, method: Method) -> Response {
        let instance = self.control.agent.as_ref().unwrap().instance_id.clone();
        self.control.handle_request(Request {
            protocol_version: super::super::PROTOCOL_VERSION,
            request_id: "regression".into(),
            token: self.token.clone(),
            instance_id: Some(instance),
            method,
        })
    }
    fn command(&self, command: TorrentCommand) -> Method {
        Method::Mutate {
            torrent_id: "v1: abc".into(),
            command,
            expected_revision: self.control.command_revision("v1: abc").unwrap(),
            idempotency_key: "regression-key".into(),
            deadline_unix_ms: now_ms() + 60_000,
        }
    }
    fn accept(&mut self, command: TorrentCommand) -> String {
        let method = self.command(command);
        let value = self.call(method).result.unwrap();
        value["operation_id"].as_str().unwrap().into()
    }
    fn running(&mut self, command: TorrentCommand) -> String {
        let id = self.accept(command);
        let agent = self.control.agent_access().unwrap();
        let pending = agent.pending.get_mut(&id).unwrap();
        pending.dispatched = true;
        pending.operation.state = OperationState::Running;
        agent.store.save(&pending.operation).unwrap();
        id
    }
    fn record(&self, id: &str) -> Operation {
        self.control
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .into_iter()
            .find(|op| op.id == id)
            .unwrap()
    }
    fn emit(&mut self, event: SessionEvent) {
        self.control.apply(event);
        self.control.publish();
    }
    fn expire(&mut self, id: &str) {
        self.control
            .agent_access()
            .unwrap()
            .pending
            .get_mut(id)
            .unwrap()
            .operation
            .deadline_unix_ms = 0;
    }
}

#[test]
fn disabled_service_rejects_even_a_valid_token_without_creating_work() {
    let mut f = Fixture::manager();
    f.control.agent_access().unwrap().enabled = false;
    let method = f.command(TorrentCommand::Pause);
    assert_eq!(f.call(method).error, Some(ErrorCode::Disabled));
    assert!(
        f.control
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn read_only_clients_can_page_status_files_and_their_operation_list() {
    let mut f = Fixture::new(Grants::default());
    for method in [
        Method::List {
            offset: 0,
            limit: 1,
            revision: None,
            after_revision: None,
        },
        Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 0,
            limit: 1,
        },
        Method::Operations {
            offset: 0,
            limit: 1,
        },
    ] {
        assert!(f.call(method).error.is_none());
    }
}
#[test]
fn all_permission_combinations_are_enforced_for_every_command_kind() {
    let commands = [
        TorrentCommand::Pause,
        TorrentCommand::Resume,
        TorrentCommand::Recheck,
        TorrentCommand::SetFiles(vec![0]),
        TorrentCommand::SetVerifyAfterDownload(true),
        TorrentCommand::SetRepairPolicy(Default::default()),
        TorrentCommand::SetMode(Default::default()),
        TorrentCommand::Remove,
    ];
    for mask in 0..16 {
        for command in &commands {
            let grants = Grants {
                manage: mask & 1 != 0,
                reporting: mask & 2 != 0,
                export: mask & 4 != 0,
                remove: mask & 8 != 0,
            };
            let allowed = match command {
                TorrentCommand::SetMode(_) => grants.reporting,
                TorrentCommand::Remove => grants.remove,
                _ => grants.manage,
            };
            let mut f = Fixture::new(grants);
            let method = f.command(command.clone());
            let response = f.call(method);
            assert_eq!(
                response.error,
                if allowed {
                    None
                } else {
                    Some(ErrorCode::PermissionDenied)
                },
                "mask={mask} {command:?}"
            );
            assert_eq!(
                f.control
                    .agent
                    .as_ref()
                    .unwrap()
                    .store
                    .list()
                    .unwrap()
                    .len(),
                usize::from(allowed)
            );
        }
    }
}
#[test]
fn missing_instance_is_allowed_for_discovery_but_not_for_mutation() {
    let mut f = Fixture::manager();
    for (method, error) in [
        (Method::Discover, None),
        (f.command(TorrentCommand::Pause), Some(ErrorCode::Conflict)),
    ] {
        let reply = f.control.handle_request(Request {
            protocol_version: 1,
            request_id: "id".into(),
            token: f.token.clone(),
            instance_id: None,
            method,
        });
        assert_eq!(reply.error, error);
    }
}
#[test]
fn unknown_torrent_is_not_accepted_or_added_to_operation_history() {
    let mut f = Fixture::manager();
    let mut method = f.command(TorrentCommand::Pause);
    if let Method::Mutate { torrent_id, .. } = &mut method {
        *torrent_id = "unknown".into();
    }
    assert_eq!(f.call(method).error, Some(ErrorCode::UnknownTorrent));
    assert!(
        f.control
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn empty_idempotency_key_is_rejected_before_operation_creation() {
    let mut f = Fixture::manager();
    let mut method = f.command(TorrentCommand::Pause);
    if let Method::Mutate {
        idempotency_key, ..
    } = &mut method
    {
        idempotency_key.clear();
    }
    assert_eq!(f.call(method).error, Some(ErrorCode::InvalidRequest));
}
#[test]
fn overlong_idempotency_key_is_rejected() {
    let mut f = Fixture::manager();
    let mut method = f.command(TorrentCommand::Pause);
    if let Method::Mutate {
        idempotency_key, ..
    } = &mut method
    {
        *idempotency_key = "k".repeat(129);
    }
    assert_eq!(f.call(method).error, Some(ErrorCode::InvalidRequest));
}
#[test]
fn distant_deadline_is_not_accepted() {
    let mut f = Fixture::manager();
    let mut method = f.command(TorrentCommand::Pause);
    if let Method::Mutate {
        deadline_unix_ms, ..
    } = &mut method
    {
        *deadline_unix_ms = u64::MAX;
    }
    assert_eq!(f.call(method).error, Some(ErrorCode::DeadlineExceeded));
}
#[test]
fn external_file_selection_cannot_exceed_wire_cardinality_limit() {
    let mut f = Fixture::manager();
    let method = f.command(TorrentCommand::SetFiles(vec![0; 4097]));
    assert_eq!(f.call(method).error, Some(ErrorCode::TooLarge));
    assert!(
        f.control
            .agent
            .as_ref()
            .unwrap()
            .store
            .list()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn pending_torrent_rejects_a_second_operation_even_with_another_key() {
    let mut f = Fixture::manager();
    f.accept(TorrentCommand::Pause);
    let mut method = f.command(TorrentCommand::Resume);
    if let Method::Mutate {
        idempotency_key, ..
    } = &mut method
    {
        *idempotency_key = "second".into();
    }
    assert_eq!(f.call(method).error, Some(ErrorCode::Busy));
}
#[test]
fn queued_deadline_expiry_cancels_without_sending_to_the_engine() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    let revision = f.control.command_revision("v1: abc").unwrap();
    f.expire(&id);
    f.control.tick();
    let op = f.record(&id);
    assert_eq!(op.state, OperationState::Cancelled);
    assert_eq!(op.error, Some(ErrorCode::DeadlineExceeded));
    assert_eq!(f.control.command_revision("v1: abc").unwrap(), revision);
}
#[test]
fn running_deadline_expiry_requires_reconciliation_without_replay() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Resume);
    f.expire(&id);
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::ReconciliationRequired);
    assert_eq!(f.record(&id).error, Some(ErrorCode::ReconciliationRequired));
    assert!(f.control.agent.as_ref().unwrap().pending.is_empty());
}
#[test]
fn disabled_access_does_not_dispatch_queued_authorized_work() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Resume);
    f.control.agent_access().unwrap().enabled = false;
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Queued);
    assert!(!f.control.agent.as_ref().unwrap().pending[&id].dispatched);
}
#[test]
fn revoking_client_cancels_undispatched_work_and_persists_the_result() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    let client = f.record(&id).client_id;
    f.control.agent_access().unwrap().revoke(&client).unwrap();
    assert_eq!(f.record(&id).state, OperationState::Cancelled);
    assert!(f.control.agent.as_ref().unwrap().pending.is_empty());
    assert_eq!(
        f.call(Method::Discover).error,
        Some(ErrorCode::Unauthorized)
    );
}
#[test]
fn revocation_does_not_roll_back_an_already_dispatched_command() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    let client = f.record(&id).client_id;
    f.control.agent_access().unwrap().revoke(&client).unwrap();
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "paused",
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
}
#[test]
fn user_revision_change_between_acceptance_and_dispatch_fails_cleanly() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    f.control
        .observed
        .get_mut("v1: abc")
        .unwrap()
        .command_revision += 1;
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Failed);
    assert_eq!(f.record(&id).error, Some(ErrorCode::Conflict));
}
#[test]
fn gui_command_is_rejected_while_an_agent_mutation_awaits_confirmation() {
    let mut f = Fixture::manager();
    f.running(TorrentCommand::Pause);
    assert_eq!(
        f.control
            .submit("v1: abc", TorrentCommand::Resume, None)
            .err(),
        Some(super::super::CommandError::OperationInProgress)
    );
}
#[test]
fn queued_engine_rejection_is_a_failed_operation_not_a_success() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Failed);
    assert_eq!(f.record(&id).error, Some(ErrorCode::EngineRejected));
}
#[test]
fn wrong_torrent_acknowledgement_does_not_complete_an_operation() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.emit(SessionEvent::StateChanged {
        id: "other".into(),
        state: "paused",
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn wrong_state_acknowledgement_does_not_complete_pause() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "downloading",
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn queued_unrelated_acknowledgement_is_not_confirmation_of_an_undispatched_command() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "paused",
    });
    assert_eq!(f.record(&id).state, OperationState::Queued);
}
#[test]
fn matching_pause_acknowledgement_checkpoints_before_succeeding() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "paused",
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert_eq!(
        f.control.backend.load_torrents().unwrap()[0].state,
        "paused"
    );
}
#[test]
fn matching_resume_acknowledgement_checkpoints_active_intent() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Resume);
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "downloading",
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert_eq!(
        f.control.backend.load_torrents().unwrap()[0].state,
        "downloading"
    );
}
#[test]
fn matching_file_acknowledgement_saves_selection() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetFiles(vec![]));
    f.emit(SessionEvent::FilesApplied {
        id: "v1: abc".into(),
        only_files: vec![],
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert!(!f.control.backend.load_torrents().unwrap()[0].files[0].selected);
}
#[test]
fn mismatched_file_acknowledgement_does_not_complete_selection_change() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetFiles(vec![]));
    f.emit(SessionEvent::FilesApplied {
        id: "v1: abc".into(),
        only_files: vec![0],
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn matching_mode_acknowledgement_saves_the_applied_policy() {
    let mut f = Fixture::manager();
    let config = crate::runtime_status::ModeConfig {
        mode: crate::download_mode::DownloadMode::HideDownloaded,
        ..Default::default()
    };
    let id = f.running(TorrentCommand::SetMode(config));
    f.emit(SessionEvent::Mode {
        id: "v1: abc".into(),
        application: crate::runtime_status::ModeApplication::Active {
            config,
            tracker_enabled: true,
        },
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert_eq!(
        f.control.backend.load_torrents().unwrap()[0].download_mode,
        config.mode
    );
}
#[test]
fn wrong_mode_acknowledgement_does_not_complete_policy_change() {
    let mut f = Fixture::manager();
    let config = crate::runtime_status::ModeConfig {
        mode: crate::download_mode::DownloadMode::HideDownloaded,
        ..Default::default()
    };
    let id = f.running(TorrentCommand::SetMode(config));
    f.emit(SessionEvent::Mode {
        id: "v1: abc".into(),
        application: crate::runtime_status::ModeApplication::Active {
            config: Default::default(),
            tracker_enabled: true,
        },
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn matching_repair_acknowledgement_saves_required_verification() {
    let mut f = Fixture::manager();
    let policy = crate::runtime_status::RepairPolicy {
        enabled: true,
        max_attempts: 2,
    };
    let id = f.running(TorrentCommand::SetRepairPolicy(policy));
    f.emit(SessionEvent::RepairPolicy {
        id: "v1: abc".into(),
        policy,
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    let record = &f.control.backend.load_torrents().unwrap()[0];
    assert_eq!(record.repair_policy, policy);
    assert!(record.verify_after_download);
}
#[test]
fn wrong_repair_acknowledgement_does_not_complete_the_operation() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetRepairPolicy(
        crate::runtime_status::RepairPolicy {
            enabled: true,
            max_attempts: 2,
        },
    ));
    f.emit(SessionEvent::RepairPolicy {
        id: "v1: abc".into(),
        policy: Default::default(),
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn matching_verification_setting_acknowledgement_is_persisted() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetVerifyAfterDownload(true));
    f.emit(SessionEvent::VerifyAfterDownload {
        id: "v1: abc".into(),
        enabled: true,
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert!(f.control.backend.load_torrents().unwrap()[0].verify_after_download);
}
#[test]
fn repair_required_verification_can_override_a_disable_request() {
    let mut f = Fixture::manager();
    f.control.torrents[0].overrides.repair_policy.enabled = true;
    let id = f.running(TorrentCommand::SetVerifyAfterDownload(false));
    f.emit(SessionEvent::VerifyAfterDownload {
        id: "v1: abc".into(),
        enabled: true,
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
}
#[test]
fn stale_verified_status_cannot_complete_a_new_recheck() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Recheck);
    f.emit(SessionEvent::Verification {
        id: "v1: abc".into(),
        status: crate::completion::CompletionStatus::Verified,
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
#[test]
fn fresh_checking_then_verified_completes_recheck() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Recheck);
    f.emit(SessionEvent::Verification {
        id: "v1: abc".into(),
        status: crate::completion::CompletionStatus::Checking {
            checked_bytes: 50,
            total_bytes: 100,
        },
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
    f.emit(SessionEvent::Verification {
        id: "v1: abc".into(),
        status: crate::completion::CompletionStatus::Verified,
    });
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
}
#[test]
fn failed_recheck_does_not_leak_engine_error_text() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Recheck);
    f.emit(SessionEvent::Verification {
        id: "v1: abc".into(),
        status: crate::completion::CompletionStatus::Failed {
            message: "secret tracker passkey".into(),
        },
    });
    let reply = f.call(Method::Operation { operation_id: id });
    assert_eq!(reply.result.as_ref().unwrap()["state"], "failed");
    assert!(!serde_json::to_string(&reply).unwrap().contains("passkey"));
}
#[test]
fn engine_error_fails_the_pending_operation_with_a_typed_error_only() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.emit(SessionEvent::Error {
        id: "v1: abc".into(),
        message: "SECRET source path".into(),
    });
    assert_eq!(f.record(&id).state, OperationState::Failed);
    assert_eq!(f.record(&id).error, Some(ErrorCode::EngineRejected));
    assert!(
        !serde_json::to_string(&operation_json(&f.record(&id)))
            .unwrap()
            .contains("SECRET")
    );
}
#[test]
fn failed_checkpoint_never_turns_a_confirmed_command_into_success() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.control.torrents[0].uploaded_bytes = u64::MAX;
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "paused",
    });
    assert_eq!(f.record(&id).state, OperationState::ReconciliationRequired);
    assert_eq!(f.record(&id).error, Some(ErrorCode::PersistenceUnavailable));
}
#[test]
fn approval_cannot_be_reused_or_applied_after_expiry() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Remove);
    f.control
        .agent_access()
        .unwrap()
        .approve(&id, true)
        .unwrap();
    assert_eq!(
        f.control.agent_access().unwrap().approve(&id, true),
        Err(ErrorCode::Conflict)
    );
    f.expire(&id);
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Cancelled);
}
#[test]
fn expired_pending_approval_cannot_be_granted() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Remove);
    f.expire(&id);
    assert_eq!(
        f.control.agent_access().unwrap().approve(&id, true),
        Err(ErrorCode::DeadlineExceeded)
    );
}
#[test]
fn another_client_cannot_cancel_or_read_an_owned_operation() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    f.token = f
        .control
        .agent_access()
        .unwrap()
        .grant(Grants::default())
        .unwrap();
    for method in [
        Method::Operation {
            operation_id: id.clone(),
        },
        Method::Cancel { operation_id: id },
    ] {
        assert_eq!(f.call(method).error, Some(ErrorCode::UnknownOperation));
    }
    assert_eq!(
        f.call(Method::Operations {
            offset: 0,
            limit: 128
        })
        .result
        .unwrap()["operations"],
        json!([])
    );
}
#[test]
fn client_limit_is_enforced_before_a_65th_token_is_persisted() {
    let mut f = Fixture::manager();
    for _ in 1..64 {
        f.control
            .agent_access()
            .unwrap()
            .grant(Grants::default())
            .unwrap();
    }
    assert_eq!(
        f.control
            .agent_access()
            .unwrap()
            .grant(Grants::default())
            .err(),
        Some(ErrorCode::Busy)
    );
    let agent = f.control.agent_access().unwrap();
    let count: i64 = agent
        .store
        .connection
        .query_row("SELECT count(*) FROM agent_clients", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 64);
}
#[test]
fn corrupt_client_record_fails_closed_on_reopen() {
    let f = Fixture::manager();
    f.control
        .agent
        .as_ref()
        .unwrap()
        .store
        .connection
        .execute("UPDATE agent_clients SET record='broken'", [])
        .unwrap();
    assert_eq!(
        AgentService::open(f._root.path(), f._root.path().into()).err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
}
#[test]
fn invalid_operation_pages_are_rejected_before_slicing() {
    let mut f = Fixture::manager();
    for (offset, limit) in [(0, 0), (0, 129), (usize::MAX, 1), (1, 1)] {
        assert_eq!(
            f.call(Method::Operations { offset, limit }).error,
            Some(ErrorCode::InvalidPage)
        );
    }
}
#[test]
fn file_detail_pages_preserve_global_indices_and_hide_names() {
    let mut f = Fixture::manager();
    f.control.torrents[0].files = crate::file_selection::FileSelectionModel::from_files([
        ("SECRET-a".into(), 10),
        ("SECRET-b".into(), 20),
    ]);
    let a = f
        .call(Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 0,
            limit: 1,
        })
        .result
        .unwrap();
    let b = f
        .call(Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 1,
            limit: 1,
        })
        .result
        .unwrap();
    assert_eq!(a["next_offset"], 1);
    assert_eq!(b["files"][0]["index"], 1);
    assert_eq!(b["files"][0]["size_bytes"], 20);
    assert!(b["next_offset"].is_null());
    assert!(!a.to_string().contains("SECRET"));
}
#[test]
fn empty_end_file_page_is_valid_but_past_end_is_not() {
    let mut f = Fixture::manager();
    let end = f
        .call(Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 1,
            limit: 1,
        })
        .result
        .unwrap();
    assert_eq!(end["files"], json!([]));
    assert_eq!(
        f.call(Method::Details {
            torrent_id: "v1: abc".into(),
            offset: 2,
            limit: 1
        })
        .error,
        Some(ErrorCode::InvalidPage)
    );
}
#[test]
fn offline_peer_view_distinguishes_absent_tracker_reply_from_connected_count() {
    let mut f = Fixture::manager();
    f.control.torrents[0].peers = 7;
    let value = f
        .call(Method::Peers {
            torrent_id: "v1: abc".into(),
            offset: 0,
            limit: 1,
        })
        .result
        .unwrap();
    assert_eq!(value["tracker_returned"], json!([]));
    assert_eq!(value["connected_count"], 7);
    assert!(value["reply_sequence"].is_null());
    assert!(value["connected_addresses"].is_null());
}
#[test]
fn missing_journal_is_unsupported_not_an_invented_empty_history() {
    let mut f = Fixture::manager();
    assert_eq!(
        f.call(Method::Journal {
            torrent_id: "v1: abc".into(),
            offset: 0,
            limit: 1
        })
        .error,
        Some(ErrorCode::Unsupported)
    );
}
#[test]
fn journal_page_bound_is_stricter_than_general_read_pages() {
    let mut f = Fixture::manager();
    for limit in [0, 33, usize::MAX] {
        assert_eq!(
            f.call(Method::Journal {
                torrent_id: "v1: abc".into(),
                offset: 0,
                limit
            })
            .error,
            Some(ErrorCode::InvalidPage)
        );
    }
}
#[test]
fn read_queries_for_unknown_torrents_return_typed_errors() {
    let mut f = Fixture::manager();
    for method in [
        Method::Details {
            torrent_id: "missing".into(),
            offset: 0,
            limit: 1,
        },
        Method::Peers {
            torrent_id: "missing".into(),
            offset: 0,
            limit: 1,
        },
        Method::Journal {
            torrent_id: "missing".into(),
            offset: 0,
            limit: 1,
        },
    ] {
        assert_eq!(f.call(method).error, Some(ErrorCode::UnknownTorrent));
    }
}
#[test]
fn readonly_preview_has_no_filesystem_side_effects() {
    let mut f = Fixture::new(Grants::default());
    assert_eq!(
        f.call(Method::Preview {
            source: "missing.torrent".into()
        })
        .error,
        Some(ErrorCode::PermissionDenied)
    );
    assert!(!f._root.path().join("imports").exists());
}
#[test]
fn malformed_preview_is_an_input_error_without_raw_parser_text() {
    let mut f = Fixture::manager();
    std::fs::create_dir(f._root.path().join("imports")).unwrap();
    std::fs::write(f._root.path().join("imports/bad.torrent"), b"SECRET").unwrap();
    let response = f.call(Method::Preview {
        source: "bad.torrent".into(),
    });
    assert_eq!(response.error, Some(ErrorCode::InvalidRequest));
    assert!(!serde_json::to_string(&response).unwrap().contains("SECRET"));
}
#[test]
fn queued_add_outside_import_root_fails_without_a_torrent_record() {
    let mut f = Fixture::manager();
    let response = f.call(Method::Add {
        source: "../secret.torrent".into(),
        destination: "target".into(),
        start: false,
        idempotency_key: "add".into(),
        deadline_unix_ms: now_ms() + 60_000,
    });
    let id = response.result.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Failed);
    assert_eq!(f.record(&id).error, Some(ErrorCode::PathDenied));
    assert_eq!(f.control.torrents.len(), 1);
}
#[test]
fn export_approval_escapes_untrusted_destination_control_characters() {
    let mut f = Fixture::manager();
    let response = f.call(Method::Export {
        torrent_id: "v1: abc".into(),
        destination: "line\nINSTRUCTION".into(),
        idempotency_key: "export".into(),
        deadline_unix_ms: now_ms() + 60_000,
    });
    let id = response.result.unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let description = f
        .control
        .agent
        .as_ref()
        .unwrap()
        .approval_description(&id)
        .unwrap();
    assert!(description.contains("\\n"));
    assert!(!description.contains('\n'));
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Queued);
}
#[test]
fn terminal_operation_save_failure_is_retried_without_reenqueuing() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::Pause);
    f.control.agent_access().unwrap().store.connection.execute_batch("CREATE TEMP TRIGGER fail_save BEFORE INSERT ON agent_operations BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    f.emit(SessionEvent::StateChanged {
        id: "v1: abc".into(),
        state: "paused",
    });
    assert!(f.control.agent.as_ref().unwrap().pending.contains_key(&id));
    f.control.tick();
    assert!(f.control.agent.as_ref().unwrap().pending.contains_key(&id));
    f.control
        .agent_access()
        .unwrap()
        .store
        .connection
        .execute_batch("DROP TRIGGER fail_save;")
        .unwrap();
    f.control.tick();
    assert_eq!(f.record(&id).state, OperationState::Succeeded);
    assert!(f.control.agent.as_ref().unwrap().pending.is_empty());
}
#[test]
fn missing_running_boundary_write_does_not_enqueue_a_command() {
    let mut f = Fixture::manager();
    let id = f.accept(TorrentCommand::Pause);
    f.control.agent_access().unwrap().store.connection.execute_batch("CREATE TEMP TRIGGER fail_save BEFORE INSERT ON agent_operations BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    f.control.tick();
    assert!(!f.control.agent.as_ref().unwrap().pending[&id].dispatched);
    assert_eq!(f.record(&id).state, OperationState::Queued);
}
#[test]
fn repair_status_projection_covers_every_runtime_state() {
    use crate::runtime_status::RepairStatus;
    let mut f = Fixture::manager();
    for (status, label) in [
        (RepairStatus::Idle, "idle"),
        (
            RepairStatus::Downloading {
                attempt: 1,
                limit: 2,
            },
            "downloading",
        ),
        (
            RepairStatus::Checking {
                attempt: 1,
                limit: 2,
            },
            "checking",
        ),
        (RepairStatus::Succeeded { attempts: 1 }, "succeeded"),
        (
            RepairStatus::Exhausted {
                attempts: 2,
                limit: 2,
                message: "SECRET".into(),
            },
            "exhausted",
        ),
    ] {
        f.control.torrents[0].runtime.repair = status;
        let value = f
            .call(Method::Details {
                torrent_id: "v1: abc".into(),
                offset: 0,
                limit: 1,
            })
            .result
            .unwrap();
        assert_eq!(value["repair_state"], label);
        assert!(!value.to_string().contains("SECRET"));
    }
}

#[test]
fn unrelated_enabled_verification_ack_does_not_confirm_a_disable_request() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetVerifyAfterDownload(false));
    f.emit(SessionEvent::VerifyAfterDownload {
        id: "v1: abc".into(),
        enabled: true,
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}

#[test]
fn explicit_mode_failure_completes_the_matching_operation_without_raw_text() {
    let mut f = Fixture::manager();
    let config = crate::runtime_status::ModeConfig::default();
    let id = f.running(TorrentCommand::SetMode(config));
    f.emit(SessionEvent::Mode {
        id: "v1: abc".into(),
        application: crate::runtime_status::ModeApplication::Failed {
            requested: config,
            active: None,
            message: "SECRET".into(),
        },
    });
    let operation = f.record(&id);
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(operation.error, Some(ErrorCode::EngineRejected));
    assert!(!operation_json(&operation).to_string().contains("SECRET"));
}

#[test]
fn mode_failure_for_a_different_requested_policy_does_not_complete_the_operation() {
    let mut f = Fixture::manager();
    let id = f.running(TorrentCommand::SetMode(Default::default()));
    let other = crate::runtime_status::ModeConfig {
        mode: crate::download_mode::DownloadMode::HideDownloaded,
        ..Default::default()
    };
    f.emit(SessionEvent::Mode {
        id: "v1: abc".into(),
        application: crate::runtime_status::ModeApplication::Failed {
            requested: other,
            active: None,
            message: "failure for a different policy".into(),
        },
    });
    assert_eq!(f.record(&id).state, OperationState::Running);
}
