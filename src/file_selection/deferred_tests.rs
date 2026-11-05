use super::*;

#[test]
fn cold_models_release_paths_tree_and_search_while_preserving_authoritative_arithmetic() {
    let mut model = FileSelectionModel::from_snapshot((0..2048).map(|index| {
        (
            format!(
                "season-{}/episode-{index:04}-{}.mkv",
                index / 20,
                "x".repeat(80)
            ),
            100,
            25,
            index % 2 == 0,
        )
    }));
    model.set_high_priority(&[0, 17, 2047]);
    let _ = model.view_rows("episode", 2);
    let before = model.memory_usage();
    let summary = model.summary();
    assert!(before.path_capacity_bytes > 200_000);
    assert!(before.tree_capacity_bytes > 0 && before.view_capacity_bytes > 0);
    assert!(model.defer_details());
    assert!(!model.defer_details());
    let after = model.memory_usage();
    assert_eq!(model.summary(), summary);
    assert_eq!(model.high_priority(), vec![0, 17, 2047]);
    assert!(model.files().is_empty() && model.rows().is_empty());
    assert_eq!(
        (
            after.file_slots_bytes,
            after.path_capacity_bytes,
            after.tree_capacity_bytes,
            after.view_capacity_bytes
        ),
        (0, 0, 0, 0)
    );
    assert!(after.total_bytes() < before.total_bytes() / 4);
    assert_eq!(model.selected_indices().unwrap().len(), 1024);
    model.set_selected(1, true);
    model.set_file_downloaded(1, 70);
    assert_eq!(model.selected_size(), 102_500);
    assert_eq!(model.selected_downloaded(), 25_670);
    assert_eq!(model.file_state(1).unwrap().downloaded_bytes, 70);
}

#[test]
fn hydration_merges_latest_compact_selection_and_progress_without_stale_db_counters() {
    let mut model =
        FileSelectionModel::from_compact_snapshot([(100, 10, true), (200, 20, false)], &[1]);
    model.set_file_downloaded(0, 90);
    model.set_selected(1, true);
    model.set_file_downloaded(1, 150);
    let before = model.summary();
    assert!(model.hydrate_paths(vec!["only-one".into()]).is_err());
    assert!(
        model
            .hydrate_paths(vec!["../unsafe".into(), "ok".into()])
            .is_err()
    );
    assert!(!model.details_loaded());
    model
        .hydrate_paths(vec!["folder/a".into(), "folder/b".into()])
        .unwrap();
    assert!(model.details_loaded());
    assert_eq!(model.summary(), before);
    assert_eq!(model.files()[0].downloaded_bytes, 90);
    assert_eq!(model.files()[1].downloaded_bytes, 150);
    assert!(model.files()[1].selected && model.is_high_priority(1));
}

#[test]
fn summary_mirrors_cannot_manufacture_empty_authoritative_selection_or_persistence() {
    let summary = FileSummary {
        file_count: 500_000,
        total_size: 10_000,
        selected_count: 123,
        selected_size: 9000,
        selected_downloaded: 4500,
    };
    let mut model = FileSelectionModel::from_summary(summary);
    assert_eq!(model.file_count(), 500_000);
    assert_eq!(model.summary(), summary);
    assert_eq!(model.memory_usage().total_bytes(), 0);
    assert_eq!(model.selected_indices(), None);
    assert_eq!(model.persistence_snapshot(true), None);
    assert_eq!(
        model.selection_after_edit(Some(true), &BTreeMap::new()),
        None
    );
    assert!(!model.set_selected(0, false));
    let mut invalid = summary;
    invalid.selected_downloaded = invalid.selected_size + 1;
    assert!(model.apply_summary(invalid).is_err());
    assert_eq!(model.summary(), summary);
}

#[test]
fn compact_snapshots_update_loaded_models_and_reject_mismatched_layout_atomically() {
    let mut source =
        FileSelectionModel::from_compact_snapshot([(100, 70, false), (200, 50, true)], &[1]);
    let snapshot = source.persistence_snapshot(true).unwrap();
    let mut mirror = FileSelectionModel::from_files([("a".into(), 100), ("b".into(), 200)]);
    mirror.apply_persistence_snapshot(&snapshot).unwrap();
    assert_eq!(mirror.selected_indices(), Some(vec![1]));
    assert_eq!(mirror.summary(), source.summary());
    assert_eq!(mirror.high_priority(), vec![1]);
    source.set_file_downloaded(1, 100);
    let before = mirror.summary();
    let mut invalid = snapshot;
    invalid.file_count = 3;
    assert!(mirror.apply_persistence_snapshot(&invalid).is_err());
    assert_eq!(mirror.summary(), before);
}

#[test]
fn checked_total_does_not_turn_overflowed_metadata_into_a_valid_saturated_size() {
    let mut loaded = FileSelectionModel::from_files([("a".into(), u64::MAX), ("b".into(), 1)]);
    assert_eq!(loaded.total_size(), u64::MAX);
    assert_eq!(loaded.checked_total_size(), None);
    loaded.defer_details();
    assert_eq!(loaded.checked_total_size(), None);
    loaded.set_selected(1, false);
    loaded.set_selected(1, true);
    assert_eq!(loaded.checked_total_size(), None);
}
