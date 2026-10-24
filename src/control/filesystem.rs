//! Pinned directory capabilities for agent imports and exports.
use super::{access, wire::ErrorCode};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

pub struct Root(Dir);
impl Root {
    /// The human-selected root is trusted; all descendant opens are relative.
    pub fn open(path: &Path) -> Result<Self, ErrorCode> {
        Dir::open_ambient_dir(path, cap_std::ambient_authority())
            .map(Self)
            .map_err(|_| ErrorCode::PathDenied)
    }

    fn parent(&self, relative: &str) -> Result<(Dir, String), ErrorCode> {
        access::validate_relative(relative)?;
        let mut components = Path::new(relative).components().peekable();
        let mut dir = self.0.try_clone().map_err(|_| ErrorCode::PathDenied)?;
        while let Some(Component::Normal(part)) = components.next() {
            if components.peek().is_none() {
                return Ok((dir, part.to_string_lossy().into_owned()));
            }
            dir = dir
                .open_dir_nofollow(part)
                .map_err(|_| ErrorCode::PathDenied)?;
        }
        Err(ErrorCode::PathDenied)
    }

    pub fn read_metainfo(
        &self,
        relative: &str,
    ) -> Result<crate::metainfo::TorrentMetainfo, ErrorCode> {
        let (dir, name) = self.parent(relative)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let input = dir
            .open_with(name, &options)
            .map_err(|_| ErrorCode::PathDenied)?;
        if !input
            .metadata()
            .map_err(|_| ErrorCode::PathDenied)?
            .is_file()
        {
            return Err(ErrorCode::PathDenied);
        }
        let limit = crate::limits::ResourceLimits::default().max_torrent_bytes;
        let mut bytes = Vec::new();
        input
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        if bytes.len() as u64 > limit {
            return Err(ErrorCode::TooLarge);
        }
        crate::metainfo::TorrentMetainfo::parse(&bytes).map_err(|_| ErrorCode::InvalidRequest)
    }

    pub fn create_export(&self, relative: &str) -> Result<File, ErrorCode> {
        if relative.contains(['/', '\\', ':']) || relative.len() > 200 {
            return Err(ErrorCode::PathDenied);
        }
        let (dir, name) = self.parent(relative)?;
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        dir.open_with(name, &options)
            .map(|file| file.into_std())
            .map_err(|_| ErrorCode::PathDenied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_never_overwrites_and_rejects_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        root.create_export("new.jsonl").unwrap();
        assert!(root.create_export("new.jsonl").is_err());
        for name in ["../out", "a/b", "a\\b", "CON", "NUL.jsonl", "a:b", ""] {
            assert!(root.create_export(name).is_err(), "{name}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn pinned_root_survives_path_replacement_without_writing_outside() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let root = Root::open(&path).unwrap();
        std::fs::rename(&path, temp.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        root.create_export("evidence.jsonl").unwrap();
        assert!(!outside.join("evidence.jsonl").exists());
        assert!(temp.path().join("original/evidence.jsonl").exists());
    }
    #[cfg(unix)]
    #[test]
    fn import_refuses_leaf_and_intermediate_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), b"x").unwrap();
        std::os::unix::fs::symlink("file", temp.path().join("leaf")).unwrap();
        std::os::unix::fs::symlink(".", temp.path().join("dir")).unwrap();
        let root = Root::open(temp.path()).unwrap();
        assert_eq!(
            root.read_metainfo("leaf").err(),
            Some(ErrorCode::PathDenied)
        );
        assert_eq!(
            root.read_metainfo("dir/file").err(),
            Some(ErrorCode::PathDenied)
        );
    }
}
