use super::*;
use crate::{
    client::{ClientBackend, tests::torrent, torrent_to_record},
    session::SessionEvent,
    state::StateStore,
};

fn service() -> (tempfile::TempDir, Arc<Service>) {
    let directory = tempfile::tempdir().unwrap();
    let mut item = torrent();
    item.files = FileSelectionModel::from_files([
        ("folder/first.bin".into(), 100),
        ("folder/second.bin".into(), 100),
    ]);
    item.files.set_selected(1, false);
    let mut backend = ClientBackend::with_state_store(
        StateStore::open(&directory.path().join("state.sqlite3")).unwrap(),
    );
    backend
        .save_torrent(&torrent_to_record(&item, None))
        .unwrap();
    item.files.defer_details();
    let owner = Controller::new(backend, vec![item]);
    let service = Arc::new(Service {
        owner: Arc::new(Mutex::new(owner)),
        directory: directory.path().into(),
        settings: Mutex::new(ClientSettings::default()),
        saved_speed: Default::default(),
        history: Some(History::memory()),
        instance: "synthetic-file-fence-owner".into(),
        token_hash: String::new(),
        requested: Default::default(),
        request_wake: Default::default(),
        stop: Default::default(),
        wake: Default::default(),
        imports: tokio::sync::Semaphore::new(1),
        reviews: Default::default(),
        plans: Default::default(),
    });
    (directory, service)
}
fn dispatch(service: &Arc<Service>, action: Action) -> Result<Reply, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(service.dispatch("synthetic-file-client".into(), action, None))
}
fn revisions(service: &Service) -> (u64, u64) {
    let owner = service.owner.lock().unwrap();
    (
        owner.command_revision("v1: abc").unwrap(),
        owner.torrents[0].files.metadata_revision(),
    )
}
fn files(command: u64, metadata: u64, offset: usize, paths: bool) -> Action {
    Action::Files {
        id: "v1: abc".into(),
        offset,
        limit: 1,
        revision: command,
        file_revision: metadata,
        paths,
    }
}

#[test]
fn same_count_selection_ack_invalidates_remaining_pages_and_old_folder_intents() {
    let (_directory, service) = service();
    let (command, metadata) = revisions(&service);
    let Reply::Files(first) = dispatch(&service, files(command, metadata, 0, true)).unwrap() else {
        panic!("file page required")
    };
    assert_eq!(first.file_revision, metadata);
    assert!(first.files[0].selected);
    {
        let mut owner = service.owner.lock().unwrap();
        owner.apply(SessionEvent::FilesApplied {
            id: "v1: abc".into(),
            only_files: vec![1],
        });
        assert_eq!(owner.torrents[0].files.selected_count(), 1);
    }
    let (current_command, current_metadata) = revisions(&service);
    assert_eq!(
        current_command, command,
        "engine ACKs do not consume command acceptance revisions"
    );
    assert_ne!(current_metadata, metadata);
    assert!(matches!(
        dispatch(&service, files(command, metadata, 1, true)),
        Err(Error::Stale)
    ));
    assert!(matches!(
        dispatch(
            &service,
            Action::Selection {
                id: "v1: abc".into(),
                files: PackedIndices::new(2, &[0]).unwrap(),
                revision: command,
                file_revision: metadata,
            }
        ),
        Err(Error::Stale)
    ));
    assert!(matches!(
        dispatch(
            &service,
            Action::Priorities {
                id: "v1: abc".into(),
                files: PackedIndices::new(2, &[0]).unwrap(),
                revision: command,
                file_revision: metadata,
            }
        ),
        Err(Error::Stale)
    ));
    let Reply::Files(current) =
        dispatch(&service, files(command, current_metadata, 1, true)).unwrap()
    else {
        panic!("file page required")
    };
    assert_eq!(current.file_revision, current_metadata);
    assert_eq!(current.files[0].path, "folder/second.bin");
    assert!(
        current.files[0].selected,
        "canonical ACK bits overlay SQL flags before durability retry"
    );
    let owner = service.owner.lock().unwrap();
    assert_eq!(owner.torrents[0].files.selected_indices(), Some(vec![1]));
    assert!(
        !owner.backend.load_torrents().unwrap()[0].files[1].selected,
        "fixture intentionally leaves persisted selection stale"
    );
}

#[test]
fn progress_pages_continue_but_priority_ack_invalidates_the_old_file_generation() {
    let (_directory, service) = service();
    let (command, metadata) = revisions(&service);
    dispatch(&service, files(command, metadata, 0, false)).unwrap();
    service
        .owner
        .lock()
        .unwrap()
        .apply(SessionEvent::FileSnapshot {
            id: "v1: abc".into(),
            progress: vec![25, 75],
        });
    assert_eq!(revisions(&service), (command, metadata));
    let Reply::Files(second) = dispatch(&service, files(command, metadata, 1, false)).unwrap()
    else {
        panic!("file page required")
    };
    assert_eq!(second.files[0].downloaded_bytes, 75);
    service
        .owner
        .lock()
        .unwrap()
        .apply(SessionEvent::FilePrioritiesApplied {
            id: "v1: abc".into(),
            high: vec![1],
        });
    let (after_command, after_metadata) = revisions(&service);
    assert_eq!(after_command, command);
    assert_ne!(after_metadata, metadata);
    assert!(matches!(
        dispatch(&service, files(command, metadata, 0, false)),
        Err(Error::Stale)
    ));
    let Reply::Files(page) = dispatch(&service, files(command, after_metadata, 1, true)).unwrap()
    else {
        panic!("file page required")
    };
    assert_eq!(page.files[0].priority, 1);
    assert_eq!(page.files[0].downloaded_bytes, 75);
}

#[test]
fn invalid_or_incomplete_file_generations_and_unfenced_mutation_routes_are_rejected() {
    let (_directory, service) = service();
    let (command, metadata) = revisions(&service);
    for invalid in [0, u64::MAX] {
        assert!(matches!(
            dispatch(&service, files(command, invalid, 0, true)),
            Err(Error::Stale)
        ));
        assert!(matches!(
            dispatch(
                &service,
                Action::Selection {
                    id: "v1: abc".into(),
                    files: PackedIndices::new(2, &[0]).unwrap(),
                    revision: command,
                    file_revision: invalid,
                }
            ),
            Err(Error::Stale)
        ));
    }
    assert!(matches!(
        dispatch(&service, files(command.wrapping_add(1), metadata, 0, false)),
        Err(Error::Stale)
    ));
    assert!(matches!(
        dispatch(
            &service,
            Action::Selection {
                id: "v1: abc".into(),
                files: PackedIndices::new(1, &[0]).unwrap(),
                revision: command,
                file_revision: metadata,
            }
        ),
        Err(Error::Stale)
    ));
    assert!(matches!(dispatch(&service, Action::Mutate {
        id: "v1: abc".into(), command: crate::control::TorrentCommand::SetFiles(vec![1]), revision: Some(command),
    }), Err(Error::Failed(message)) if message.contains("generation fence")));
    assert!(matches!(dispatch(&service, Action::Storage {
        id: "v1: abc".into(), action: StorageAction::Priorities { high: vec![1] }, revision: command,
    }), Err(Error::Failed(message)) if message.contains("generation fence")));
    assert_eq!(revisions(&service), (command, metadata));
    let owner = service.owner.lock().unwrap();
    assert_eq!(owner.torrents[0].files.selected_indices(), Some(vec![0]));
    assert!(owner.torrents[0].files.high_priority().is_empty());
    assert!(
        !owner.backend.session_available(),
        "synthetic regression never launches an engine"
    );
}

#[test]
fn native_wire_requires_a_valid_file_generation_without_changing_agent_protocol() {
    let item = torrent();
    let metadata = item.files.metadata_revision();
    let wire = WireTorrent::capture(&item, 17, true, false);
    let native = wire.clone().into_native().unwrap();
    assert_eq!(native.file_revision, metadata);
    assert_eq!(native.item.files.metadata_revision(), metadata);
    assert!(!native.item.files.details_loaded());
    for invalid in [0, u64::MAX] {
        let mut bad = wire.clone();
        bad.file_revision = invalid;
        assert!(matches!(bad.into_native(), Err(Error::Stale)));
    }
    let mut encoded = serde_json::to_value(files(17, metadata, 0, true)).unwrap();
    encoded["Files"]
        .as_object_mut()
        .unwrap()
        .remove("file_revision");
    assert!(
        serde_json::from_value::<Action>(encoded).is_err(),
        "the file fence is mandatory on the wire"
    );
    assert_eq!(PROTOCOL_VERSION, 5);
    assert_eq!(crate::control::PROTOCOL_VERSION, 1);
}

#[test]
fn path_byte_bounded_pages_keep_the_same_fence_and_contiguous_cursor() {
    let (_directory, service) = service();
    {
        let mut owner = service.owner.lock().unwrap();
        let mut item = torrent();
        item.files = FileSelectionModel::from_files(
            (0..128).map(|index| (format!("{index:03}-{}.bin", "x".repeat(1024)), 100)),
        );
        owner
            .backend
            .save_torrent(&torrent_to_record(&item, None))
            .unwrap();
        item.files.defer_details();
        owner.torrents[0] = item;
    }
    let (command, metadata) = revisions(&service);
    let request = |offset| Action::Files {
        id: "v1: abc".into(),
        offset,
        limit: 128,
        revision: command,
        file_revision: metadata,
        paths: true,
    };
    let Reply::Files(first) = dispatch(&service, request(0)).unwrap() else {
        panic!("file page required")
    };
    assert!(
        first.files.len() < 128,
        "SQL pages retain the 64 KiB path-byte bound"
    );
    assert!(!first.files.is_empty());
    assert_eq!(first.file_revision, metadata);
    let next = first.next_offset.unwrap();
    assert_eq!(next, first.files.len());
    let Reply::Files(second) = dispatch(&service, request(next)).unwrap() else {
        panic!("file page required")
    };
    assert_eq!(second.files[0].index as usize, next);
    assert_eq!(second.file_revision, metadata);
}
