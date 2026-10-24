use super::*;
fn item() -> TorrentItem {
    let mut item = demo_torrents().remove(0);
    item.files = FileSelectionModel::from_files([("payload.bin".into(), 1000)]);
    item.downloaded_bytes = 0;
    item.uploaded_bytes = 0;
    item.completion = crate::completion::CompletionStatus::Pending;
    item
}
#[test]
fn progress_never_rounds_an_unfinished_download_to_done() {
    let mut t = item();
    t.downloaded_bytes = 999;
    assert_eq!(t.progress_text(), "99%");
    t.downloaded_bytes = u64::MAX;
    assert_eq!(t.progress(), 1.0);
    assert_eq!(t.progress_text(), "100%");
    assert!(!t.completed_successfully());
}
#[test]
fn only_verified_selected_data_in_a_terminal_activity_can_be_done() {
    let mut t = item();
    t.downloaded_bytes = 1000;
    t.completion = crate::completion::CompletionStatus::Verified;
    for state in [
        TorrentState::Downloading,
        TorrentState::WaitingForEngine,
        TorrentState::Checking,
        TorrentState::VerificationFailed,
        TorrentState::Error,
    ] {
        t.state = state;
        assert!(!t.completed_successfully());
    }
    for state in [TorrentState::Paused, TorrentState::Seeding] {
        t.state = state;
        assert!(t.completed_successfully());
        assert_eq!(t.progress_text(), "Done");
        assert!(t.status_text().starts_with("Done"));
    }
    t.files.select_all(false);
    assert!(!t.completed_successfully());
    assert_eq!(t.progress(), 0.0);
}
#[test]
fn eta_and_ratio_handle_zero_overflow_and_non_download_states() {
    let mut t = item();
    t.state = TorrentState::Downloading;
    t.download_rate = 100;
    assert_eq!(t.eta_seconds(), Some(10));
    t.downloaded_bytes = 1500;
    assert_eq!(t.eta_seconds(), Some(0));
    t.download_rate = 0;
    assert_eq!(t.eta_seconds(), None);
    t.download_rate = 100;
    t.state = TorrentState::Paused;
    assert_eq!(t.eta_seconds(), None);
    t.downloaded_bytes = 0;
    t.uploaded_bytes = u64::MAX;
    assert_eq!(t.ratio(), 0.0);
    t.downloaded_bytes = 1;
    assert!(t.ratio().is_finite());
}
#[test]
fn checking_progress_replaces_payload_percentage_without_changing_it() {
    let mut t = item();
    t.downloaded_bytes = 900;
    t.completion = crate::completion::CompletionStatus::Checking {
        checked_bytes: 20,
        total_bytes: 100,
    };
    assert!((t.display_progress() - 0.2).abs() < 0.001);
    assert!(t.progress_text().contains("20%"));
    assert_eq!(t.downloaded_bytes, 900);
    t.completion = crate::completion::CompletionStatus::Failed {
        message: "synthetic".into(),
    };
    assert_eq!(t.progress_text(), "Verification failed");
}
#[test]
fn effective_overrides_do_not_expand_private_discovery() {
    let g = ClientSettings::default();
    let o = TorrentOverrides {
        sequential_download: Some(true),
        download_directory: Some("isolated".into()),
        download_limit_kib: Some(1),
        upload_limit_kib: Some(2),
        max_peers: Some(3),
        ratio_limit: Some(4.0),
        seeding_time_limit_minutes: Some(5),
        ..Default::default()
    };
    let s = EffectiveTorrentSettings::resolve(&g, &o, true);
    assert_eq!(s.download_directory, "isolated");
    assert_eq!(s.piece_strategy, PieceStrategy::Sequential);
    assert_eq!(s.download_limit_kib, 1);
    assert_eq!(s.upload_limit_kib, 2);
    assert_eq!(s.max_peers, 3);
    assert_eq!(s.ratio_limit, Some(4.0));
    assert_eq!(s.seeding_time_limit_minutes, Some(5));
    assert!(!s.dht && !s.pex && !s.lsd);
}
