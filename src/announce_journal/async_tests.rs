use super::*;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "synthetic journal condition did not arrive"
        );
        std::thread::yield_now();
    }
}

#[test]
fn slow_writer_cannot_hold_live_views_and_queue_overflow_is_explicit() {
    let root = tempfile::tempdir().unwrap();
    let gate = Arc::new(writer::TestGate::default());
    let journal = AnnounceJournal::open_with_options(
        root.path(),
        JournalOptions {
            queue_capacity: 1,
            gate: Some(gate.clone()),
            ..Default::default()
        },
    );
    journal.record(tests::observation("a"));
    wait_until(|| gate.entered.load(Ordering::Acquire));
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = journal.clone();
    let worker = std::thread::spawn(move || {
        sender.send(reader.page(None, 0, 32).total).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(Duration::from_millis(500)).unwrap(),
        1
    );
    worker.join().unwrap();
    journal.record(tests::observation("a"));
    journal.record(tests::observation("a"));
    assert_eq!(journal.entries(None).len(), 3);
    assert_eq!(journal.status().accepted_sequence, 2);
    assert_eq!(journal.status().rejected_records, 1);
    assert!(journal.error().unwrap().contains("admission queue"));
    gate.release();
    wait_until(|| journal.status().flushed_sequence == 2);
    assert!(journal.finish().is_err());
    assert!(
        journal
            .export(&root.path().join("incomplete.jsonl"), None)
            .is_err()
    );
    assert!(AnnounceJournal::reopen(&journal.manifest_path().unwrap()).is_err());
}

#[test]
fn rotation_exports_one_complete_sequence_and_reopens_after_clean_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let journal = AnnounceJournal::open_with_options(
        root.path(),
        JournalOptions {
            segment_bytes: 1024,
            ..Default::default()
        },
    );
    for _ in 0..20 {
        journal.record(tests::observation("a"));
    }
    journal.flush().unwrap();
    let status = journal.status();
    assert_eq!(
        (
            status.accepted_sequence,
            status.written_sequence,
            status.flushed_sequence,
            status.durable_sequence
        ),
        (20, 20, 20, 20)
    );
    assert!(status.segments > 1);
    assert_eq!(status.queued_entries, 0);
    assert!(
        AnnounceJournal::reopen(&journal.manifest_path().unwrap()).is_err(),
        "an active manifest cannot prove clean shutdown"
    );
    journal.finish().unwrap();
    let restored = AnnounceJournal::reopen(&journal.manifest_path().unwrap()).unwrap();
    assert_eq!(restored.run_id(), journal.run_id());
    let output = root.path().join("all.jsonl");
    assert_eq!(restored.export(&output, None).unwrap(), 20);
    let records: Vec<JournalEntry> = std::fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        (1..=20).collect::<Vec<_>>()
    );
    assert!(records.iter().all(|entry| entry.run_id == journal.run_id()));
}

#[test]
fn concurrent_producers_and_drop_drain_all_admitted_records() {
    let root = tempfile::tempdir().unwrap();
    let journal = AnnounceJournal::open(root.path());
    let manifest = journal.manifest_path().unwrap();
    let mut workers = Vec::new();
    for _ in 0..4 {
        let producer = journal.clone();
        workers.push(std::thread::spawn(move || {
            for _ in 0..100 {
                producer.record(tests::observation("a"));
            }
        }));
    }
    for _ in 0..1000 {
        let status = journal.status();
        assert!(
            status.durable_sequence <= status.flushed_sequence
                && status.flushed_sequence <= status.written_sequence
                && status.written_sequence <= status.accepted_sequence
        );
        std::thread::yield_now();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(journal.status().accepted_sequence, 400);
    drop(journal);
    let restored = AnnounceJournal::reopen(&manifest).unwrap();
    assert_eq!(
        restored
            .export(&root.path().join("all.jsonl"), None)
            .unwrap(),
        400
    );
}

#[test]
fn interrupted_record_and_missing_rotated_segment_are_never_successful_history() {
    for missing in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let journal = AnnounceJournal::open_with_options(
            root.path(),
            JournalOptions {
                segment_bytes: 1024,
                ..Default::default()
            },
        );
        for _ in 0..4 {
            journal.record(tests::observation("a"));
        }
        journal.finish().unwrap();
        let path = journal.manifest_path().unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let segment = manifest["segments"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        let payload = root.path().join(segment["file_name"].as_str().unwrap());
        if missing {
            std::fs::remove_file(payload).unwrap();
        } else {
            let length = segment["length"].as_u64().unwrap() - 1;
            OpenOptions::new()
                .write(true)
                .open(payload)
                .unwrap()
                .set_len(length)
                .unwrap();
            segment["length"] = serde_json::json!(length);
            std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        }
        assert!(AnnounceJournal::reopen(&path).is_err());
    }
}

#[test]
fn partial_write_keeps_live_diagnostics_and_marks_the_manifest_incomplete() {
    let root = tempfile::tempdir().unwrap();
    let journal = AnnounceJournal::open_with_options(
        root.path(),
        JournalOptions {
            fail_sequence: Some(2),
            ..Default::default()
        },
    );
    for _ in 0..3 {
        journal.record(tests::observation("a"));
    }
    wait_until(|| journal.error().is_some());
    assert_eq!(journal.entries(None).len(), 3);
    assert_eq!(journal.status().flushed_sequence, 1);
    assert!(journal.finish().is_err());
    assert!(
        journal
            .export(&root.path().join("incomplete.jsonl"), None)
            .is_err()
    );
    assert!(AnnounceJournal::reopen(&journal.manifest_path().unwrap()).is_err());
}

#[test]
fn a_timed_out_barrier_is_not_fabricated_success_and_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let gate = Arc::new(writer::TestGate::default());
    let journal = AnnounceJournal::open_with_options(
        root.path(),
        JournalOptions {
            gate: Some(gate.clone()),
            queue_capacity: 1,
            barrier_timeout: Duration::from_millis(30),
            ..Default::default()
        },
    );
    journal.record(tests::observation("a"));
    wait_until(|| gate.entered.load(Ordering::Acquire));
    assert!(journal.flush().unwrap_err().contains("timed out"));
    assert_eq!(journal.status().flushed_sequence, 0);
    assert!(
        journal.error().is_none(),
        "a timed-out caller does not corrupt the writer"
    );
    // The original fence still occupies the one-slot queue. Repeated 30 ms
    // flush calls would add serial fsync work after their receivers time out.
    // Retry once under the same overall recovery budget, including admission.
    let deadline = Instant::now() + Duration::from_secs(5);
    let disk = journal.disk.as_ref().unwrap().clone();
    let (started, entered) = mpsc::sync_channel(1);
    let (reply, recovered) = mpsc::sync_channel(1);
    let recovery = std::thread::spawn(move || {
        started.send(()).unwrap();
        reply.send(disk.snapshot_until(None, deadline)).unwrap();
    });
    entered
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap();
    assert!(
        matches!(
            recovered.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "a queued recovery fence must not succeed while the writer is blocked"
    );
    assert_eq!(journal.status().durable_sequence, 0);
    gate.release();
    let snapshot = recovered
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect(
            "released journal recovery fence did not reply within its existing five-second budget",
        )
        .expect("released journal recovery fence did not complete");
    recovery.join().unwrap();
    assert_eq!(snapshot.stream(&mut Vec::new(), None, |_| {}).unwrap(), 1);
    assert_eq!(journal.status().flushed_sequence, 1);
    assert_eq!(journal.status().durable_sequence, 1);
}

#[test]
fn frontend_projection_preserves_owner_identity_and_cannot_write_or_export_locally() {
    let owner = AnnounceJournal::memory();
    for _ in 0..3 {
        owner.record(tests::observation("a"));
    }
    let entries: Vec<JournalEntry> = owner
        .page(None, 0, 32)
        .entries
        .iter()
        .map(|entry| (**entry).clone())
        .collect();
    let view = AnnounceJournal::frontend_view();
    view.replace_frontend_entries(entries, Some(PathBuf::from("owner-segment.jsonl")), None)
        .unwrap();
    assert_eq!(view.run_id(), owner.run_id());
    assert_eq!(
        view.entries(None)
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let root = tempfile::tempdir().unwrap();
    assert!(view.export(&root.path().join("local.jsonl"), None).is_err());
    let mut duplicates = owner
        .entries(None)
        .iter()
        .map(|entry| (**entry).clone())
        .collect::<Vec<_>>();
    duplicates.push(duplicates[0].clone());
    assert!(
        view.replace_frontend_entries(duplicates, None, None)
            .is_err()
    );
    view.record(tests::observation("a"));
    assert_eq!(view.entries(None).len(), 3);
    assert!(view.error().is_some());
}

#[test]
fn manifest_paths_are_confined_to_generated_segment_basenames() {
    let root = tempfile::tempdir().unwrap();
    let journal = AnnounceJournal::open(root.path());
    journal.record(tests::observation("a"));
    journal.finish().unwrap();
    let path = journal.manifest_path().unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["segments"][0]["file_name"] = serde_json::json!("../foreign.jsonl");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(AnnounceJournal::reopen(&path).is_err());
}

#[test]
fn a_closed_rotated_segment_cannot_silently_grow_behind_a_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let journal = AnnounceJournal::open_with_options(
        root.path(),
        JournalOptions {
            segment_bytes: 1024,
            ..Default::default()
        },
    );
    for _ in 0..8 {
        journal.record(tests::observation("a"));
    }
    journal.flush().unwrap();
    assert!(journal.status().segments > 1);
    let mut first = OpenOptions::new()
        .append(true)
        .open(journal.path().unwrap())
        .unwrap();
    first.write_all(b"unexpected trailing bytes\n").unwrap();
    first.flush().unwrap();
    assert!(
        journal
            .export(&root.path().join("changed.jsonl"), None)
            .is_err()
    );
    journal.finish().unwrap();
    assert!(AnnounceJournal::reopen(&journal.manifest_path().unwrap()).is_err());
}
