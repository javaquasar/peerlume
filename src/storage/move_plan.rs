//! Atomic per-file same-volume renames with a durable intent across SQLite ACK.
//! Keep Windows capability-directory locks rather than weakening confinement.
use super::sandbox::Directory;
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub files: Vec<(PathBuf, u64)>,
}
pub(crate) fn root_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
            .trim_start_matches("//?/")
            .to_ascii_lowercase()
    } else {
        text.into_owned()
    }
}
pub(crate) fn prepare(
    profile: &Path,
    id: &str,
    source: &Path,
    parent: &Path,
    root: &Directory,
    files: Vec<(PathBuf, u64)>,
) -> io::Result<(Directory, String)> {
    if !source.is_absolute() || !parent.is_absolute() || source.parent().is_none() {
        return Err(io::Error::other(
            "Move roots must be absolute, non-root directories",
        ));
    }
    let source = source.canonicalize()?;
    let parent = parent.canonicalize()?;
    if parent.starts_with(&source) || !root.matches_path(&source)? || !root.only_payload(&files)? {
        return Err(io::Error::other(
            "Fast move needs an exclusive complete torrent directory; use verified copy for shared directories",
        ));
    }
    let name = format!("PeerLume-move-{}", crate::control::access::random_id()?);
    let destination = parent.join(&name);
    if destination.try_exists()? {
        return Err(io::Error::other("Move destination already exists"));
    }
    let target = Directory::open(&parent)?.new_child(&name)?;
    for (path, _) in &files {
        root.sync_payload(path)?;
    }
    let plan = Plan {
        id: id.into(),
        source: source.clone(),
        destination: destination.clone(),
        files,
    };
    let journal = profile.join(format!("storage-move-{name}.json"));
    let bytes = serde_json::to_vec(&plan)?;
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::other(
            "Move journal exceeds budget; original retained",
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&journal)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    sync(profile)?;
    // An initial EXDEV leaves all originals intact. Partial failures retain intent.
    for (index, (path, _)) in plan.files.iter().enumerate() {
        if let Err(error) = root.rename_payload(&target, path) {
            if index == 0 && root.read_payload(path).is_ok() {
                std::fs::remove_file(&journal)?;
                sync(profile)?;
            }
            return Err(error);
        }
    }
    sync(&source)?;
    sync(&destination)?;
    sync(&parent)?;
    if !target.matches_path(&destination)? {
        return Err(io::Error::other(
            "Moved directory identity changed; journal retained; engine remains paused",
        ));
    }
    Ok((target, destination.to_string_lossy().into_owned()))
}
pub(crate) fn pending(profile: &Path) -> io::Result<Vec<(PathBuf, Plan)>> {
    let mut plans = Vec::new();
    for entry in profile.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("storage-move-") || !name.ends_with(".json") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(io::Error::other("Invalid move journal type"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(entry.path())?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("Move journal exceeds budget"));
        }
        let plan: Plan = serde_json::from_slice(&bytes)?;
        if !plan.source.is_absolute()
            || !plan.destination.is_absolute()
            || plan.source == plan.destination
        {
            return Err(io::Error::other("Invalid move journal roots"));
        }
        plans.push((entry.path(), plan));
    }
    Ok(plans)
}
pub(crate) fn sync(path: &Path) -> io::Result<()> {
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
    fn exclusive_root_moves_without_copy_and_intent_survives_until_sql_ack() {
        let parent = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let source = parent.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("x"), b"abc").unwrap();
        let root = Directory::open(&source).unwrap();
        let (_, destination) = prepare(
            profile.path(),
            "hash",
            &source,
            parent.path(),
            &root,
            vec![("x".into(), 3)],
        )
        .unwrap();
        assert!(!source.join("x").exists());
        assert_eq!(
            std::fs::read(Path::new(&destination).join("x")).unwrap(),
            b"abc"
        );
        assert!(
            Directory::open(Path::new(&destination))
                .unwrap()
                .only_payload(&[("x".into(), 3)])
                .unwrap()
        );
        assert_eq!(pending(profile.path()).unwrap().len(), 1);
        std::fs::write(Path::new(&destination).join("unrelated"), b"keep").unwrap();
        assert!(
            prepare(
                profile.path(),
                "hash",
                Path::new(&destination),
                parent.path(),
                &root,
                vec![("x".into(), 3)]
            )
            .is_err()
        );
    }
}
