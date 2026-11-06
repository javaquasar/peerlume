use std::collections::BTreeMap;

use crate::metainfo::MetainfoFile;
mod deferred;
mod hierarchy;
mod metadata;
pub use hierarchy::{Expansion, FileHierarchy, FolderSelection, HierarchyRow};
#[cfg(test)]
mod deferred_tests;
pub use deferred::{CompactFileUpdate, FileMemoryUsage, FileState, FileSummary};
use deferred::{CompactFiles, DeferredFiles, bit, set_bit};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    high_priority: Vec<u8>,
    view_cache: std::sync::Arc<std::sync::Mutex<Option<FileViewCache>>>,
    hierarchy_cache: hierarchy::Cache,
    selection_revision: u64,
    metadata: metadata::Revision,
    files: Vec<TorrentFile>,
    deferred: Option<DeferredFiles>,
    rows: Vec<FileTreeRow>,
    total_size: u64,
    selected_count: usize,
    selected_size: u64,
    selected_downloaded: u64,
    // Exact engine aggregate while detailed file telemetry is not subscribed.
    // Individual file counts remain the last genuine snapshot, never guessed.
    selected_downloaded_summary: Option<u64>,
    download_cursor: usize,
}

#[derive(Clone, Debug)]
struct FileViewCache {
    query: String,
    sort: u8,
    rows: std::sync::Arc<Vec<FileTreeRow>>,
}

impl FileSelectionModel {
    pub fn high_priority(&self) -> Vec<usize> {
        self.high_priority
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte != 0)
            .flat_map(|(index, byte)| {
                (0..8).filter_map(move |bit| (byte & (1 << bit) != 0).then_some(index * 8 + bit))
            })
            .filter(|index| *index < self.file_count())
            .collect()
    }
    pub fn is_high_priority(&self, index: usize) -> bool {
        bit(&self.high_priority, index)
    }
    pub fn set_high_priority(&mut self, indices: &[usize]) {
        let count = self.file_count();
        let mut incoming = vec![0; count.div_ceil(8)];
        for &index in indices.iter().filter(|index| **index < count) {
            set_bit(&mut incoming, index, true);
        }
        let changed = metadata::priorities_changed(&self.high_priority, &incoming, count);
        self.high_priority = incoming;
        if changed {
            self.metadata.changed();
        }
    }
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
        let hierarchy_cache = hierarchy::Cache::loaded(&files);
        let mut model = Self {
            files,
            rows,
            hierarchy_cache,
            ..Self::default()
        };
        model.refresh_selection_totals();
        model.metadata.loaded(&model.files);
        model
    }

    pub fn files(&self) -> &[TorrentFile] {
        &self.files
    }

    pub fn file_count(&self) -> usize {
        self.deferred
            .as_ref()
            .map(DeferredFiles::len)
            .unwrap_or(self.files.len())
    }
    pub fn details_loaded(&self) -> bool {
        self.deferred.is_none()
    }
    /// O(1) mutation fence, scoped to the authoritative owner instance. Download
    /// telemetry and loaded/cold representation changes do not advance it.
    pub fn metadata_revision(&self) -> u64 {
        self.metadata.value()
    }
    /// Trusted native projection only: call after validating owner identity and
    /// every detail page's command/file fence. Never bless stale retained bits
    /// with a newer header's fence when those pages were not obtained.
    pub fn adopt_metadata_revision(&mut self, revision: u64) -> Result<(), String> {
        self.metadata.adopt(revision)
    }
    pub fn total_size(&self) -> u64 {
        self.total_size
    }
    pub fn checked_total_size(&self) -> Option<u64> {
        match &self.deferred {
            Some(DeferredFiles::Summary(summary)) => Some(summary.total_size),
            Some(DeferredFiles::Compact(files)) => files
                .sizes
                .iter()
                .try_fold(0_u64, |sum, size| sum.checked_add(*size)),
            None => self
                .files
                .iter()
                .try_fold(0_u64, |sum, file| sum.checked_add(file.size_bytes)),
        }
    }
    pub fn try_from_compact_states<E>(
        states: impl IntoIterator<Item = Result<FileState, E>>,
    ) -> Result<Self, E> {
        let mut compact = CompactFiles::default();
        let mut priority = Vec::new();
        for (index, state) in states.into_iter().enumerate() {
            let state = state?;
            compact.sizes.push(state.size_bytes);
            compact
                .downloaded
                .push(state.downloaded_bytes.min(state.size_bytes));
            if index % 8 == 0 {
                compact.selected.push(0);
                priority.push(0);
            }
            set_bit(&mut compact.selected, index, state.selected);
            set_bit(&mut priority, index, state.high_priority);
        }
        let mut model = Self {
            high_priority: priority,
            deferred: Some(DeferredFiles::Compact(compact)),
            ..Default::default()
        };
        model.refresh_selection_totals();
        Ok(model)
    }
    pub fn summary(&self) -> FileSummary {
        FileSummary {
            file_count: self.file_count(),
            total_size: self.total_size,
            selected_count: self.selected_count,
            selected_size: self.selected_size,
            selected_downloaded: self.selected_downloaded(),
        }
    }
    /// A trusted frontend mirror can retain an O(1) header until detail pages arrive.
    /// This state cannot manufacture a complete selection or a persistence snapshot.
    pub fn from_summary(summary: FileSummary) -> Self {
        Self {
            deferred: Some(DeferredFiles::Summary(summary)),
            total_size: summary.total_size,
            selected_count: summary.selected_count,
            selected_size: summary.selected_size,
            selected_downloaded: summary.selected_downloaded,
            selected_downloaded_summary: Some(summary.selected_downloaded),
            ..Default::default()
        }
    }
    pub fn from_compact_snapshot(
        files: impl IntoIterator<Item = (u64, u64, bool)>,
        high: &[usize],
    ) -> Self {
        let mut compact = CompactFiles::default();
        for (index, (size, downloaded, selected)) in files.into_iter().enumerate() {
            compact.sizes.push(size);
            compact.downloaded.push(downloaded.min(size));
            if index % 8 == 0 {
                compact.selected.push(0);
            }
            set_bit(&mut compact.selected, index, selected);
        }
        let mut model = Self {
            deferred: Some(DeferredFiles::Compact(compact)),
            ..Default::default()
        };
        model.refresh_selection_totals();
        model.set_high_priority(high);
        model
    }
    pub fn file_state(&self, index: usize) -> Option<FileState> {
        let (size_bytes, downloaded_bytes, selected) = match &self.deferred {
            Some(DeferredFiles::Compact(files)) => (
                *files.sizes.get(index)?,
                *files.downloaded.get(index)?,
                bit(&files.selected, index),
            ),
            Some(DeferredFiles::Summary(_)) => return None,
            None => {
                let file = self.files.get(index)?;
                (file.size_bytes, file.downloaded_bytes, file.selected)
            }
        };
        Some(FileState {
            size_bytes,
            downloaded_bytes,
            selected,
            high_priority: self.is_high_priority(index),
        })
    }
    pub fn selected_indices(&self) -> Option<Vec<usize>> {
        if matches!(self.deferred, Some(DeferredFiles::Summary(_))) {
            return None;
        }
        Some(
            (0..self.file_count())
                .filter(|index| self.file_state(*index).is_some_and(|file| file.selected))
                .collect(),
        )
    }
    pub fn persistence_snapshot(&self, with_progress: bool) -> Option<CompactFileUpdate> {
        if matches!(self.deferred, Some(DeferredFiles::Summary(_))) {
            return None;
        }
        let count = self.file_count();
        let mut snapshot = CompactFileUpdate {
            file_count: count,
            selected_bits: vec![0; count.div_ceil(8)],
            priority_bits: vec![0; count.div_ceil(8)],
            downloaded: if with_progress {
                Vec::with_capacity(count)
            } else {
                Vec::new()
            },
        };
        for index in 0..count {
            let state = self.file_state(index)?;
            set_bit(&mut snapshot.selected_bits, index, state.selected);
            set_bit(&mut snapshot.priority_bits, index, state.high_priority);
            if with_progress {
                snapshot.downloaded.push(state.downloaded_bytes);
            }
        }
        Some(snapshot)
    }
    pub fn apply_persistence_snapshot(
        &mut self,
        snapshot: &CompactFileUpdate,
    ) -> Result<(), String> {
        let with_progress = !snapshot.downloaded.is_empty();
        if !snapshot.valid(with_progress)
            || snapshot.file_count != self.file_count()
            || matches!(self.deferred, Some(DeferredFiles::Summary(_)))
        {
            return Err("File snapshot requires matching authoritative detail metadata".into());
        }
        for index in 0..self.file_count() {
            self.set_selected(index, snapshot.selected(index));
            if with_progress {
                self.set_file_downloaded(index, snapshot.downloaded[index]);
            }
        }
        let priorities_changed = metadata::priorities_changed(
            &self.high_priority,
            &snapshot.priority_bits,
            self.file_count(),
        );
        self.high_priority = snapshot.priority_bits.clone();
        if priorities_changed {
            self.metadata.changed();
        }
        Ok(())
    }
    pub fn apply_summary(&mut self, summary: FileSummary) -> Result<(), String> {
        if summary.file_count != self.file_count()
            || summary.total_size != self.total_size
            || summary.selected_count > summary.file_count
            || summary.selected_size > summary.total_size
            || summary.selected_downloaded > summary.selected_size
        {
            return Err("File summary identity or bounds changed; reload details".into());
        }
        self.selected_count = summary.selected_count;
        self.selected_size = summary.selected_size;
        self.selected_downloaded_summary = Some(summary.selected_downloaded);
        if let Some(DeferredFiles::Summary(saved)) = &mut self.deferred {
            *saved = summary;
        }
        Ok(())
    }
    /// Release paths, tree and search results; retain exact selection arithmetic.
    pub fn defer_details(&mut self) -> bool {
        if self.deferred.is_some() {
            return false;
        }
        if self.metadata.legacy_pending() {
            self.refresh_selection_totals();
        }
        let mut compact = CompactFiles {
            sizes: Vec::with_capacity(self.files.len()),
            downloaded: Vec::with_capacity(self.files.len()),
            selected: vec![0; self.files.len().div_ceil(8)],
        };
        for (index, file) in std::mem::take(&mut self.files).into_iter().enumerate() {
            compact.sizes.push(file.size_bytes);
            compact.downloaded.push(file.downloaded_bytes);
            set_bit(&mut compact.selected, index, file.selected);
        }
        self.deferred = Some(DeferredFiles::Compact(compact));
        self.rows = Vec::new();
        self.view_cache = Default::default();
        self.hierarchy_cache = Default::default();
        true
    }
    /// Paths from a bounded loader are merged with current canonical compact data.
    /// A caller must reject stale owner/selection generations before this method.
    pub fn hydrate_paths(&mut self, paths: Vec<String>) -> Result<(), String> {
        if paths.len() != self.file_count() {
            return Err("File detail count changed; reload the authoritative snapshot".into());
        }
        crate::limits::validate_relative_paths(
            paths.iter().map(String::as_str),
            &crate::limits::ResourceLimits::default(),
        )?;
        let Some(DeferredFiles::Compact(_)) = &self.deferred else {
            return if self.details_loaded() {
                Ok(())
            } else {
                Err("Summary-only file details require complete authoritative pages".into())
            };
        };
        let Some(DeferredFiles::Compact(compact)) = self.deferred.take() else {
            unreachable!()
        };
        self.files = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| TorrentFile {
                path,
                size_bytes: compact.sizes[index],
                downloaded_bytes: compact.downloaded[index],
                selected: bit(&compact.selected, index),
            })
            .collect();
        self.metadata.hydrate(
            self.files
                .iter()
                .map(|file| (file.path.as_str(), file.size_bytes)),
        );
        self.rows = build_tree_rows(&self.files);
        self.view_cache = Default::default();
        self.hierarchy_cache = hierarchy::Cache::loaded(&self.files);
        Ok(())
    }
    pub fn memory_usage(&self) -> FileMemoryUsage {
        let row_bytes = |rows: &Vec<FileTreeRow>| {
            rows.capacity() * std::mem::size_of::<FileTreeRow>()
                + rows
                    .iter()
                    .map(|row| match row {
                        FileTreeRow::Group { name, .. } => name.capacity(),
                        _ => 0,
                    })
                    .sum::<usize>()
        };
        let view_capacity_bytes = self
            .view_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|cache| row_bytes(&cache.rows) + cache.query.capacity())
            .unwrap_or_default();
        let compact_capacity_bytes = match &self.deferred {
            Some(DeferredFiles::Compact(files)) => {
                (files.sizes.capacity() + files.downloaded.capacity()) * std::mem::size_of::<u64>()
                    + files.selected.capacity()
            }
            _ => 0,
        };
        let (hierarchy_bytes, hierarchy_view_bytes) = self.hierarchy_cache.allocated_bytes();
        FileMemoryUsage {
            file_slots_bytes: self.files.capacity() * std::mem::size_of::<TorrentFile>(),
            path_capacity_bytes: self.files.iter().map(|file| file.path.capacity()).sum(),
            tree_capacity_bytes: row_bytes(&self.rows) + hierarchy_bytes,
            view_capacity_bytes: view_capacity_bytes + hierarchy_view_bytes,
            compact_capacity_bytes,
            priority_capacity_bytes: self.high_priority.capacity(),
        }
    }

    pub fn rows(&self) -> &[FileTreeRow] {
        &self.rows
    }

    /// Rebuild only when the view changes, not on each progress tick or frame.
    pub fn view_rows(&self, query: &str, sort: u8) -> std::sync::Arc<Vec<FileTreeRow>> {
        let query = query.to_lowercase();
        let mut cache = self.view_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(view) = cache
            .as_ref()
            .filter(|view| view.query == query && view.sort == sort)
        {
            return view.rows.clone();
        }
        let rows = if query.is_empty() && sort == 0 {
            self.rows.clone()
        } else {
            let mut indices: Vec<_> = self
                .files
                .iter()
                .enumerate()
                .filter_map(|(i, file)| file.path.to_lowercase().contains(&query).then_some(i))
                .collect();
            if sort == 2 {
                indices.sort_by_key(|i| (std::cmp::Reverse(self.files[*i].size_bytes), *i));
            } else {
                indices.sort_by(|a, b| self.files[*a].path.cmp(&self.files[*b].path));
            }
            indices
                .into_iter()
                .map(|index| FileTreeRow::File {
                    index,
                    display_start: 0,
                })
                .collect()
        };
        let rows = std::sync::Arc::new(rows);
        *cache = Some(FileViewCache {
            query,
            sort,
            rows: rows.clone(),
        });
        rows
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
        if matches!(self.deferred, Some(DeferredFiles::Summary(_))) {
            return None;
        }
        Some(
            (0..self.file_count())
                .filter(|&index| {
                    edits
                        .get(&index)
                        .copied()
                        .or(all)
                        .unwrap_or_else(|| self.file_state(index).is_some_and(|file| file.selected))
                })
                .collect(),
        )
    }

    pub fn rows_and_files_mut(&mut self) -> (&[FileTreeRow], &mut [TorrentFile]) {
        // Compatibility bulk-edit path. Reconcile with refresh_selection_totals
        // before reading a mutation fence or dispatching an intent. New views
        // render readonly rows and use set_selected for actual checkbox edits.
        self.metadata.begin_legacy(&self.files);
        (&self.rows, &mut self.files)
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn select_all(&mut self, selected: bool) {
        for index in 0..self.file_count() {
            self.set_selected(index, selected);
        }
    }

    pub fn set_selected(&mut self, index: usize, selected: bool) -> bool {
        if let Some(DeferredFiles::Compact(files)) = &mut self.deferred {
            let Some(&size) = files.sizes.get(index) else {
                return false;
            };
            let previous = bit(&files.selected, index);
            if previous == selected {
                return false;
            }
            self.selected_downloaded_summary = None;
            self.selection_revision = self.selection_revision.wrapping_add(1);
            self.metadata.changed();
            let downloaded = files.downloaded[index].min(size);
            if selected {
                self.selected_count += 1;
                self.selected_size = self.selected_size.saturating_add(size);
                self.selected_downloaded = self.selected_downloaded.saturating_add(downloaded);
            } else {
                self.selected_count = self.selected_count.saturating_sub(1);
                self.selected_size = self.selected_size.saturating_sub(size);
                self.selected_downloaded = self.selected_downloaded.saturating_sub(downloaded);
            }
            set_bit(&mut files.selected, index, selected);
            return true;
        }
        let Some(file) = self.files.get_mut(index) else {
            return false;
        };
        if file.selected == selected {
            return false;
        }
        self.selected_downloaded_summary = None;
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.metadata.changed();
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
        if self.metadata.finish_legacy(&self.files) {
            self.rows = build_tree_rows(&self.files);
            self.hierarchy_cache = hierarchy::Cache::loaded(&self.files);
            self.view_cache = Default::default();
        }
        self.selection_revision = self.selection_revision.wrapping_add(1);
        if matches!(self.deferred, Some(DeferredFiles::Summary(_))) {
            return;
        }
        self.selected_downloaded_summary = None;
        self.selected_count = 0;
        self.selected_size = 0;
        self.selected_downloaded = 0;
        self.total_size = 0;
        if let Some(DeferredFiles::Compact(files)) = &self.deferred {
            for (index, size) in files.sizes.iter().copied().enumerate() {
                self.total_size = self.total_size.saturating_add(size);
                if bit(&files.selected, index) {
                    self.selected_count += 1;
                    self.selected_size = self.selected_size.saturating_add(size);
                    self.selected_downloaded = self
                        .selected_downloaded
                        .saturating_add(files.downloaded[index].min(size));
                }
            }
            return;
        }
        for file in &self.files {
            self.total_size = self.total_size.saturating_add(file.size_bytes);
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
        self.selected_downloaded_summary
            .unwrap_or(self.selected_downloaded)
    }

    pub fn set_selected_downloaded_summary(&mut self, bytes: u64) {
        self.selected_downloaded_summary = Some(bytes.min(self.selected_size));
    }

    pub fn set_file_downloaded(&mut self, index: usize, downloaded_bytes: u64) -> bool {
        if let Some(DeferredFiles::Compact(files)) = &mut self.deferred {
            let Some(&size) = files.sizes.get(index) else {
                return false;
            };
            self.selected_downloaded_summary = None;
            let old = files.downloaded[index];
            let new = downloaded_bytes.min(size);
            files.downloaded[index] = new;
            if bit(&files.selected, index) {
                self.selected_downloaded = self
                    .selected_downloaded
                    .saturating_sub(old)
                    .saturating_add(new);
            }
            return old != new;
        }
        let Some(file) = self.files.get_mut(index) else {
            return false;
        };
        self.selected_downloaded_summary = None;
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
        self.selected_downloaded_summary = None;
        if bytes == 0 || self.file_count() == 0 || self.selected_downloaded >= self.selected_size {
            return 0;
        }
        let original = bytes;
        let mut visited = 0;
        while bytes > 0 && visited < self.file_count() {
            let index = self.download_cursor % self.file_count();
            self.download_cursor = (index + 1) % self.file_count();
            visited += 1;
            let Some(file) = self.file_state(index) else {
                break;
            };
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
        if matches!(self.deferred, Some(DeferredFiles::Summary(_))) {
            self.set_selected_downloaded_summary(total_downloaded);
            return;
        }
        self.selected_downloaded_summary = None;
        if let Some(DeferredFiles::Compact(files)) = &mut self.deferred {
            files.downloaded.fill(0);
        }
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
    fn aggregate_progress_never_fabricates_files_or_double_counts_fresh_details() {
        let mut files = FileSelectionModel::from_files([("a".into(), 100), ("b".into(), 100)]);
        files.set_selected_downloaded_summary(150);
        assert_eq!(files.selected_downloaded(), 150);
        assert_eq!(files.files()[0].downloaded_bytes, 0);
        assert_eq!(files.files()[1].downloaded_bytes, 0);
        files.set_file_downloaded(0, 100);
        files.set_file_downloaded(1, 50);
        assert_eq!(files.selected_downloaded(), 150);
        files.set_selected_downloaded_summary(200);
        files.set_file_downloaded(0, 100);
        files.set_file_downloaded(1, 100);
        assert_eq!(files.selected_downloaded(), 200);
        files.set_selected(1, false);
        assert_eq!(files.selected_downloaded(), 100);
        files.set_selected_downloaded_summary(u64::MAX);
        assert_eq!(files.selected_downloaded(), 100);
    }
    #[test]
    fn file_search_and_sort_cache_survive_progress_without_rebuilding_or_mutating_selection() {
        let mut files = FileSelectionModel::from_files([
            ("folder/z.bin".into(), 1),
            ("folder/a.bin".into(), 8),
            ("other.bin".into(), 2),
        ]);
        let view = files.view_rows("folder", 2);
        assert_eq!(
            view.as_ref(),
            &[
                FileTreeRow::File {
                    index: 1,
                    display_start: 0
                },
                FileTreeRow::File {
                    index: 0,
                    display_start: 0
                }
            ]
        );
        files.set_file_downloaded(0, 1);
        assert!(std::sync::Arc::ptr_eq(&view, &files.view_rows("folder", 2)));
        assert_eq!(files.selected_count(), 3);
        assert!(files.view_rows("missing", 0).is_empty());
        assert_eq!(files.selected_count(), 3);
    }

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
