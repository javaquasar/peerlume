use super::*;

fn target(byte: char) -> String {
    format!("v1: {}", byte.to_string().repeat(40))
}

fn begin(history: &mut History, action: Action) -> (Ticket, String) {
    history
        .begin(
            &"a".repeat(64),
            &"b".repeat(64),
            Intent::from_action(&action).unwrap(),
        )
        .unwrap()
}

fn mutate(id: &str, command: TorrentCommand) -> Action {
    Action::Mutate {
        id: id.into(),
        command,
        revision: Some(4),
    }
}

fn entry(history: &mut History, id: &str) -> NativeOperation {
    history
        .page(None, MAX_PAGE)
        .unwrap()
        .entries
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
}

#[test]
fn native_history_keeps_targets_independent_and_requires_actual_durable_ack() {
    let shared = History::memory();
    let mut history = shared.lock().unwrap();
    let first = target('1');
    let second = target('2');
    let (copy, copy_id) = begin(
        &mut history,
        Action::Storage {
            id: first.clone(),
            action: StorageAction::Select {
                directory: "secret filesystem location".into(),
                copy: true,
            },
            revision: 4,
        },
    );
    let (pause, pause_id) = begin(&mut history, mutate(&second, TorrentCommand::Pause));
    history.accepted(copy, &first, 5, true);
    history.accepted(pause, &second, 6, true);
    history.finish_rpc(pause, &Ok(Reply::Done("Owner accepted".into())));
    history.observe(&first, 5, Feedback::Verified);
    assert_eq!(entry(&mut history, &copy_id).state, State::Accepted);
    history.observe(&first, 5, Feedback::Copying(128));
    history.observe(&first, 5, Feedback::Copying(256));
    let progress = entry(&mut history, &copy_id);
    assert_eq!(progress.progress.unwrap().bytes, 128);
    assert!(!serde_json::to_string(&progress).unwrap().contains("secret"));
    history.observe(&second, 6, Feedback::Paused);
    assert_eq!(entry(&mut history, &pause_id).state, State::EffectConfirmed);
    history.persisted(&second, 6, false);
    assert_eq!(entry(&mut history, &pause_id).state, State::Succeeded);
    history.observe(&first, 5, Feedback::CheckingDestination);
    history.persisted(&first, 5, false);
    assert_eq!(entry(&mut history, &copy_id).state, State::Running);
    history.observe(&first, 5, Feedback::Verified);
    assert_eq!(entry(&mut history, &copy_id).state, State::Succeeded);

    let (remove, remove_id) = begin(&mut history, mutate(&first, TorrentCommand::Remove));
    history.accepted(remove, &first, 7, true);
    history.observe(&first, 7, Feedback::Removed);
    history.persisted(&first, 7, false);
    assert_eq!(
        entry(&mut history, &remove_id).state,
        State::EffectConfirmed
    );
    history.persisted(&first, 7, true);
    assert_eq!(entry(&mut history, &remove_id).state, State::Succeeded);
}

#[test]
fn native_history_does_not_assign_delayed_events_after_supersession() {
    let shared = History::memory();
    let mut history = shared.lock().unwrap();
    let id = target('a');
    let (first, first_id) = begin(&mut history, mutate(&id, TorrentCommand::Pause));
    history.accepted(first, &id, 5, true);
    let (second, second_id) = begin(&mut history, mutate(&id, TorrentCommand::Resume));
    history.accepted(second, &id, 6, true);
    history.observe(&id, 6, Feedback::Resumed);
    history.persisted(&id, 6, false);
    assert_eq!(entry(&mut history, &first_id).state, State::NeedsReview);
    assert_eq!(entry(&mut history, &second_id).state, State::NeedsReview);
    assert_eq!(entry(&mut history, &second_id).revision, Some(6));
}

#[test]
fn native_history_requires_manual_restart_before_recording_recheck_success() {
    let shared = History::memory();
    let mut history = shared.lock().unwrap();
    let id = target('d');
    let (ticket, operation) = begin(&mut history, mutate(&id, TorrentCommand::Recheck));
    history.accepted(ticket, &id, 6, true);
    let checking = Feedback::Checking(NativeOperationProgress {
        bytes: 2,
        total: Some(4),
    });
    history.observe(&id, 6, checking);
    history.observe(&id, 6, Feedback::Verified);
    assert_eq!(entry(&mut history, &operation).state, State::Running);
    history.observe(&id, 6, Feedback::Added);
    assert_eq!(entry(&mut history, &operation).state, State::Running);
    history.observe(&id, 6, Feedback::Verified);
    assert_eq!(entry(&mut history, &operation).state, State::Succeeded);
}

#[test]
fn native_history_reopens_pending_records_as_review_and_keeps_stable_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-operations.sqlite3");
    let mut history = History::open(&path).unwrap();
    let id = target('c');
    let (pause, pause_id) = begin(&mut history, mutate(&id, TorrentCommand::Pause));
    history.accepted(pause, &id, 5, true);
    let (save, save_id) = begin(&mut history, Action::SaveView(Default::default()));
    history.finish_rpc(save, &Ok(Reply::Done("View saved".into())));
    let before = history.page(None, 1).unwrap().next_before;
    drop(history);
    let mut history = History::open(&path).unwrap();
    let (_, newer) = begin(&mut history, Action::SaveView(Default::default()));
    assert!(entry(&mut history, &newer).sequence > entry(&mut history, &save_id).sequence);
    let page = history.page(before, 1).unwrap();
    assert_eq!(page.entries[0].id, pause_id);
    assert_eq!(page.entries[0].state, State::NeedsReview);
    assert_eq!(page.entries[0].message.as_deref(), Some(REPLACED));
    assert_eq!(entry(&mut history, &save_id).state, State::Succeeded);
    assert!(matches!(
        history.page(None, MAX_PAGE + 1),
        Err(Error::TooLarge)
    ));
}

#[test]
fn native_history_evicts_terminal_records_before_active_work() {
    let shared = History::memory();
    let mut history = shared.lock().unwrap();
    let (_, retained) = begin(&mut history, mutate(&target('e'), TorrentCommand::Pause));
    let (completed, _) = begin(&mut history, Action::SaveView(Default::default()));
    history.finish_rpc(completed, &Ok(Reply::Done(String::new())));
    for _ in 2..CAPACITY {
        begin(&mut history, Action::SaveView(Default::default()));
    }
    begin(&mut history, Action::SaveView(Default::default()));
    let intent = Intent::from_action(&Action::SaveView(Default::default())).unwrap();
    assert!(matches!(
        history.begin("owner", "request", intent),
        Err(Error::Busy)
    ));
    let (state, _) = history
        .update(
            Ticket {
                sequence: 1,
                effect: Effect::Pause,
            },
            |_| {},
        )
        .unwrap();
    assert_eq!(state, State::Queued);
    assert_eq!(history.page(Some(2), 1).unwrap().entries[0].id, retained);
}

#[test]
fn native_history_rejects_nonregular_database_and_drops_untrusted_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-operations.sqlite3");
    std::fs::create_dir(&path).unwrap();
    assert!(History::open(&path).is_err());
    let shared = History::memory();
    let mut history = shared.lock().unwrap();
    let intent = Intent::from_action(&mutate("secret target", TorrentCommand::Pause)).unwrap();
    let (ticket, id) = history.begin("owner", "secret request", intent).unwrap();
    history.finish_rpc(ticket, &Err(Error::Failed("secret failure".into())));
    let row = entry(&mut history, &id);
    assert_eq!(row.state, State::NeedsReview);
    assert!(!serde_json::to_string(&row).unwrap().contains("secret"));
}
