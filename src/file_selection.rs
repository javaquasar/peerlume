use std::collections::BTreeMap;

use crate::metainfo::MetainfoFile;

#[derive(Clone, Debug)]
pub struct TorrentFile {
    pub path: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileTreeRow {
    Group { name: String, file_count: usize },
    File { index: usize, display_start: usize },
}

#[derive(Clone, Debug, Default)]
pub struct FileSelectionModel {
    files: Vec<TorrentFile>,
    rows: Vec<FileTreeRow>,
    selected_count: usize,
    selected_size: u64,
    selected_downloaded: u64,
    download_cursor: usize,
}

impl FileSelectionModel {
    pub fn from_files(files: impl IntoIterator<Item = (String, u64)>) -> Self {
        Self::from_torrent_files(files.into_iter().map(|(path, size_bytes)| TorrentFile {
            path,
            size_bytes,
            downloaded_bytes: 0,
            selected: true,
        }))
    }

    pub fn from_metainfo_files(files: Vec<MetainfoFile>) -> Self {
        Self::from_torrent_files(files.into_iter().map(|file| TorrentFile {
            path: file.path,
            size_bytes: file.size_bytes,
            downloaded_bytes: 0,
            selected: true,
        }))
    }

    pub fn from_snapshot(files: impl IntoIterator<Item = (String, u64, u64, bool)>) -> Self {
        Self::from_torrent_files(files.into_iter().map(
            |(path, size_bytes, downloaded_bytes, selected)| TorrentFile {
                path,
                size_bytes,
                downloaded_bytes: downloaded_bytes.min(size_bytes),
                selected,
            },
        ))
    }

    fn from_torrent_files(files: impl IntoIterator<Item = TorrentFile>) -> Self {
        let files: Vec<_> = files.into_iter().collect();
        let rows = build_tree_rows(&files);
        let mut model = Self {
            files,
            rows,
            ..Self::default()
        };
        model.refresh_selection_totals();
        model
    }

    pub fn files(&self) -> &[TorrentFile] {
        &self.files
    }

    pub fn rows(&self) -> &[FileTreeRow] {
        &self.rows
    }

    /// Build a command only when the UI edited a selection. Never change the
    /// confirmed model or clone its file tree during an ordinary render frame.
    pub fn selection_after_edit(
        &self,
        all: Option<bool>,
        edits: &BTreeMap<usize, bool>,
    ) -> Option<Vec<usize>> {
        if all.is_none() && edits.is_empty() {
            return None;
        }
        Some(
            self.files
                .iter()
                .enumerate()
                .filter_map(|(index, file)| {
                    edits
                        .get(&index)
                        .copied()
                        .or(all)
                        .unwrap_or(file.selected)
                        .then_some(index)
                })
                .collect(),
        )
    }

    pub fn rows_and_files_mut(&mut self) -> (&[FileTreeRow], &mut [TorrentFile]) {
        (&self.rows, &mut self.files)
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn select_all(&mut self, selected: bool) {
        for index in 0..self.files.len() {
            self.set_selected(index, selected);
        }
    }

    pub fn set_selected(&mut self, index: usize, selected: bool) -> bool {
        let Some(file) = self.files.get_mut(index) else {
            return false;
        };
        if file.selected == selected {
            return false;
        }
        if selected {
            self.selected_count += 1;
            self.selected_size = self.selected_size.saturating_add(file.size_bytes);
            self.selected_downloaded = self
                .selected_downloaded
                .saturating_add(file.downloaded_bytes.min(file.size_bytes));
        } else {
            self.selected_count = self.selected_count.saturating_sub(1);
            self.selected_size = self.selected_size.saturating_sub(file.size_bytes);
            self.selected_downloaded = self
                .selected_downloaded
                .saturating_sub(file.downloaded_bytes.min(file.size_bytes));
        }
        file.selected = selected;
        true
    }

    pub fn refresh_selection_totals(&mut self) {
        self.selected_count = 0;
        self.selected_size = 0;
        self.selected_downloaded = 0;
        for file in &self.files {
            if file.selected {
                self.selected_count += 1;
                self.selected_size = self.selected_size.saturating_add(file.size_bytes);
                self.selected_downloaded = self
                    .selected_downloaded
                    .saturating_add(file.downloaded_bytes.min(file.size_bytes));
            }
        }
        if self.download_cursor >= self.files.len() {
            self.download_cursor = 0;
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selected_count
    }

    pub fn selected_size(&self) -> u64 {
        self.selected_size
    }

    pub fn selected_downloaded(&self) -> u64 {
        self.selected_downloaded
    }

    pub fn set_file_downloaded(&mut self, index: usize, downloaded_bytes: u64) -> bool {
        let Some(file) = self.files.get_mut(index) else {
            return false;
        };
        let old = file.downloaded_bytes.min(file.size_bytes);
        let new = downloaded_bytes.min(file.size_bytes);
        file.downloaded_bytes = new;
        if file.selected {
            self.selected_downloaded = self
                .selected_downloaded
                .saturating_sub(old)
                .saturating_add(new);
        }
        old != new
    }

    pub fn apply_downloaded_bytes(&mut self, mut bytes: u64) -> u64 {
        if bytes == 0 || self.files.is_empty() || self.selected_downloaded >= self.selected_size {
            return 0;
        }
        let original = bytes;
        let mut visited = 0;
        while bytes > 0 && visited < self.files.len() {
            let index = self.download_cursor % self.files.len();
            self.download_cursor = (index + 1) % self.files.len();
            visited += 1;
            let file = &self.files[index];
            if !file.selected || file.downloaded_bytes >= file.size_bytes {
                continue;
            }
            let applied = bytes.min(file.size_bytes - file.downloaded_bytes);
            let downloaded = file.downloaded_bytes + applied;
            self.set_file_downloaded(index, downloaded);
            bytes -= applied;
            if downloaded < self.files[index].size_bytes {
                self.download_cursor = index;
                break;
            }
        }
        original - bytes
    }

    pub fn set_total_downloaded(&mut self, total_downloaded: u64) {
        for file in &mut self.files {
            file.downloaded_bytes = 0;
        }
        self.selected_downloaded = 0;
        self.download_cursor = 0;
        self.apply_downloaded_bytes(total_downloaded.min(self.selected_size));
    }
}

fn build_tree_rows(files: &[TorrentFile]) -> Vec<FileTreeRow> {
    let mut grouped: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        let separator = file.path.find(['/', '\\']);
        let (group, display_start) = separator
            .map(|position| (&file.path[..position], position + 1))
            .unwrap_or(("Files", 0));
        grouped
            .entry(group)
            .or_default()
            .push((index, display_start));
    }

    let mut rows = Vec::with_capacity(files.len() + grouped.len());
    for (name, entries) in grouped {
        rows.push(FileTreeRow::Group {
            name: name.to_owned(),
            file_count: entries.len(),
        });
        rows.extend(
            entries
                .into_iter()
                .map(|(index, display_start)| FileTreeRow::File {
                    index,
                    display_start,
                }),
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_edits_build_commands_without_mutating_or_rebuilding_file_models() {
        let model = model();
        let rows = model.rows().as_ptr();
        let files = model.files().as_ptr();
        assert_eq!(model.selection_after_edit(None, &BTreeMap::new()), None);
        assert_eq!(
            model.selection_after_edit(Some(false), &BTreeMap::new()),
            Some(vec![])
        );
        assert_eq!(
            model.selection_after_edit(Some(true), &BTreeMap::new()),
            Some(vec![0, 1, 2])
        );
        assert_eq!(
            model.selection_after_edit(None, &BTreeMap::from([(1, false)])),
            Some(vec![0, 2])
        );
        assert_eq!(
            model.selection_after_edit(Some(false), &BTreeMap::from([(2, true)])),
            Some(vec![2])
        );
        assert_eq!(model.selected_count(), 3);
        assert_eq!(model.rows().as_ptr(), rows);
        assert_eq!(model.files().as_ptr(), files);
    }

    fn model() -> FileSelectionModel {
        FileSelectionModel::from_files([
            ("season/episode-1.mkv".to_owned(), 100),
            ("season/episode-2.mkv".to_owned(), 300),
            ("extras/readme.txt".to_owned(), 20),
        ])
    }

    #[test]
    fn selection_updates_cached_totals_and_lazy_indices() {
        let mut model = model();
        assert!(model.set_selected(1, false));
        assert_eq!(model.selected_count(), 2);
        assert_eq!(model.selected_size(), 120);
        let selected: Vec<_> = model
            .files()
            .iter()
            .enumerate()
            .filter_map(|(index, file)| file.selected.then_some(index))
            .collect();
        assert_eq!(selected, vec![0, 2]);
        model.select_all(false);
        assert_eq!(model.selected_count(), 0);
        assert_eq!(model.selected_size(), 0);
    }

    #[test]
    fn progress_events_touch_selected_files_incrementally() {
        let mut model = model();
        model.set_selected(1, false);
        assert_eq!(model.apply_downloaded_bytes(60), 60);
        assert_eq!(model.files[0].downloaded_bytes, 60);
        assert_eq!(model.files[1].downloaded_bytes, 0);
        assert_eq!(model.files[2].downloaded_bytes, 0);
        assert_eq!(model.selected_downloaded(), 60);
        assert!(model.set_file_downloaded(2, 10));
        assert_eq!(model.selected_downloaded(), 70);
    }

    #[test]
    fn tree_rows_are_built_once_for_one_hundred_thousand_files() {
        const FILE_COUNT: usize = 100_000;
        let files = (0..FILE_COUNT).map(|index| (format!("root/dir/file-{index}.bin"), 1));
        let model = FileSelectionModel::from_files(files);
        assert_eq!(model.files().len(), FILE_COUNT);
        assert_eq!(model.row_count(), FILE_COUNT + 1);
        assert_eq!(model.selected_size(), FILE_COUNT as u64);
        assert!(matches!(
            model.rows[0],
            FileTreeRow::Group {
                file_count: FILE_COUNT,
                ..
            }
        ));
    }

    #[test]
    fn metainfo_paths_are_moved_without_reallocating_strings() {
        let path = "root/a-large-file.bin".to_owned();
        let allocation = path.as_ptr();
        let model = FileSelectionModel::from_metainfo_files(vec![MetainfoFile {
            path,
            size_bytes: 42,
        }]);
        assert_eq!(model.files()[0].path.as_ptr(), allocation);
    }
}
