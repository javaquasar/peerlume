//! Capability-relative payload storage used by the real transfer backend.
use anyhow::{Context, Result, bail};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use librqbit::{
    ManagedTorrentShared, TorrentMetadata,
    storage::{StorageFactory, StorageFactoryExt, TorrentStorage},
};
use std::{
    fs::File,
    io,
    path::{Component, Path},
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct Directory {
    dir: Arc<Dir>,
    resume: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    #[cfg(test)]
    read_calls: Arc<std::sync::atomic::AtomicU64>,
}
impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Directory(<pinned>)")
    }
}
impl PartialEq for Directory {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.dir, &other.dir)
    }
}
impl Eq for Directory {}

impl Directory {
    #[cfg(test)]
    pub(crate) fn read_calls(&self) -> u64 {
        self.read_calls.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn supply_resume(&self, bits: Vec<u8>) {
        *self
            .resume
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(bits);
    }
    pub(crate) fn clear_resume(&self) {
        self.resume
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
    /// Only the explicitly selected root uses ambient authority. Descendants never do.
    pub fn open(path: &Path) -> io::Result<Self> {
        Dir::open_ambient_dir(path, cap_std::ambient_authority()).map(|dir| Self {
            dir: Arc::new(dir),
            resume: Default::default(),
            #[cfg(test)]
            read_calls: Default::default(),
        })
    }
    pub fn create_root(path: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(path)?;
        Self::open(path)
    }
    pub(crate) fn new_child(&self, name: &str) -> io::Result<Self> {
        let parts = Self::components(Path::new(name))?;
        if parts.len() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected one new directory name",
            ));
        }
        self.dir.create_dir(name)?;
        #[cfg(unix)]
        self.dir.try_clone()?.into_std_file().sync_all()?;
        self.descendant(name)
    }
    pub(crate) fn read_payload(&self, relative: &Path) -> io::Result<File> {
        let parts = Self::components(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Empty payload path"))?;
        let dir = self.walk(parents, false)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = dir.open_with(name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Not a regular payload file",
            ));
        }
        Ok(file.into_std())
    }
    pub(crate) fn sync_payload(&self, relative: &Path) -> io::Result<()> {
        let parts = Self::components(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| io::Error::other("Empty payload path"))?;
        let dir = self.walk(parents, false)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).follow(FollowSymlinks::No);
        dir.open_with(name, &options)?.sync_all()
    }
    pub(crate) fn remove_payload(&self, relative: &Path) -> io::Result<()> {
        self.remove(relative, false)
    }
    pub(crate) fn rename_payload(&self, target: &Self, relative: &Path) -> io::Result<()> {
        let parts = Self::components(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| io::Error::other("Empty move path"))?;
        match target.read_payload(relative) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Move destination file exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let identity = same_file::Handle::from_file(self.read_payload(relative)?)?;
        let from = self.walk(parents, false)?;
        let to = target.walk(parents, true)?;
        from.rename(name, &to, name)?;
        if identity != same_file::Handle::from_file(target.read_payload(relative)?)? {
            return Err(io::Error::other(
                "Moved file identity changed; recovery required",
            ));
        }
        target.sync_payload(relative)?;
        #[cfg(unix)]
        {
            from.into_std_file().sync_all()?;
            to.into_std_file().sync_all()?;
        }
        Ok(())
    }
    pub(crate) fn matches_path(&self, path: &Path) -> io::Result<bool> {
        Ok(
            same_file::Handle::from_file(self.dir.try_clone()?.into_std_file())?
                == same_file::Handle::from_path(path)?,
        )
    }
    /// Reject a shared directory, unexpected files, missing data and links.
    pub(crate) fn only_payload(&self, files: &[(std::path::PathBuf, u64)]) -> io::Result<bool> {
        self.payload_subset(files, true)
    }
    pub(crate) fn payload_subset(
        &self,
        files: &[(std::path::PathBuf, u64)],
        complete: bool,
    ) -> io::Result<bool> {
        use std::collections::{HashMap, HashSet};
        let expected: HashMap<_, _> = files.iter().cloned().collect();
        let mut parents = HashSet::new();
        for path in expected.keys() {
            Self::components(path)?;
            let mut parent = path.parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                parents.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        let mut queue = vec![(self.dir.try_clone()?, std::path::PathBuf::new())];
        let mut seen = HashSet::new();
        while let Some((dir, prefix)) = queue.pop() {
            for entry in dir.entries()? {
                let entry = entry?;
                let path = prefix.join(entry.file_name());
                let kind = entry.file_type()?;
                if kind.is_dir() && parents.contains(&path) {
                    queue.push((dir.open_dir_nofollow(entry.file_name())?, path));
                } else if kind.is_file()
                    && expected
                        .get(&path)
                        .is_some_and(|length| entry.metadata().is_ok_and(|m| m.len() == *length))
                {
                    seen.insert(path);
                } else {
                    return Ok(false);
                }
            }
        }
        Ok(!complete || (seen.len() == expected.len() && !seen.is_empty()))
    }
    fn components(relative: &Path) -> io::Result<Vec<&std::ffi::OsStr>> {
        let text = relative
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 payload path"))?;
        crate::limits::validate_relative_paths([text], &crate::limits::ResourceLimits::default())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid payload path"))?;
        relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid payload component",
                )),
            })
            .collect()
    }
    fn walk(&self, components: &[&std::ffi::OsStr], create: bool) -> io::Result<Dir> {
        let mut dir = self.dir.try_clone()?;
        for name in components {
            if create {
                match dir.create_dir(name) {
                    Ok(()) => {
                        // Persist each new parent link before a move unlinks its source.
                        #[cfg(unix)]
                        dir.try_clone()?.into_std_file().sync_all()?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            dir = dir.open_dir_nofollow(name)?;
        }
        Ok(dir)
    }
    pub fn descendant(&self, relative: &str) -> io::Result<Self> {
        let parts = Self::components(Path::new(relative))?;
        self.walk(&parts, false).map(|dir| Self {
            dir: Arc::new(dir),
            resume: Default::default(),
            #[cfg(test)]
            read_calls: Default::default(),
        })
    }
    pub(crate) fn open_payload(&self, relative: &Path, overwrite: bool) -> io::Result<File> {
        let parts = Self::components(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty payload path"))?;
        let dir = self.walk(parents, true)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).follow(FollowSymlinks::No);
        if overwrite {
            options.create(true);
        } else {
            options.create_new(true);
        }
        let file = dir.open_with(name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload is not a regular file",
            ));
        }
        let file = file.into_std();
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let mut returned = 0u32;
            // The owned handle stays valid; no input/output buffer is passed.
            unsafe {
                windows_sys::Win32::System::IO::DeviceIoControl(
                    file.as_raw_handle(),
                    windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                );
            }
        }
        Ok(file)
    }
    fn remove(&self, path: &Path, directory: bool) -> io::Result<()> {
        let parts = Self::components(path)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty removal path"))?;
        let dir = self.walk(parents, false)?;
        if directory {
            dir.remove_dir(name)
        } else {
            dir.remove_file(name)
        }
    }
}

#[derive(Clone)]
pub struct Factory {
    pub root: Directory,
}
impl StorageFactory for Factory {
    type Storage = Payload;
    fn create(&self, _: &ManagedTorrentShared, _: &TorrentMetadata) -> Result<Payload> {
        Ok(Payload {
            root: self.root.clone(),
            files: Default::default(),
        })
    }
    fn clone_box(&self) -> librqbit::storage::BoxStorageFactory {
        self.clone().boxed()
    }
}
struct Entry {
    file: Option<File>,
    path: std::path::PathBuf,
    length: u64,
}
pub struct Payload {
    root: Directory,
    files: RwLock<Vec<Entry>>,
}
impl Payload {
    fn access<T>(&self, index: usize, action: impl FnOnce(&Entry) -> Result<T>) -> Result<T> {
        let files = self
            .files
            .read()
            .map_err(|_| anyhow::anyhow!("payload lock unavailable"))?;
        action(files.get(index).context("payload handle unavailable")?)
    }
}
impl TorrentStorage for Payload {
    fn initial_verified_pieces(&self) -> Option<Vec<u8>> {
        self.root
            .resume
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
    fn init(&mut self, _: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<()> {
        let limits = crate::limits::ResourceLimits::default();
        crate::limits::validate_relative_paths(
            metadata
                .file_infos
                .iter()
                .map(|f| f.relative_filename.to_str().unwrap_or("")),
            &limits,
        )
        .map_err(anyhow::Error::msg)?;
        let mut entries = Vec::with_capacity(metadata.file_infos.len());
        for info in &metadata.file_infos {
            let file = if info.attrs.padding {
                None
            } else {
                Some(
                    self.root
                        .open_payload(&info.relative_filename, true)
                        .context("could not open confined payload")?,
                )
            };
            entries.push(Entry {
                file,
                path: info.relative_filename.clone(),
                length: info.len,
            });
        }
        *self
            .files
            .get_mut()
            .map_err(|_| anyhow::anyhow!("payload lock unavailable"))? = entries;
        Ok(())
    }
    fn pread_exact(&self, index: usize, offset: u64, buffer: &mut [u8]) -> Result<()> {
        #[cfg(test)]
        self.root
            .read_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.access(index, |entry| {
            if offset
                .checked_add(buffer.len() as u64)
                .is_none_or(|end| end > entry.length)
            {
                bail!("payload read exceeds file bounds");
            }
            if let Some(file) = &entry.file {
                read_exact_at(file, offset, buffer)?;
            } else {
                buffer.fill(0);
            }
            Ok(())
        })
    }
    fn pwrite_all(&self, index: usize, offset: u64, buffer: &[u8]) -> Result<()> {
        self.access(index, |entry| {
            if offset
                .checked_add(buffer.len() as u64)
                .is_none_or(|end| end > entry.length)
            {
                bail!("payload write exceeds file bounds");
            }
            if let Some(file) = &entry.file {
                write_all_at(file, offset, buffer)?;
            }
            Ok(())
        })
    }
    fn ensure_file_length(&self, index: usize, length: u64) -> Result<()> {
        self.access(index, |entry| {
            if length != entry.length {
                bail!("unexpected payload length");
            }
            if let Some(file) = &entry.file {
                file.set_len(length)?;
            }
            Ok(())
        })
    }
    fn remove_file(&self, index: usize, path: &Path) -> Result<()> {
        self.access(index, |entry| {
            if entry.path != path {
                bail!("mismatched payload removal path");
            }
            if entry.file.is_some() {
                self.root.remove(path, false)?;
            }
            Ok(())
        })
    }
    fn remove_directory_if_empty(&self, path: &Path) -> Result<()> {
        Ok(self.root.remove(path, true)?)
    }
    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        let mut files = self
            .files
            .write()
            .map_err(|_| anyhow::anyhow!("payload lock unavailable"))?;
        Ok(Box::new(Self {
            root: self.root.clone(),
            files: RwLock::new(std::mem::take(&mut *files)),
        }))
    }
}
fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, buffer, offset)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, buffer, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "payload read reached EOF",
            ));
        }
        offset += n as u64;
        buffer = &mut buffer[n..];
    }
    Ok(())
}
fn write_all_at(file: &File, mut offset: u64, mut buffer: &[u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::write_at(file, buffer, offset)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_write(file, buffer, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "payload write made no progress",
            ));
        }
        offset += n as u64;
        buffer = &buffer[n..];
    }
    Ok(())
}

#[cfg(test)]
mod tests;
