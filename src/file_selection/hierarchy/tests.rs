use super::*;

fn model() -> FileSelectionModel {
    FileSelectionModel::from_files([
        ("root/season/two.mkv".into(), 200),
        ("outside.txt".into(), 1),
        ("root/one.mkv".into(), 100),
        ("root/season/bonus/zero.txt".into(), 0),
        ("other\\three.txt".into(), 30),
    ])
}
fn folder(index: &FileHierarchy, name: &str) -> usize {
    (0..index.folder_count())
        .find(|id| index.folder(*id).unwrap().name == name)
        .unwrap()
}
fn files(rows: &[HierarchyRow]) -> Vec<usize> {
    rows.iter()
        .filter_map(|row| match row {
            HierarchyRow::File { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}

#[test]
fn nested_descendants_preserve_original_indices_and_exact_mixed_selection() {
    let mut model = model();
    let index = model.hierarchy().unwrap().unwrap();
    let root = folder(&index, "root");
    let season = folder(&index, "season");
    assert_eq!(index.descendants(root), &[3, 0, 2]);
    assert_eq!(index.descendants(season), &[3, 0]);
    assert_eq!(
        model.folder_selection(root),
        Some(FolderSelection {
            selected: 3,
            total: 3
        })
    );
    model.set_selected(0, false);
    assert!(model.folder_selection(root).unwrap().mixed());
    assert!(model.folder_selection(season).unwrap().mixed());
    let mut edits = BTreeMap::new();
    assert!(model.folder_selection_edits(root, false, &mut edits));
    assert_eq!(model.selection_after_edit(None, &edits), Some(vec![1, 4]));
    assert_eq!(
        model.selected_count(),
        4,
        "building intent cannot mutate confirmed selection"
    );
    assert!(!model.folder_selection_edits(usize::MAX, true, &mut edits));
}

#[test]
fn search_exposes_ancestors_without_destroying_pre_search_collapse_state() {
    let model = model();
    let index = model.hierarchy().unwrap().unwrap();
    let root = folder(&index, "root");
    let mut expansion = Expansion::default();
    expansion.toggle(root);
    let before = model.tree_view("", &expansion).unwrap();
    assert_eq!(files(&before), vec![4, 1]);
    let searched = model.tree_view("two.mkv", &expansion).unwrap();
    assert_eq!(files(&searched), vec![0]);
    assert!(
        searched
            .iter()
            .any(|row| matches!(row,HierarchyRow::Folder{id,..} if *id==root))
    );
    assert!(!expansion.is_open(root));
    assert_eq!(*model.tree_view("", &expansion).unwrap(), *before);
    expansion.set_all(false);
    assert!(files(&model.tree_view("", &expansion).unwrap()).contains(&1));
    expansion.set_all(true);
    assert_eq!(files(&model.tree_view("", &expansion).unwrap()).len(), 5);
}

#[test]
fn same_total_selected_count_swap_and_cloned_models_do_not_share_wrong_folder_counts() {
    let mut first = FileSelectionModel::from_files([("a/one".into(), 1), ("b/two".into(), 1)]);
    first.set_selected(1, false);
    let index = first.hierarchy().unwrap().unwrap();
    let a = folder(&index, "a");
    let b = folder(&index, "b");
    assert!(first.folder_selection(a).unwrap().all());
    let mut second = first.clone();
    assert!(
        Arc::ptr_eq(&index, &second.hierarchy().unwrap().unwrap()),
        "only immutable metadata may be shared"
    );
    second.set_selected(0, false);
    second.set_selected(1, true);
    assert_eq!(first.selected_count(), second.selected_count());
    assert_eq!(second.folder_selection(a).unwrap().selected, 0);
    assert!(second.folder_selection(b).unwrap().all());
    assert!(first.folder_selection(a).unwrap().all());
    assert_eq!(first.folder_selection(b).unwrap().selected, 0);
}

#[test]
fn hierarchy_and_visible_rows_survive_progress_but_defer_releases_heavy_caches() {
    let mut model = model();
    let index = model.hierarchy().unwrap().unwrap();
    let weak = Arc::downgrade(&index);
    let expansion = Expansion::default();
    let rows = model.tree_view("", &expansion).unwrap();
    model.folder_selection(0).unwrap();
    let before = model.memory_usage();
    model.set_file_downloaded(0, 120);
    assert!(Arc::ptr_eq(&index, &model.hierarchy().unwrap().unwrap()));
    assert!(Arc::ptr_eq(
        &rows,
        &model.tree_view("", &expansion).unwrap()
    ));
    let summary = model.summary();
    drop(rows);
    drop(index);
    model.defer_details();
    assert!(weak.upgrade().is_none());
    assert!(model.hierarchy().unwrap().is_none());
    assert!(model.tree_view("", &expansion).is_err());
    let after = model.memory_usage();
    assert_eq!(
        (
            after.tree_capacity_bytes,
            after.view_capacity_bytes,
            after.path_capacity_bytes
        ),
        (0, 0, 0)
    );
    assert!(before.tree_capacity_bytes > 0 && before.view_capacity_bytes > 0);
    assert_eq!(model.summary(), summary);
    model
        .hydrate_paths(vec![
            "root/season/two.mkv".into(),
            "outside.txt".into(),
            "root/one.mkv".into(),
            "root/season/bonus/zero.txt".into(),
            "other/three.txt".into(),
        ])
        .unwrap();
    assert_eq!(model.hierarchy().unwrap().unwrap().folder_count(), 4);
    assert_eq!(model.selected_downloaded(), 120);
}

#[test]
fn deep_and_unicode_hierarchies_are_iterative_and_budget_failure_remains_explicit() {
    let path = format!(
        "{}\u{444}\u{430}\u{439}\u{43b}.bin",
        "\u{76ee}\u{5f55}/".repeat(1024)
    );
    let model = FileSelectionModel::from_files([(path, 1)]);
    let index = model.hierarchy().unwrap().unwrap();
    assert_eq!(index.folder_count(), 1024);
    let rows = model
        .tree_view("\u{444}\u{430}\u{439}\u{43b}", &Expansion::default())
        .unwrap();
    assert_eq!(files(&rows), vec![0]);
    assert!(
        FileHierarchy::build_bounded(model.files(), 10)
            .unwrap_err()
            .contains("node budget")
    );
    let cold = FileSelectionModel::from_summary(model.summary());
    assert!(cold.hierarchy().unwrap().is_none());
    assert!(cold.folder_selection(0).is_none());
    assert!(
        FileSelectionModel::default()
            .tree_view("", &Expansion::default())
            .unwrap()
            .is_empty()
    );
}
