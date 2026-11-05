//! Copy preserves originals. Only a separately confirmed duplicate cleanup deletes them.
use super::sandbox::Directory;
#[path = "bulk_budget.rs"]
mod bulk_budget;
pub(crate) use bulk_budget::Budget as BulkBudget;
use bulk_budget::{Admission, Volume};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
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
    thread: RefCell<Option<thread::JoinHandle<()>>>,
    admission: RefCell<Option<Admission>>,
    sender: RefCell<Option<mpsc::SyncSender<Result<(), String>>>>,
    budget: Arc<BulkBudget>,
}
impl CopyJob {
    pub fn start(
        source: Directory,
        parent: &Path,
        files: Vec<(PathBuf, u64)>,
        budget: Arc<BulkBudget>,
    ) -> io::Result<Self> {
        if !parent.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Destination must be absolute",
            ));
        }
        let parent_path = parent.canonicalize()?;
        let parent = Directory::open(&parent_path)?;
        let admission = budget.register(vec![volume(&source), volume(&parent)])?;
        let name = format!(
            "PeerLume-copy-{}",
            &crate::control::access::random_id()?[..16]
        );
        let root = parent.new_child(&name)?;
        let destination = parent_path.join(name).to_string_lossy().into_owned();
        let bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::sync_channel(1);
        Ok(Self {
            removed: Default::default(),
            source,
            files,
            cleanup: false,
            destination,
            root,
            bytes,
            cancel,
            result,
            thread: RefCell::new(None),
            admission: RefCell::new(Some(admission)),
            sender: RefCell::new(Some(sender)),
            budget,
        })
    }
    pub fn cleanup(
        source: Directory,
        root: Directory,
        destination: String,
        files: Vec<(PathBuf, u64)>,
        budget: Arc<BulkBudget>,
    ) -> io::Result<Self> {
        let bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::sync_channel(1);
        let removed = Arc::new(AtomicU64::new(0));
        let admission = budget.register(vec![volume(&source), volume(&root)])?;
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
            thread: RefCell::new(None),
            admission: RefCell::new(Some(admission)),
            sender: RefCell::new(Some(sender)),
            budget,
        })
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.budget.wake();
    }
    pub fn finished(&self) -> Option<Result<(), String>> {
        if let Err(error) = self.launch_if_admitted() {
            return Some(Err(error.to_string()));
        }
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Copy worker stopped unexpectedly".into()))
            }
            Err(_) => None,
        }
    }

    fn launch_if_admitted(&self) -> io::Result<()> {
        if self.thread.borrow().is_some() || self.sender.borrow().is_none() {
            return Ok(());
        }
        if self.cancel.load(Ordering::Relaxed) {
            self.admission.borrow_mut().take();
            if let Some(sender) = self.sender.borrow_mut().take() {
                let _ = sender.send(Err(
                    "Bulk operation cancelled while queued; originals retained".into(),
                ));
            }
            return Ok(());
        }
        let Some(permit) = self
            .admission
            .borrow()
            .as_ref()
            .and_then(Admission::try_enter)
        else {
            return Ok(());
        };
        self.admission.borrow_mut().take();
        let sender = self
            .sender
            .borrow_mut()
            .take()
            .ok_or_else(|| io::Error::other("Bulk result sender unavailable"))?;
        let (source, target, files) = (self.source.clone(), self.root.clone(), self.files.clone());
        let (progress, removed, stop, budget, cleanup) = (
            self.bytes.clone(),
            self.removed.clone(),
            self.cancel.clone(),
            self.budget.clone(),
            self.cleanup,
        );
        let worker = thread::Builder::new()
            .name(
                if cleanup {
                    "peerlume-cleanup"
                } else {
                    "peerlume-copy"
                }
                .into(),
            )
            .spawn(move || {
                let _permit = permit;
                let result = if cleanup {
                    remove_duplicates_counted_budgeted(
                        &source,
                        &target,
                        &files,
                        &stop,
                        &removed,
                        Some(&budget),
                    )
                } else {
                    copy_files_budgeted(&source, &target, &files, &progress, &stop, Some(&budget))
                }
                .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })?;
        *self.thread.borrow_mut() = Some(worker);
        Ok(())
    }
}
impl Drop for CopyJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.thread.get_mut().take() {
            let _ = worker.join();
        }
    }
}
fn volume(directory: &Directory) -> Volume {
    directory
        .volume_key()
        .map(Volume::Known)
        .unwrap_or(Volume::Unknown)
}
#[cfg(test)]
fn copy_files(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    copy_files_budgeted(source, target, files, progress, cancel, None)
}
fn copy_files_budgeted(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    progress: &AtomicU64,
    cancel: &AtomicBool,
    budget: Option<&BulkBudget>,
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
            throttle(budget, 2 * read as u64, cancel)?;
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
            throttle(budget, read as u64, cancel)?;
            verified.update(&buffer[..read]);
        }
        if digest.finalize() != verified.finalize() {
            return Err(io::Error::other("Destination read-back checksum mismatch"));
        }
    }
    Ok(())
}

fn digest(
    mut file: std::fs::File,
    cancel: &AtomicBool,
    budget: Option<&BulkBudget>,
) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        throttle(budget, count as u64, cancel)?;
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
#[cfg(test)]
fn remove_duplicates_counted(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    cancel: &AtomicBool,
    removed: &AtomicU64,
) -> io::Result<()> {
    remove_duplicates_counted_budgeted(source, target, files, cancel, removed, None)
}
fn remove_duplicates_counted_budgeted(
    source: &Directory,
    target: &Directory,
    files: &[(PathBuf, u64)],
    cancel: &AtomicBool,
    removed: &AtomicU64,
    budget: Option<&BulkBudget>,
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
        let expected = digest(input, cancel, budget)?;
        if digest(target.read_payload(path)?, cancel, budget)? != expected {
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
            || digest(input, cancel, budget)? != expected
            || digest(target.read_payload(path)?, cancel, budget)? != expected
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
fn throttle(budget: Option<&BulkBudget>, bytes: u64, cancel: &AtomicBool) -> io::Result<()> {
    if let Some(budget) = budget {
        budget.throttle(bytes, cancel)?;
    }
    check_cancel(cancel)
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
    fn queued_copy_owns_no_worker_and_can_cancel_before_admission_without_touching_originals() {
        let source = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("x"), b"safe").unwrap();
        let root = Directory::open(source.path()).unwrap();
        let budget = BulkBudget::new(1, 0);
        let blocker = budget.register(vec![Volume::Unknown]).unwrap();
        let permit = blocker.try_enter().unwrap();
        let files = vec![("x".into(), 4)];
        let first =
            CopyJob::start(root.clone(), parent.path(), files.clone(), budget.clone()).unwrap();
        let second = CopyJob::start(root, parent.path(), files, budget).unwrap();
        assert!(first.finished().is_none());
        assert!(second.finished().is_none());
        assert!(first.thread.borrow().is_none());
        assert!(second.thread.borrow().is_none());
        assert_eq!(first.bytes.load(Ordering::Relaxed), 0);
        second.cancel();
        assert!(second.finished().unwrap().is_err());
        assert!(second.thread.borrow().is_none());
        assert!(!Path::new(&second.destination).join("x").exists());
        drop(permit);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = first.finished() {
                result.unwrap();
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(first.bytes.load(Ordering::Relaxed), 4);
        assert_eq!(
            std::fs::read(Path::new(&first.destination).join("x")).unwrap(),
            b"safe"
        );
        assert_eq!(std::fs::read(source.path().join("x")).unwrap(), b"safe");
    }
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
