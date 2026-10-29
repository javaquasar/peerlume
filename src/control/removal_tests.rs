//! Removal must work for saved rows even when engine registration failed.
use super::{test_support::LiveFixture, *};
use crate::{
    client::{add_torrent_command, torrent_to_record},
    file_selection::FileSelectionModel,
    session::SessionCommand,
    settings::ClientSettings,
    torrent::TorrentState,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn wait_notice(fixture: &mut LiveFixture, matches: impl Fn(&SessionNotice) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while let Some(notice) = fixture.controller.poll_next() {
            if matches(&notice) {
                fixture.controller.publish();
                return;
            }
            assert!(!matches!(notice, SessionNotice::Error { .. }), "{notice:?}");
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for engine acknowledgement"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn remove_failed_registration_case(source_missing: bool) {
    let mut fixture = LiveFixture::new();
    fixture
        .controller
        .submit(&fixture.id.clone(), TorrentCommand::Resume, None)
        .unwrap();
    fixture.wait_for(|fixture| fixture.controller.torrents[0].state == TorrentState::Seeding);
    let engine = fixture.controller.backend.test_engine().unwrap();
    let neighbor = engine.with_torrents(|torrents| torrents.next().unwrap().1.clone());
    let neighbor_payload = std::fs::read(fixture.root.path().join("payload.bin")).unwrap();
    let source = fixture.root.path().join("invalid.torrent");
    let payload = fixture.root.path().join("unmanaged.bin");
    if !source_missing {
        std::fs::write(&source, b"invalid synthetic metainfo").unwrap();
    }
    std::fs::write(&payload, b"keep this partial download").unwrap();
    let mut failed = fixture.controller.torrents[0].clone();
    failed.info_hashes = vec![format!("v1: {}", "42".repeat(20))];
    let failed_id = failed.info_hashes.join(";");
    failed.source = source.to_string_lossy().into_owned();
    failed.files = FileSelectionModel::from_files([("unmanaged.bin".into(), 26)]);
    fixture
        .controller
        .backend
        .save_torrent(&torrent_to_record(&failed, None))
        .unwrap();
    fixture
        .controller
        .backend
        .send(add_torrent_command(&failed, &ClientSettings::default()))
        .unwrap();
    fixture.controller.torrents.push(failed);
    fixture.controller.publish();
    wait_notice(
        &mut fixture,
        |notice| matches!(notice, SessionNotice::Error { id, .. } if id == &failed_id),
    );
    assert_eq!(fixture.controller.torrents[1].state, TorrentState::Error);
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 1);

    fixture.controller.remove(&failed_id).unwrap();
    assert_eq!(
        fixture.controller.torrents.len(),
        2,
        "Removal still requires engine acknowledgement"
    );
    wait_notice(
        &mut fixture,
        |notice| matches!(notice, SessionNotice::Removed { id } if id == &failed_id),
    );
    assert_eq!(fixture.controller.torrents.len(), 1);
    assert_eq!(fixture.controller.backend.load_torrents().unwrap().len(), 1);
    assert_eq!(
        fixture.controller.torrents[0].info_hashes.join(";"),
        fixture.id
    );
    assert_eq!(fixture.controller.torrents[0].state, TorrentState::Seeding);
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 1);
    assert!(engine.with_torrents(|torrents| Arc::ptr_eq(&neighbor, torrents.next().unwrap().1)));
    assert!(Arc::ptr_eq(
        &engine,
        &fixture.controller.backend.test_engine().unwrap()
    ));
    assert_eq!(
        std::fs::read(&payload).unwrap(),
        b"keep this partial download"
    );
    if source_missing {
        assert!(!source.exists());
    } else {
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"invalid synthetic metainfo"
        );
    }
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        neighbor_payload
    );

    // A repeated actor command must acknowledge absence, not resurrect the row
    // or stop another registered torrent.
    fixture
        .controller
        .backend
        .send(SessionCommand::Remove {
            id: failed_id.clone(),
        })
        .unwrap();
    wait_notice(
        &mut fixture,
        |notice| matches!(notice, SessionNotice::Removed { id } if id == &failed_id),
    );
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 1);
    assert_eq!(fixture.controller.backend.load_torrents().unwrap().len(), 1);
}

#[test]
fn remove_failed_registration_keeps_payload_and_neighbor_and_stays_removed_on_disk() {
    remove_failed_registration_case(false);
}

#[test]
fn remove_missing_source_keeps_payload_and_neighbor_and_stays_removed_on_disk() {
    remove_failed_registration_case(true);
}

fn readd_using_engine(
    fixture: &LiveFixture,
    engine: &Arc<librqbit::Session>,
    executor: &tokio::runtime::Runtime,
) -> Arc<librqbit::ManagedTorrent> {
    executor
        .block_on(engine.add_torrent(
            librqbit::AddTorrent::from_bytes(
                std::fs::read(fixture.root.path().join("fixture.torrent")).unwrap(),
            ),
            Some(librqbit::AddTorrentOptions {
                paused: true,
                overwrite: true,
                output_folder: Some(fixture.root.path().to_string_lossy().into_owned()),
                ..Default::default()
            }),
        ))
        .unwrap()
        .into_handle()
        .unwrap()
}

fn remove_after_engine_handle_change(replace: bool) {
    let mut fixture = LiveFixture::new();
    let engine = fixture.controller.backend.test_engine().unwrap();
    let original = engine.with_torrents(|torrents| torrents.next().unwrap().1.clone());
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    executor
        .block_on(engine.delete(original.id().into(), false))
        .unwrap();
    if replace {
        let current = readd_using_engine(&fixture, &engine, &executor);
        assert_ne!(current.id(), original.id());
    }
    fixture.controller.remove(&fixture.id.clone()).unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Removed { .. })
    });
    assert!(fixture.controller.torrents.is_empty());
    assert!(
        fixture
            .controller
            .backend
            .load_torrents()
            .unwrap()
            .is_empty()
    );
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 0);
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        b"local command fixture"
    );
    assert!(fixture.root.path().join("fixture.torrent").is_file());
}

#[test]
fn remove_stale_handle_after_failed_recheck_acknowledges_absence() {
    remove_after_engine_handle_change(false);
}

#[test]
fn remove_stale_handle_stops_the_current_generation_by_hash() {
    remove_after_engine_handle_change(true);
}

#[test]
fn remove_unmapped_registration_stops_it_before_forgetting_the_saved_row() {
    let mut fixture = LiveFixture::new();
    let engine = fixture.controller.backend.test_engine().unwrap();
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
    assert_eq!(
        fixture.controller.torrents.len(),
        1,
        "Raw actor removal alone must not forget the saved row"
    );
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    readd_using_engine(&fixture, &engine, &executor);
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 1);
    fixture.controller.remove(&fixture.id.clone()).unwrap();
    wait_notice(&mut fixture, |notice| {
        matches!(notice, SessionNotice::Removed { .. })
    });
    assert_eq!(engine.with_torrents(|torrents| torrents.count()), 0);
    assert!(fixture.controller.torrents.is_empty());
    assert!(
        fixture
            .controller
            .backend
            .load_torrents()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("payload.bin")).unwrap(),
        b"local command fixture"
    );
    assert!(fixture.root.path().join("fixture.torrent").is_file());
}
