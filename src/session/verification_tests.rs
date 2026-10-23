use super::*;
use sha1::{Digest, Sha1};
use std::{fs, time::Instant};

struct CheckedFixture {
    runtime: Option<SessionRuntime>,
    root: tempfile::TempDir,
    id: String,
    payload: Vec<u8>,
}

impl CheckedFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let payload = vec![17; 32768];
        let mut info = format!(
            "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces40:",
            payload.len()
        )
        .into_bytes();
        for piece in payload.as_chunks::<16384>().0 {
            info.extend_from_slice(&Sha1::digest(piece));
        }
        info.extend_from_slice(b"7:privatei1ee");
        let id = format!("v1: {:x}", Sha1::digest(&info));
        let mut metainfo = b"d4:info".to_vec();
        metainfo.extend(info);
        metainfo.push(b'e');
        let source = root.path().join("fixture.torrent");
        let output = root.path().join("output");
        fs::write(&source, metainfo).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("payload.bin"), &payload).unwrap();
        let runtime = SessionRuntime::start_with_options(
            128,
            output.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..session_options(true)
            },
        )
        .unwrap();
        runtime
            .try_send(SessionCommand::AddTorrent {
                verify_after_download: false,
                download_mode: DownloadMode::Normal,
                freeze_after_bytes: 0,
                full_size: payload.len() as u64,
                id: id.clone(),
                source: source.display().to_string(),
                output_folder: output.display().to_string(),
                only_files: None,
                trackers: Vec::new(),
                paused: true,
                peer_limit: Some(1),
            })
            .unwrap();
        let fixture = Self {
            runtime: Some(runtime),
            root,
            id,
            payload,
        };
        fixture.wait_for(|event| {
            matches!(
                event,
                SessionEvent::Stats {
                    finished: true,
                    paused: true,
                    ..
                }
            )
        });
        fixture
    }

    fn runtime(&self) -> &SessionRuntime {
        self.runtime.as_ref().unwrap()
    }

    fn wait_for(&self, predicate: impl Fn(&SessionEvent) -> bool) -> SessionEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(event) = self.runtime().recv_timeout(Duration::from_millis(100)) {
                if let SessionEvent::Error { message, .. } = &event {
                    panic!("{message}");
                }
                if predicate(&event) {
                    return event;
                }
            }
        }
        panic!("expected verification event was not received");
    }

    fn recheck(&self) {
        self.runtime()
            .try_send(SessionCommand::Recheck {
                id: self.id.clone(),
            })
            .unwrap();
        self.wait_for(|event| {
            matches!(
                event,
                SessionEvent::Verification {
                    status: CompletionStatus::Checking { .. },
                    ..
                }
            )
        });
    }
}

impl Drop for CheckedFixture {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let _ = runtime.shutdown();
        }
    }
}

#[test]
fn corrupt_files_fail_real_recheck_and_repaired_files_can_be_verified() {
    let fixture = CheckedFixture::new();
    // Verification must use resolved metadata rather than reopening the source.
    fs::remove_file(fixture.root.path().join("fixture.torrent")).unwrap();
    let destination = fixture.root.path().join("output/payload.bin");
    let mut corrupted = fixture.payload.clone();
    corrupted[100] ^= 0xff;
    fs::write(&destination, &corrupted).unwrap();
    fixture.recheck();
    let event = fixture.wait_for(|event| {
        assert!(
            !matches!(event, SessionEvent::Stats { finished: true, .. }),
            "corruption must never be reported as Done"
        );
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Failed { .. },
                ..
            }
        )
    });
    assert!(matches!(
        event,
        SessionEvent::Verification {
            status: CompletionStatus::Failed { .. },
            ..
        }
    ));
    assert_eq!(
        fs::read(&destination).unwrap(),
        corrupted,
        "a failed check must not delete payload files"
    );
    fs::write(&destination, &fixture.payload).unwrap();
    fixture.recheck();
    fixture.wait_for(|event| {
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Verified,
                ..
            }
        )
    });
    fixture.wait_for(|event| {
        matches!(
            event,
            SessionEvent::Stats {
                finished: true,
                paused: true,
                ..
            }
        )
    });
    assert_eq!(fs::read(&destination).unwrap(), fixture.payload);
}

#[test]
fn enabling_post_download_verification_checks_once_and_preserves_pause() {
    let fixture = CheckedFixture::new();
    fixture
        .runtime()
        .try_send(SessionCommand::SetVerifyAfterDownload {
            id: fixture.id.clone(),
            enabled: true,
        })
        .unwrap();
    fixture.wait_for(|event| {
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Checking {
                    checked_bytes: 0,
                    ..
                },
                ..
            }
        )
    });
    fixture.wait_for(|event| {
        matches!(
            event,
            SessionEvent::Verification {
                status: CompletionStatus::Verified,
                ..
            }
        )
    });
    for _ in 0..3 {
        fixture.wait_for(|event| {
            assert!(
                !matches!(
                    event,
                    SessionEvent::Verification {
                        status: CompletionStatus::Checking { .. },
                        ..
                    }
                ),
                "successful verification must not loop"
            );
            matches!(
                event,
                SessionEvent::Stats {
                    finished: true,
                    paused: true,
                    ..
                }
            )
        });
    }
}
