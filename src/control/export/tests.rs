use super::*;
fn output(root: &std::path::Path) -> File {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("output.jsonl"))
        .unwrap()
}
fn run_bytes(
    bytes: &[u8],
    hash: &str,
    deadline: u64,
) -> (tempfile::TempDir, Result<(), ErrorCode>, u64) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.jsonl"), bytes).unwrap();
    let input = File::open(root.path().join("input.jsonl")).unwrap();
    let count = AtomicU64::new(0);
    let result = stream(
        input,
        output(root.path()),
        bytes.len() as u64,
        hash,
        deadline,
        &count,
    );
    (root, result, count.load(Ordering::Relaxed))
}
#[test]
fn empty_journal_exports_an_empty_valid_file() {
    let (root, result, count) = run_bytes(b"", "hash", now_ms() + 10_000);
    result.unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::metadata(root.path().join("output.jsonl"))
            .unwrap()
            .len(),
        0
    );
}
#[test]
fn expired_export_stops_before_writing_any_entries() {
    let (root, result, count) = run_bytes(b"invalid record\n", "hash", 0);
    assert_eq!(result, Err(ErrorCode::DeadlineExceeded));
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::metadata(root.path().join("output.jsonl"))
            .unwrap()
            .len(),
        0
    );
}
#[test]
fn malformed_journal_record_is_not_passed_through_to_output() {
    let (root, result, count) = run_bytes(b"secret malformed record\n", "hash", now_ms() + 10_000);
    assert_eq!(result, Err(ErrorCode::PersistenceUnavailable));
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::read(root.path().join("output.jsonl")).unwrap(),
        b""
    );
}
#[test]
fn individual_journal_record_is_bounded_before_json_parsing() {
    let (_, result, count) = run_bytes(
        &vec![b'x'; crate::announce_journal::LIVE_BYTE_BUDGET + 1],
        "hash",
        now_ms() + 10_000,
    );
    assert_eq!(result, Err(ErrorCode::TooLarge));
    assert_eq!(count, 0);
}
#[test]
fn foreign_torrent_records_are_filtered_from_the_full_disk_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(root.path());
    for hash in ["a", "b", "a"] {
        journal.record(crate::announce_journal::tests::observation(hash));
    }
    let job = start(&journal, "a".into(), output(root.path()), now_ms() + 10_000).unwrap();
    job.receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(job.entries.load(Ordering::Relaxed), 2);
    let bytes = std::fs::read_to_string(root.path().join("output.jsonl")).unwrap();
    assert_eq!(bytes.lines().count(), 2);
}
#[test]
fn disk_snapshot_excludes_data_appended_after_its_length_was_captured() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(root.path());
    journal.record(crate::announce_journal::tests::observation("a"));
    let (input, length) = journal.disk_snapshot().unwrap();
    journal.record(crate::announce_journal::tests::observation("a"));
    let count = AtomicU64::new(0);
    stream(
        input,
        output(root.path()),
        length,
        "a",
        now_ms() + 10_000,
        &count,
    )
    .unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);
}
#[test]
fn memory_only_journal_cannot_claim_to_export_disk_history() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::memory();
    assert_eq!(
        start(&journal, "a".into(), output(root.path()), now_ms() + 10_000).err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
}
#[test]
fn removed_disk_journal_is_a_typed_failure() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(root.path());
    let path = journal.path().unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        journal.disk_snapshot().err().as_deref(),
        Some("Disk journal is unavailable")
    );
}
#[test]
fn oversized_journal_fails_without_reading_half_a_gigabyte() {
    let root = tempfile::tempdir().unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(root.path());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(journal.path().unwrap())
        .unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut returned = 0;
        // Preserve the large logical file without allocating half a GiB in CI.
        let result = unsafe {
            windows_sys::Win32::System::IO::DeviceIoControl(
                file.as_raw_handle(),
                windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            result, 0,
            "Sparse journal fixture requires a sparse-capable scratch filesystem"
        );
    }
    file.set_len(512 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        start(&journal, "a".into(), output(root.path()), now_ms() + 10_000).err(),
        Some(ErrorCode::TooLarge)
    );
}

#[test]
fn journal_creation_failure_is_not_a_successful_empty_export() {
    let root = tempfile::tempdir().unwrap();
    let blocker = root.path().join("not-a-directory");
    std::fs::write(&blocker, b"keep me").unwrap();
    let journal = crate::announce_journal::AnnounceJournal::open(&blocker);
    assert!(journal.error().is_some());
    assert_eq!(
        start(
            &journal,
            "hash".into(),
            output(root.path()),
            now_ms() + 10_000
        )
        .err(),
        Some(ErrorCode::PersistenceUnavailable)
    );
    assert_eq!(std::fs::read(blocker).unwrap(), b"keep me");
}
