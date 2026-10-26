//! Native selection requests, not shell command-line parsing or file launching.
use std::path::Path;

pub(super) fn show(path: &Path, select_file: bool) -> Result<(), String> {
    if !path.is_absolute() || (select_file && !path.is_file()) || (!select_file && !path.is_dir()) {
        return Err("Desktop location is not an existing absolute file or folder".into());
    }
    if select_file {
        select(path)
    } else {
        open_folder(path)
    }
}

fn open_folder(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let program = "explorer.exe";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn select(path: &Path) -> Result<(), String> {
    windows::with_shell_item(path, |item| {
        // cidl=0 means the absolute PIDL is the ITEM to select, not its parent.
        // SAFETY: with_shell_item owns this PIDL until the call returns.
        let result = unsafe {
            windows_sys::Win32::UI::Shell::SHOpenFolderAndSelectItems(item, 0, std::ptr::null(), 0)
        };
        windows::check(result, "Explorer file selection")
    })
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{Common::ITEMIDLIST, SHParseDisplayName},
    };

    pub(super) fn check(result: i32, operation: &str) -> Result<(), String> {
        if result < 0 {
            Err(format!(
                "{operation} failed (HRESULT 0x{:08X})",
                result as u32
            ))
        } else {
            Ok(())
        }
    }

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: created only after a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }

    struct Item(*mut ITEMIDLIST);
    impl Drop for Item {
        fn drop(&mut self) {
            // SAFETY: SHParseDisplayName allocates the PIDL with the COM allocator.
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }

    fn shell_name(path: &Path) -> Result<Vec<u16>, String> {
        let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err("File location contains a NUL character".into());
        }
        // Rust canonical paths may use a verbatim prefix. Shell APIs expect the
        // equivalent normal drive/UNC spelling, without lossy Unicode conversion.
        let unc: Vec<_> = r"\\?\UNC\".encode_utf16().collect();
        let verbatim: Vec<_> = r"\\?\".encode_utf16().collect();
        if name.starts_with(&unc) {
            name.splice(..unc.len(), [b'\\' as u16, b'\\' as u16]);
        } else if name.starts_with(&verbatim) {
            if name.get(5) != Some(&(b':' as u16)) {
                return Err("Unsupported Windows device path".into());
            }
            name.drain(..verbatim.len());
        }
        name.push(0);
        Ok(name)
    }

    pub(super) fn with_shell_item<T>(
        path: &Path,
        action: impl FnOnce(*const ITEMIDLIST) -> Result<T, String>,
    ) -> Result<T, String> {
        let name = shell_name(path)?;
        // SAFETY: navigation runs on its own fresh thread. S_OK and S_FALSE both
        // require a matching CoUninitialize; a failed initialization does not.
        check(
            unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) },
            "Shell COM initialization",
        )?;
        let _apartment = Apartment;
        let mut item = Item(ptr::null_mut());
        // SAFETY: name is NUL-terminated and item is a valid output pointer.
        let result = unsafe {
            SHParseDisplayName(
                name.as_ptr(),
                ptr::null_mut(),
                &mut item.0,
                0,
                ptr::null_mut(),
            )
        };
        check(result, "Resolving Explorer item")?;
        if item.0.is_null() {
            return Err("Shell returned an empty file identity".into());
        }
        action(item.0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn preserves_windows_unicode_punctuation_and_normalizes_verbatim_paths() {
            for (input, expected) in [
                (
                    r"C:\download folder\café, & #100%.torrent",
                    r"C:\download folder\café, & #100%.torrent",
                ),
                (
                    r"\\?\C:\download folder\file.bin",
                    r"C:\download folder\file.bin",
                ),
                (r"\\?\UNC\server\share\file.bin", r"\\server\share\file.bin"),
            ] {
                let mut expected: Vec<_> = expected.encode_utf16().collect();
                expected.push(0);
                assert_eq!(shell_name(Path::new(input)).unwrap(), expected);
            }
            assert!(shell_name(Path::new("C:\\bad\0name")).is_err());
            assert!(shell_name(Path::new(r"\\?\Volume{device}\file")).is_err());
            assert!(check(0, "test").is_ok());
            assert!(check(1, "test").is_ok());
            assert!(
                check(0x80070002u32 as i32, "test")
                    .unwrap_err()
                    .contains("0x80070002")
            );
        }

        #[test]
        fn native_shell_identity_is_the_file_not_the_parent_for_both_file_types() {
            // Parse real shell identities without opening windows in a unit test.
            std::thread::spawn(|| {
                let root = tempfile::tempdir().unwrap();
                for filename in ["caf\u{e9}, & #100%.torrent", "partial file.bin"] {
                    let path = root.path().join(filename);
                    std::fs::write(&path, b"unchanged").unwrap();
                    for path in [path.clone(), path.canonicalize().unwrap()] {
                        with_shell_item(&path, |item| {
                            let mut buffer = [0u16; 32768];
                            // SAFETY: item is live and buffer has the supplied capacity.
                            let ok = unsafe {
                                windows_sys::Win32::UI::Shell::SHGetPathFromIDListEx(
                                    item,
                                    buffer.as_mut_ptr(),
                                    buffer.len() as u32,
                                    0,
                                )
                            };
                            assert_ne!(ok, 0);
                            let end = buffer.iter().position(|c| *c == 0).unwrap();
                            let resolved = std::path::PathBuf::from(
                                String::from_utf16(&buffer[..end]).unwrap(),
                            );
                            assert_eq!(resolved.file_name(), path.file_name());
                            assert_eq!(
                                resolved.canonicalize().unwrap(),
                                path.canonicalize().unwrap()
                            );
                            Ok(())
                        })
                        .unwrap();
                    }
                }
            })
            .join()
            .unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
fn select(path: &Path) -> Result<(), String> {
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(|builder| {
            builder
                .method_timeout(std::time::Duration::from_secs(5))
                .build()
        })
        .map_err(|error| format!("Cannot connect to the desktop session bus: {error}"))?;
    linux::show_items(&connection, path)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    const NAME: &str = "org.freedesktop.FileManager1";
    const OBJECT: &str = "/org/freedesktop/FileManager1";

    fn file_uri(path: &Path) -> Result<String, String> {
        url::Url::from_file_path(path)
            .map(|uri| uri.into())
            .map_err(|()| "File location cannot be represented as an absolute file URI".into())
    }

    pub(super) fn show_items(
        connection: &zbus::blocking::Connection,
        path: &Path,
    ) -> Result<(), String> {
        let uri = file_uri(path)?;
        // Typed `as,s` body preserves commas, quotes, spaces and Unicode. Never
        // use ShowFolders/xdg-open(parent) as a successful selection fallback.
        connection
            .call_method(
                Some(NAME),
                OBJECT,
                Some(NAME),
                "ShowItems",
                &(vec![uri], ""),
            )
            .map(|_| ())
            .map_err(|error| {
                format!("File manager could not select the file (FileManager1.ShowItems): {error}")
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            io::{BufRead, BufReader},
            process::{Child, Command, Stdio},
            sync::mpsc,
            time::Duration,
        };

        struct Bus(Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        struct FileManager(mpsc::Sender<(Vec<String>, String)>);
        #[zbus::interface(name = "org.freedesktop.FileManager1")]
        impl FileManager {
            fn show_items(&self, uris: Vec<String>, startup_id: String) -> zbus::fdo::Result<()> {
                self.0.send((uris.clone(), startup_id)).unwrap();
                if uris[0].contains("denied") {
                    return Err(zbus::fdo::Error::AccessDenied("selection denied".into()));
                }
                if uris[0].contains("silent") {
                    std::thread::sleep(Duration::from_secs(1));
                }
                Ok(())
            }
        }

        #[test]
        fn show_items_selects_exact_file_uris_and_propagates_errors_and_timeout() {
            // Private bus: no desktop windows, real data, global environment
            // mutation or installed file manager is needed. CI installs dbus.
            let mut bus = Bus(Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("dbus-daemon is required for desktop protocol tests"));
            let mut address = String::new();
            BufReader::new(bus.0.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            let address = address.trim();
            let client = zbus::blocking::connection::Builder::address(address)
                .unwrap()
                .method_timeout(Duration::from_secs(2))
                .build()
                .unwrap();
            let root = tempfile::tempdir().unwrap();
            assert!(show_items(&client, &root.path().join("unavailable.bin")).is_err());
            let (sender, receiver) = mpsc::channel();
            let _server = zbus::blocking::connection::Builder::address(address)
                .unwrap()
                .name(NAME)
                .unwrap()
                .serve_at(OBJECT, FileManager(sender))
                .unwrap()
                .build()
                .unwrap();
            for name in [
                "caf\u{e9}, & #100% 'file'.torrent",
                "partial downloaded file.bin",
            ] {
                let path = root.path().join(name);
                std::fs::write(&path, b"untouched").unwrap();
                show_items(&client, &path).unwrap();
                let (uris, startup) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(startup.is_empty());
                assert_eq!(uris.len(), 1);
                assert_eq!(
                    url::Url::parse(&uris[0]).unwrap().to_file_path().unwrap(),
                    path
                );
                assert_eq!(std::fs::read(&path).unwrap(), b"untouched");
            }
            assert!(
                show_items(&client, &root.path().join("denied.bin"))
                    .unwrap_err()
                    .contains("selection denied")
            );
            let impatient = zbus::blocking::connection::Builder::address(address)
                .unwrap()
                .method_timeout(Duration::from_millis(200))
                .build()
                .unwrap();
            let started = std::time::Instant::now();
            assert!(show_items(&impatient, &root.path().join("silent.bin")).is_err());
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(file_uri(Path::new("relative.bin")).is_err());
        }
    }
}

#[cfg(target_os = "macos")]
fn select(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|error| error.to_string())?
        .success()
        .then_some(())
        .ok_or_else(|| "Finder could not reveal the file".into())
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn select(_path: &Path) -> Result<(), String> {
    Err("File selection is not supported on this desktop".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_relative_or_wrong_kind_locations_fail_without_launching_anything() {
        let root = tempfile::tempdir().unwrap();
        assert!(show(Path::new("relative.bin"), true).is_err());
        assert!(show(&root.path().join("missing.bin"), true).is_err());
        assert!(show(root.path(), true).is_err());
        let file = root.path().join("file.bin");
        std::fs::write(&file, b"keep").unwrap();
        assert!(show(&file, false).is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"keep");
    }
}
