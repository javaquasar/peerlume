//! Post-engine-acknowledgement recycling flow with a disposable filesystem adapter.
//! No operating-system Recycle Bin, application session or user profile is opened.
use super::*;
use crate::state::StateStore;

struct Fixture {
    _root: tempfile::TempDir,
    app: PeerLumeApp,
    database: PathBuf,
    target: String,
    neighbor: String,
    source: PathBuf,
    payloads: Vec<PathBuf>,
    neighbor_payload: PathBuf,
    recycled: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let recycled = root.path().join("fake-recycle-bin");
        fs::create_dir_all(data.join("nested")).unwrap();
        fs::create_dir_all(&recycled).unwrap();
        let source = root.path().join("target.torrent");
        let neighbor_source = root.path().join("neighbor.torrent");
        fs::write(&source, b"synthetic source metadata").unwrap();
        fs::write(&neighbor_source, b"neighbor metadata").unwrap();
        let payloads = vec![data.join("a.bin"), data.join("nested/b.bin")];
        fs::write(&payloads[0], b"DATA").unwrap();
        fs::write(&payloads[1], b"MORE").unwrap();
        let neighbor_payload = data.join("neighbor.bin");
        fs::write(&neighbor_payload, b"KEEP").unwrap();
        let mut target = crate::client::tests::torrent();
        target.name = "Display name is not a payload directory".into();
        target.source = source.display().to_string();
        target.info_hashes = vec![format!("v1: {}", "11".repeat(20))];
        target.files = FileSelectionModel::from_snapshot([
            ("a.bin".into(), 4, 4, true),
            ("nested\\b.bin".into(), 4, 4, true),
        ]);
        target.downloaded_bytes = 8;
        target.uploaded_bytes = 123;
        target.download_rate = 100;
        target.upload_rate = 25;
        target.peers = 3;
        target.state = TorrentState::Downloading;
        target.runtime.registered = true;
        target.runtime.network_downloaded = 8;
        target.runtime.network_uploaded = 123;
        target.overrides.download_directory = Some(data.display().to_string());
        target.trackers.clear();
        let mut neighbor = target.clone();
        neighbor.name = "Neighbor".into();
        neighbor.source = neighbor_source.display().to_string();
        neighbor.info_hashes = vec![format!("v1: {}", "22".repeat(20))];
        neighbor.files = FileSelectionModel::from_snapshot([("neighbor.bin".into(), 4, 4, true)]);
        neighbor.downloaded_bytes = 4;
        let target_id = target.info_hashes.join(";");
        let neighbor_id = neighbor.info_hashes.join(";");
        let mut tracker_audits = HashMap::new();
        for torrent in [&target, &neighbor] {
            tracker_audits.insert(
                torrent.info_hashes.join(";"),
                TorrentTrackerAudit::new(String::new(), torrent.selected_size(), 6881),
            );
        }
        let database = root.path().join("state.sqlite3");
        let mut store = StateStore::open(&database).unwrap();
        for torrent in [&target, &neighbor] {
            store
                .save_torrent(&torrent_record(
                    torrent,
                    tracker_audits.get(&torrent.info_hashes.join(";")),
                ))
                .unwrap();
        }
        let settings = ClientSettings::default();
        let app = PeerLumeApp {
            log_offset: 0,
            log_torrent: String::new(),
            page: MainPage::Torrents,
            details_tab: DetailsTab::Files,
            preferences_tab: Default::default(),
            selected_torrent: 1,
            status_filter: Default::default(),
            name_column: Default::default(),
            view_cache: Default::default(),
            metainfo_loader: Default::default(),
            settings: settings.clone(),
            saved_settings: settings.clone(),
            settings_draft: settings.clone(),
            settings_status: String::new(),
            validation_messages: vec![],
            engine: EngineRuntime::new(settings),
            blocklist: Default::default(),
            blocklist_status: String::new(),
            blocklist_update: None,
            blocklist_probe: String::new(),
            blocklist_probe_result: String::new(),
            add_dialog: None,
            last_download_directory: None,
            pending_torrent_paths: Default::default(),
            add_status: String::new(),
            file_navigation: Default::default(),
            magnet_input: String::new(),
            control: Controller::new(
                ClientBackend::with_state_store(store),
                vec![target, neighbor],
            ),
            tracker_audits,
            behavior_catalog: Err("Not used by recycling tests".into()),
            trash_confirmation: None,
            pending_trash: None,
        };
        Self {
            _root: root,
            app,
            database,
            target: target_id,
            neighbor: neighbor_id,
            source,
            payloads,
            neighbor_payload,
            recycled,
        }
    }

    fn confirmed(&mut self, scope: TrashScope) {
        self.app.prepare_trash_request(&self.target, scope);
        self.app.pending_trash = Some(
            self.app
                .trash_confirmation
                .take()
                .expect("explicit review exists"),
        );
    }

    fn assert_neighbor(&self) {
        assert!(
            self.app
                .control
                .torrents
                .iter()
                .any(|torrent| torrent.info_hashes.join(";") == self.neighbor)
        );
        assert!(self.app.tracker_audits.contains_key(&self.neighbor));
        assert_eq!(fs::read(&self.neighbor_payload).unwrap(), b"KEEP");
    }

    fn assert_retained_error(&self, token: &str) {
        let torrent = self
            .app
            .control
            .torrents
            .iter()
            .find(|torrent| torrent.info_hashes.join(";") == self.target)
            .unwrap();
        assert_eq!(torrent.state, TorrentState::Error);
        assert!(!torrent.runtime.registered);
        assert_eq!((torrent.download_rate, torrent.upload_rate), (0, 0));
        assert_eq!((torrent.downloaded_bytes, torrent.uploaded_bytes), (8, 123));
        assert_eq!(
            (
                torrent.runtime.network_downloaded,
                torrent.runtime.network_uploaded
            ),
            (8, 123)
        );
        assert!(
            torrent
                .runtime
                .last_error
                .as_ref()
                .is_some_and(|message| message.contains(token))
        );
        assert!(
            self.app.add_status.contains(token),
            "{}",
            self.app.add_status
        );
        assert!(self.app.tracker_audits.contains_key(&self.target));
        assert!(
            self.app
                .control
                .backend
                .load_torrents()
                .unwrap()
                .iter()
                .any(|record| record.id == self.target)
        );
        self.assert_neighbor();
    }
}

fn fake_recycle(paths: &[PathBuf], destination: &Path) -> Result<(), String> {
    for (index, path) in paths.iter().enumerate() {
        fs::rename(
            path,
            destination.join(format!(
                "{index}-{}",
                path.file_name().unwrap().to_string_lossy()
            )),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[test]
fn every_recycle_scope_removes_the_target_row_after_success_and_preserves_the_neighbor() {
    for scope in [
        TrashScope::DownloadedFiles,
        TrashScope::TorrentFile,
        TrashScope::Everything,
    ] {
        let mut fixture = Fixture::new();
        fixture.confirmed(scope);
        let paths = &fixture.app.pending_trash.as_ref().unwrap().paths;
        assert_eq!(
            paths.len(),
            if scope.includes_data() { 2 } else { 0 } + usize::from(scope.includes_source())
        );
        assert_eq!(paths.contains(&fixture.source), scope.includes_source());
        for payload in &fixture.payloads {
            assert_eq!(paths.contains(payload), scope.includes_data());
        }
        let database = fixture.database.clone();
        let target = fixture.target.clone();
        let destination = fixture.recycled.clone();
        fixture.app.complete_trash_request_with(|paths| {
            let connection = rusqlite::Connection::open(&database).unwrap();
            let (state, downloaded, uploaded): (String, i64, i64) = connection
                .query_row(
                    "SELECT state, downloaded_bytes, uploaded_bytes FROM torrents WHERE id=?1",
                    [&target],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                state, "paused",
                "durable stop must precede filesystem recycling"
            );
            assert_eq!((downloaded, uploaded), (8, 123));
            fake_recycle(paths, &destination)
        });
        assert!(
            !fixture
                .app
                .control
                .torrents
                .iter()
                .any(|torrent| torrent.info_hashes.join(";") == fixture.target)
        );
        assert!(!fixture.app.tracker_audits.contains_key(&fixture.target));
        assert_eq!(
            fixture.app.control.torrents[fixture.app.selected_torrent]
                .info_hashes
                .join(";"),
            fixture.neighbor
        );
        assert_eq!(fixture.source.exists(), !scope.includes_source());
        for payload in &fixture.payloads {
            assert_eq!(payload.exists(), !scope.includes_data());
        }
        fixture.assert_neighbor();
        drop(fixture.app);
        let restored = StateStore::open(&fixture.database)
            .unwrap()
            .load_all_torrents()
            .unwrap();
        assert!(!restored.iter().any(|record| record.id == fixture.target));
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, fixture.neighbor);
    }
}

#[test]
fn partial_recycle_failure_keeps_the_entry_and_saved_metadata_without_readding_or_zeroing_totals() {
    let mut fixture = Fixture::new();
    fixture.confirmed(TrashScope::DownloadedFiles);
    let destination = fixture.recycled.clone();
    fixture.app.complete_trash_request_with(|paths| {
        fake_recycle(&paths[..1], &destination)?;
        Err("SYNTHETIC_RECYCLE_FAILURE".into())
    });
    fixture.assert_retained_error("SYNTHETIC_RECYCLE_FAILURE");
    assert!(fixture.source.exists());
    assert_eq!(
        fixture.payloads.iter().filter(|path| path.exists()).count(),
        1
    );
}

#[test]
fn persistence_failure_before_recycling_never_calls_the_filesystem_adapter() {
    let mut fixture = Fixture::new();
    rusqlite::Connection::open(&fixture.database).unwrap().execute_batch(
        "CREATE TRIGGER reject_stop BEFORE UPDATE ON torrents BEGIN SELECT RAISE(ABORT,'SYNTHETIC_STOP_SAVE_FAILURE'); END;",
    ).unwrap();
    fixture.confirmed(TrashScope::DownloadedFiles);
    let mut called = false;
    fixture.app.complete_trash_request_with(|_| {
        called = true;
        Ok(())
    });
    assert!(!called);
    fixture.assert_retained_error("SYNTHETIC_STOP_SAVE_FAILURE");
    assert!(fixture.source.exists());
    assert!(fixture.payloads.iter().all(|path| path.exists()));
}

#[test]
fn database_delete_failure_after_recycling_retains_an_error_row_and_never_claims_success() {
    let mut fixture = Fixture::new();
    fixture.confirmed(TrashScope::DownloadedFiles);
    let destination = fixture.recycled.clone();
    let database = fixture.database.clone();
    fixture.app.complete_trash_request_with(|paths| {
        fake_recycle(paths, &destination)?;
        rusqlite::Connection::open(&database).unwrap().execute_batch(
            "CREATE TRIGGER reject_removal BEFORE DELETE ON torrents BEGIN SELECT RAISE(ABORT,'SYNTHETIC_DELETE_FAILURE'); END;",
        ).unwrap();
        Ok(())
    });
    fixture.assert_retained_error("SYNTHETIC_DELETE_FAILURE");
    assert!(fixture.source.exists());
    assert!(fixture.payloads.iter().all(|path| !path.exists()));
    let persisted = fixture.app.control.backend.load_torrents().unwrap();
    assert_eq!(
        persisted
            .iter()
            .find(|record| record.id == fixture.target)
            .unwrap()
            .state,
        "error"
    );
}

#[test]
fn already_missing_downloaded_files_still_allow_confirmed_row_removal_without_recycling_source() {
    let mut fixture = Fixture::new();
    fake_recycle(&fixture.payloads, &fixture.recycled).unwrap();
    fixture.confirmed(TrashScope::DownloadedFiles);
    assert!(fixture.app.pending_trash.as_ref().unwrap().paths.is_empty());
    let mut called = false;
    fixture.app.complete_trash_request_with(|_| {
        called = true;
        Ok(())
    });
    assert!(!called, "empty payload sets need no filesystem operation");
    assert!(
        !fixture
            .app
            .control
            .torrents
            .iter()
            .any(|torrent| torrent.info_hashes.join(";") == fixture.target)
    );
    assert!(fixture.source.exists());
    fixture.assert_neighbor();
    assert!(
        !fixture
            .app
            .control
            .backend
            .load_torrents()
            .unwrap()
            .iter()
            .any(|record| record.id == fixture.target)
    );
}

#[test]
fn locations_changed_after_confirmation_retain_the_row_and_do_not_invoke_recycling() {
    let mut fixture = Fixture::new();
    fixture.confirmed(TrashScope::DownloadedFiles);
    fs::rename(
        &fixture.payloads[0],
        fixture.recycled.join("externally-moved.bin"),
    )
    .unwrap();
    let mut called = false;
    fixture.app.complete_trash_request_with(|_| {
        called = true;
        Ok(())
    });
    assert!(!called);
    fixture.assert_retained_error("File locations changed");
    assert!(fixture.source.exists());
    assert!(fixture.payloads[1].exists());
}
