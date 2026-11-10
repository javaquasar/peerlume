//! Virtualized, identity-selected table. Widgets emit presentation changes and
//! context intents; they never perform IPC or touch payloads.
use super::name_column::{self, NameColumn};
use crate::{
    appearance::{self, RowColors, StatusPresentation},
    table_view::{Column, TableColumns},
    torrent::TorrentItem,
    torrent_filter::StatusFilter,
    torrent_selection::{Modifiers, TorrentSelection},
    torrent_view::Sort,
};
use eframe::egui;
use std::sync::Arc;

pub(super) struct TableInput<'a> {
    pub torrents: &'a [TorrentItem],
    pub visible: &'a [usize],
    pub filter: StatusFilter,
    pub owner_connected: bool,
    pub columns: &'a mut TableColumns,
    pub sort: &'a mut Sort,
    pub descending: &'a mut bool,
    pub name_column: &'a mut NameColumn,
    pub selection: &'a mut TorrentSelection,
}
#[derive(Default)]
pub(super) struct TableOutput {
    pub focused_index: Option<usize>,
    pub view_changed: bool,
    pub rendered_rows: usize,
    #[cfg(test)]
    pub headers: Vec<(Column, egui::Rect)>,
    #[cfg(test)]
    pub rows: Vec<(String, egui::Rect)>,
    #[cfg(test)]
    pub cells: Vec<(String, Column, egui::Rect)>,
}
#[derive(Clone, Default)]
struct Runtime {
    selection_frame: Option<u64>,
    layout_frame: Option<u64>,
    changed_frame: Option<u64>,
    dragged: Option<Column>,
    context: Option<Arc<ContextTargets>>,
}
struct ContextTargets {
    selected: Vec<String>,
    visible: Vec<String>,
    targets: Vec<String>,
}
fn canonical_matches(id: &str, hashes: &[String]) -> bool {
    let mut remaining = id;
    for (index, hash) in hashes.iter().enumerate() {
        if index > 0 {
            let Some(rest) = remaining.strip_prefix(';') else {
                return false;
            };
            remaining = rest;
        }
        let Some(rest) = remaining.strip_prefix(hash.as_str()) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}
#[derive(Clone, Copy)]
struct Cell {
    column: Column,
    x: f32,
    width: f32,
}
fn layout(
    columns: &TableColumns,
    measured: [f32; 6],
    name: &NameColumn,
    available: f32,
    gap: f32,
) -> (Vec<Cell>, f32) {
    let automatic = |column| match column {
        Column::Name => 0.0,
        Column::Progress => name_column::PROGRESS_WIDTH,
        Column::Status => measured[0] + 16.0,
        Column::Download => measured[1],
        Column::Upload => measured[2],
        Column::Peers => measured[3],
        Column::Eta => measured[4],
        Column::Ratio => measured[5],
    };
    let count = columns.visible().count();
    let reserved: f32 = columns
        .visible()
        .filter(|column| *column != Column::Name)
        .map(|column| {
            columns
                .widths
                .get(&column)
                .copied()
                .map_or_else(|| automatic(column), |width| width as f32)
        })
        .sum::<f32>()
        + gap * count.saturating_sub(1) as f32;
    let name_width = name.current_width(available, reserved);
    let mut x = 0.0;
    let cells = columns
        .visible()
        .map(|column| {
            let width = if column == Column::Name {
                name_width
            } else {
                columns
                    .widths
                    .get(&column)
                    .copied()
                    .map_or_else(|| automatic(column), |width| width as f32)
            };
            let cell = Cell { column, x, width };
            x += width + gap;
            cell
        })
        .collect();
    (cells, (x - gap).max(0.0))
}
fn changed(runtime: &mut Runtime, frame: u64, ui: &egui::Ui) {
    runtime.layout_frame = Some(frame);
    runtime.changed_frame = Some(frame);
    ui.ctx().request_repaint();
}

pub(super) fn show(
    ui: &mut egui::Ui,
    input: TableInput<'_>,
    mut context: impl FnMut(&mut egui::Ui, usize, &TorrentItem, &[String], usize),
) -> TableOutput {
    let TableInput {
        torrents,
        visible,
        filter,
        owner_connected,
        columns,
        sort,
        descending,
        name_column,
        selection,
    } = input;
    let mut output = TableOutput::default();
    columns.normalize();
    if let Some(&index) = visible.first() {
        selection.initialize(&torrents[index].info_hashes.join(";"));
    }
    let measured = name_column.cached_widths(ui, torrents, filter);
    let available = name_column::viewport_width(ui);
    let runtime_id = ui.make_persistent_id("torrent_table_runtime");
    let mut runtime = ui
        .ctx()
        .data_mut(|data| data.get_temp::<Runtime>(runtime_id))
        .unwrap_or_default();
    let frame = ui.ctx().cumulative_frame_nr();
    let mut context_open = false;
    ui.scope(|ui| {
        ui.style_mut().spacing.scroll.floating = false;
        name_column::scroll_area(available)
            .vscroll(true)
            .min_scrolled_height(0.0)
            .max_height(ui.available_height().max(0.0))
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let gap = ui.spacing().item_spacing.x;
                let row_height = appearance::gui::row_height(ui).max(20.0);
                let step = row_height + ui.spacing().item_spacing.y;
                let origin = ui.max_rect().min;
                let header_top = origin.y + viewport.min.y;
                let (mut cells, mut table_width) =
                    layout(columns, measured, name_column, viewport.width(), gap);
                let name_cell = cells
                    .iter()
                    .find(|cell| cell.column == Column::Name)
                    .copied()
                    .expect("Name remains visible");
                let reserved = (table_width - name_cell.width).max(0.0);
                let name_rect = egui::Rect::from_min_size(
                    egui::pos2(origin.x + name_cell.x, header_top),
                    egui::vec2(name_cell.width, row_height),
                );
                let mut name_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("torrent_name_header")
                        .max_rect(name_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let previous_gap = name_column.saved_gap();
                let name_offset = if cells.last().is_some_and(|cell| cell.column == Column::Name) {
                    -4.0
                } else {
                    gap * 0.5
                };
                let name_layout = name_column.begin_with_grip(
                    &mut name_ui,
                    viewport.width(),
                    reserved,
                    row_height,
                    name_offset,
                );
                if previous_gap != name_column.saved_gap() {
                    runtime.changed_frame = Some(frame);
                }
                if name_layout.width != name_cell.width {
                    (cells, table_width) =
                        layout(columns, measured, name_column, viewport.width(), gap);
                }
                let full_width = table_width.max(viewport.width());
                let total_height = (step * (visible.len() + 1) as f32
                    - ui.spacing().item_spacing.y)
                    .max(row_height);
                ui.set_min_width(table_width);
                ui.set_height(total_height);
                let mut first = (viewport.min.y / step).floor().max(0.0) as usize;
                first = first.saturating_sub(1).min(visible.len());
                let end = ((viewport.max.y / step).ceil() as usize + 1).min(visible.len());
                let clip = ui.clip_rect().intersect(egui::Rect::from_min_max(
                    egui::pos2(ui.clip_rect().left(), header_top + step),
                    ui.clip_rect().max,
                ));
                ui.scope_builder(egui::UiBuilder::new().max_rect(ui.max_rect()), |ui| {
                    ui.set_clip_rect(clip);
                    for (relative, &index) in visible[first.min(end)..end].iter().enumerate() {
                        let position = relative + first.min(end);
                        let torrent = &torrents[index];
                        let id = torrent.info_hashes.join(";");
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(origin.x, origin.y + step * (position + 1) as f32),
                            egui::vec2(full_width, row_height),
                        );
                        let response = ui.interact(
                            rect,
                            ui.make_persistent_id(("torrent_table_row", &id)),
                            egui::Sense::click(),
                        );
                        // egui resolves the click against the previous pass's
                        // rectangles. A live sort/scroll can move that identity
                        // before release, so confirm the current visible target.
                        let current_hit = ui
                            .input(|input| input.pointer.interact_pos())
                            .is_some_and(|point| rect.contains(point) && clip.contains(point));
                        if (response.clicked() || response.secondary_clicked())
                            && current_hit
                            && runtime.selection_frame != Some(frame)
                        {
                            if response.secondary_clicked() {
                                selection.context(&id);
                            } else {
                                let modifiers = ui.input(|input| Modifiers {
                                    ctrl: input.modifiers.command || input.modifiers.ctrl,
                                    shift: input.modifiers.shift,
                                });
                                let order = if modifiers.shift {
                                    visible
                                        .iter()
                                        .map(|index| torrents[*index].info_hashes.join(";"))
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                                selection.click(&id, &order, modifiers);
                            }
                            output.focused_index = Some(index);
                            runtime.selection_frame = Some(frame);
                        }
                        let selected = selection.is_selected(&id);
                        let colors = appearance::row_colors(
                            torrent.state,
                            ui.visuals().dark_mode,
                            selected,
                            response.hovered(),
                        );
                        ui.painter()
                            .rect_filled(rect, 0.0, colors.background.color32());
                        if selection.focus() == Some(id.as_str()) {
                            ui.painter().rect_stroke(
                                rect.shrink(0.5),
                                0.0,
                                egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
                                egui::StrokeKind::Inside,
                            );
                        }
                        for cell in &cells {
                            let cell_rect = egui::Rect::from_min_size(
                                egui::pos2(origin.x + cell.x, rect.top()),
                                egui::vec2(cell.width, row_height),
                            );
                            #[cfg(test)]
                            output.cells.push((id.clone(), cell.column, cell_rect));
                            row_cell(
                                ui,
                                cell_rect,
                                cell.column,
                                torrent,
                                colors,
                                !columns.is_visible(Column::Status),
                                owner_connected,
                            );
                        }
                        if !response.secondary_clicked() || current_hit {
                            response.context_menu(|ui| {
                                context_open = true;
                                let unchanged = runtime.context.as_ref().is_some_and(|cache| {
                                    selection
                                        .selected_ids()
                                        .eq(cache.selected.iter().map(String::as_str))
                                        && cache.visible.len() == visible.len()
                                        && cache.visible.iter().zip(visible).all(|(id, index)| {
                                            canonical_matches(id, &torrents[*index].info_hashes)
                                        })
                                });
                                if !unchanged {
                                    let visible: Vec<_> = visible
                                        .iter()
                                        .map(|index| torrents[*index].info_hashes.join(";"))
                                        .collect();
                                    let targets = selection.visible_targets(
                                        visible
                                            .iter()
                                            .filter(|id| selection.is_selected(id))
                                            .cloned(),
                                    );
                                    runtime.context = Some(Arc::new(ContextTargets {
                                        selected: selection
                                            .selected_ids()
                                            .map(str::to_owned)
                                            .collect(),
                                        visible,
                                        targets,
                                    }));
                                }
                                let targets =
                                    &runtime.context.as_ref().expect("menu target cache").targets;
                                context(
                                    ui,
                                    index,
                                    torrent,
                                    targets,
                                    selection.len().saturating_sub(targets.len()),
                                );
                            });
                        }
                        output.rendered_rows += 1;
                        #[cfg(test)]
                        output.rows.push((id, rect));
                    }
                });
                let header = egui::Rect::from_min_size(
                    egui::pos2(origin.x, header_top),
                    egui::vec2(full_width, step),
                );
                ui.painter()
                    .rect_filled(header, 0.0, ui.visuals().panel_fill);
                let mut header_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("torrent_header")
                        .max_rect(header)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                header_ui.set_clip_rect(ui.clip_rect().intersect(header));
                for cell in &cells {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(origin.x + cell.x, header_top),
                        egui::vec2(cell.width, row_height),
                    );
                    let last = cells.last().is_some_and(|last| last.column == cell.column);
                    let hit = egui::Rect::from_min_max(
                        rect.min + egui::vec2(2.0, 0.0),
                        rect.max - egui::vec2(if last { 10.0 } else { 2.0 }, 0.0),
                    );
                    let response = header_ui
                        .interact(
                            hit,
                            header_ui.make_persistent_id(("torrent_column", cell.column)),
                            egui::Sense::click_and_drag(),
                        )
                        .on_hover_text(format!(
                            "{}: click to sort; drag to reorder; right-click for columns",
                            cell.column.label()
                        ));
                    let label = egui::Rect::from_min_max(
                        rect.min + egui::vec2(4.0, 0.0),
                        egui::pos2(rect.right() - 16.0, rect.bottom()),
                    );
                    let mut text_ui = header_ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("torrent_header_label", cell.column))
                            .max_rect(label)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    text_ui.set_clip_rect(header_ui.clip_rect().intersect(label));
                    text_ui.add(
                        egui::Label::new(egui::RichText::new(cell.column.label()).strong())
                            .selectable(false)
                            .truncate(),
                    );
                    if *sort == cell.column.sort() {
                        header_ui.painter().text(
                            egui::pos2(rect.right() - 4.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            if *descending { "↓" } else { "↑" },
                            egui::TextStyle::Body.resolve(ui.style()),
                            ui.visuals().text_color(),
                        );
                    }
                    if response.clicked() && runtime.layout_frame != Some(frame) {
                        if *sort == cell.column.sort() {
                            *descending = !*descending;
                        } else {
                            *sort = cell.column.sort();
                            *descending = false;
                        }
                        changed(&mut runtime, frame, ui);
                    }
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        runtime.dragged = Some(cell.column);
                    }
                    response.context_menu(|ui| {
                        column_menu(ui, cell.column, columns, name_column, &mut runtime, frame)
                    });
                    if cell.column != Column::Name {
                        let grip = egui::Rect::from_center_size(
                            egui::pos2(
                                rect.right() + if last { -4.0 } else { gap * 0.5 },
                                rect.center().y,
                            ),
                            egui::vec2(gap.max(8.0), row_height),
                        );
                        let resize = header_ui
                            .interact(
                                grip,
                                header_ui
                                    .make_persistent_id(("torrent_column_resize", cell.column)),
                                egui::Sense::click_and_drag(),
                            )
                            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                            .on_hover_text("Drag to resize. Double-click for automatic width.");
                        if runtime.layout_frame != Some(frame) {
                            let modified = if resize.double_clicked_by(egui::PointerButton::Primary)
                            {
                                columns.reset_width(cell.column)
                            } else if resize.dragged_by(egui::PointerButton::Primary)
                                && resize.drag_delta().x != 0.0
                            {
                                columns.set_width(
                                    cell.column,
                                    (cell.width + resize.drag_delta().x).round().max(0.0) as u32,
                                )
                            } else {
                                false
                            };
                            if modified {
                                changed(&mut runtime, frame, ui);
                            }
                        }
                    }
                    #[cfg(test)]
                    output.headers.push((cell.column, rect));
                }
                if runtime.dragged.is_some()
                    && ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
                {
                    if let (Some(source), Some(position)) = (
                        runtime.dragged.take(),
                        ui.input(|input| input.pointer.interact_pos()),
                    ) && runtime.layout_frame != Some(frame)
                        && let Some(target) = cells.iter().find(|cell| {
                            egui::Rect::from_min_size(
                                egui::pos2(origin.x + cell.x, header_top),
                                egui::vec2(cell.width, row_height),
                            )
                            .contains(position)
                        })
                    {
                        let target_index = columns
                            .order
                            .iter()
                            .position(|column| *column == target.column)
                            .expect("normalized columns");
                        let source_index = columns
                            .order
                            .iter()
                            .position(|column| *column == source)
                            .expect("normalized columns");
                        let after = position.x > origin.x + target.x + target.width * 0.5;
                        let destination = target_index + usize::from(after);
                        if columns.move_to(
                            source,
                            destination.saturating_sub(usize::from(source_index < destination)),
                        ) {
                            changed(&mut runtime, frame, ui);
                        }
                    }
                } else if !ui.input(|input| input.pointer.primary_down()) {
                    runtime.dragged = None;
                }
                header_ui.painter().hline(
                    header.x_range(),
                    header.bottom() - 1.0,
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
                let name_cell = cells
                    .iter()
                    .find(|cell| cell.column == Column::Name)
                    .expect("Name visible");
                name_layout.paint_divider(
                    ui,
                    egui::Rect::from_min_max(
                        egui::pos2(origin.x + name_cell.x, header_top),
                        egui::pos2(
                            origin.x + name_cell.x + name_cell.width,
                            origin.y + total_height,
                        ),
                    ),
                );
            });
    });
    output.view_changed = runtime.changed_frame == Some(frame);
    if !context_open {
        runtime.context = None;
    }
    ui.ctx()
        .data_mut(|data| data.insert_temp(runtime_id, runtime));
    output
}

fn row_cell(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    column: Column,
    torrent: &TorrentItem,
    colors: RowColors,
    status_hidden: bool,
    owner_connected: bool,
) {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("torrent_cell", &torrent.info_hashes, column))
            .max_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
            .layout(if column.numeric() {
                egui::Layout::right_to_left(egui::Align::Center)
            } else {
                egui::Layout::left_to_right(egui::Align::Center)
            }),
    );
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    child.visuals_mut().override_text_color = Some(colors.text.color32());
    if column == Column::Progress {
        child.add(
            egui::ProgressBar::new(torrent.display_progress())
                .desired_width((rect.width() - 8.0).max(1.0))
                .desired_height((rect.height() - 4.0).max(1.0))
                .text(torrent.progress_text())
                .fill(colors.accent.color32()),
        );
        return;
    }
    let marker = StatusPresentation::for_state(torrent.state).marker;
    let value = match column {
        Column::Name => format!(
            "{}{}{}",
            if status_hidden { marker } else { "" },
            if torrent.is_private { "🔒 " } else { "" },
            torrent.name
        ),
        Column::Status => format!("{marker} {}", torrent.status_text()),
        Column::Download => super::format_rate(torrent.download_rate),
        Column::Upload => super::format_rate(torrent.upload_rate),
        Column::Peers => format!("{} / {}", torrent.peers, torrent.seeds),
        Column::Eta => torrent
            .eta_seconds()
            .map(super::format_duration)
            .unwrap_or_else(|| "--".into()),
        Column::Ratio => format!("{:.2}", torrent.ratio()),
        Column::Progress => unreachable!(),
    };
    let response = child.add(egui::Label::new(value).selectable(false).truncate().halign(
        if column.numeric() {
            egui::Align::Max
        } else {
            egui::Align::Min
        },
    ));
    if column == Column::Name || column == Column::Status {
        response.on_hover_ui(|ui| {
            if column == Column::Name {
                ui.label(&torrent.name);
            }
            if column == Column::Status || status_hidden {
                let readiness = crate::diagnostics::readiness(torrent, None, owner_connected);
                ui.strong(readiness.reason);
                ui.label(readiness.next_action);
            }
        });
    }
}

fn column_menu(
    ui: &mut egui::Ui,
    column: Column,
    columns: &mut TableColumns,
    name: &mut NameColumn,
    runtime: &mut Runtime,
    frame: u64,
) {
    ui.strong(format!("{} column", column.label()));
    let visible: Vec<_> = columns.visible().collect();
    let position = visible
        .iter()
        .position(|candidate| *candidate == column)
        .expect("visible header");
    let target_index = |candidate| {
        columns
            .order
            .iter()
            .position(|column| *column == candidate)
            .expect("normalized columns")
    };
    let previous = target_index(visible[position.saturating_sub(1)]);
    let next = target_index(visible[(position + 1).min(visible.len() - 1)]);
    for (label, target, enabled) in [
        ("Move left", previous, position > 0),
        ("Move right", next, position + 1 < visible.len()),
    ] {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
            && runtime.layout_frame != Some(frame)
        {
            if columns.move_to(column, target) {
                changed(runtime, frame, ui);
            }
            ui.close();
        }
    }
    if ui
        .button(if column == Column::Name {
            "Fill available width"
        } else {
            "Automatic width"
        })
        .clicked()
        && runtime.layout_frame != Some(frame)
    {
        if column == Column::Name {
            name.reset_width();
            changed(runtime, frame, ui);
        } else if columns.reset_width(column) {
            changed(runtime, frame, ui);
        }
        ui.close();
    }
    ui.separator();
    ui.label("Visible columns");
    for candidate in Column::ALL {
        let mut visible = columns.is_visible(candidate);
        let checkbox = ui.add_enabled(
            candidate != Column::Name,
            egui::Checkbox::new(&mut visible, candidate.label()),
        );
        if checkbox.changed()
            && runtime.layout_frame != Some(frame)
            && columns.set_visible(candidate, visible)
        {
            changed(runtime, frame, ui);
        }
    }
    ui.separator();
    if ui.button("Restore default columns").clicked() && runtime.layout_frame != Some(frame) {
        *columns = TableColumns::default();
        name.reset_width();
        changed(runtime, frame, ui);
        ui.close();
    }
}

#[cfg(test)]
mod tests;
