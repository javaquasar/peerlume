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
    pub fn restart(self) -> Self {
        let Self {
            controller,
            root,
            id,
            ..
        } = self;
        drop(controller);
        let store = StateStore::open(&root.path().join("test-state.sqlite3")).unwrap();
        let record = store
            .load_all_torrents()
            .unwrap()
            .into_iter()
            .find(|record| record.id == id)
            .unwrap();
        let settings = ClientSettings::default();
        let item = crate::client::torrent_from_record(record, &settings);
        let paused = item.state == TorrentState::Paused;
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
        let backend = ClientBackend::with_session_and_store(runtime, store);
        backend.send(add_torrent_command(&item, &settings)).unwrap();
        let mut fixture = Self {
            controller: Controller::new(backend, vec![item]),
            root,
            id,
            settings_applied: 0,
        };
        fixture.wait_for(|fixture| {
            fixture.controller.torrents[0].completion == CompletionStatus::Verified
                && fixture.controller.torrents[0].state
                    == if paused {
                        TorrentState::Paused
                    } else {
                        TorrentState::Seeding
                    }
        });
        fixture
    }

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
                    if self.controller.has_confirmed_update(&id) {
                        self.controller.persist_confirmed_update(&id, None).unwrap();
                    }
                    self.settings_applied += 1;
                }
                SessionNotice::StateApplied { id } => {
                    if self.controller.has_confirmed_update(&id) {
                        self.controller.persist_confirmed_update(&id, None).unwrap();
                    }
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

#[cfg(any(feature = "gui", feature = "tui"))]
pub(crate) fn installed_frontend_workflow(
    mut dispatch: impl FnMut(&mut Controller, TorrentCommand),
    mut render: impl FnMut(&mut Controller),
) {
    use std::{
        io::Write,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
    };
    let binary = std::env::var_os("PEERLUME_CONTROL_BIN")
        .expect("Set PEERLUME_CONTROL_BIN to the built adapter");
    let mut fixture = LiveFixture::new();
    let id = fixture.id.clone();
    let profile = fixture.root.path().to_path_buf();
    let copied = profile.join(if cfg!(windows) {
        "installed-control.exe"
    } else {
        "installed-control"
    });
    std::fs::copy(binary, &copied).unwrap();
    fixture
        .controller
        .configure_agents(&profile, profile.clone())
        .unwrap();
    let agent = fixture.controller.agent_access().unwrap();
    agent.enabled = true;
    let token = agent
        .grant(access::Grants {
            manage: true,
            reporting: true,
            ..Default::default()
        })
        .unwrap();
    let instance = agent.instance_id.clone();
    let owner = Arc::new(Mutex::new(fixture));
    let lock = host::InstanceLock::acquire(&profile).unwrap();
    let host = host::Host::start(owner.clone(), |f| &mut f.controller, profile.clone()).unwrap();
    let cli = |method: wire::Method| -> wire::Response {
        let mut child = Command::new(&copied)
            .arg("request")
            .arg("--profile")
            .arg(&profile)
            .env("PEERLUME_CONTROL_TOKEN", &token)
            .current_dir(&profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.take().unwrap(),
            "{}",
            serde_json::to_string(&adapters::Input {
                instance_id: Some(instance.clone()),
                method
            })
            .unwrap()
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let ready = Instant::now();
    while cli(wire::Method::Discover).error.is_some() {
        assert!(ready.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    for (command, state, persisted) in [
        (TorrentCommand::Resume, TorrentState::Seeding, "downloading"),
        (TorrentCommand::Pause, TorrentState::Paused, "paused"),
    ] {
        dispatch(&mut owner.lock().unwrap().controller, command);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let mut f = owner.lock().unwrap();
            render(&mut f.controller);
            if f.controller.torrents[0].state == state {
                assert_eq!(
                    f.controller.backend.load_torrents().unwrap()[0].state,
                    persisted
                );
                break;
            }
            drop(f);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    for (index, command) in [
        TorrentCommand::SetMode(crate::runtime_status::ModeConfig {
            mode: crate::download_mode::DownloadMode::HideDownloaded,
            ..Default::default()
        }),
        TorrentCommand::Recheck,
    ]
    .into_iter()
    .enumerate()
    {
        let revision = owner
            .lock()
            .unwrap()
            .controller
            .command_revision(&id)
            .unwrap();
        let response = cli(wire::Method::Mutate {
            torrent_id: id.clone(),
            expected_revision: revision,
            command,
            idempotency_key: format!("frontend-{index}"),
            deadline_unix_ms: operations::now_ms() + 15000,
        });
        assert!(response.error.is_none(), "{:?}", response.error);
        let operation = response.result.unwrap()["operation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            render(&mut owner.lock().unwrap().controller);
            let reply = cli(wire::Method::Operation {
                operation_id: operation.clone(),
            });
            assert!(reply.error.is_none());
            let value = reply.result.unwrap();
            if value["state"] == "succeeded" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "operation did not succeed: {value}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    assert!(host.error.lock().unwrap().is_none());
    drop(host);
    drop(lock);
    let fixture = Arc::try_unwrap(owner)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .restart();
    assert_eq!(
        fixture.controller.torrents[0].overrides.download_mode,
        crate::download_mode::DownloadMode::HideDownloaded
    );
    assert!(fixture.controller.torrents[0].completed_successfully());
}
