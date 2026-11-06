//! Virtualized file hierarchy. Rendering produces intents, never transfer mutations.
use super::{FileTreeActions, file_location, format_size, name_column};
use crate::{
    file_selection::{Expansion, FileHierarchy, FileSelectionModel, FileTreeRow, HierarchyRow},
    torrent::TorrentItem,
};
use eframe::egui;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Default)]
struct ExpansionState {
    file_count: usize,
    expansion: Expansion,
}
enum Rows {
    Tree(Arc<Vec<HierarchyRow>>, usize, usize),
    Flat(Arc<Vec<FileTreeRow>>),
}
impl Rows {
    fn len(&self) -> usize {
        match self {
            Self::Tree(rows, offset, _) => rows.len().saturating_sub(*offset),
            Self::Flat(rows) => rows.len(),
        }
    }
    fn has_files(&self) -> bool {
        match self {
            Self::Tree(_, _, count) => *count > 0,
            Self::Flat(rows) => !rows.is_empty(),
        }
    }
    fn row(&self, position: usize) -> Option<HierarchyRow> {
        match self {
            Self::Tree(rows, offset, _) => rows.get(position + offset).cloned(),
            Self::Flat(rows) => match rows.get(position)? {
                FileTreeRow::File {
                    index,
                    display_start,
                } => Some(HierarchyRow::File {
                    index: *index,
                    depth: 0,
                    display_start: *display_start,
                }),
                _ => None,
            },
        }
    }
    fn file_indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter_map(|row| match self.row(row)? {
            HierarchyRow::File { index, .. } => Some(index),
            _ => None,
        })
    }
}

#[cfg(test)]
pub(super) fn show(ui: &mut egui::Ui, torrent: &TorrentItem) -> FileTreeActions {
    show_with_connection(ui, torrent, true)
}

pub(super) fn show_with_connection(
    ui: &mut egui::Ui,
    torrent: &TorrentItem,
    editable: bool,
) -> FileTreeActions {
    if !torrent.files.details_loaded() {
        ui.label(format!("Files/ ({} files)", torrent.files.file_count()));
        ui.spinner();
        ui.label("Loading authoritative file details. Pending or failed requests are reported in the status line.");
        return (None, None, None);
    }
    let visible_width = name_column::viewport_width(ui);
    let mut all = None;
    let mut changes = BTreeMap::new();
    let mut reveal = None;
    let mut priorities = None;
    let view_id = ui.make_persistent_id(("file_view", torrent.info_hashes.join(";")));
    let (mut query, mut sort): (String, u8) = ui
        .ctx()
        .data_mut(|data| data.get_temp(view_id))
        .unwrap_or_default();
    let mut state: ExpansionState = ui
        .ctx()
        .data_mut(|data| data.get_temp(view_id.with("expansion")))
        .unwrap_or_default();
    if state.file_count != torrent.files.file_count() {
        state = ExpansionState {
            file_count: torrent.files.file_count(),
            ..Default::default()
        };
    }
    let hierarchy = match torrent.files.hierarchy() {
        Ok(index) => index,
        Err(error) => {
            ui.label(error);
            None
        }
    };
    ui.scope(|ui| {
        ui.set_max_width(visible_width);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(editable,egui::Button::new("Select all").small()).clicked() {all=Some(true);}
            if ui.add_enabled(editable,egui::Button::new("Select none").small()).clicked() {all=Some(false);}
            ui.label(format!("{} of {} files selected ({}), {} downloaded",torrent.files.selected_count(),torrent.files.file_count(),format_size(torrent.files.selected_size()),format_size(torrent.files.selected_downloaded())));
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search files / folders").desired_width(220.0_f32.min(visible_width.max(40.0))));
            egui::ComboBox::from_id_salt("file_sort").selected_text(match sort {1=>"Name",2=>"Largest first",_=>"Tree"}).show_ui(ui,|ui| {
                ui.selectable_value(&mut sort,0,"Tree");ui.selectable_value(&mut sort,1,"Name");ui.selectable_value(&mut sort,2,"Largest first");
            });
            let can_expand=sort==0 && query.is_empty() && hierarchy.as_ref().is_some_and(|index|index.folder_count()>0);
            for (label,open) in [("Expand all",true),("Collapse all",false)] {
                if ui.add_enabled(can_expand,egui::Button::new(label).small()).on_hover_text("Search temporarily exposes matching folders; clear the search to change saved expansion").clicked() {state.expansion.set_all(open);}
            }
        });
    });
    let fixed_root = if sort == 0 {
        hierarchy.as_ref().and_then(|index| index.single_root())
    } else {
        None
    };
    let viewed = if sort == 0 && hierarchy.is_some() {
        let rows = torrent
            .files
            .tree_view(&query, &state.expansion)
            .expect("loaded hierarchy checked above");
        let offset = usize::from(
            matches!(rows.first(),Some(HierarchyRow::Folder{id,..}) if Some(*id)==fixed_root),
        );
        let count = torrent
            .files
            .tree_view_file_count(&query, &state.expansion)
            .unwrap_or_default();
        Rows::Tree(rows, offset, count)
    } else {
        Rows::Flat(
            torrent
                .files
                .view_rows(&query, if sort == 2 { 2 } else { 1 }),
        )
    };
    ui.scope(|ui| {
        ui.set_max_width(visible_width);
        ui.horizontal_wrapped(|ui| {
            for (label, high) in [
                ("High priority (visible)", true),
                ("Normal priority (visible)", false),
            ] {
                if ui
                    .add_enabled(
                        editable && viewed.has_files(),
                        egui::Button::new(label).small(),
                    )
                    .clicked()
                {
                    priorities = Some(priority_intent(&torrent.files, viewed.file_indices(), high));
                }
            }
        });
    });
    let row_height = ui
        .text_style_height(&egui::TextStyle::Body)
        .max(ui.spacing().interact_size.y)
        .max(20.0)
        + 2.0;
    if let Some(id) = fixed_root {
        folder_row(
            ui,
            &torrent.files,
            hierarchy.as_deref().unwrap(),
            id,
            0,
            row_height,
            &query,
            &mut state.expansion,
            all,
            &mut changes,
            editable,
        );
    } else {
        ui.strong(format!("Files/ ({} files)", torrent.files.file_count()));
    }
    ui.scope(|ui| {
        ui.set_max_width(visible_width);
        ui.style_mut().spacing.scroll.floating = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
            egui::ScrollArea::vertical()
                .id_salt("selected_torrent_files_scroll")
                .max_width(name_column::viewport_width(ui))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .max_height(ui.available_height().max(0.0))
                .show_rows(ui, row_height, viewed.len(), |ui, range| {
                    let measure = |text: String| {
                        ui.painter()
                            .layout_no_wrap(
                                text,
                                egui::TextStyle::Body.resolve(ui.style()),
                                ui.visuals().text_color(),
                            )
                            .size()
                            .x
                    };
                    let size_width = range
                        .clone()
                        .filter_map(|position| match viewed.row(position)? {
                            HierarchyRow::File { index, .. } => Some(measure(format_size(
                                torrent.files.files()[index].size_bytes,
                            ))),
                            _ => None,
                        })
                        .fold(measure("1024.0 MiB".into()), f32::max)
                        .ceil();
                    for position in range {
                        let Some(row) = viewed.row(position) else {
                            continue;
                        };
                        match row {
                            HierarchyRow::Folder { id, depth } => {
                                folder_row(
                                    ui,
                                    &torrent.files,
                                    hierarchy.as_deref().unwrap(),
                                    id,
                                    depth.saturating_sub(usize::from(fixed_root.is_some())),
                                    row_height,
                                    &query,
                                    &mut state.expansion,
                                    all,
                                    &mut changes,
                                    editable,
                                );
                            }
                            HierarchyRow::File {
                                index,
                                depth,
                                display_start,
                            } => {
                                let file = &torrent.files.files()[index];
                                let progress = if file.size_bytes == 0 {
                                    0.0
                                } else {
                                    file.downloaded_bytes as f32 / file.size_bytes as f32
                                };
                                ui.push_id(("file", index), |ui| {
                                    ui.horizontal(|ui| {
                                        let mut selected = changes
                                            .get(&index)
                                            .copied()
                                            .or(all)
                                            .unwrap_or(file.selected);
                                        if ui
                                            .add_enabled(
                                                editable,
                                                egui::Checkbox::new(&mut selected, ""),
                                            )
                                            .changed()
                                        {
                                            changes.insert(index, selected);
                                        }
                                        let name_width = (name_column::viewport_width(ui)
                                            - 80.0
                                            - size_width
                                            - 2.0 * ui.spacing().item_spacing.x)
                                            .max(0.0);
                                        let (name_rect, response) = ui.allocate_exact_size(
                                            egui::vec2(name_width, row_height),
                                            egui::Sense::click(),
                                        );
                                        let mut name_ui = ui.new_child(
                                            egui::UiBuilder::new().max_rect(name_rect).layout(
                                                egui::Layout::left_to_right(egui::Align::Center),
                                            ),
                                        );
                                        name_ui.set_clip_rect(ui.clip_rect().intersect(name_rect));
                                        let depth =
                                            depth.saturating_sub(usize::from(fixed_root.is_some()));
                                        name_ui.add_space(
                                            (depth as f32 * 12.0).min((name_width - 30.0).max(0.0)),
                                        );
                                        name_ui
                                            .add(
                                                egui::Label::new(&file.path[display_start..])
                                                    .selectable(false)
                                                    .truncate()
                                                    .sense(egui::Sense::hover()),
                                            )
                                            .on_hover_text(&file.path);
                                        if file_location::context_menu_extra(&response, |ui| {
                                            ui.separator();
                                            for (label, high) in [
                                                ("High priority", true),
                                                ("Normal priority", false),
                                            ] {
                                                if ui
                                                    .add_enabled(editable, egui::Button::new(label))
                                                    .clicked()
                                                {
                                                    priorities = Some(priority_intent(
                                                        &torrent.files,
                                                        [index],
                                                        high,
                                                    ));
                                                    ui.close();
                                                }
                                            }
                                            if ui
                                                .add_enabled(
                                                    editable,
                                                    egui::Button::new("Skip file"),
                                                )
                                                .clicked()
                                            {
                                                changes.insert(index, false);
                                                ui.close();
                                            }
                                            ui.label(if torrent.files.is_high_priority(index) {
                                                "Priority: high"
                                            } else {
                                                "Priority: normal"
                                            });
                                        }) {
                                            reveal = Some(index);
                                        }
                                        ui.add(
                                            egui::ProgressBar::new(progress)
                                                .desired_width(80.0)
                                                .show_percentage(),
                                        );
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(size_width, row_height),
                                            egui::Sense::hover(),
                                        );
                                        let mut size_ui = ui.new_child(
                                            egui::UiBuilder::new().max_rect(rect).layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                            ),
                                        );
                                        size_ui.set_clip_rect(ui.clip_rect().intersect(rect));
                                        size_ui.add(
                                            egui::Label::new(format_size(file.size_bytes))
                                                .halign(egui::Align::Min)
                                                .truncate(),
                                        );
                                    })
                                });
                            }
                        }
                    }
                });
        });
    });
    ui.ctx().data_mut(|data| {
        data.insert_temp(view_id, (query, sort));
        data.insert_temp(view_id.with("expansion"), state);
    });
    (
        torrent.files.selection_after_edit(all, &changes),
        reveal,
        priorities,
    )
}

#[allow(clippy::too_many_arguments)]
fn folder_row(
    ui: &mut egui::Ui,
    model: &FileSelectionModel,
    hierarchy: &FileHierarchy,
    id: usize,
    depth: usize,
    height: f32,
    query: &str,
    expansion: &mut Expansion,
    all: Option<bool>,
    changes: &mut BTreeMap<usize, bool>,
    editable: bool,
) {
    let folder = hierarchy
        .folder(id)
        .expect("visible folder belongs to this hierarchy");
    let counts = model.folder_selection(id).unwrap_or_default();
    let open = !query.is_empty() || expansion.is_open(id);
    let width = name_column::viewport_width(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("folder", id))
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row.set_clip_rect(ui.clip_rect().intersect(rect));
    row.add_space((depth as f32 * 12.0).min((width - 60.0).max(0.0)));
    if row
        .add_enabled(
            query.is_empty(),
            egui::Button::new(if open { "▾" } else { "▸" }).small(),
        )
        .on_hover_text(if query.is_empty() {
            "Expand or collapse this folder"
        } else {
            "Matching descendants stay expanded while searching"
        })
        .clicked()
    {
        expansion.toggle(id);
        ui.ctx().request_repaint();
    }
    row.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    let mut selected = all.unwrap_or(counts.all());
    let response=row.add_enabled(editable,egui::Checkbox::new(&mut selected,egui::RichText::new(format!("{}/ ({} files)",folder.name,counts.total)).strong()).indeterminate(all.is_none() && counts.mixed()))
        .on_hover_text("Select or clear ALL descendant files, including search-hidden and collapsed files. A mixed checkbox selects the whole folder.");
    if response.changed() {
        model.folder_selection_edits(id, selected, changes);
    }
}

fn priority_intent(
    model: &FileSelectionModel,
    indices: impl IntoIterator<Item = usize>,
    high: bool,
) -> Vec<usize> {
    let mut values: BTreeSet<_> = model.high_priority().into_iter().collect();
    for index in indices {
        if high {
            values.insert(index);
        } else {
            values.remove(&index);
        }
    }
    values.into_iter().collect()
}

#[cfg(test)]
mod tests;
