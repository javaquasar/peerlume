//! Realistic, streamed single-engine workload with concurrent downloads and faults.
use super::{
    soak_tests::{Report, Sampler, disk_sizes, record, resources},
    *,
};
use crate::{
    announce_journal::{AnnounceJournal, JournalEntry},
    completion::CompletionStatus,
    download_mode::DownloadMode,
    runtime_status::{RepairPolicy, RepairStatus},
    session::SessionRuntime,
    state::StateStore,
    torrent::TorrentState,
};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};
mod data;
mod network;
use data::{BLOCK, Data, PIECE};
use network::{Anchor, Fault, Leecher, Proof, Servers};

fn wait(proof: &Proof, seconds: u64, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !ready() {
        proof.healthy();
        assert!(Instant::now() < deadline, "realistic swarm phase timed out");
        thread::sleep(Duration::from_millis(20));
    }
}
fn ready(owner: &Controller, id: &str) -> bool {
    let item = owner
        .torrents
        .iter()
        .find(|t| t.info_hashes.join(";") == id)
        .unwrap();
    assert!(
        !matches!(
            item.state,
            TorrentState::Error | TorrentState::VerificationFailed
        ),
        "torrent failed: {:?}",
        item.state
    );
    item.completion == CompletionStatus::Verified && item.state == TorrentState::Seeding
}
fn item(root: &Path, index: usize, data: &Data, tracker: &str) -> crate::torrent::TorrentItem {
    let source = root.join(format!("fixture-{index}.torrent"));
    fs::write(&source, &*data.meta).unwrap();
    let mut item = crate::client::tests::torrent();
    item.name = data.name.clone();
    item.source = source.display().to_string();
    item.info_hashes = vec![format!("v1: {}", data.hash_hex())];
    item.is_private = true;
    item.trackers = vec![tracker.into()];
    item.files = crate::file_selection::FileSelectionModel::from_files(data.files.clone());
    item.overrides.download_directory =
        Some(root.join(format!("payloads/{index}")).display().to_string());
    item.overrides.download_mode = DownloadMode::Normal;
    item.overrides.verify_after_download = true;
    item.overrides.repair_policy = RepairPolicy {
        enabled: true,
        max_attempts: 3,
    };
    item.state = TorrentState::WaitingForEngine;
    item
}
fn sample(
    report: &Report,
    root: &Path,
    start: Instant,
    cycle: u64,
    proof: &Proof,
    latency: u64,
) -> [u64; 3] {
    let (rss, threads, handles) = resources();
    let (profile, database, journal) = disk_sizes(root);
    assert!(
        database <= 32 * 1024 * 1024 && journal <= 128 * 1024 * 1024,
        "profile exceeded its size budget"
    );
    record(
        report,
        serde_json::json!({"kind":"idle","elapsed_ms":start.elapsed().as_millis(),"cycle":cycle,"rss_bytes":rss,"threads":threads,"open_handles":handles,"profile_bytes":profile,"database_bytes":database,"journal_bytes":journal,"ipc_latency_us":latency,"activity":proof.snapshot()}),
    );
    [rss, threads, handles]
}
fn growth(sample: [u64; 3], base: [u64; 3]) {
    for (i, budget) in [192 * 1024 * 1024, 32, 64].into_iter().enumerate() {
        assert!(
            sample[i] <= base[i] + budget,
            "resource growth exceeded budget at {i}"
        );
    }
}
fn repair_fetches(before: &[u64], after: &[u64], piece: usize) -> u64 {
    assert_eq!(before.len(), after.len());
    let mut bytes = 0;
    for (index, (&a, &b)) in before.iter().zip(after).enumerate() {
        assert!(b >= a);
        if index == piece {
            assert!(b - a >= PIECE as u64, "damaged piece was not refetched");
            bytes += b - a;
        } else {
            assert_eq!(b - a, 0, "healthy piece was fetched during repair");
        }
    }
    bytes
}

#[test]
#[ignore = "streamed multi-torrent workload; dedicated GitHub smoke and four-hour runs"]
fn reliability_realistic_swarm_soak() {
    let seconds = std::env::var("PEERLUME_SWARM_SECONDS")
        .unwrap_or_else(|_| "180".into())
        .parse::<u64>()
        .unwrap();
    assert!((120..=14400).contains(&seconds));
    let profile = std::env::var("PEERLUME_SWARM_PROFILE").unwrap_or_else(|_| "smoke".into());
    assert!(matches!(profile.as_str(), "smoke" | "large"));
    let destination = PathBuf::from(
        std::env::var_os("PEERLUME_SWARM_REPORT").expect("new resource report path required"),
    );
    let report: Report = Arc::new(Mutex::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap(),
    ));
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("payloads")).unwrap();
    let size = if profile == "large" {
        2 * 1024 * 1024 * 1024u64
    } else {
        16 * 1024 * 1024
    };
    // Metadata generation is bounded-memory setup. The timed workload begins
    // immediately before starting its one real engine, not during fixture hashing.
    let data = vec![
        Data::new("large.bin", size + PIECE as u64, false, 17),
        Data::new("bundle", size, true, 29),
        Data::new("anchor.bin", PIECE as u64, false, 41),
    ];
    data[2]
        .write_files(&root.path().join("payloads/2"))
        .unwrap();
    for index in 0..2 {
        assert!(!root.path().join(format!("payloads/{index}")).exists());
    }
    let proof = Arc::new(Proof::new(&data));
    let servers = Servers::start(data.clone(), proof.clone());
    let journal = AnnounceJournal::open(&root.path().join("announce-logs"));
    let start = Instant::now();
    let runtime = SessionRuntime::start_test_with_journal(
        512,
        root.path().into(),
        librqbit::SessionOptions {
            dht: None,
            listen: Some(librqbit::ListenerOptions {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ipv4_only: true,
                ..Default::default()
            }),
            ipv4_only: true,
            concurrent_init_limit: Some(3),
            disable_local_service_discovery: true,
            ..Default::default()
        },
        journal.clone(),
    )
    .unwrap();
    wait(&proof, 20, || runtime.test_engine().is_some());
    let (engine_identity, address) = {
        let engine = runtime.test_engine().unwrap();
        (Arc::downgrade(&engine), engine.listen_addr().unwrap())
    };
    let observed = proof.clone();
    let sampler = Sampler::with_activity(report.clone(), root.path().into(), start, move || {
        observed.snapshot()
    });
    record(
        &report,
        serde_json::json!({"kind":"start","workload":"realistic_swarm","profile":profile,"requested_seconds":seconds,"mode":"normal","session_starts":1,"fixed_torrents":3,"piece_bytes":PIECE,"torrents":data.iter().map(|d|serde_json::json!({"info_hash":d.hash_hex(),"bytes":d.bytes,"files":d.files})).collect::<Vec<_>>(),"fixture_payload_template_bytes":3*BLOCK}),
    );
    let mut controller = Controller::new(
        ClientBackend::with_session_and_store(
            runtime,
            StateStore::open(&root.path().join("state.sqlite3")).unwrap(),
        ),
        vec![],
    );
    let mut ids = Vec::new();
    for (index, torrent) in data.iter().enumerate() {
        let item = item(root.path(), index, torrent, &servers.tracker_url);
        ids.push(item.info_hashes.join(";"));
        controller
            .add(item, &crate::settings::ClientSettings::default(), None)
            .unwrap();
    }
    controller
        .configure_agents(root.path(), root.path().into())
        .unwrap();
    let service = controller.agent_access().unwrap();
    service.enabled = true;
    let token = service
        .grant(access::Grants {
            manage: true,
            ..Default::default()
        })
        .unwrap();
    let instance = service.instance_id.clone();
    let _instance = host::InstanceLock::acquire(root.path()).unwrap();
    let owner = Arc::new(Mutex::new(controller));
    let host = host::Host::start(owner.clone(), |c| c, root.path().into()).unwrap();
    let ipc = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request_id = 0;
    let mut call = |method| {
        request_id += 1;
        let sent = Instant::now();
        let request = wire::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("swarm-{request_id}"),
            token: token.clone(),
            instance_id: Some(instance.clone()),
            method,
        };
        let reply = loop {
            match ipc.block_on(host::call(root.path(), &request)) {
                Ok(r) => break r,
                Err(_) if sent.elapsed() < Duration::from_secs(5) => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("IPC failed: {e:?}"),
            }
        };
        assert!(reply.error.is_none(), "IPC rejected: {:?}", reply.error);
        assert!(sent.elapsed() < Duration::from_secs(5));
        sampler
            .latency
            .store(sent.elapsed().as_micros() as u64, Ordering::Relaxed);
        reply.result.unwrap()
    };
    wait(&proof, 900, || {
        let c = owner.lock().unwrap();
        ids.iter().all(|id| ready(&c, id))
    });
    for (index, torrent) in data.iter().enumerate() {
        torrent
            .verify_files(&root.path().join(format!("payloads/{index}")))
            .unwrap();
    }
    assert!(proof.overlapping.load(Ordering::Relaxed));
    for index in 0..2 {
        assert!(
            proof
                .endpoints
                .lock()
                .unwrap()
                .iter()
                .filter(|(i, _)| *i == index)
                .count()
                >= 2,
            "download did not use two complementary peers"
        );
    }
    let anchor = Anchor::start(address, data[2].clone(), proof.clone());
    wait(&proof, 20, || proof.uploads[2].load(Ordering::Relaxed) > 0);
    record(
        &report,
        serde_json::json!({"kind":"initial_download","elapsed_ms":start.elapsed().as_millis(),"verified":true,"parallel_download_proven":true,"activity":proof.snapshot()}),
    );
    let mut cycle = 0u64;
    let mut baseline = None;
    let mut repaired = 0u64;
    let mut expected_failures = Vec::new();
    while cycle < 3 || start.elapsed() < Duration::from_secs(seconds) {
        let cycle_start = Instant::now();
        proof.healthy();
        anchor.healthy();
        // A failed regular announce must be visible, followed by a real
        // successful retry while the same engine and control connection live.
        let fault = [Fault::Unavailable, Fault::DroppedReply, Fault::Timeout][cycle as usize % 3];
        let before_sequence = journal
            .entries(None)
            .last()
            .map(|e| e.sequence)
            .unwrap_or(0);
        let fault_before = proof.tracker_faults[fault.index()].load(Ordering::Relaxed);
        proof.arm(fault, 0);
        wait(&proof, 60, || {
            proof.tracker_faults[fault.index()].load(Ordering::Relaxed) > fault_before
        });
        let mut failed_sequence = 0;
        wait(&proof, 60, || {
            let entries = journal.entries(Some(&data[0].hash_hex()));
            if let Some(entry) = entries
                .iter()
                .find(|e| e.sequence > before_sequence && e.announce.outcome == fault.outcome())
            {
                failed_sequence = entry.sequence;
                true
            } else {
                false
            }
        });
        wait(&proof, 60, || {
            journal
                .entries(Some(&data[0].hash_hex()))
                .iter()
                .any(|e| e.sequence > failed_sequence && e.announce.outcome == "ok")
        });
        expected_failures.push(failed_sequence);
        record(
            &report,
            serde_json::json!({"kind":"fault_recovery","cycle":cycle,"elapsed_ms":start.elapsed().as_millis(),"fault":fault.name(),"failed_sequence":failed_sequence,"recovered":true,"activity":proof.snapshot()}),
        );
        // Every receiver connection is new; offsets rotate through each large
        // torrent, including its tail beyond the signed 32-bit offset boundary.
        for (index, torrent) in data.iter().enumerate().take(2) {
            let mut leecher =
                Leecher::connect(address, torrent.clone(), proof.clone(), index).unwrap();
            let blocks = torrent.bytes / BLOCK as u64;
            for block in 0..128u64 {
                let offset = ((cycle * 131 + block) % blocks) * BLOCK as u64;
                leecher.block(offset).unwrap();
            }
            leecher.block((blocks - 1) * BLOCK as u64).unwrap();
        }
        let before_anchor = proof.uploads[2].load(Ordering::Relaxed);
        proof.epoch.fetch_add(1, Ordering::Relaxed);
        proof.truncate_next.store(true, Ordering::Relaxed);
        let mut repair_evidence = Vec::new();
        for (index, torrent) in data.iter().enumerate().take(2) {
            let path = root.path().join(format!("payloads/{index}"));
            let boundary = torrent.files[0].1;
            let piece = if index == 1 && cycle == 0 {
                (boundary / PIECE as u64) as usize
            } else {
                (cycle as usize * 17 + index * 7) % torrent.pieces()
            };
            let offsets = if index == 1 && cycle == 0 {
                vec![boundary - 1, boundary + 1]
            } else {
                vec![piece as u64 * PIECE as u64 + 17]
            };
            let before = proof.counts(index);
            torrent.corrupt(&path, &offsets).unwrap();
            assert!(torrent.verify_files(&path).is_err());
            let revision = owner.lock().unwrap().command_revision(&ids[index]).unwrap();
            let operation = call(wire::Method::Mutate {
                torrent_id: ids[index].clone(),
                command: TorrentCommand::Recheck,
                expected_revision: revision,
                idempotency_key: format!("swarm-repair-{cycle}-{index}"),
                deadline_unix_ms: operations::now_ms() + 900000,
            })["operation_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut downloading = false;
            let mut checking = false;
            wait(&proof, 900, || {
                anchor.healthy();
                let c = owner.lock().unwrap();
                let t = c
                    .torrents
                    .iter()
                    .find(|t| t.info_hashes.join(";") == ids[index])
                    .unwrap();
                downloading |= matches!(t.runtime.repair, RepairStatus::Downloading { .. });
                checking |=
                    downloading && matches!(t.runtime.repair, RepairStatus::Checking { .. });
                assert!(!matches!(t.runtime.repair, RepairStatus::Exhausted { .. }));
                downloading
                    && checking
                    && matches!(t.runtime.repair, RepairStatus::Succeeded { .. })
                    && ready(&c, &ids[index])
            });
            let operation = call(wire::Method::Operation {
                operation_id: operation,
            });
            assert!(
                matches!(operation["state"].as_str(), Some("succeeded" | "failed")),
                "recheck did not finish"
            );
            torrent.verify_files(&path).unwrap();
            let fetched = repair_fetches(&before, &proof.counts(index), piece);
            repaired += 1;
            repair_evidence.push(serde_json::json!({"info_hash":torrent.hash_hex(),"piece":piece,"damaged_offsets":offsets,"refetched_bytes":fetched,"repair_downloading_observed":downloading,"repair_checking_observed":checking,"repair_verified":true}));
        }
        assert!(
            !proof.truncate_next.load(Ordering::Relaxed),
            "truncated peer frame was not exercised"
        );
        assert!(proof.uploads[2].load(Ordering::Relaxed) > before_anchor);
        assert_eq!(proof.anchor_connections.load(Ordering::Relaxed), 1);
        {
            let c = owner.lock().unwrap();
            let engine = c.backend.test_engine().unwrap();
            assert!(Arc::downgrade(&engine).ptr_eq(&engine_identity));
            assert_eq!(engine.listen_addr(), Some(address));
            assert_eq!(engine.with_torrents(|ts| ts.count()), 3);
            assert!(ids.iter().all(|id| ready(&c, id)));
            for t in &c.torrents {
                assert_eq!(t.runtime.mode.active().unwrap().mode, DownloadMode::Normal);
            }
            assert!(
                c.backend
                    .load_torrents()
                    .unwrap()
                    .iter()
                    .all(|t| t.download_mode == DownloadMode::Normal)
            );
        }
        let snapshot = call(wire::Method::List {
            offset: 0,
            limit: 3,
            revision: None,
            after_revision: None,
        });
        record(
            &report,
            serde_json::json!({"kind":"cycle","cycle":cycle,"elapsed_ms":start.elapsed().as_millis(),"repairs":repair_evidence,"engine_identity_unchanged":true,"listener_unchanged":true,"mode":"normal","anchor_connections":1,"torrents":snapshot["torrents"],"activity":proof.snapshot()}),
        );
        let sample = sample(
            &report,
            root.path(),
            start,
            cycle,
            &proof,
            sampler.latency.load(Ordering::Relaxed),
        );
        if cycle == 2 {
            baseline = Some((sample, sampler.peaks()));
        }
        if let Some((idle, peak)) = baseline {
            growth(sample, idle);
            growth(sampler.peaks(), peak);
        }
        cycle += 1;
        sampler.cycle.store(cycle, Ordering::Relaxed);
        while cycle_start.elapsed() < Duration::from_secs(10) {
            anchor.healthy();
            thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(
        proof
            .tracker_faults
            .iter()
            .all(|v| v.load(Ordering::Relaxed) > 0)
    );
    assert!(proof.truncated_frames.load(Ordering::Relaxed) > 0);
    assert!(proof.source_disconnects.load(Ordering::Relaxed) > 0);
    anchor.finish();
    let mut network_totals = (0u64, 0u64);
    let download_floor = data[0].bytes + data[1].bytes + repaired * PIECE as u64;
    let uploads: u64 = proof
        .uploads
        .iter()
        .map(|v| v.load(Ordering::Relaxed))
        .sum();
    wait(&proof, 30, || {
        let snapshot = call(wire::Method::List {
            offset: 0,
            limit: 3,
            revision: None,
            after_revision: None,
        });
        let rows = snapshot["torrents"].as_array().unwrap();
        network_totals = (
            rows.iter()
                .map(|r| r["network_downloaded_bytes"].as_u64().unwrap())
                .sum(),
            rows.iter()
                .map(|r| r["network_uploaded_bytes"].as_u64().unwrap())
                .sum(),
        );
        network_totals.0 >= download_floor && network_totals.1 >= uploads
    });
    assert!(host.error.lock().unwrap().is_none());
    assert!(journal.error().is_none());
    if let Some((_, peak)) = baseline {
        growth(sampler.peaks(), peak);
    }
    let peaks = sampler.peaks();
    drop(sampler);
    let elapsed = start.elapsed().as_millis();
    drop(host);
    drop(owner);
    assert!(engine_identity.upgrade().is_none(), "engine did not stop");
    drop(servers);
    proof.healthy();
    let exported = journal
        .export(&destination.parent().unwrap().join("announces.jsonl"), None)
        .unwrap();
    let mut lifecycles = HashMap::<String, (usize, usize, usize, Option<String>, u64, u64)>::new();
    let mut parsed = 0;
    for line in BufReader::new(
        fs::File::open(destination.parent().unwrap().join("announces.jsonl")).unwrap(),
    )
    .lines()
    {
        let entry: JournalEntry = serde_json::from_str(&line.unwrap()).unwrap();
        parsed += 1;
        assert_eq!(entry.sequence, parsed);
        let a = entry.announce;
        if a.outcome != "ok" {
            assert!(
                expected_failures.contains(&parsed),
                "unexpected announce failure: {} at {}",
                a.outcome,
                parsed
            );
        }
        if a.event.as_deref() == Some("stopped") {
            assert_eq!(
                a.outcome, "ok",
                "final/lifecycle stopped must have a real successful reply"
            );
        }
        assert_eq!(a.mode, "normal");
        assert_eq!(a.actual, a.sent);
        let summary = lifecycles.entry(a.info_hash).or_default();
        assert!(
            a.actual.downloaded >= summary.4 && a.actual.uploaded >= summary.5,
            "announce counters regressed"
        );
        summary.4 = a.actual.downloaded;
        summary.5 = a.actual.uploaded;
        if a.outcome == "ok" {
            summary.0 += usize::from(a.event.as_deref() == Some("completed"));
            summary.1 += usize::from(a.event.as_deref() == Some("stopped"));
        } else {
            summary.2 += 1;
        }
        summary.3 = a.event;
    }
    assert_eq!(parsed, exported);
    assert_eq!(lifecycles.len(), 3);
    for (index, torrent) in data.iter().enumerate() {
        let summary = &lifecycles[&torrent.hash_hex()];
        assert_eq!(summary.0, usize::from(index < 2));
        assert!(summary.1 > 0);
        assert_eq!(summary.3.as_deref(), Some("stopped"));
    }
    assert_eq!(lifecycles[&data[0].hash_hex()].2, expected_failures.len());
    record(
        &report,
        serde_json::json!({"kind":"result","completed":true,"workload":"realistic_swarm","profile":profile,"mode":"normal","requested_seconds":seconds,"elapsed_ms":elapsed,"cycles":cycle,"repaired_pieces":repaired,"session_starts":1,"session_identity_unchanged":true,"fixed_torrents":3,"permanent_seeding_connections":1,"clean_shutdown":true,"initial_download_verified":true,"parallel_download_proven":true,"network_downloaded_bytes":network_totals.0,"verified_download_floor_bytes":download_floor,"network_uploaded_bytes":network_totals.1,"exported_announces":exported,"lifecycle_counts":lifecycles,"expected_failed_sequences":expected_failures,"sampled_peaks":peaks,"activity":proof.snapshot()}),
    );
}

#[test]
fn repair_accounting_rejects_missing_faults_healthy_refetches_and_counter_regression() {
    assert_eq!(repair_fetches(&[0, 0], &[PIECE as u64, 0], 0), PIECE as u64);
    for after in [[0, 0], [PIECE as u64, 1]] {
        assert!(std::panic::catch_unwind(|| repair_fetches(&[0, 0], &after, 0)).is_err());
    }
    assert!(std::panic::catch_unwind(|| repair_fetches(&[1, 0], &[0, PIECE as u64], 1)).is_err());
}
