//! File mutation fences are distinct from download telemetry and representation.
use super::TorrentFile;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);
fn next_after(previous: u64) -> u64 {
    let minimum = previous
        .checked_add(1)
        .expect("File metadata revision exhausted");
    let previous_next = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.max(minimum).checked_add(1)
        })
        .expect("File metadata revision exhausted");
    previous_next.max(minimum)
}

#[derive(Debug)]
struct Legacy {
    revision: u64,
    geometry: [u8; 32],
    selection: [u8; 32],
}
#[derive(Debug)]
pub(super) struct Revision {
    value: u64,
    geometry: Option<[u8; 32]>,
    legacy: Option<Legacy>,
}
impl Default for Revision {
    fn default() -> Self {
        Self {
            value: next_after(0),
            geometry: None,
            legacy: None,
        }
    }
}
impl Clone for Revision {
    fn clone(&self) -> Self {
        Self {
            value: self.value,
            geometry: self.geometry,
            legacy: None,
        }
    }
}
impl Revision {
    pub fn value(&self) -> u64 {
        self.value
    }
    pub fn legacy_pending(&self) -> bool {
        self.legacy.is_some()
    }
    pub fn changed(&mut self) {
        self.value = next_after(self.value);
    }
    pub fn adopt(&mut self, value: u64) -> Result<(), String> {
        if value == 0 || value == u64::MAX {
            return Err("Invalid authoritative file metadata revision".into());
        }
        self.value = value;
        Ok(())
    }
    pub fn loaded(&mut self, files: &[TorrentFile]) {
        self.geometry = Some(geometry(
            files
                .iter()
                .map(|file| (file.path.as_str(), file.size_bytes)),
        ));
    }
    pub fn hydrate<'a>(&mut self, files: impl IntoIterator<Item = (&'a str, u64)>) {
        let incoming = geometry(files);
        if self.geometry.is_some_and(|previous| previous != incoming) {
            self.changed();
        }
        self.geometry = Some(incoming);
    }
    pub fn begin_legacy(&mut self, files: &[TorrentFile]) {
        // Used only by the explicit mutable-slice compatibility API. Ordinary
        // renders/setters never scan or hash a bitmap to read their fence.
        self.legacy = Some(Legacy {
            revision: self.value,
            geometry: geometry(
                files
                    .iter()
                    .map(|file| (file.path.as_str(), file.size_bytes)),
            ),
            selection: selection(files),
        });
    }
    pub fn finish_legacy(&mut self, files: &[TorrentFile]) -> bool {
        let Some(before) = self.legacy.take() else {
            return false;
        };
        let geometry = geometry(
            files
                .iter()
                .map(|file| (file.path.as_str(), file.size_bytes)),
        );
        let changed_geometry = before.geometry != geometry;
        if (changed_geometry || before.selection != selection(files))
            && self.value == before.revision
        {
            self.changed();
        }
        self.geometry = Some(geometry);
        changed_geometry
    }
}

fn geometry<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> [u8; 32] {
    let mut digest = Sha256::new();
    for (path, size) in files {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(size.to_le_bytes());
    }
    digest.finalize().into()
}
fn selection(files: &[TorrentFile]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for file in files {
        digest.update([u8::from(file.selected)]);
    }
    digest.finalize().into()
}
pub(super) fn priorities_changed(old: &[u8], new: &[u8], count: usize) -> bool {
    (0..count.div_ceil(8)).any(|index| {
        let mask = if index + 1 == count.div_ceil(8) && !count.is_multiple_of(8) {
            (1_u8 << (count % 8)) - 1
        } else {
            u8::MAX
        };
        (old.get(index).copied().unwrap_or(0) & mask)
            != (new.get(index).copied().unwrap_or(0) & mask)
    })
}

#[cfg(test)]
mod tests;
