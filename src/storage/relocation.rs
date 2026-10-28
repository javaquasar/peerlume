//! Copy-first relocation. No original is ever deleted, including on cancellation.
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
    pub destination: String,
    pub root: Directory,
    pub bytes: Arc<AtomicU64>,
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
        let worker = thread::Builder::new()
            .name("peerlume-copy".into())
            .spawn(move || {
                let result = copy_files(&source, &target, &files, &progress, &stop)
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })?;
        Ok(Self {
            destination,
            root,
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
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Copy cancelled; originals retained; partial destination retained",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
