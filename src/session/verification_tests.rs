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
        Self::with_tracker(None)
    }

    fn with_tracker(tracker: Option<String>) -> Self {
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
                ..session_options(tracker.is_none())
            },
        )
        .unwrap();
        runtime
            .try_send(SessionCommand::AddTorrent {
                repair_policy: Default::default(),
                verify_after_download: false,
                download_mode: DownloadMode::Normal,
                freeze_after_bytes: 0,
                full_size: payload.len() as u64,
                id: id.clone(),
                source: source.display().to_string(),
                output_folder: output.display().to_string(),
                only_files: None,
                trackers: tracker.into_iter().collect(),
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
fn every_mode_transition_preserves_paused_payload_and_rejects_invalid_threshold() {
    let fixture = CheckedFixture::new();
    for from in DownloadMode::ALL {
        for to in DownloadMode::ALL {
            for mode in [from, to] {
                let config = ModeConfig {
                    mode,
                    freeze_after_bytes: 16384,
                };
                fixture
                    .runtime()
                    .try_send(SessionCommand::SetDownloadMode {
                        id: fixture.id.clone(),
                        config,
                    })
                    .unwrap();
                fixture.wait_for(|e| matches!(e, SessionEvent::Mode { application: ModeApplication::Active { config: actual, tracker_enabled: false }, .. } if *actual == config));
                fixture.wait_for(|e| {
                    matches!(
                        e,
                        SessionEvent::Stats {
                            paused: true,
                            finished: true,
                            downloaded_bytes: 32768,
                            ..
                        }
                    )
                });
                assert_eq!(
                    fs::read(fixture.root.path().join("output/payload.bin")).unwrap(),
                    fixture.payload
                );
            }
        }
    }
    let invalid = ModeConfig {
        mode: DownloadMode::FreezeAfter,
        freeze_after_bytes: 0,
    };
    fixture
        .runtime()
        .try_send(SessionCommand::SetDownloadMode {
            id: fixture.id.clone(),
            config: invalid,
        })
        .unwrap();
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Mode {
                application: ModeApplication::Failed {
                    active: Some(_),
                    ..
                },
                ..
            }
        )
    });
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Stats {
                paused: true,
                finished: true,
                ..
            }
        )
    });
}

#[test]
fn automatic_repair_does_not_override_a_user_paused_integrity_check() {
    let fixture = CheckedFixture::new();
    fixture
        .runtime()
        .try_send(SessionCommand::SetRepairPolicy {
            id: fixture.id.clone(),
            policy: RepairPolicy {
                enabled: true,
                max_attempts: 2,
            },
        })
        .unwrap();
    fixture.wait_for(|e| matches!(e, SessionEvent::RepairPolicy { .. }));
    let mut damaged = fixture.payload.clone();
    damaged[1] ^= 0xff;
    fs::write(fixture.root.path().join("output/payload.bin"), &damaged).unwrap();
    fixture.recheck();
    fixture.wait_for(|e| {
        assert!(
            !matches!(
                e,
                SessionEvent::Repair {
                    status: RepairStatus::Downloading { .. },
                    ..
                }
            ),
            "paused checks must not start automatic downloads"
        );
        matches!(
            e,
            SessionEvent::Verification {
                status: CompletionStatus::Failed { .. },
                ..
            }
        )
    });
    let config = ModeConfig {
        mode: DownloadMode::NoProgress,
        freeze_after_bytes: 16384,
    };
    fixture
        .runtime()
        .try_send(SessionCommand::SetDownloadMode {
            id: fixture.id.clone(),
            config,
        })
        .unwrap();
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Mode {
                application: ModeApplication::Active { .. },
                ..
            }
        )
    });
    assert_eq!(
        fs::read(fixture.root.path().join("output/payload.bin")).unwrap(),
        damaged
    );
}

#[test]
fn automatic_repair_fetches_only_the_damaged_piece_and_keeps_the_reporting_mode() {
    use crate::protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    struct Servers {
        stop: Arc<AtomicBool>,
        workers: Vec<thread::JoinHandle<()>>,
    }
    impl Drop for Servers {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            for w in self.workers.drain(..) {
                let _ = w.join();
            }
        }
    }
    let tracker = TcpListener::bind("127.0.0.1:0").unwrap();
    tracker.set_nonblocking(true).unwrap();
    let tracker_url = format!("http://{}/announce", tracker.local_addr().unwrap());
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    peer.set_nonblocking(true).unwrap();
    let peers = vec![peer.local_addr().unwrap()];
    let stop = Arc::new(AtomicBool::new(false));
    let tracker_stop = stop.clone();
    let mut servers = Servers {
        stop: stop.clone(),
        workers: vec![thread::spawn(move || {
            super::multi_peer_tests::serve_tracker(tracker, peers, tracker_stop).unwrap();
        })],
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let peer_requests = requests.clone();
    let peer_stop = stop.clone();
    servers.workers.push(thread::spawn(move || {
        while let Some(mut stream) =
            super::multi_peer_tests::accept_before_deadline(&peer, &peer_stop).unwrap()
        {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut encoded = [0; HANDSHAKE_LENGTH];
            if stream.read_exact(&mut encoded).is_err() {
                continue;
            }
            let incoming = Handshake::decode(&encoded).unwrap();
            let outgoing = Handshake {
                reserved: [0; 8],
                info_hash: incoming.info_hash,
                peer_id: *b"-PLTEST-repair000000",
            };
            if stream.write_all(&outgoing.encode()).is_err() {
                continue;
            }
            if stream
                .write_all(&PeerMessage::Bitfield(vec![0xc0]).encode())
                .is_err()
            {
                continue;
            }
            if stream.write_all(&PeerMessage::Unchoke.encode()).is_err() {
                continue;
            }
            while !peer_stop.load(Ordering::Relaxed) {
                let mut prefix = [0; 4];
                if stream.read_exact(&mut prefix).is_err() {
                    break;
                }
                let length = u32::from_be_bytes(prefix) as usize;
                assert!(length <= 65536);
                let mut message = prefix.to_vec();
                message.resize(length + 4, 0);
                if stream.read_exact(&mut message[4..]).is_err() {
                    break;
                }
                if let Ok(PeerMessage::Request {
                    index,
                    begin,
                    length,
                }) = PeerMessage::decode(&message, 65536)
                {
                    assert!(index < 2 && u64::from(begin) + u64::from(length) <= 16384);
                    peer_requests.lock().unwrap().push(index);
                    if stream
                        .write_all(
                            &PeerMessage::Piece {
                                index,
                                begin,
                                block: vec![17; length as usize],
                            }
                            .encode(),
                        )
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    }));
    let fixture = CheckedFixture::with_tracker(Some(tracker_url));
    let config = ModeConfig {
        mode: DownloadMode::NoProgress,
        freeze_after_bytes: 16384,
    };
    fixture
        .runtime()
        .try_send(SessionCommand::SetDownloadMode {
            id: fixture.id.clone(),
            config,
        })
        .unwrap();
    fixture.wait_for(|e| matches!(e, SessionEvent::Mode { application: ModeApplication::Active { config: actual, tracker_enabled: true }, .. } if *actual == config));
    let mut damaged = fixture.payload.clone();
    damaged[1] ^= 0xff;
    fs::write(fixture.root.path().join("output/payload.bin"), damaged).unwrap();
    fixture
        .runtime()
        .try_send(SessionCommand::Resume {
            id: fixture.id.clone(),
        })
        .unwrap();
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::StateChanged {
                state: "downloading",
                ..
            }
        )
    });
    fixture
        .runtime()
        .try_send(SessionCommand::SetRepairPolicy {
            id: fixture.id.clone(),
            policy: RepairPolicy {
                enabled: true,
                max_attempts: 2,
            },
        })
        .unwrap();
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Repair {
                status: RepairStatus::Downloading {
                    attempt: 1,
                    limit: 2
                },
                ..
            }
        )
    });
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Repair {
                status: RepairStatus::Checking { attempt: 1, .. },
                ..
            }
        )
    });
    fixture.wait_for(|e| {
        matches!(
            e,
            SessionEvent::Repair {
                status: RepairStatus::Succeeded { attempts: 1 },
                ..
            }
        )
    });
    fixture.wait_for(|e| matches!(e, SessionEvent::Stats { finished: true, .. }));
    assert_eq!(
        fs::read(fixture.root.path().join("output/payload.bin")).unwrap(),
        fixture.payload
    );
    assert!(!requests.lock().unwrap().is_empty());
    assert!(
        requests.lock().unwrap().iter().all(|index| *index == 0),
        "healthy piece must not be downloaded again"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture
            .runtime()
            .journal
            .entries(None)
            .iter()
            .any(|e| e.announce.actual.downloaded >= 16384 && e.announce.outcome == "ok")
        {
            break;
        }
        assert!(Instant::now() < deadline, "no real repair announce");
        while fixture.runtime().try_recv().is_some() {}
        thread::sleep(Duration::from_millis(10));
    }
    for entry in fixture.runtime().journal.entries(None) {
        assert_eq!(entry.announce.mode, "no_progress");
        assert_eq!(entry.announce.sent.uploaded, 0);
        assert_eq!(entry.announce.sent.downloaded, 0);
        assert_eq!(entry.announce.sent.left, 32768);
        assert_ne!(entry.announce.event.as_deref(), Some("completed"));
    }
    // Changing a running transfer must restart real tracker workers with the new
    // policy, not merely change a UI label or discard cumulative network totals.
    for mode in DownloadMode::ALL {
        let before = fixture
            .runtime()
            .journal
            .entries(None)
            .last()
            .map(|e| e.sequence)
            .unwrap_or(0);
        let config = ModeConfig {
            mode,
            freeze_after_bytes: 16384,
        };
        fixture
            .runtime()
            .try_send(SessionCommand::SetDownloadMode {
                id: fixture.id.clone(),
                config,
            })
            .unwrap();
        fixture.wait_for(|e| matches!(e, SessionEvent::Mode { application: ModeApplication::Active { config: actual, .. }, .. } if *actual == config));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(entry) = fixture.runtime().journal.entries(None).iter().find(|e| {
                e.sequence > before && e.announce.mode == mode.id() && e.announce.outcome == "ok"
            }) {
                assert_eq!(
                    entry.announce.actual.downloaded, 16384,
                    "mode changes must preserve real network totals"
                );
                let expected = mode
                    .reporting_policy_with_threshold(32768, 16384)
                    .apply_metrics(entry.announce.actual);
                assert_eq!(entry.announce.sent, expected);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real tracker worker did not adopt {mode:?}"
            );
            while fixture.runtime().try_recv().is_some() {}
            thread::sleep(Duration::from_millis(10));
        }
    }
    drop(fixture);
    drop(servers);
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
