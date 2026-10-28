//! One-shot clean-stop cache. Metadata is not proof against timestamp-preserving
//! edits: force recheck remains the full cryptographic audit.
use crate::storage::sandbox::Directory;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Cache {
    #[serde(skip)]
    path: PathBuf,
    entries: BTreeMap<String, Snapshot>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    root: String,
    bits: Vec<u8>,
    files: Vec<(String, Stamp)>,
}
#[derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Stamp {
    identity: u64,
    length: u64,
    modified_ns: u128,
    created_ns: u128,
}
fn stamp(root: &Directory, path: &str) -> io::Result<Stamp> {
    use std::hash::{Hash, Hasher};
    let file = root.read_payload(Path::new(path))?;
    let metadata = file.metadata()?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    same_file::Handle::from_file(file)?.hash(&mut identity);
    Ok(Stamp {
        identity: identity.finish(),
        length: metadata.len(),
        modified_ns: metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
        created_ns: metadata
            .created()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    })
}
pub(crate) type Checkpoint = (String, String, Vec<u8>, Directory, Vec<String>);
impl Cache {
    pub fn begin(profile: &Path) -> io::Result<Self> {
        let path = profile.join("fast-resume.json");
        let mut cache = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() <= 32 * 1024 * 1024 {
                    serde_json::from_slice(&bytes).unwrap_or_default()
                } else {
                    Self::default()
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        // Invalidate before starting peers. A crash cannot reuse a stale clean marker.
        match std::fs::remove_file(&path) {
            Ok(()) => sync_parent(profile)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        cache.path = path;
        Ok(cache)
    }
    pub fn take(&mut self, id: &str, folder: &str, root: &Directory) -> Option<Vec<u8>> {
        let saved = self.entries.remove(id)?;
        if saved.root != folder
            || saved
                .files
                .iter()
                .any(|(path, expected)| stamp(root, path).ok().as_ref() != Some(expected))
        {
            return None;
        }
        Some(saved.bits)
    }
    pub fn checkpoint(&mut self, entries: Vec<Checkpoint>) -> io::Result<()> {
        self.entries.clear();
        for (id, folder, bits, root, paths) in entries {
            let files: io::Result<Vec<_>> = paths
                .into_iter()
                .map(|path| {
                    root.sync_payload(Path::new(&path))?;
                    let state = stamp(&root, &path)?;
                    Ok((path, state))
                })
                .collect();
            if let Ok(files) = files {
                self.entries.insert(
                    id,
                    Snapshot {
                        root: folder,
                        bits,
                        files,
                    },
                );
            }
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Resume cache has no parent"))?;
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut output, &self)?;
        if output.as_file().metadata()?.len() > 32 * 1024 * 1024 {
            return Err(io::Error::other(
                "Resume cache budget exceeded; full recheck required",
            ));
        }
        output.flush()?;
        output.as_file().sync_all()?;
        output.persist(&self.path).map_err(|error| error.error)?;
        sync_parent(parent)
    }
}
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clean_cache_is_one_shot_and_crash_or_changes_require_check() {
        let profile = tempfile::tempdir().unwrap();
        let payload = tempfile::tempdir().unwrap();
        std::fs::write(payload.path().join("x"), b"abc").unwrap();
        let root = Directory::open(payload.path()).unwrap();
        let folder = payload.path().display().to_string();
        let mut cache = Cache::begin(profile.path()).unwrap();
        cache
            .checkpoint(vec![(
                "hash".into(),
                folder.clone(),
                vec![128],
                root.clone(),
                vec!["x".into()],
            )])
            .unwrap();
        let mut restored = Cache::begin(profile.path()).unwrap();
        assert_eq!(restored.take("hash", &folder, &root), Some(vec![128]));
        assert_eq!(restored.take("hash", &folder, &root), None);
        assert!(Cache::begin(profile.path()).unwrap().entries.is_empty());
        cache
            .checkpoint(vec![(
                "hash".into(),
                folder.clone(),
                vec![128],
                root.clone(),
                vec!["x".into()],
            )])
            .unwrap();
        std::fs::write(payload.path().join("x"), b"changed length").unwrap();
        assert!(
            Cache::begin(profile.path())
                .unwrap()
                .take("hash", &folder, &root)
                .is_none()
        );
    }
}
