//! Real engine retries use the same identity, keep data, and persist only an ACK.
use super::{test_support::LiveFixture, *};
use crate::{
    client::{add_torrent_command, torrent_to_record},
    runtime_status::{TransferAction, TransferFeedback},
    session::SessionCommand,
    settings::ClientSettings,
    torrent::TorrentState,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn wait_notice(fixture: &mut LiveFixture, predicate: impl Fn(&SessionNotice) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while let Some(notice) = fixture.controller.poll_next() {
            if predicate(&notice) {
                fixture.controller.publish();
                return;
            }
            assert!(!matches!(notice, SessionNotice::Error { .. }), "{notice:?}");
        }
        assert!(
            Instant::now() < deadline,
            "No expected engine acknowledgement"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn failed_registration() -> LiveFixture {
    let mut fixture = LiveFixture::new();
    fixture
        .controller
        .backend
        .send(SessionCommand::Remove {
            id: fixture.id.clone(),
        })
        .unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Removed { .. })
    });
    let item = &mut fixture.controller.torrents[0];
    item.runtime = Default::default();
    item.source = fixture
        .root
        .path()
        .join("unavailable.torrent")
        .to_string_lossy()
        .into_owned();
    let record = torrent_to_record(item, None);
    let command = add_torrent_command(item, &ClientSettings::default());
    fixture.controller.backend.save_torrent(&record).unwrap();
    fixture.controller.backend.send(command).unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Error { .. })
    });
    assert_eq!(fixture.controller.torrents[0].state, TorrentState::Error);
    assert!(!fixture.controller.torrents[0].runtime.registered);
    assert!(
        fixture.controller.torrents[0]
            .runtime
            .last_error
            .as_deref()
            .unwrap()
            .contains("file was not found")
    );
    fixture
}

#[test]
fn retry_start_uses_the_same_engine_and_persists_the_acknowledged_start() {
    let mut fixture = failed_registration();
    let engine = fixture.controller.backend.test_engine().unwrap();
    let metadata = std::fs::read(fixture.root.path().join("fixture.torrent")).unwrap();
    std::fs::write(fixture.root.path().join("unavailable.torrent"), &metadata).unwrap();
    let before = fixture.controller.backend.load_torrents().unwrap()[0]
        .state
        .clone();
    fixture
        .controller
        .retry_start(&fixture.id.clone(), &ClientSettings::default())
        .unwrap();
    assert_eq!(
        fixture.controller.torrents[0].state,
        TorrentState::Error,
        "Queue acceptance is not a running-state ACK"
    );
    assert_eq!(
        fixture.controller.backend.load_torrents().unwrap()[0].state,
        before
    );
    assert_eq!(
        fixture.controller.torrents[0]
            .runtime
            .command
            .pending_action(),
        Some(TransferAction::Start)
    );
    assert!(
        fixture
            .controller
            .retry_start(&fixture.id.clone(), &ClientSettings::default())
            .is_err()
    );
    fixture.wait_for(|f| f.controller.torrents[0].state == TorrentState::Seeding);
    assert!(fixture.controller.torrents[0].runtime.registered);
    assert!(fixture.controller.torrents[0].runtime.last_error.is_none());
    assert!(matches!(
        fixture.controller.torrents[0].runtime.command,
        TransferFeedback::Confirmed {
            action: TransferAction::Start
        }
    ));
    assert_ne!(
        fixture.controller.backend.load_torrents().unwrap()[0]
            .state
            .to_ascii_lowercase(),
        "error"
    );
    assert!(Arc::ptr_eq(
        &engine,
        &fixture.controller.backend.test_engine().unwrap()
    ));
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 1);
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        b"local command fixture"
    );
    drop(engine);
    let restarted = fixture.restart();
    assert_eq!(
        restarted.controller.torrents[0].state,
        TorrentState::Seeding,
        "Confirmed start was not restored"
    );
}

#[test]
fn failed_retry_stops_waiting_and_keeps_the_original_files_and_saved_state() {
    let mut fixture = failed_registration();
    let before = fixture.controller.backend.load_torrents().unwrap()[0]
        .state
        .clone();
    fixture
        .controller
        .retry_start(&fixture.id.clone(), &ClientSettings::default())
        .unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Error { .. })
    });
    assert_eq!(fixture.controller.torrents[0].state, TorrentState::Error);
    assert!(
        matches!(&fixture.controller.torrents[0].runtime.command, TransferFeedback::Failed { action: TransferAction::Start, message } if message.contains("file was not found"))
    );
    assert_eq!(
        fixture.controller.backend.load_torrents().unwrap()[0].state,
        before
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        b"local command fixture"
    );
}

#[test]
fn retry_rejects_a_changed_source_identity_without_touching_payload() {
    let mut fixture = failed_registration();
    let mut metadata = std::fs::read(fixture.root.path().join("fixture.torrent")).unwrap();
    let name = metadata
        .windows(b"payload.bin".len())
        .position(|value| value == b"payload.bin")
        .unwrap();
    metadata[name] = b'X';
    std::fs::write(fixture.root.path().join("unavailable.torrent"), metadata).unwrap();
    fixture
        .controller
        .retry_start(&fixture.id.clone(), &ClientSettings::default())
        .unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Error { .. })
    });
    assert!(
        fixture.controller.torrents[0]
            .runtime
            .last_error
            .as_deref()
            .unwrap()
            .contains("different torrent")
    );
    assert_eq!(
        fixture
            .controller
            .backend
            .test_engine()
            .unwrap()
            .with_torrents(|torrents| torrents.count()),
        0
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        b"local command fixture"
    );
    assert!(!fixture.root.path().join("Xayload.bin").exists());
}
