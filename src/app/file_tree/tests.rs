use super::*;

fn frame(
    context: &egui::Context,
    torrent: &TorrentItem,
    events: Vec<egui::Event>,
    query: Option<&str>,
) -> (egui::FullOutput, FileTreeActions) {
    frame_with_connection(context, torrent, events, query, true)
}
fn frame_with_connection(
    context: &egui::Context,
    torrent: &TorrentItem,
    events: Vec<egui::Event>,
    query: Option<&str>,
    editable: bool,
) -> (egui::FullOutput, FileTreeActions) {
    let mut actions = (None, None, None);
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.style_mut().animation_time = 0.0;
                if let Some(query) = query {
                    let id = ui.make_persistent_id(("file_view", torrent.info_hashes.join(";")));
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(id, (query.to_owned(), 0_u8)));
                }
                actions = show_with_connection(ui, torrent, editable);
            });
        },
    );
    (output, actions)
}

#[test]
fn offline_file_tree_blocks_checkbox_and_priority_edits_but_retains_navigation() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_snapshot([
        ("root/one.bin".into(), 100, 20, true),
        ("root/two.bin".into(), 100, 0, false),
    ]);
    let frame = |events, query| frame_with_connection(&context, &torrent, events, query, false);
    let click = |position| {
        frame(pointer(position, egui::PointerButton::Primary, true), None);
        frame(pointer(position, egui::PointerButton::Primary, false), None).1
    };
    frame(vec![], None);
    let (output, _) = frame(vec![], None);
    for position in [
        checkbox(&output, "root/ (2 files)"),
        checkbox(&output, "one.bin"),
        label(&output, "Select all").center(),
        label(&output, "Select none").center(),
        label(&output, "High priority (visible)").center(),
        label(&output, "Normal priority (visible)").center(),
    ] {
        assert_eq!(click(position), (None, None, None));
    }
    assert_eq!(
        click(disclosure(&output, "root/ (2 files)")),
        (None, None, None)
    );
    let (collapsed, _) = frame(vec![], None);
    assert!(!texts(&collapsed).iter().any(|(text, _)| *text == "one.bin"));
    assert_eq!(
        click(label(&collapsed, "Expand all").center()),
        (None, None, None)
    );
    frame(vec![], Some("one"));
    let (filtered, _) = frame(vec![], None);
    label(&filtered, "one.bin");
    assert!(!texts(&filtered).iter().any(|(text, _)| *text == "two.bin"));
    let target = egui::pos2(400.0, label(&filtered, "one.bin").center().y);
    frame(pointer(target, egui::PointerButton::Secondary, true), None);
    frame(pointer(target, egui::PointerButton::Secondary, false), None);
    frame(vec![], None);
    let (menu, _) = frame(vec![], None);
    let reveal = click(label(&menu, "Show in folder").center());
    assert_eq!(reveal, (None, Some(0), None));
    assert_eq!(torrent.files.selected_indices(), Some(vec![0]));
    assert!(torrent.files.high_priority().is_empty());
}
fn texts(output: &egui::FullOutput) -> Vec<(&str, egui::Rect)> {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some((
                text.galley.job.text.as_str(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            _ => None,
        })
        .collect()
}
fn label(output: &egui::FullOutput, name: &str) -> egui::Rect {
    texts(output)
        .into_iter()
        .find(|(text, _)| *text == name)
        .unwrap_or_else(|| {
            panic!(
                "Missing label: {name}; visible: {:?}",
                texts(output)
                    .iter()
                    .map(|(text, rect)| (*text, rect.center()))
                    .collect::<Vec<_>>()
            )
        })
        .1
}
fn pointer(position: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
fn click(context: &egui::Context, torrent: &TorrentItem, position: egui::Pos2) -> FileTreeActions {
    frame(
        context,
        torrent,
        pointer(position, egui::PointerButton::Primary, true),
        None,
    );
    frame(
        context,
        torrent,
        pointer(position, egui::PointerButton::Primary, false),
        None,
    )
    .1
}
fn checkbox(output: &egui::FullOutput, label_text: &str) -> egui::Pos2 {
    let text = label(output, label_text);
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.rect.width() <= 22.0
                    && rect.rect.height() <= 22.0
                    && rect.rect.right() < text.left()
                    && (rect.rect.center().y - text.center().y).abs() < 3.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .max_by(|a, b| a.right().total_cmp(&b.right()))
        .expect("checkbox shape")
        .center()
}
fn disclosure(output: &egui::FullOutput, label_text: &str) -> egui::Pos2 {
    let text = label(output, label_text);
    texts(output)
        .into_iter()
        .find(|(label, rect)| *label == "▾" && (rect.center().y - text.center().y).abs() < 3.0)
        .expect("folder disclosure")
        .1
        .center()
}

#[test]
fn mixed_folder_checkbox_selects_hidden_descendants_without_mutating_confirmed_files() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_snapshot([
        ("root/a.bin".into(), 100, 20, true),
        ("root/hidden.bin".into(), 100, 0, false),
        ("outside.bin".into(), 100, 0, false),
    ]);
    frame(&context, &torrent, vec![], Some("a.bin"));
    let (output, _) = frame(&context, &torrent, vec![], None);
    assert!(!texts(&output).iter().any(|(name, _)| *name == "hidden.bin"));
    let actions = click(&context, &torrent, checkbox(&output, "root/ (2 files)"));
    assert_eq!(actions, (Some(vec![0, 1]), None, None));
    assert_eq!(torrent.files.selected_indices(), Some(vec![0]));
    assert_eq!(torrent.files.files()[0].downloaded_bytes, 20);
}

#[test]
fn collapse_search_clear_and_expand_controls_keep_intents_separate() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files =
        FileSelectionModel::from_files([("a/nested/one.bin".into(), 1), ("b/two.bin".into(), 1)]);
    frame(&context, &torrent, vec![], None);
    let (output, _) = frame(&context, &torrent, vec![], None);
    assert_eq!(
        click(&context, &torrent, disclosure(&output, "a/ (1 files)")),
        (None, None, None)
    );
    let (collapsed, _) = frame(&context, &torrent, vec![], None);
    assert!(!texts(&collapsed).iter().any(|(text, _)| *text == "one.bin"));
    assert!(texts(&collapsed).iter().any(|(text, _)| *text == "two.bin"));
    let (search, _) = frame(&context, &torrent, vec![], Some("one.bin"));
    label(&search, "a/ (1 files)");
    label(&search, "nested/ (1 files)");
    label(&search, "one.bin");
    let (restored, _) = frame(&context, &torrent, vec![], Some(""));
    assert!(!texts(&restored).iter().any(|(text, _)| *text == "one.bin"));
    assert_eq!(
        click(&context, &torrent, label(&restored, "Expand all").center()),
        (None, None, None)
    );
    let (expanded, _) = frame(&context, &torrent, vec![], None);
    label(&expanded, "one.bin");
    assert_eq!(
        click(
            &context,
            &torrent,
            label(&expanded, "Collapse all").center()
        ),
        (None, None, None)
    );
    let (closed, _) = frame(&context, &torrent, vec![], None);
    assert!(
        !texts(&closed)
            .iter()
            .any(|(text, _)| matches!(*text, "one.bin" | "two.bin"))
    );
}

#[test]
fn checked_single_root_header_clears_all_descendants_and_collapses_without_mutation() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_files([
        ("root/one.bin".into(), 1),
        ("root/nested/two.bin".into(), 1),
    ]);
    frame(&context, &torrent, vec![], None);
    let (output, _) = frame(&context, &torrent, vec![], None);
    let header = label(&output, "root/ (2 files)");
    assert_eq!(
        click(&context, &torrent, checkbox(&output, "root/ (2 files)")),
        (Some(vec![]), None, None)
    );
    assert_eq!(torrent.files.selected_indices(), Some(vec![0, 1]));
    let (output, _) = frame(&context, &torrent, vec![], None);
    assert_eq!(
        click(&context, &torrent, disclosure(&output, "root/ (2 files)")),
        (None, None, None)
    );
    let (closed, _) = frame(&context, &torrent, vec![], None);
    assert_eq!(
        label(&closed, "root/ (2 files)").y_range(),
        header.y_range()
    );
    assert!(
        !texts(&closed)
            .iter()
            .any(|(text, _)| matches!(*text, "one.bin" | "two.bin"))
    );
}

#[test]
fn visible_priorities_preserve_collapsed_and_filtered_file_bits() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files =
        FileSelectionModel::from_files([("a/visible.bin".into(), 1), ("b/hidden.bin".into(), 1)]);
    torrent.files.set_high_priority(&[1]);
    frame(&context, &torrent, vec![], Some("visible.bin"));
    let (output, _) = frame(&context, &torrent, vec![], None);
    let actions = click(
        &context,
        &torrent,
        label(&output, "High priority (visible)").center(),
    );
    assert_eq!(actions, (None, None, Some(vec![0, 1])));
    assert_eq!(torrent.files.high_priority(), vec![1]);
    torrent.files.set_high_priority(&[0, 1]);
    let (output, _) = frame(&context, &torrent, vec![], None);
    assert_eq!(
        click(
            &context,
            &torrent,
            label(&output, "Normal priority (visible)").center()
        ),
        (None, None, Some(vec![1]))
    );
}

#[test]
fn filtered_nested_reveal_uses_canonical_index_and_keeps_selection() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_files([
        ("b/first.bin".into(), 1),
        ("a/nested/wanted.bin".into(), 1),
    ]);
    frame(&context, &torrent, vec![], Some("wanted"));
    let (output, _) = frame(&context, &torrent, vec![], None);
    let name = label(&output, "wanted.bin");
    let point = egui::pos2(400.0, name.center().y);
    frame(
        &context,
        &torrent,
        pointer(point, egui::PointerButton::Secondary, true),
        None,
    );
    frame(
        &context,
        &torrent,
        pointer(point, egui::PointerButton::Secondary, false),
        None,
    );
    let (menu, _) = frame(&context, &torrent, vec![], None);
    assert_eq!(
        click(&context, &torrent, label(&menu, "Show in folder").center()),
        (None, Some(1), None)
    );
    assert_eq!(torrent.files.selected_indices(), Some(vec![0, 1]));
}

#[test]
fn cold_details_have_fixed_count_and_no_speculative_controls_or_actions() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    let summary = torrent.files.summary();
    torrent.files = FileSelectionModel::from_summary(summary);
    let (output, actions) = frame(&context, &torrent, vec![], None);
    assert_eq!(actions, (None, None, None));
    label(&output, "Files/ (1 files)");
    assert!(!texts(&output).iter().any(|(name, _)| matches!(
        *name,
        "Select all" | "Collapse all" | "High priority (visible)"
    )));
}

#[test]
fn large_nested_view_keeps_rendered_rows_bounded_and_reuses_hierarchy_on_progress() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_files((0..20_000).map(|index| {
        (
            format!("root/group-{}/file-{index:05}.bin", index / 100),
            100,
        )
    }));
    let before = torrent.files.hierarchy().unwrap().unwrap();
    frame(&context, &torrent, vec![], None);
    let (output, actions) = frame(&context, &torrent, vec![], None);
    assert_eq!(actions, (None, None, None));
    let file_labels = texts(&output)
        .iter()
        .filter(|(label, _)| label.starts_with("file-"))
        .count();
    assert!(
        file_labels > 0 && file_labels < 30,
        "rendered {file_labels} files"
    );
    let header = label(&output, "root/ (20000 files)");
    let border = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.stroke.width > 0.0
                    && rect.rect.width() > 900.0
                    && rect.rect.height() > 100.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .unwrap();
    assert!(header.bottom() < border.top());
    assert!((border.bottom() - 592.0).abs() <= 1.0);
    torrent.files.set_file_downloaded(0, 50);
    frame(&context, &torrent, vec![], None);
    assert!(Arc::ptr_eq(
        &before,
        &torrent.files.hierarchy().unwrap().unwrap()
    ));
}
