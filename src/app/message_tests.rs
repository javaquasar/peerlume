//! The real app polling path retains a cause even if the old status line changes.
#[test]
fn navigation_failure_survives_a_later_unrelated_status_message() {
    let mut app = super::torrent_panes::tests::fixture(1, 1);
    let root = tempfile::tempdir().unwrap();
    app.add_status = app
        .file_navigation
        .reveal_source(&root.path().join("missing.torrent"));
    app.poll_file_navigation();
    assert_eq!(app.operation_tracker.history.failed_count(), 1);
    let first = app.operation_tracker.history.entries().next().unwrap().id;
    assert!(
        app.operation_tracker
            .history
            .get(first)
            .unwrap()
            .message
            .contains("Source .torrent file is not on disk")
    );
    app.add_status = "Later unrelated operation succeeded".into();
    app.poll_file_navigation();
    assert_eq!(app.operation_tracker.history.failed_count(), 1);
    assert!(
        app.operation_tracker
            .history
            .get(first)
            .unwrap()
            .message
            .contains("Source .torrent file is not on disk")
    );
    assert_eq!(app.add_status, "Later unrelated operation succeeded");
}
