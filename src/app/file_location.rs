//! Desktop-only file navigation. Never sends transfer commands or modifies payloads.
use std::{fs, io, path::PathBuf, process::Command};

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

pub(super) fn reveal(
    torrent: &TorrentItem,
    settings: &ClientSettings,
    index: usize,
) -> Result<String, String> {
    let location = resolve(torrent, settings, index)?;
    command(&location, std::env::consts::OS)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(if location.select_file {
        "Opened file location".into()
    } else {
        "File is not on disk yet; opened the nearest existing folder".into()
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

fn command(location: &Location, os: &str) -> Command {
    match os {
        "windows" => {
            let mut command = Command::new("explorer.exe");
            if location.select_file {
                command.arg("/select,");
            }
            command.arg(&location.path);
            command
        }
        "macos" => {
            let mut command = Command::new("open");
            if location.select_file {
                command.arg("-R");
            }
            command.arg(&location.path);
            command
        }
        _ => {
            // xdg-open has no portable select-file operation. Open its parent
            // rather than launching a media player for the payload itself.
            let mut command = Command::new("xdg-open");
            command.arg(if location.select_file {
                location
                    .path
                    .parent()
                    .expect("resolved files have a parent")
            } else {
                &location.path
            });
            command
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
    fn platform_commands_select_files_without_shell_interpretation() {
        let path = PathBuf::from("download folder").join("caf\u{e9}, & sample.bin");
        let location = Location {
            path: path.clone(),
            select_file: true,
        };
        for (os, program, prefix) in [
            ("windows", "explorer.exe", "/select,"),
            ("macos", "open", "-R"),
        ] {
            let cmd = command(&location, os);
            assert_eq!(cmd.get_program(), program);
            assert_eq!(
                cmd.get_args().collect::<Vec<_>>(),
                vec![std::ffi::OsStr::new(prefix), path.as_os_str()]
            );
        }
        let cmd = command(&location, "linux");
        assert_eq!(cmd.get_program(), "xdg-open");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            vec![path.parent().unwrap().as_os_str()]
        );
        let folder = Location {
            path,
            select_file: false,
        };
        for os in ["windows", "macos", "linux"] {
            assert_eq!(
                command(&folder, os).get_args().collect::<Vec<_>>(),
                vec![folder.path.as_os_str()]
            );
        }
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
