//! Copy preserves originals. Only a separately confirmed duplicate cleanup deletes them.
use super::sandbox::Directory;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StorageStatus {
    OriginalsRemoved {
        destination: String,
        removed_files: u64,
    },
    #[default]
    Idle,
    Copying {
        bytes: u64,
        destination: String,
    },
    Checking {
        destination: String,
    },
    Active {
        destination: String,
    },
    Failed {
        message: String,
    },
}
pub(crate) struct CopyJob {
    pub source: Directory,
    pub files: Vec<(PathBuf, u64)>,
    pub cleanup: bool,
    pub destination: String,
    pub root: Directory,
    pub bytes: Arc<AtomicU64>,
    pub removed: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<(), String>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl CopyJob {
    pub fn start(source: Directory, parent: &Path, files: Vec<(PathBuf, u64)>) -> io::Result<Self> {
        if !parent.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Destination must be absolute",
            ));
        }
        let parent_path = parent.canonicalize()?;
        let parent = Directory::open(&parent_path)?;
        let name = format!(
            "PeerLume-copy-{}",
            &crate::control::access::random_id()?[..16]
        );
        let root = parent.new_child(&name)?;
        let destination = parent_path.join(name).to_string_lossy().into_owned();
        let bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::sync_channel(1);
        let (target, progress, stop) = (root.clone(), bytes.clone(), cancel.clone());
        let (original, paths) = (source.clone(), files.clone());
        let worker = thread::Builder::new()
            .name("peerlume-copy".into())
            .spawn(move || {
                let result = copy_files(&source, &target, &files, &progress, &stop)
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })?;
        Ok(Self {
            removed: Default::default(),
            source: original,
            files: paths,
            cleanup: false,
            destination,
            root,
            bytes,
            cancel,
            result,
            thread: Some(worker),
        })
    }
    pub fn cleanup(
        source: Directory,
        root: Directory,
        destination: String,
        files: Vec<(PathBuf, u64)>,
    ) -> io::Result<Self> {
        let bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::sync_channel(1);
        let (from, to, paths, stop) = (source.clone(), root.clone(), files.clone(), cancel.clone());
        let removed = Arc::new(AtomicU64::new(0));
        let observed = removed.clone();
        let worker = thread::Builder::new()
            .name("peerlume-cleanup".into())
            .spawn(move || {
                let result = remove_duplicates_counted(&from, &to, &paths, &stop, &observed)
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })?;
        Ok(Self {
            removed,
            source,
            root,
            destination,
            files,
            cleanup: true,
            bytes,
            cancel,
            result,
            thread: Some(worker),
        })
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn finished(&self) -> Option<Result<(), String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Copy worker stopped unexpectedly".into()))
            }
            Err(_) => None,
        }
    }
}
impl Drop for CopyJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
fn copy_files(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = vec![0u8; 1024 * 1024];
    for (path, expected) in files {
        check_cancel(cancel)?;
        let mut input = match source.read_payload(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let before = input.metadata()?;
        if before.len() > *expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Source payload exceeds declared torrent length",
            ));
        }
        let mut output = target.open_payload(path, false)?;
        let mut digest = Sha256::new();
        let mut copied = 0u64;
        loop {
            check_cancel(cancel)?;
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(read as u64);
            if copied > before.len() {
                return Err(io::Error::other("Source changed during copy"));
            }
            output.write_all(&buffer[..read])?;
            digest.update(&buffer[..read]);
            progress.fetch_add(read as u64, Ordering::Relaxed);
        }
        let after = input.metadata()?;
        if copied != before.len()
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(io::Error::other("Source changed during copy"));
        }
        output.sync_all()?;
        output.rewind()?;
        let mut verified = Sha256::new();
        loop {
            check_cancel(cancel)?;
            let read = output.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            verified.update(&buffer[..read]);
        }
        if digest.finalize() != verified.finalize() {
            return Err(io::Error::other("Destination read-back checksum mismatch"));
        }
    }
    Ok(())
}

fn digest(mut file: std::fs::File, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().to_vec())
}
/// Explicit local confirmation only. Preflight EVERY file before deleting any.
/// A failed/partial cleanup leaves the verified destination untouched.
#[cfg(test)]
fn remove_duplicates(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    cancel: &AtomicBool,
) -> io::Result<()> {
    remove_duplicates_counted(source, target, files, cancel, &AtomicU64::new(0))
}
fn remove_duplicates_counted(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    cancel: &AtomicBool,
    removed: &AtomicU64,
) -> io::Result<()> {
    let mut proofs = Vec::new();
    for (path, _) in files {
        check_cancel(cancel)?;
        let input = match source.read_payload(path) {
            Ok(input) => input,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let before = input.metadata()?;
        let expected = digest(input, cancel)?;
        if digest(target.read_payload(path)?, cancel)? != expected {
            return Err(io::Error::other(
                "Original and destination differ; originals retained",
            ));
        }
        let after = source.read_payload(path)?.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(io::Error::other("Source changed; originals retained"));
        }
        proofs.push((path, before, expected));
    }
    for (path, before, expected) in proofs {
        check_cancel(cancel)?;
        let input = source.read_payload(path)?;
        let current = input.metadata()?;
        if before.len() != current.len()
            || before.modified()? != current.modified()?
            || digest(input, cancel)? != expected
            || digest(target.read_payload(path)?, cancel)? != expected
        {
            return Err(io::Error::other(
                "Source changed during cleanup; remaining originals retained",
            ));
        }
        source.remove_payload(path)?;
        removed.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Operation cancelled; destination retained; inspect cleanup count for remaining originals",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_preflights_all_files_and_never_removes_a_different_original() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let from = Directory::open(source.path()).unwrap();
        let to = Directory::open(target.path()).unwrap();
        for root in [source.path(), target.path()] {
            std::fs::write(root.join("a"), b"same").unwrap();
            std::fs::write(root.join("b"), b"same").unwrap();
        }
        std::fs::write(target.path().join("b"), b"different").unwrap();
        let files = vec![("a".into(), 4), ("b".into(), 4)];
        assert!(remove_duplicates(&from, &to, &files, &AtomicBool::new(false)).is_err());
        assert!(source.path().join("a").exists());
        assert!(source.path().join("b").exists());
        std::fs::write(target.path().join("b"), b"same").unwrap();
        remove_duplicates(&from, &to, &files, &AtomicBool::new(false)).unwrap();
        assert!(!source.path().join("a").exists());
        assert!(!source.path().join("b").exists());
        assert_eq!(std::fs::read(target.path().join("a")).unwrap(), b"same");
    }
    #[cfg(unix)]
    #[test]
    fn permission_failure_preserves_source_and_does_not_report_copy_success() {
        use std::os::unix::fs::PermissionsExt;
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("x"), b"preserve").unwrap();
        std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = copy_files(
            &Directory::open(source.path()).unwrap(),
            &Directory::open(target.path()).unwrap(),
            &[("x".into(), 8)],
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        );
        std::fs::set_permissions(target.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(source.path().join("x")).unwrap(), b"preserve");
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Requires a disposable GitHub runner tmpfs with a small capacity"]
    fn reliability_storage_disk_full() {
        assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
        let parent = PathBuf::from(std::env::var("PEERLUME_FULL_FS").unwrap())
            .canonicalize()
            .unwrap();
        let runner = PathBuf::from(std::env::var("RUNNER_TEMP").unwrap())
            .canonicalize()
            .unwrap();
        assert!(parent.starts_with(runner));
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir_in(parent).unwrap();
        let payload = vec![91u8; 16 * 1024 * 1024];
        std::fs::write(source.path().join("x"), &payload).unwrap();
        let result = copy_files(
            &Directory::open(source.path()).unwrap(),
            &Directory::open(target.path()).unwrap(),
            &[("x".into(), payload.len() as u64)],
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(28));
        assert_eq!(std::fs::read(source.path().join("x")).unwrap(), payload);
        assert!(target.path().join("x").exists()); // Partial evidence is retained.
    }
    #[test]
    fn copies_verify_and_keep_originals_and_cancellation_never_deletes_them() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("file.bin"), b"verified content").unwrap();
        let from = Directory::open(source.path()).unwrap();
        let to = Directory::open(target.path()).unwrap();
        let progress = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);
        let files = vec![(PathBuf::from("file.bin"), 16)];
        copy_files(&from, &to, &files, &progress, &cancel).unwrap();
        assert_eq!(
            std::fs::read(target.path().join("file.bin")).unwrap(),
            b"verified content"
        );
        assert_eq!(
            std::fs::read(source.path().join("file.bin")).unwrap(),
            b"verified content"
        );
        assert!(copy_files(&from, &to, &files, &progress, &cancel).is_err()); // Never overwrite.
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            copy_files(&from, &to, &files, &progress, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(source.path().join("file.bin").is_file());
        assert!(
            copy_files(
                &from,
                &to,
                &[(PathBuf::from("../escape"), 1)],
                &progress,
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
}
