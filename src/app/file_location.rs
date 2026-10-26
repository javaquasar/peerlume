//! Desktop-only file navigation. Never sends transfer commands or modifies payloads.
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

mod desktop;

use eframe::egui;

use crate::{settings::ClientSettings, torrent::TorrentItem};

#[derive(Debug, PartialEq, Eq)]
struct Location {
    path: PathBuf,
    select_file: bool,
}

pub(super) fn context_menu(response: &egui::Response) -> bool {
    let mut reveal = false;
    response.context_menu(|ui| {
        if ui.button("Show in folder").clicked() {
            reveal = true;
            ui.close();
        }
    });
    reveal
}

/// Only one desktop request may be outstanding. Shell/DBus calls never hold
/// the GUI/controller owner lock or run on an existing COM/runtime thread.
#[derive(Default)]
pub(super) struct Navigator {
    pending: Option<Receiver<Result<String, String>>>,
}

impl Navigator {
    pub(super) fn reveal(
        &mut self,
        torrent: &TorrentItem,
        settings: &ClientSettings,
        index: usize,
    ) -> String {
        self.start(resolve(torrent, settings, index))
    }

    pub(super) fn reveal_source(&mut self, path: &Path) -> String {
        self.start(source_location(path))
    }

    fn start(&mut self, location: Result<Location, String>) -> String {
        if self.pending.is_some() {
            return "A file location request is still pending".into();
        }
        let location = match location {
            Ok(location) => location,
            Err(error) => return navigation_status(Err(error)),
        };
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("file-location".into())
            .spawn(move || {
                let result = desktop::show(&location.path, location.select_file).map(|()| {
                    if location.select_file {
                        "Requested file selection in its containing folder".into()
                    } else {
                        "File is not on disk yet; opened the nearest existing folder".into()
                    }
                });
                let _ = sender.send(result);
            });
        match worker {
            Ok(_) => {
                self.pending = Some(receiver);
                "Opening file location...".into()
            }
            Err(error) => navigation_status(Err(error.to_string())),
        }
    }

    pub(super) fn poll(&mut self) -> Option<String> {
        let result = match self.pending.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("Desktop navigation worker stopped unexpectedly".into())
            }
        };
        self.pending = None;
        Some(navigation_status(result))
    }
}

fn navigation_status(result: Result<String, String>) -> String {
    result.unwrap_or_else(|error| format!("Could not show file in folder: {error}"))
}

fn source_location(path: &Path) -> Result<Location, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    if !path.is_file() {
        return Err("Source .torrent file is not on disk".into());
    }
    // Keep the filename, not just its parent. Sources need not be inside the
    // download root; payload resolution below remains confined to that root.
    Ok(Location {
        path,
        select_file: true,
    })
}

fn resolve(
    torrent: &TorrentItem,
    settings: &ClientSettings,
    index: usize,
) -> Result<Location, String> {
    let file = torrent
        .files
        .files()
        .get(index)
        .ok_or("Unknown torrent file")?;
    crate::limits::validate_relative_paths(
        [file.path.as_str()],
        &crate::limits::ResourceLimits::default(),
    )?;
    let effective = crate::torrent::EffectiveTorrentSettings::resolve(
        settings,
        &torrent.overrides,
        torrent.is_private,
    );
    let root = PathBuf::from(effective.download_directory);
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(root)
    };
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    if !canonical_root.is_dir() {
        return Err("Download location is not a directory".into());
    }
    // The confined storage backend writes relative filenames directly beneath
    // the configured output folder, without adding the torrent's display name.
    let relative: PathBuf = file.path.split(['/', '\\']).collect();
    let candidate = root.join(relative);
    let mut existing = candidate.clone();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => {
                let canonical = existing.canonicalize().map_err(|error| error.to_string())?;
                if !canonical.starts_with(&canonical_root) {
                    return Err("File location is outside the download directory".into());
                }
                let select_file = existing == candidate;
                if (select_file && !canonical.is_file()) || (!select_file && !canonical.is_dir()) {
                    return Err("File location is not a regular file or containing folder".into());
                }
                // Keep the native absolute path: Explorer does not reliably
                // accept the extended-length prefix produced by canonicalize.
                return Ok(Location {
                    path: existing,
                    select_file,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !existing.pop() || !existing.starts_with(&root) {
                    return Err("No existing download folder was found".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_selection::FileSelectionModel;

    fn fixture(root: &std::path::Path, files: &[&str]) -> (TorrentItem, ClientSettings) {
        let mut torrent = crate::client::tests::torrent();
        torrent.name = "Display name is not a folder".into();
        torrent.files =
            FileSelectionModel::from_files(files.iter().map(|path| ((*path).into(), 4)));
        let mut settings = ClientSettings::default();
        settings.download.download_directory = root.display().to_string();
        (torrent, settings)
    }

    #[test]
    fn resolves_single_and_nested_multi_file_paths_without_extra_name_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        for path in ["one.bin", "nested/two.bin"] {
            fs::write(root.path().join(path), b"data").unwrap();
        }
        for files in [vec!["one.bin"], vec!["one.bin", "nested/two.bin"]] {
            let (torrent, settings) = fixture(root.path(), &files);
            for (index, path) in files.iter().enumerate() {
                assert_eq!(
                    resolve(&torrent, &settings, index).unwrap(),
                    Location {
                        path: root.path().join(path),
                        select_file: true,
                    }
                );
            }
        }
    }

    #[test]
    fn uses_torrent_directory_override_and_can_reveal_incomplete_unselected_files() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("partial.bin"), b"x").unwrap();
        let (mut torrent, settings) = fixture(root.path(), &["partial.bin"]);
        torrent.files.select_all(false);
        torrent.overrides.download_directory = Some(other.path().display().to_string());
        assert_eq!(
            resolve(&torrent, &settings, 0).unwrap().path,
            other.path().join("partial.bin")
        );
        assert!(!torrent.files.files()[0].selected);
        assert_eq!(fs::read(other.path().join("partial.bin")).unwrap(), b"x");
    }

    #[test]
    fn missing_files_open_nearest_existing_parent_without_creating_anything() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        for (path, parent) in [("missing.bin", ""), ("nested/missing/file.bin", "nested")] {
            let (torrent, settings) = fixture(root.path(), &[path]);
            assert_eq!(
                resolve(&torrent, &settings, 0).unwrap(),
                Location {
                    path: root.path().join(parent),
                    select_file: false,
                }
            );
            assert!(!root.path().join(path).exists());
        }
        assert!(!root.path().join("nested/missing").exists());
    }

    #[test]
    fn resolves_relative_roots_and_normalizes_metadata_separators() {
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::tempdir_in(&cwd).unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        let expected = root.path().join("nested").join("caf\u{e9}, & sample.bin");
        fs::write(&expected, b"data").unwrap();
        let (torrent, mut settings) = fixture(root.path(), &["nested\\caf\u{e9}, & sample.bin"]);
        settings.download.download_directory =
            root.path().strip_prefix(cwd).unwrap().display().to_string();
        assert_eq!(
            resolve(&torrent, &settings, 0).unwrap(),
            Location {
                path: expected,
                select_file: true
            }
        );
    }

    #[test]
    fn rejects_unsafe_paths_invalid_indices_missing_roots_and_directories_as_files() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "../outside",
            "/absolute",
            "C:\\outside",
            "\\\\server\\share",
            "a/../b",
            "a\\..\\b",
            "a:stream",
            "",
            "a\0b",
        ] {
            let (torrent, settings) = fixture(root.path(), &[path]);
            assert!(
                resolve(&torrent, &settings, 0).is_err(),
                "accepted {path:?}"
            );
        }
        let (torrent, mut settings) = fixture(root.path(), &["folder"]);
        assert!(resolve(&torrent, &settings, 1).is_err());
        fs::create_dir(root.path().join("folder")).unwrap();
        assert!(resolve(&torrent, &settings, 0).is_err());
        settings.download.download_directory = root.path().join("absent").display().to_string();
        assert!(resolve(&torrent, &settings, 0).is_err());
        assert!(!root.path().join("absent").exists());
    }

    #[test]
    fn source_location_preserves_the_actual_filename_and_rejects_missing_files() {
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::tempdir_in(&cwd).unwrap();
        let path = root.path().join("caf\u{e9}, & #100%.torrent");
        fs::write(&path, b"source metadata").unwrap();
        for input in [path.clone(), path.strip_prefix(&cwd).unwrap().to_path_buf()] {
            assert_eq!(
                source_location(&input).unwrap(),
                Location {
                    path: path.clone(),
                    select_file: true
                }
            );
        }
        assert!(source_location(root.path()).is_err());
        assert!(source_location(&root.path().join("missing.torrent")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"source metadata");
    }

    #[test]
    fn navigator_polls_without_blocking_and_reports_failure_not_success() {
        let (sender, receiver) = mpsc::channel();
        let mut navigator = Navigator {
            pending: Some(receiver),
        };
        assert!(navigator.poll().is_none());
        assert!(navigator.start(Err("unused".into())).contains("pending"));
        sender
            .send(Err("FileManager1 is unavailable".into()))
            .unwrap();
        assert_eq!(
            navigator.poll().unwrap(),
            "Could not show file in folder: FileManager1 is unavailable"
        );
        assert!(navigator.pending.is_none());
        assert!(navigator.poll().is_none());
        assert!(
            navigator
                .start(Err("missing file".into()))
                .contains("Could not")
        );
        navigator.pending = Some(receiver_after_worker_exit());
        assert!(navigator.poll().unwrap().contains("stopped unexpectedly"));
    }

    fn receiver_after_worker_exit() -> Receiver<Result<String, String>> {
        let (_, receiver) = mpsc::channel();
        receiver
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escapes_for_existing_and_missing_payloads() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("file.bin"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        for path in ["escape/file.bin", "escape/missing/file.bin"] {
            let (torrent, settings) = fixture(root.path(), &[path]);
            assert!(resolve(&torrent, &settings, 0).is_err());
        }
        assert_eq!(fs::read(outside.path().join("file.bin")).unwrap(), b"keep");
    }
}
