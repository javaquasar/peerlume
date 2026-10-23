//! Local-only integration fixture shared by controller and frontend tests.
use super::*;
use crate::{
    client::{add_torrent_command, torrent_to_record},
    completion::CompletionStatus,
    session::SessionRuntime,
    settings::ClientSettings,
    state::StateStore,
    torrent::TorrentState,
};
use sha1::{Digest, Sha1};
use std::time::{Duration, Instant};

pub(crate) struct LiveFixture {
    // Drop the runtime/files before removing the temporary directory on Windows.
    pub controller: Controller,
    pub root: tempfile::TempDir,
    pub id: String,
    pub settings_applied: u64,
}

impl LiveFixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let payload = b"local command fixture";
        let mut info = format!(
            "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces20:",
            payload.len()
        )
        .into_bytes();
        info.extend_from_slice(&Sha1::digest(payload));
        info.extend_from_slice(b"7:privatei1ee");
        let id = format!("v1: {:x}", Sha1::digest(&info));
        let mut metainfo = b"d4:info".to_vec();
        metainfo.extend_from_slice(&info);
        metainfo.push(b'e');
        let source = root.path().join("fixture.torrent");
        std::fs::write(&source, metainfo).unwrap();
        std::fs::write(root.path().join("payload.bin"), payload).unwrap();
        let runtime = SessionRuntime::start_with_options(
            64,
            root.path().into(),
            librqbit::SessionOptions {
                dht: None,
                listen: None,
                disable_trackers: true,
                disable_local_service_discovery: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut item = crate::client::tests::torrent();
        item.info_hashes = vec![id.clone()];
        item.source = source.display().to_string();
        item.files = crate::file_selection::FileSelectionModel::from_files([(
            "payload.bin".into(),
            payload.len() as u64,
        )]);
        item.overrides.download_directory = Some(root.path().display().to_string());
        item.state = TorrentState::Paused;
        let store = StateStore::open(&root.path().join("test-state.sqlite3")).unwrap();
        let mut backend = ClientBackend::with_session_and_store(runtime, store);
        backend
            .save_torrent(&torrent_to_record(&item, None))
            .unwrap();
        backend
            .send(add_torrent_command(&item, &ClientSettings::default()))
            .unwrap();
        let mut fixture = Self {
            controller: Controller::new(backend, vec![item]),
            root,
            id,
            settings_applied: 0,
        };
        fixture.wait_for(|fixture| {
            fixture.controller.torrents[0].completion == CompletionStatus::Verified
                && fixture.controller.torrents[0].state == TorrentState::Paused
        });
        fixture
    }

    pub fn poll(&mut self) {
        while let Some(notice) = self.controller.poll_next() {
            match notice {
                SessionNotice::SettingsApplied { id } => {
                    self.controller
                        .persist_confirmed_settings(&id, None)
                        .unwrap();
                    self.settings_applied += 1;
                }
                SessionNotice::Error { .. } => panic!("{notice:?}"),
                _ => {}
            }
        }
        self.controller.publish();
    }

    pub fn wait_for(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.poll();
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for fixture state"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
