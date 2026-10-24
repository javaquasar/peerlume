use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use crate::limits::{ResourceLimits, validate_relative_paths};

pub mod sandbox;

pub struct PositionedFile {
    file: File,
    pub path: PathBuf,
}

impl PositionedFile {
    pub fn create(root: &Path, relative: &str, length: u64) -> io::Result<Self> {
        validate_relative_paths([relative], &ResourceLimits::default())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        file.set_len(length)?;
        Ok(Self { file, path })
    }

    pub fn write_all_at(&self, mut offset: u64, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let written = write_at(&self.file, bytes, offset)?;
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "positioned write returned zero",
                ));
            }
            offset += written as u64;
            bytes = &bytes[written..];
        }
        self.file.sync_data()
    }

    pub fn read_exact_at(&self, mut offset: u64, mut bytes: &mut [u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let read = read_at(&self.file, bytes, offset)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "positioned read reached EOF",
                ));
            }
            offset += read as u64;
            bytes = &mut bytes[read..];
        }
        Ok(())
    }
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(bytes, offset)
}
#[cfg(windows)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(bytes, offset)
}
#[cfg(unix)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(bytes, offset)
}
#[cfg(windows)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(bytes, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn positioned_storage_writes_and_reads_without_shared_cursor() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("peerlume-storage-{nonce}"));
        let file = PositionedFile::create(&root, "dir/file.bin", 16).unwrap();
        assert!(file.path.ends_with("dir/file.bin"));
        file.write_all_at(4, b"piece").unwrap();
        let mut output = [0; 5];
        file.read_exact_at(4, &mut output).unwrap();
        assert_eq!(&output, b"piece");
        drop(file);
        fs::remove_dir_all(root).unwrap();
    }
}
