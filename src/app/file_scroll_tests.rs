use super::*;

#[test]
fn file_region_fills_remaining_height_for_empty_short_and_long_lists() {
    for count in [0, 3, 65] {
        let context = egui::Context::default();
        let mut torrent = crate::client::tests::torrent();
        torrent.files = FileSelectionModel::from_files(
            (0..count).map(|index| (format!("file-{index:04}.bin"), 100)),
        );
        for height in [360.0, 600.0, 1000.0, 600.0] {
            let mut output = None;
            for _ in 0..3 {
                output = Some(context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1000.0, height),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            assert_eq!(
                                PeerLumeApp::show_file_tree(ui, &torrent),
                                (None, None, None)
                            );
                        });
                    },
                ));
            }
            let output = output.unwrap();
            let border = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.stroke.width > 0.0 && rect.rect.width() > 900.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("file-list border");
            assert!(
                (border.bottom() - (height - 8.0)).abs() <= 1.0,
                "file region leaves unused height for {count} files at {height}: {border:?}"
            );
        }
    }
}

#[test]
fn blank_space_after_a_left_aligned_file_name_keeps_its_context_menu() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_files([("a.bin".into(), 100)]);
    let frame = |events| {
        let mut action = (None, None, None);
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
                    action = PeerLumeApp::show_file_tree(ui, &torrent);
                });
            },
        );
        (output, action)
    };
    let text_center = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing rendered text: {label}"))
    };
    let pointer = |pos, button, pressed| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    frame(vec![]);
    let (output, action) = frame(vec![]);
    assert_eq!(action, (None, None, None));
    let name = text_center(&output, "a.bin");
    assert!(name.x < 100.0);
    let blank = egui::pos2(400.0, name.y);
    frame(pointer(blank, egui::PointerButton::Secondary, true));
    frame(pointer(blank, egui::PointerButton::Secondary, false));
    let (output, action) = frame(vec![]);
    assert_eq!(action, (None, None, None));
    let menu = text_center(&output, "Show in folder");
    frame(pointer(menu, egui::PointerButton::Primary, true));
    let (_, action) = frame(pointer(menu, egui::PointerButton::Primary, false));
    assert_eq!(action, (None, Some(0), None));
    assert!(torrent.files.files()[0].selected);
    assert_eq!(torrent.downloaded_bytes, 0);
}

#[test]
fn file_names_start_at_the_left_inside_a_visible_list_border() {
    for dark in [false, true] {
        for width in [420.0, 1000.0, 1453.0] {
            let context = egui::Context::default();
            context.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let mut torrent = crate::client::tests::torrent();
            let names = [
                "a.bin".to_owned(),
                "normal file name.mkv".to_owned(),
                "long name ".repeat(60),
            ];
            torrent.files =
                FileSelectionModel::from_files(names.iter().map(|name| (name.clone(), 100)));
            let mut output = None;
            for _ in 0..3 {
                output = Some(context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            assert_eq!(
                                PeerLumeApp::show_file_tree(ui, &torrent),
                                (None, None, None)
                            );
                        });
                    },
                ));
            }
            let output = output.unwrap();
            let text = |name: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) if text.galley.job.text == name => {
                            Some(text)
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            let left = text(&names[0]).pos.x;
            assert!(
                left < 50.0,
                "file name is centered at width {width}: {left}"
            );
            for name in &names {
                let label = text(name);
                assert!(
                    (label.pos.x - left).abs() <= 1.0,
                    "all file names must share a left edge"
                );
                assert_eq!(
                    label.galley.rows.len(),
                    1,
                    "long names must stay on one line"
                );
            }
            let border = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.stroke.width > 0.0
                            && rect.rect.width() > width * 0.8
                            && rect.rect.height() > 40.0 =>
                    {
                        Some(rect)
                    }
                    _ => None,
                })
                .expect("a visible frame must enclose the file list");
            assert_ne!(border.stroke.color, egui::Color32::TRANSPARENT);
            assert!(border.rect.left() >= 7.0 && border.rect.right() <= width - 7.0);
            let header = text("Files/ (3 files)");
            assert!(header.pos.y + header.galley.size().y < border.rect.top());
            for name in &names {
                let label = text(name);
                assert!(
                    border
                        .rect
                        .contains_rect(egui::Rect::from_min_size(label.pos, label.galley.size()))
                );
            }
        }
    }
}

#[test]
fn file_count_header_stays_fixed_while_rows_scroll_to_the_end_and_back() {
    fn frame(
        context: &egui::Context,
        torrent: &TorrentItem,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
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
                    assert_eq!(PeerLumeApp::show_file_tree(ui, torrent), (None, None, None));
                });
            },
        )
    }
    fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        let matches: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(matches.len(), 1, "one visible header/row: {label}");
        matches[0]
    }
    fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    for prefix in ["", "folder/"] {
        let context = egui::Context::default();
        let mut torrent = crate::client::tests::torrent();
        torrent.files = FileSelectionModel::from_files(
            (0..65).map(|index| (format!("{prefix}file-{index:04}.bin"), 100)),
        );
        let label = if prefix.is_empty() {
            "Files/ (65 files)"
        } else {
            "folder/ (65 files)"
        };
        for _ in 0..3 {
            frame(&context, &torrent, vec![]);
        }
        let output = frame(&context, &torrent, vec![]);
        let header = text_rect(&output, label);
        let first = text_rect(&output, "file-0000.bin");
        let rail = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.rect.width() <= 14.0 && rect.rect.height() > 200.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .max_by(|a, b| a.right().total_cmp(&b.right()))
            .unwrap();
        assert!(
            header.bottom() < rail.top(),
            "header must be outside the scroll viewport"
        );
        let start = egui::pos2(rail.center().x, rail.top() + 3.0);
        let end = egui::pos2(rail.center().x, rail.bottom() - 1.0);
        frame(&context, &torrent, pointer(start, true));
        frame(&context, &torrent, vec![egui::Event::PointerMoved(end)]);
        frame(&context, &torrent, pointer(end, false));
        let bottom = frame(&context, &torrent, vec![]);
        assert_eq!(text_rect(&bottom, label), header);
        text_rect(&bottom, "file-0064.bin");
        frame(&context, &torrent, pointer(end, true));
        frame(&context, &torrent, vec![egui::Event::PointerMoved(start)]);
        frame(&context, &torrent, pointer(start, false));
        for _ in 0..3 {
            frame(&context, &torrent, vec![]);
        }
        let top = frame(&context, &torrent, vec![]);
        assert_eq!(text_rect(&top, label), header);
        let restored = text_rect(&top, "file-0000.bin");
        assert_eq!(restored.y_range(), first.y_range());
        // Scrollbar animation/pixel rounding can shift horizontal layout slightly.
        assert!((restored.left() - first.left()).abs() <= 1.0);
    }
}

#[test]
fn fixed_file_count_covers_empty_single_and_multiple_root_lists() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    for paths in [vec![], vec!["one.bin"], vec!["a/one.bin", "b/two.bin"]] {
        torrent.files =
            FileSelectionModel::from_files(paths.iter().map(|path| ((*path).to_owned(), 100)));
        let label = format!("Files/ ({} files)", paths.len());
        let output = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_eq!(
                    PeerLumeApp::show_file_tree(ui, &torrent),
                    (None, None, None)
                );
            });
        });
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        let headers: Vec<_> = texts
            .iter()
            .filter(|text| text.galley.job.text == label)
            .collect();
        assert_eq!(headers.len(), 1, "exactly one total heading: {label}");
        let header_bottom = headers[0].pos.y + headers[0].galley.size().y;
        for path in &paths {
            let name = path.rsplit('/').next().unwrap();
            let file = texts
                .iter()
                .find(|text| text.galley.job.text == name)
                .unwrap();
            assert!(file.pos.y > header_bottom);
        }
        if paths.len() == 2 {
            for group in ["a/ (1 files)", "b/ (1 files)"] {
                let heading = texts
                    .iter()
                    .find(|text| text.galley.job.text == group)
                    .unwrap();
                assert!(
                    heading.pos.y > header_bottom,
                    "folder headings remain in the list"
                );
            }
        }
    }
}

#[test]
fn file_sizes_stay_clear_of_the_scrollbar_when_narrowed_or_hovered() {
    let context = egui::Context::default();
    let mut torrent = crate::client::tests::torrent();
    torrent.files = FileSelectionModel::from_files((0..10_000).map(|index| {
        (
            format!("folder/long file name {index:05}.bin"),
            9_876_543_210,
        )
    }));
    let size = format_size(9_876_543_210);
    for width in [1000.0, 420.0, 280.0, 1000.0] {
        for hovered in [false, true] {
            let mut output = None;
            for _ in 0..3 {
                output = Some(context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        events: if hovered {
                            vec![egui::Event::PointerMoved(egui::pos2(width - 15.0, 100.0))]
                        } else {
                            vec![egui::Event::PointerGone]
                        },
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.style_mut().animation_time = 0.0;
                            ui.style_mut().spacing.scroll.bar_width = 18.0;
                            ui.style_mut().spacing.scroll.bar_inner_margin = 6.0;
                            ui.style_mut()
                                .text_styles
                                .insert(egui::TextStyle::Body, egui::FontId::proportional(22.0));
                            // An overflowing toolbar must not widen the file viewport.
                            ui.horizontal(|ui| {
                                ui.allocate_exact_size(
                                    egui::vec2(1800.0, 20.0),
                                    egui::Sense::hover(),
                                );
                            });
                            assert_eq!(
                                PeerLumeApp::show_file_tree(ui, &torrent),
                                (None, None, None)
                            );
                            assert!(ui.spacing().scroll.floating, "style must remain local");
                        });
                    },
                ));
            }
            let output = output.unwrap();
            let sizes: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == size => Some((
                        egui::Rect::from_min_size(text.pos, text.galley.size()),
                        shape.clip_rect,
                    )),
                    _ => None,
                })
                .collect();
            assert!(!sizes.is_empty(), "size information must be rendered");
            assert!(sizes.len() < 30, "the file list must stay virtualized");
            let rail = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.rect.width() <= 20.0 && rect.rect.height() > 200.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .max_by(|a, b| a.right().total_cmp(&b.right()))
                .expect("vertical scrollbar track");
            assert!(
                rail.right() <= width + 0.5,
                "scrollbar must stay inside the window"
            );
            for (rect, clip) in sizes {
                assert!(
                    rect.right() <= rail.left() - 5.0,
                    "size overlaps the scrollbar at width {width}: {rect:?}, {rail:?}"
                );
                assert!(
                    rect.right() <= clip.right() + 0.5,
                    "size is clipped at width {width}: {rect:?}, {clip:?}"
                );
                assert!(
                    clip.right() <= rail.left(),
                    "content must not paint under the scrollbar"
                );
            }
        }
    }
}
