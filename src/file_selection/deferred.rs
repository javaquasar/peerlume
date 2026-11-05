//! Cold frontend metadata contains no paths, tree rows, or search cache.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub file_count: usize,
    pub total_size: u64,
    pub selected_count: usize,
    pub selected_size: u64,
    pub selected_downloaded: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub selected: bool,
    pub high_priority: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactFileUpdate {
    pub file_count: usize,
    pub selected_bits: Vec<u8>,
    pub priority_bits: Vec<u8>,
    pub downloaded: Vec<u64>,
}
impl CompactFileUpdate {
    pub fn selected(&self, index: usize) -> bool {
        bit(&self.selected_bits, index)
    }
    pub fn high_priority(&self, index: usize) -> bool {
        bit(&self.priority_bits, index)
    }
    pub fn valid(&self, with_progress: bool) -> bool {
        self.selected_bits.len() == self.file_count.div_ceil(8)
            && self.priority_bits.len() == self.file_count.div_ceil(8)
            && (!with_progress || self.downloaded.len() == self.file_count)
            && self.file_count <= crate::limits::ResourceLimits::default().max_files
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMemoryUsage {
    pub file_slots_bytes: usize,
    pub path_capacity_bytes: usize,
    pub tree_capacity_bytes: usize,
    pub view_capacity_bytes: usize,
    pub compact_capacity_bytes: usize,
    pub priority_capacity_bytes: usize,
}
impl FileMemoryUsage {
    /// Reachable buffer capacity, not allocator metadata or a whole-process RSS claim.
    pub fn total_bytes(&self) -> usize {
        self.file_slots_bytes
            + self.path_capacity_bytes
            + self.tree_capacity_bytes
            + self.view_capacity_bytes
            + self.compact_capacity_bytes
            + self.priority_capacity_bytes
    }
}

#[derive(Clone, Debug)]
pub(super) enum DeferredFiles {
    Compact(CompactFiles),
    Summary(FileSummary),
}
impl DeferredFiles {
    pub fn len(&self) -> usize {
        match self {
            Self::Compact(files) => files.sizes.len(),
            Self::Summary(summary) => summary.file_count,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub(super) struct CompactFiles {
    pub sizes: Vec<u64>,
    pub downloaded: Vec<u64>,
    pub selected: Vec<u8>,
}
pub(super) fn bit(bits: &[u8], index: usize) -> bool {
    bits.get(index / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}
pub(super) fn set_bit(bits: &mut [u8], index: usize, value: bool) {
    if let Some(byte) = bits.get_mut(index / 8) {
        let mask = 1 << (index % 8);
        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
}
