use super::*;
use sha1::{Digest, Sha1};
use std::{path::Path, time::Instant};

fn metadata() -> (Vec<u8>, String) {
    let mut info = b"d6:lengthi8e4:name11:payload.bin12:piece lengthi16384e6:pieces20:".to_vec();
    info.extend_from_slice(&Sha1::digest(b"verified"));
    info.extend_from_slice(b"7:privatei1ee");
    let id = format!("v1: {:x}", Sha1::digest(&info));
    let mut bytes = b"d4:info".to_vec();
    bytes.extend(info);
    bytes.push(b'e');
    (bytes, id)
}
fn runtime(root: &Path) -> SessionRuntime {
    SessionRuntime::start_with_options(
        64,
        root.into(),
        SessionOptions {
            dht: None,
            listen: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .unwrap()
}
fn add(id: &str, source: &Path, output: &Path) -> SessionCommand {
    SessionCommand::AddTorrent {
        repair_policy: Default::default(),
        verify_after_download: false,
        download_mode: DownloadMode::Normal,
        freeze_after_bytes: 0,
        full_size: 8,
        id: id.into(),
        source: source.display().to_string(),
        output_folder: output.display().to_string(),
        only_files: None,
        trackers: vec![],
        paused: true,
        peer_limit: None,
    }
}
fn wait(runtime: &SessionRuntime, mut predicate: impl FnMut(SessionEvent) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(event) = runtime.try_recv() {
            if let SessionEvent::Error { message, .. } = &event {
                panic!("sandbox backend error: {message}");
            }
            if predicate(event) {
                return;
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Instant::now() < end, "sandbox event timed out");
    }
}
#[test]
fn prepared_add_uses_the_validated_bytes_without_reopening_a_missing_source() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("payload.bin"), b"verified").unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(root.path());
    let source = root.path().join("no-longer-present.torrent");
    let prepared = PreparedTorrent {
        bytes: bytes.into(),
        directory: crate::storage::sandbox::Directory::open(root.path()).unwrap(),
    };
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &source, root.path())),
            prepared,
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: true,
                ..
            }
        )
    });
    assert!(!source.exists());
    runtime.shutdown().unwrap();
}
#[cfg(unix)]
#[test]
fn prepared_root_identity_survives_path_replacement_and_a_real_recheck() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("selected");
    let pinned = temp.path().join("pinned");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(original.join("payload.bin"), b"verified").unwrap();
    let directory = crate::storage::sandbox::Directory::open(&original).unwrap();
    std::fs::rename(&original, &pinned).unwrap();
    std::os::unix::fs::symlink(&outside, &original).unwrap();
    let (bytes, id) = metadata();
    let runtime = runtime(temp.path());
    runtime
        .try_send(SessionCommand::PreparedAdd {
            command: Box::new(add(&id, &temp.path().join("absent.torrent"), &original)),
            prepared: PreparedTorrent {
                bytes: bytes.into(),
                directory,
            },
        })
        .unwrap();
    wait(&runtime, |event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: true,
                ..
            }
        )
    });
    runtime
        .try_send(SessionCommand::Recheck { id: id.clone() })
        .unwrap();
    let mut checking = false;
    wait(&runtime, |event| match event {
        SessionEvent::Verification {
            status: CompletionStatus::Checking { .. },
            ..
        } => {
            checking = true;
            false
        }
        SessionEvent::Verification {
            status: CompletionStatus::Verified,
            ..
        } => checking,
        _ => false,
    });
    assert_eq!(
        std::fs::read(pinned.join("payload.bin")).unwrap(),
        b"verified"
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    runtime.shutdown().unwrap();
}
#[cfg(unix)]
#[test]
fn actual_backend_refuses_a_symlink_payload_without_modifying_the_target() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (bytes, id) = metadata();
    let source = temp.path().join("source.torrent");
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(outside.path().join("original"), b"untouched").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("original"),
        temp.path().join("payload.bin"),
    )
    .unwrap();
    let runtime = runtime(temp.path());
    runtime.try_send(add(&id, &source, temp.path())).unwrap();
    wait(&runtime, |event| {
        matches!(event, SessionEvent::Error { .. })
    });
    assert_eq!(
        std::fs::read(outside.path().join("original")).unwrap(),
        b"untouched"
    );
    runtime.shutdown().unwrap();
}
#[test]
fn prepared_debug_never_prints_metainfo_contents() {
    let temp = tempfile::tempdir().unwrap();
    let prepared = PreparedTorrent {
        bytes: Arc::from(&b"SYNTHETIC_SECRET"[..]),
        directory: crate::storage::sandbox::Directory::open(temp.path()).unwrap(),
    };
    assert!(!format!("{prepared:?}").contains("SYNTHETIC_SECRET"));
}
