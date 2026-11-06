use super::super::*;

#[test]
fn selection_and_priority_fences_change_on_actual_bits_not_counts_or_progress() {
    let mut model =
        FileSelectionModel::from_files([("folder/a".into(), 100), ("folder/b".into(), 100)]);
    let initial = model.metadata_revision();
    assert!(initial > 0);
    model.set_selected(0, true);
    model.set_high_priority(&[]);
    model.set_file_downloaded(0, 50);
    model.set_selected_downloaded_summary(75);
    assert_eq!(model.metadata_revision(), initial);
    model.set_selected(0, false);
    let changed = model.metadata_revision();
    assert!(changed > initial);
    model.set_selected(1, false);
    model.set_selected(0, true);
    assert_eq!(model.selected_count(), 1);
    assert!(model.metadata_revision() > changed);
    model.set_high_priority(&[1]);
    let high = model.metadata_revision();
    model.set_high_priority(&[1, 1, usize::MAX]);
    assert_eq!(model.metadata_revision(), high);
    let mut snapshot = model.persistence_snapshot(true).unwrap();
    snapshot.downloaded = vec![60, 70];
    model.apply_persistence_snapshot(&snapshot).unwrap();
    assert_eq!(model.metadata_revision(), high);
    snapshot.priority_bits[0] = 1;
    model.apply_persistence_snapshot(&snapshot).unwrap();
    assert!(model.metadata_revision() > high);
}

#[test]
fn unchanged_clones_share_a_fence_but_diverging_edits_and_replacement_are_unique() {
    let original = FileSelectionModel::from_files([("a".into(), 1), ("b".into(), 1)]);
    let before = original.metadata_revision();
    let mut first = original.clone();
    let mut second = original.clone();
    assert_eq!(first.metadata_revision(), before);
    first.set_selected(0, false);
    second.set_selected(1, false);
    assert!(first.metadata_revision() > before && second.metadata_revision() > before);
    assert_ne!(first.metadata_revision(), second.metadata_revision());
    assert_ne!(
        FileSelectionModel::from_files([("different".into(), 2)]).metadata_revision(),
        before
    );
}

#[test]
fn defer_and_db_hydration_preserve_fence_but_changed_known_geometry_invalidates_it() {
    let mut model = FileSelectionModel::from_files([("old/a".into(), 10), ("old/b".into(), 10)]);
    let initial = model.metadata_revision();
    model.defer_details();
    assert_eq!(model.metadata_revision(), initial);
    model
        .hydrate_paths(vec!["old/a".into(), "old/b".into()])
        .unwrap();
    assert_eq!(model.metadata_revision(), initial);
    model.defer_details();
    model
        .hydrate_paths(vec!["new/a".into(), "new/b".into()])
        .unwrap();
    assert!(model.metadata_revision() > initial);
    let mut database = FileSelectionModel::from_compact_snapshot([(10, 1, true)], &[]);
    let restored = database.metadata_revision();
    database.hydrate_paths(vec!["database/a".into()]).unwrap();
    assert_eq!(
        database.metadata_revision(),
        restored,
        "first authoritative DB path hydration is representation-only"
    );
}

#[test]
fn explicit_legacy_bulk_edit_detects_selection_swap_and_geometry_without_progress_churn() {
    let mut model =
        FileSelectionModel::from_snapshot([("a".into(), 10, 0, true), ("b".into(), 10, 0, false)]);
    let before = model.metadata_revision();
    {
        let (_, files) = model.rows_and_files_mut();
        files[0].downloaded_bytes = 4;
    }
    model.refresh_selection_totals();
    assert_eq!(model.metadata_revision(), before);
    {
        let (_, files) = model.rows_and_files_mut();
        files[0].selected = false;
        files[1].selected = true;
    }
    model.refresh_selection_totals();
    assert_eq!(model.selected_count(), 1);
    assert!(model.metadata_revision() > before);
    let changed = model.metadata_revision();
    {
        let (_, files) = model.rows_and_files_mut();
        files[0].path = "renamed/a".into();
    }
    model.refresh_selection_totals();
    assert!(model.metadata_revision() > changed);
    assert_eq!(
        model.hierarchy().unwrap().unwrap().folder(0).unwrap().name,
        "renamed"
    );
    let changed = model.metadata_revision();
    {
        let (_, files) = model.rows_and_files_mut();
        files[0].selected = true;
    }
    model.defer_details();
    assert!(
        model.metadata_revision() > changed,
        "deferring reconciles any outstanding explicit bulk edit"
    );
    assert_eq!(model.selected_count(), 2);
}

#[test]
fn straddled_selection_ack_and_old_folder_intent_have_a_rejectable_fence() {
    let mut owner =
        FileSelectionModel::from_files([("folder/a".into(), 1), ("outside/b".into(), 1)]);
    let captured = owner.metadata_revision();
    let first_page = owner.file_state(0).unwrap();
    owner.set_selected(1, false); // Actual FilesApplied changes hidden canonical state.
    assert_ne!(
        owner.metadata_revision(),
        captured,
        "page2 must reject the old captured fence"
    );
    assert!(first_page.selected);
    let canonical = owner.file_state(1).unwrap();
    assert!(
        !canonical.selected,
        "SQL path pages must overlay this canonical numeric state"
    );
    let mut mirror = FileSelectionModel::from_snapshot([
        ("folder/a".into(), 1, 0, true),
        ("outside/b".into(), 1, 0, true),
    ]);
    mirror.adopt_metadata_revision(captured).unwrap();
    assert_ne!(
        mirror.metadata_revision(),
        owner.metadata_revision(),
        "old full-selection intent must be rejected before preserving stale hidden flags"
    );
    assert!(mirror.adopt_metadata_revision(0).is_err());
}
