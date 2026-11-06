//! Loaded-model-owned hierarchy. Presentation state never owns file paths.
use super::{FileSelectionModel, TorrentFile};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    default_open: bool,
    exceptions: BTreeSet<usize>,
}
impl Default for Expansion {
    fn default() -> Self {
        Self {
            default_open: true,
            exceptions: BTreeSet::new(),
        }
    }
}
impl Expansion {
    pub fn is_open(&self, folder: usize) -> bool {
        self.default_open != self.exceptions.contains(&folder)
    }
    pub fn toggle(&mut self, folder: usize) {
        if !self.exceptions.remove(&folder) {
            self.exceptions.insert(folder);
        }
    }
    pub fn set_all(&mut self, open: bool) {
        self.default_open = open;
        self.exceptions.clear();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HierarchyRow {
    Folder {
        id: usize,
        depth: usize,
    },
    File {
        index: usize,
        depth: usize,
        display_start: usize,
    },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderSelection {
    pub selected: usize,
    pub total: usize,
}
impl FolderSelection {
    pub fn all(self) -> bool {
        self.total > 0 && self.selected == self.total
    }
    pub fn mixed(self) -> bool {
        self.selected > 0 && self.selected < self.total
    }
}
#[derive(Debug)]
pub struct Folder {
    pub name: String,
    pub parent: Option<usize>,
    pub depth: usize,
    children: Vec<usize>,
    files: Vec<usize>,
    descendants: Range<usize>,
}
#[derive(Debug, Default)]
pub struct FileHierarchy {
    folders: Vec<Folder>,
    roots: Vec<usize>,
    root_files: Vec<usize>,
    file_parents: Vec<Option<usize>>,
    display_starts: Vec<usize>,
    ordered_files: Vec<usize>,
}
enum Visit {
    Folder(usize),
    File(usize),
    End(usize),
}

impl FileHierarchy {
    fn build(files: &[TorrentFile]) -> Result<Self, String> {
        Self::build_bounded(files, crate::limits::ResourceLimits::default().max_files)
    }
    fn build_bounded(files: &[TorrentFile], max_folders: usize) -> Result<Self, String> {
        let mut index = Self {
            file_parents: Vec::with_capacity(files.len()),
            display_starts: Vec::with_capacity(files.len()),
            ..Default::default()
        };
        let mut lookup: BTreeMap<(Option<usize>, &str), usize> = BTreeMap::new();
        for (file_index, file) in files.iter().enumerate() {
            let mut components = file.path.split(['/', '\\']).peekable();
            let mut parent = None;
            while let Some(component) = components.next() {
                if components.peek().is_none() {
                    break;
                }
                if component.is_empty() {
                    continue;
                }
                let key = (parent, component);
                let folder = if let Some(id) = lookup.get(&key) {
                    *id
                } else {
                    if index.folders.len() >= max_folders {
                        return Err(
                            "Folder hierarchy exceeds its node budget; showing flat file paths"
                                .into(),
                        );
                    }
                    let id = index.folders.len();
                    index.folders.push(Folder {
                        name: component.into(),
                        parent,
                        depth: parent.map_or(0, |parent| index.folders[parent].depth + 1),
                        children: Vec::new(),
                        files: Vec::new(),
                        descendants: 0..0,
                    });
                    if let Some(parent) = parent {
                        index.folders[parent].children.push(id);
                    } else {
                        index.roots.push(id);
                    }
                    lookup.insert(key, id);
                    id
                };
                parent = Some(folder);
            }
            index.file_parents.push(parent);
            index
                .display_starts
                .push(file.path.rfind(['/', '\\']).map_or(0, |offset| offset + 1));
            if let Some(parent) = parent {
                index.folders[parent].files.push(file_index);
            } else {
                index.root_files.push(file_index);
            }
        }
        index
            .roots
            .sort_by(|a, b| index.folders[*a].name.cmp(&index.folders[*b].name));
        for folder in 0..index.folders.len() {
            let mut children = std::mem::take(&mut index.folders[folder].children);
            children.sort_by(|a, b| index.folders[*a].name.cmp(&index.folders[*b].name));
            index.folders[folder].children = children;
        }
        let mut stack = Vec::new();
        stack.extend(index.root_files.iter().rev().copied().map(Visit::File));
        stack.extend(index.roots.iter().rev().copied().map(Visit::Folder));
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Folder(id) => {
                    index.folders[id].descendants.start = index.ordered_files.len();
                    stack.push(Visit::End(id));
                    stack.extend(
                        index.folders[id]
                            .files
                            .iter()
                            .rev()
                            .copied()
                            .map(Visit::File),
                    );
                    stack.extend(
                        index.folders[id]
                            .children
                            .iter()
                            .rev()
                            .copied()
                            .map(Visit::Folder),
                    );
                }
                Visit::File(file) => index.ordered_files.push(file),
                Visit::End(id) => index.folders[id].descendants.end = index.ordered_files.len(),
            }
        }
        Ok(index)
    }
    pub fn folder(&self, id: usize) -> Option<&Folder> {
        self.folders.get(id)
    }
    pub fn folder_count(&self) -> usize {
        self.folders.len()
    }
    pub fn descendants(&self, id: usize) -> &[usize] {
        self.folders.get(id).map_or(&[], |folder| {
            &self.ordered_files[folder.descendants.clone()]
        })
    }
    pub fn single_root(&self) -> Option<usize> {
        (self.root_files.is_empty() && self.roots.len() == 1).then(|| self.roots[0])
    }
    fn rows(&self, files: &[TorrentFile], query: &str, expansion: &Expansion) -> Vec<HierarchyRow> {
        let searching = !query.is_empty();
        let matches: Vec<_> = if searching {
            files
                .iter()
                .map(|file| file.path.to_lowercase().contains(query))
                .collect()
        } else {
            Vec::new()
        };
        let mut folder_matches = vec![!searching; self.folders.len()];
        if searching {
            for (file, parent) in self.file_parents.iter().enumerate() {
                if matches[file]
                    && let Some(parent) = parent
                {
                    folder_matches[*parent] = true;
                }
            }
            for id in (0..self.folders.len()).rev() {
                if folder_matches[id]
                    && let Some(parent) = self.folders[id].parent
                {
                    folder_matches[parent] = true;
                }
            }
        }
        let mut rows = Vec::new();
        let mut stack = Vec::new();
        stack.extend(self.root_files.iter().rev().copied().map(Visit::File));
        stack.extend(self.roots.iter().rev().copied().map(Visit::Folder));
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Folder(id) if folder_matches[id] => {
                    let folder = &self.folders[id];
                    rows.push(HierarchyRow::Folder {
                        id,
                        depth: folder.depth,
                    });
                    if searching || expansion.is_open(id) {
                        stack.extend(folder.files.iter().rev().copied().map(Visit::File));
                        stack.extend(folder.children.iter().rev().copied().map(Visit::Folder));
                    }
                }
                Visit::File(index) if !searching || matches[index] => {
                    rows.push(HierarchyRow::File {
                        index,
                        depth: self.file_parents[index].map_or(0, |id| self.folders[id].depth + 1),
                        display_start: self.display_starts[index],
                    })
                }
                _ => {}
            }
        }
        rows
    }
    fn selection_counts(&self, files: &[TorrentFile]) -> Vec<usize> {
        let mut selected = vec![0; self.folders.len()];
        for (file, parent) in files.iter().zip(&self.file_parents) {
            if file.selected
                && let Some(parent) = parent
            {
                selected[*parent] += 1;
            }
        }
        for id in (0..self.folders.len()).rev() {
            if let Some(parent) = self.folders[id].parent {
                let count = selected[id];
                selected[parent] += count;
            }
        }
        selected
    }
    fn allocated_bytes(&self) -> usize {
        self.folders.capacity() * std::mem::size_of::<Folder>()
            + self
                .folders
                .iter()
                .map(|folder| {
                    folder.name.capacity()
                        + (folder.children.capacity() + folder.files.capacity())
                            * std::mem::size_of::<usize>()
                })
                .sum::<usize>()
            + (self.roots.capacity()
                + self.root_files.capacity()
                + self.display_starts.capacity()
                + self.ordered_files.capacity())
                * std::mem::size_of::<usize>()
            + self.file_parents.capacity() * std::mem::size_of::<Option<usize>>()
    }
}

#[derive(Clone, Debug)]
struct View {
    query: String,
    expansion: Expansion,
    rows: Arc<Vec<HierarchyRow>>,
    file_count: usize,
}
#[derive(Debug, Default)]
pub(super) struct Cache {
    index: Option<Result<Arc<FileHierarchy>, String>>,
    view: Mutex<Option<View>>,
    selection: Mutex<Option<(u64, Arc<Vec<usize>>)>>,
}
impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
            ..Default::default()
        }
    }
}
impl Cache {
    pub fn loaded(files: &[TorrentFile]) -> Self {
        Self {
            index: Some(FileHierarchy::build(files).map(Arc::new)),
            ..Default::default()
        }
    }
    pub fn allocated_bytes(&self) -> (usize, usize) {
        let index = self
            .index
            .as_ref()
            .and_then(|index| index.as_ref().ok())
            .map_or(0, |index| index.allocated_bytes());
        let view = self
            .view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |view| {
                view.query.capacity()
                    + view.rows.capacity() * std::mem::size_of::<HierarchyRow>()
                    + view.expansion.exceptions.len() * std::mem::size_of::<usize>()
            });
        let selection = self
            .selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |(_, counts)| {
                counts.capacity() * std::mem::size_of::<usize>()
            });
        (index + selection, view)
    }
}

impl FileSelectionModel {
    pub fn hierarchy(&self) -> Result<Option<Arc<FileHierarchy>>, String> {
        if !self.details_loaded() {
            return Ok(None);
        }
        if self.hierarchy_cache.index.is_none() {
            static EMPTY: std::sync::OnceLock<Arc<FileHierarchy>> = std::sync::OnceLock::new();
            return Ok(Some(
                EMPTY
                    .get_or_init(|| Arc::new(FileHierarchy::default()))
                    .clone(),
            ));
        }
        self.hierarchy_cache.index.clone().transpose()
    }
    pub fn tree_view(
        &self,
        query: &str,
        expansion: &Expansion,
    ) -> Result<Arc<Vec<HierarchyRow>>, String> {
        let Some(index) = self.hierarchy()? else {
            return Err("Authoritative file details are not loaded".into());
        };
        let query = query.to_lowercase();
        let mut cache = self
            .hierarchy_cache
            .view
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(view) = cache
            .as_ref()
            .filter(|view| view.query == query && view.expansion == *expansion)
        {
            return Ok(view.rows.clone());
        }
        let rows = Arc::new(index.rows(&self.files, &query, expansion));
        let file_count = rows
            .iter()
            .filter(|row| matches!(row, HierarchyRow::File { .. }))
            .count();
        *cache = Some(View {
            query,
            expansion: expansion.clone(),
            rows: rows.clone(),
            file_count,
        });
        Ok(rows)
    }
    pub fn tree_view_file_count(
        &self,
        query: &str,
        expansion: &Expansion,
    ) -> Result<usize, String> {
        self.tree_view(query, expansion)?;
        Ok(self
            .hierarchy_cache
            .view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |view| view.file_count))
    }
    pub fn folder_selection(&self, id: usize) -> Option<FolderSelection> {
        let index = self.hierarchy().ok()??;
        index.folder(id)?;
        let mut cache = self
            .hierarchy_cache
            .selection
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cache
            .as_ref()
            .is_none_or(|(revision, _)| *revision != self.selection_revision)
        {
            *cache = Some((
                self.selection_revision,
                Arc::new(index.selection_counts(&self.files)),
            ));
        }
        Some(FolderSelection {
            selected: cache.as_ref()?.1[id],
            total: index.descendants(id).len(),
        })
    }
    pub fn folder_selection_edits(
        &self,
        id: usize,
        selected: bool,
        edits: &mut BTreeMap<usize, bool>,
    ) -> bool {
        let Ok(Some(index)) = self.hierarchy() else {
            return false;
        };
        if index.folder(id).is_none() {
            return false;
        }
        edits.extend(index.descendants(id).iter().map(|index| (*index, selected)));
        true
    }
}

#[cfg(test)]
mod tests;
