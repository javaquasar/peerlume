//! One real engine, fixed torrents, incoming seeding and repeated automatic repair.
use super::soak_tests::{Report, Sampler, disk_sizes, record, resources};
use super::*;
use crate::{
    announce_journal::AnnounceJournal,
    completion::CompletionStatus,
    download_mode::DownloadMode,
    runtime_status::{RepairPolicy, RepairStatus},
    session::SessionRuntime,
    state::StateStore,
    torrent::TorrentState,
};
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
mod peers;
use peers::{Anchor, BLOCK, Data, Leecher, PIECE, Proof, Servers};

fn metainfo(name: &str, bytes: usize) -> (Data, Vec<u8>) {
    assert!(bytes > 0 && bytes.is_multiple_of(PIECE));
    let payload: Vec<_> = (0..bytes)
        .map(|i| ((i * 31 + i / PIECE * 17) % 251) as u8)
        .collect();
    let mut info = format!(
        "d6:lengthi{bytes}e4:name{}:{name}12:piece lengthi{PIECE}e6:pieces{}:",
        name.len(),
        bytes / PIECE * 20
    )
    .into_bytes();
    for piece in payload.as_chunks::<PIECE>().0 {
        info.extend_from_slice(&Sha1::digest(piece));
    }
    info.extend_from_slice(b"7:privatei1ee");
    let hash = Sha1::digest(&info).into();
    let mut meta = b"d4:info".to_vec();
    meta.extend(info);
    meta.push(b'e');
    (
        Data {
            hash,
            payload: Arc::new(payload),
        },
        meta,
    )
}

fn item(
    root: &Path,
    name: &str,
    data: &Data,
    meta: &[u8],
    tracker: &str,
    mode: DownloadMode,
) -> crate::torrent::TorrentItem {
    let source = root.join(format!("{name}.torrent"));
    fs::write(&source, meta).unwrap();
    let mut item = crate::client::tests::torrent();
    item.name = name.into();
    item.source = source.display().to_string();
    item.info_hashes = vec![format!(
        "v1: {}",
        data.hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )];
    item.trackers = vec![tracker.into()];
    item.is_private = true;
    item.files = crate::file_selection::FileSelectionModel::from_files([(
        name.into(),
        data.payload.len() as u64,
    )]);
    item.overrides.download_directory = Some(root.join("payloads").display().to_string());
    item.overrides.download_mode = mode;
    item.overrides.repair_policy = RepairPolicy {
        enabled: true,
        max_attempts: 3,
    };
    item.overrides.verify_after_download = true;
    item.state = TorrentState::WaitingForEngine;
    item
}

fn faults(cycle: u64, pieces: usize) -> Vec<usize> {
    assert!(pieces >= 2);
    let first = cycle as usize % pieces;
    if cycle.is_multiple_of(8) {
        vec![first, (first + pieces / 2) % pieces]
    } else {
        vec![first]
    }
}

fn check_fetches(before: &[u64], after: &[u64], damaged: &[usize]) -> u64 {
    assert_eq!(before.len(), after.len());
    let mut received = 0;
    for (index, (&old, &new)) in before.iter().zip(after).enumerate() {
        assert!(new >= old);
        let delta = new - old;
        if damaged.contains(&index) {
            assert!(delta >= PIECE as u64, "damaged piece was not fetched again");
        } else {
            assert_eq!(delta, 0, "a healthy piece was fetched again");
        }
        received += delta;
    }
    received
}

fn wait(proof: &Proof, mut action: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !action() {
        proof.healthy();
        assert!(
            Instant::now() < deadline,
            "continuous-session phase timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn ready(control: &Controller, id: &str) -> bool {
    let item = control
        .torrents
        .iter()
        .find(|t| t.info_hashes.join(";") == id)
        .unwrap();
    assert_ne!(
        item.state,
        TorrentState::Error,
        "engine rejected continuous workload"
    );
    assert_ne!(
        item.state,
        TorrentState::VerificationFailed,
        "automatic repair exhausted"
    );
    item.completion == CompletionStatus::Verified && item.state == TorrentState::Seeding
}

fn bounds(sample: [u64; 3], baseline: [u64; 3]) {
    for (i, budget) in [192 * 1024 * 1024, 32, 64].into_iter().enumerate() {
        assert!(
            sample[i] <= baseline[i] + budget,
            "continuous-session resource budget exceeded at index {i}"
        );
    }
}

fn sample_record(
    report: &Report,
    root: &Path,
    start: Instant,
    cycle: u64,
    proof: &Proof,
    latency: u64,
) {
    let (rss, threads, handles) = resources();
    let (profile, database, journal) = disk_sizes(root);
    assert!(database <= 32 * 1024 * 1024, "SQLite files exceeded 32 MiB");
    assert!(
        journal <= 128 * 1024 * 1024,
        "announce journal exceeded 128 MiB"
    );
    record(
        report,
        serde_json::json!({"kind":"idle","elapsed_ms":start.elapsed().as_millis(),"cycle":cycle,
        "rss_bytes":rss,"threads":threads,"open_handles":handles,"profile_bytes":profile,
        "database_bytes":database,"journal_bytes":journal,"ipc_latency_us":latency,"activity":proof.snapshot()}),
    );
}

#[test]
#[ignore = "continuous engine workload; dedicated GitHub smoke and manual four-hour runs"]
fn reliability_continuous_session_soak() {
    let seconds = std::env::var("PEERLUME_SOAK_SECONDS")
        .unwrap_or_else(|_| "90".into())
        .parse::<u64>()
        .unwrap();
    assert!((60..=14400).contains(&seconds));
    let mode = DownloadMode::from_id(
        &std::env::var("PEERLUME_SOAK_MODE").unwrap_or_else(|_| "normal".into()),
    )
    .unwrap();
    assert!(matches!(
        mode,
        DownloadMode::Normal | DownloadMode::NoProgress
    ));
    let destination = std::path::PathBuf::from(
        std::env::var_os("PEERLUME_SOAK_REPORT").expect("new report path required"),
    );
    let report = Arc::new(Mutex::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap(),
    ));
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("payloads")).unwrap();
    let (target, target_meta) = metainfo("payload.bin", 1024 * 1024);
    let (anchor_data, anchor_meta) = metainfo("anchor.bin", 2 * PIECE);
    fs::write(
        root.path().join("payloads/anchor.bin"),
        &*anchor_data.payload,
    )
    .unwrap();
    assert!(
        !root.path().join("payloads/payload.bin").exists(),
        "initial download must not be prepopulated"
    );
    let proof = Arc::new(Proof::new(target.payload.len() / PIECE));
    let servers = Servers::start(vec![target.clone(), anchor_data.clone()], proof.clone());
    let journal = AnnounceJournal::open(&root.path().join("announce-logs"));
    let start = Instant::now();
    let runtime = SessionRuntime::start_test_with_journal(
        256,
        root.path().into(),
        librqbit::SessionOptions {
            dht: None,
            listen: Some(librqbit::ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ipv4_only: true,
                ..Default::default()
            }),
            ipv4_only: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
        journal.clone(),
    )
    .unwrap();
    wait(&proof, || runtime.test_engine().is_some());
    let (engine_identity, address) = {
        let engine = runtime.test_engine().unwrap();
        (Arc::downgrade(&engine), engine.listen_addr().unwrap())
    };
    let phase = Arc::new(AtomicU64::new(0));
    let observed_phase = phase.clone();
    let observed_proof = proof.clone();
    let sampler = Sampler::with_activity(report.clone(), root.path().into(), start, move || {
        let mut activity = observed_proof.snapshot();
        activity["phase"] = observed_phase.load(Ordering::Relaxed).into();
        activity
    });
    record(
        &report,
        serde_json::json!({"kind":"start","workload":"continuous_session","requested_seconds":seconds,"mode":mode.id(),
        "session_starts":1,"fixed_torrents":2,"piece_bytes":PIECE,"target_bytes":target.payload.len(),"phase_names":["initial_download","seeding","corruption_repair","shutdown"]}),
    );
    let mut control = Controller::new(
        ClientBackend::with_session_and_store(
            runtime,
            StateStore::open(&root.path().join("state.sqlite3")).unwrap(),
        ),
        vec![],
    );
    let main_item = item(
        root.path(),
        "payload.bin",
        &target,
        &target_meta,
        &servers.tracker_url,
        mode,
    );
    let anchor_item = item(
        root.path(),
        "anchor.bin",
        &anchor_data,
        &anchor_meta,
        &servers.tracker_url,
        DownloadMode::Normal,
    );
    let id = main_item.info_hashes.join(";");
    let anchor_id = anchor_item.info_hashes.join(";");
    control
        .add(main_item, &crate::settings::ClientSettings::default(), None)
        .unwrap();
    control
        .add(
            anchor_item,
            &crate::settings::ClientSettings::default(),
            None,
        )
        .unwrap();
    control
        .configure_agents(root.path(), root.path().into())
        .unwrap();
    let service = control.agent_access().unwrap();
    service.enabled = true;
    let token = service
        .grant(access::Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let instance = service.instance_id.clone();
    let _lock = host::InstanceLock::acquire(root.path()).unwrap();
    let owner = Arc::new(Mutex::new(control));
    let host = host::Host::start(owner.clone(), |c| c, root.path().into()).unwrap();
    let ipc = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request_id = 0u64;
    let mut call = |method| {
        request_id += 1;
        let sent = Instant::now();
        let request = wire::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("continuous-{request_id}"),
            token: token.clone(),
            instance_id: Some(instance.clone()),
            method,
        };
        let reply = loop {
            match ipc.block_on(host::call(root.path(), &request)) {
                Ok(reply) => break reply,
                Err(_) if sent.elapsed() < Duration::from_secs(5) => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("continuous IPC failed: {error:?}"),
            }
        };
        assert!(
            reply.error.is_none(),
            "continuous IPC rejected: {:?}",
            reply.error
        );
        assert!(sent.elapsed() < Duration::from_secs(5));
        sampler
            .latency
            .store(sent.elapsed().as_micros() as u64, Ordering::Relaxed);
        reply.result.unwrap()
    };
    wait(&proof, || {
        let c = owner.lock().unwrap();
        ready(&c, &id) && ready(&c, &anchor_id)
    });
    assert_eq!(
        fs::read(root.path().join("payloads/payload.bin")).unwrap(),
        *target.payload
    );
    assert!(proof.downloaded.load(Ordering::Relaxed) >= target.payload.len() as u64);
    let anchor = Anchor::start(address, anchor_data.clone(), proof.clone());
    wait(&proof, || proof.anchor_uploaded.load(Ordering::Relaxed) > 0);
    record(
        &report,
        serde_json::json!({"kind":"initial_download","elapsed_ms":start.elapsed().as_millis(),"verified":true,"activity":proof.snapshot()}),
    );
    let mut cycle = 0u64;
    let mut repaired_pieces = 0usize;
    let mut baseline = None;
    let mut last_announce = 0;
    let mut checked_announces = 0u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        let cycle_start = Instant::now();
        phase.store(1, Ordering::Relaxed);
        anchor.healthy();
        proof.healthy();
        let mut leecher = Leecher::connect(address, target.clone(), proof.clone(), false).unwrap();
        for _ in 0..target.payload.len() / BLOCK {
            leecher.block().unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        drop(leecher);
        let before = proof.counts();
        let before_anchor = proof.anchor_uploaded.load(Ordering::Relaxed);
        let damaged = faults(cycle, target.payload.len() / PIECE);
        let path = root.path().join("payloads/payload.bin");
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        for &index in &damaged {
            let offset = index * PIECE + 17;
            file.seek(SeekFrom::Start(offset as u64)).unwrap();
            file.write_all(&[target.payload[offset] ^ 0xff]).unwrap();
        }
        file.sync_all().unwrap();
        drop(file);
        assert_ne!(fs::read(&path).unwrap(), *target.payload);
        phase.store(2, Ordering::Relaxed);
        let revision = owner.lock().unwrap().command_revision(&id).unwrap();
        let operation = call(wire::Method::Mutate {
            torrent_id: id.clone(),
            command: TorrentCommand::Recheck,
            expected_revision: revision,
            idempotency_key: format!("continuous-repair-{cycle}"),
            deadline_unix_ms: operations::now_ms() + 60000,
        })["operation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut saw_download = false;
        let mut saw_check = false;
        wait(&proof, || {
            anchor.healthy();
            let c = owner.lock().unwrap();
            let torrent = c
                .torrents
                .iter()
                .find(|t| t.info_hashes.join(";") == id)
                .unwrap();
            saw_download |= matches!(torrent.runtime.repair, RepairStatus::Downloading { .. });
            saw_check |=
                saw_download && matches!(torrent.runtime.repair, RepairStatus::Checking { .. });
            assert!(
                !matches!(torrent.runtime.repair, RepairStatus::Exhausted { .. }),
                "automatic repair exhausted"
            );
            saw_download
                && saw_check
                && matches!(torrent.runtime.repair, RepairStatus::Succeeded { .. })
                && ready(&c, &id)
        });
        let outcome = call(wire::Method::Operation {
            operation_id: operation,
        });
        assert!(
            matches!(outcome["state"].as_str(), Some("succeeded" | "failed")),
            "recheck operation did not terminate"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            *target.payload,
            "repair did not restore exact payload"
        );
        let fetched = check_fetches(&before, &proof.counts(), &damaged);
        repaired_pieces += damaged.len();
        assert!(
            proof.anchor_uploaded.load(Ordering::Relaxed) > before_anchor,
            "seeding stopped during repair"
        );
        assert_eq!(proof.anchor_connections.load(Ordering::Relaxed), 1);
        {
            let c = owner.lock().unwrap();
            let engine = c.backend.test_engine().unwrap();
            assert!(
                Arc::downgrade(&engine).ptr_eq(&engine_identity),
                "engine session was replaced"
            );
            assert_eq!(engine.listen_addr(), Some(address));
            assert_eq!(engine.with_torrents(|ts| ts.count()), 2);
            assert!(ready(&c, &anchor_id));
            let torrent = c
                .torrents
                .iter()
                .find(|t| t.info_hashes.join(";") == id)
                .unwrap();
            assert_eq!(torrent.runtime.mode.active().unwrap().mode, mode);
            let stored = c.backend.load_torrents().unwrap();
            assert_eq!(
                stored.iter().find(|t| t.id == id).unwrap().download_mode,
                mode
            );
        }
        for entry in journal.entries(None) {
            if entry.sequence <= last_announce {
                continue;
            }
            last_announce = entry.sequence;
            if entry.announce.info_hash != id.strip_prefix("v1: ").unwrap() {
                continue;
            }
            assert_eq!(entry.announce.mode, mode.id());
            assert_eq!(entry.announce.outcome, "ok");
            if mode == DownloadMode::Normal {
                assert_eq!(entry.announce.sent, entry.announce.actual);
            } else {
                assert_eq!(entry.announce.sent.uploaded, 0);
                assert_eq!(entry.announce.sent.downloaded, 0);
                assert_eq!(entry.announce.sent.left, target.payload.len() as u64);
                assert_ne!(entry.announce.event.as_deref(), Some("completed"));
            }
            checked_announces += 1;
        }
        let snapshot = call(wire::Method::List {
            offset: 0,
            limit: 2,
            revision: None,
            after_revision: None,
        });
        assert_eq!(snapshot["torrents"].as_array().unwrap().len(), 2);
        record(
            &report,
            serde_json::json!({"kind":"cycle","cycle":cycle,"elapsed_ms":start.elapsed().as_millis(),
            "damaged_pieces":damaged,"repair_fetched_bytes":fetched,"repair_downloading_observed":saw_download,
            "repair_checking_observed":saw_check,"repair_verified":true,"engine_identity_unchanged":true,
            "listener_unchanged":true,"anchor_connections":1,"mode":mode.id(),"activity":proof.snapshot(),"torrents":snapshot["torrents"]}),
        );
        let sample = resources();
        if cycle == 2 {
            baseline = Some(([sample.0, sample.1, sample.2], sampler.peaks()));
        }
        if let Some((idle, peak)) = baseline {
            bounds([sample.0, sample.1, sample.2], idle);
            bounds(sampler.peaks(), peak);
        }
        sample_record(
            &report,
            root.path(),
            start,
            cycle,
            &proof,
            sampler.latency.load(Ordering::Relaxed),
        );
        cycle += 1;
        sampler.cycle.store(cycle, Ordering::Relaxed);
        phase.store(1, Ordering::Relaxed);
        while cycle_start.elapsed() < Duration::from_secs(10) {
            anchor.healthy();
            proof.healthy();
            thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(cycle >= 2 && repaired_pieces >= 2 && checked_announces > 0);
    anchor.finish();
    proof.healthy();
    assert_eq!(proof.anchor_connections.load(Ordering::Relaxed), 1);
    assert!(proof.uploaded.load(Ordering::Relaxed) >= cycle * target.payload.len() as u64);
    let wire_downloaded = proof.downloaded.load(Ordering::Relaxed);
    let wire_uploaded =
        proof.uploaded.load(Ordering::Relaxed) + proof.anchor_uploaded.load(Ordering::Relaxed);
    let mut totals = (0, 0);
    wait(&proof, || {
        let snapshot = call(wire::Method::List {
            offset: 0,
            limit: 2,
            revision: None,
            after_revision: None,
        });
        let rows = snapshot["torrents"].as_array().unwrap();
        totals = (
            rows.iter()
                .map(|r| r["network_downloaded_bytes"].as_u64().unwrap())
                .sum::<u64>(),
            rows.iter()
                .map(|r| r["network_uploaded_bytes"].as_u64().unwrap())
                .sum::<u64>(),
        );
        totals.0 >= wire_downloaded && totals.1 >= wire_uploaded
    });
    assert!(host.error.lock().unwrap().is_none());
    assert!(journal.error().is_none());
    if let Some((_, peak)) = baseline {
        bounds(sampler.peaks(), peak);
    }
    let peaks = sampler.peaks();
    phase.store(3, Ordering::Relaxed);
    drop(sampler);
    let workload_elapsed_ms = start.elapsed().as_millis();
    drop(host);
    drop(owner);
    assert!(
        engine_identity.upgrade().is_none(),
        "engine did not shut down"
    );
    drop(servers);
    proof.healthy();
    let exported_announces = journal
        .export(&destination.parent().unwrap().join("announces.jsonl"), None)
        .unwrap();
    assert!(exported_announces > 0);
    record(
        &report,
        serde_json::json!({"kind":"result","completed":true,"workload":"continuous_session",
        "requested_seconds":seconds,"elapsed_ms":workload_elapsed_ms,"cycles":cycle,"repaired_pieces":repaired_pieces,
        "session_starts":1,"session_identity_unchanged":true,"fixed_torrents":2,"permanent_seeding_connections":1,
        "mode":mode.id(),"checked_announces":checked_announces,"exported_announces":exported_announces,
        "sampled_peaks":peaks,"network_downloaded_bytes":totals.0,"network_uploaded_bytes":totals.1,
        "activity":proof.snapshot(),"clean_shutdown":true}),
    );
}

#[test]
fn fault_rotation_covers_every_piece_and_never_duplicates_a_fault() {
    let mut seen = std::collections::HashSet::new();
    for cycle in 0..64 {
        let indices = faults(cycle, 32);
        assert_eq!(
            indices.len(),
            indices
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert!(indices.iter().all(|i| *i < 32));
        seen.extend(indices);
    }
    assert_eq!(seen.len(), 32);
}
#[test]
fn repair_evidence_rejects_refetching_healthy_data_and_missing_faults() {
    assert_eq!(
        check_fetches(&[0, 0], &[PIECE as u64, 0], &[0]),
        PIECE as u64
    );
    assert!(std::panic::catch_unwind(|| check_fetches(&[0, 0], &[0, 0], &[0])).is_err());
    assert!(std::panic::catch_unwind(|| check_fetches(&[0, 0], &[PIECE as u64, 1], &[0])).is_err());
}
#[test]
fn resource_growth_accepts_exact_limits_and_rejects_each_excess() {
    let budgets = [192 * 1024 * 1024, 32, 64];
    bounds(budgets, [0; 3]);
    for i in 0..3 {
        let mut excessive = budgets;
        excessive[i] += 1;
        assert!(std::panic::catch_unwind(|| bounds(excessive, [0; 3])).is_err());
    }
}
