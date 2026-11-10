use super::*;
use std::fs;

const COMMIT: &str = "2d4df223847e4ff07cf2db5f27ae38feefdc742b";

fn fixture(platform: Platform) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, executable) in platform.members() {
        let path = root.path().join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = if executable {
            match platform {
                Platform::Windows => {
                    let mut header = vec![0; 64];
                    header[..2].copy_from_slice(b"MZ");
                    header[60] = 64;
                    header.extend_from_slice(b"PE\0\0\x64\x86");
                    header.extend_from_slice(name.as_bytes());
                    header
                }
                Platform::Linux => {
                    let mut header = vec![0; 64];
                    header[..6].copy_from_slice(b"\x7fELF\x02\x01");
                    header[18] = 62;
                    header.extend_from_slice(name.as_bytes());
                    header
                }
            }
        } else if name == MANIFEST {
            serde_json::to_vec(&serde_json::json!({
                "application": "PeerLume",
                "version": "0.1.0",
                "commit": COMMIT,
                "platform": platform.label(),
                "target": platform.target(),
                "profile": "release",
                "features": ["gui", "tui"],
                "rustc": "rustc 1.93.0 (fixture)",
                "built_at_utc": "2026-11-09T13:40:16+00:00",
                "linux_baseline": null,
                "signed": false,
            }))
            .unwrap()
        } else {
            format!("Portable documentation for {name}\n").into_bytes()
        };
        fs::write(path, bytes).unwrap();
    }
    write_checksums(root.path(), platform);
    root
}

fn write_checksums(root: &Path, platform: Platform) {
    let text: String = platform
        .members()
        .keys()
        .map(|name| {
            let bytes = fs::read(root.join(name)).unwrap();
            format!("{:x}  {name}\n", Sha256::digest(bytes))
        })
        .collect();
    fs::write(root.join(CHECKSUMS), text).unwrap();
}

#[test]
fn verifies_complete_windows_and_linux_layouts_with_full_build_identity() {
    for (platform, file_count) in [(Platform::Windows, 12), (Platform::Linux, 14)] {
        let package = fixture(platform);
        let preview = verify_for(package.path(), platform).unwrap();
        assert_eq!(preview.folder, package.path());
        assert_eq!(preview.version, "0.1.0");
        assert_eq!(preview.commit, COMMIT);
        assert_eq!(preview.platform, platform.label());
        assert_eq!(preview.target, platform.target());
        assert_eq!(preview.hashed_files, file_count);
        let bytes: u64 = platform
            .members()
            .keys()
            .map(|file| fs::metadata(package.path().join(file)).unwrap().len())
            .sum();
        assert_eq!(preview.hashed_bytes, bytes);
    }
}

#[test]
fn refuses_a_missing_or_replaced_daemon_and_extra_package_members() {
    let package = fixture(Platform::Windows);
    let daemon = package.path().join("peerlume-daemon.exe");
    let original = fs::read(&daemon).unwrap();
    fs::remove_file(&daemon).unwrap();
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("missing required files")
    );
    let mut replaced = original.clone();
    replaced.extend_from_slice(b"another build");
    fs::write(&daemon, replaced).unwrap();
    assert_eq!(
        verify_for(package.path(), Platform::Windows).unwrap_err(),
        "SHA-256 mismatch: peerlume-daemon.exe"
    );
    fs::write(&daemon, original).unwrap();
    fs::write(package.path().join("unexpected.exe"), b"unlisted").unwrap();
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("unexpected package member")
    );
}

#[test]
fn checksum_paths_are_exact_and_cannot_open_files_outside_the_package() {
    let package = fixture(Platform::Windows);
    let checksums = package.path().join(CHECKSUMS);
    let original = fs::read_to_string(&checksums).unwrap();
    for invalid in [
        original.replace("peerlume.exe", "../peerlume.exe"),
        original.replace("peerlume.exe", "/peerlume.exe"),
        original.replace("peerlume.exe", "PEERLUME.EXE"),
        original.replace("docs/tui.md", "docs\\tui.md"),
        format!("{original}{}\n", original.lines().next().unwrap()),
        original.lines().skip(1).collect::<Vec<_>>().join("\n"),
    ] {
        fs::write(&checksums, invalid).unwrap();
        assert!(verify_for(package.path(), Platform::Windows).is_err());
    }
}

#[test]
fn wrong_platform_and_wrong_executable_architecture_fail_even_with_matching_hashes() {
    let package = fixture(Platform::Linux);
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("Choose a package for windows-x64")
    );
    let path = package.path().join("bin/peerlume-daemon");
    let mut bytes = fs::read(&path).unwrap();
    bytes[18] = 183; // AArch64, not x64.
    fs::write(path, bytes).unwrap();
    write_checksums(package.path(), Platform::Linux);
    assert!(
        verify_for(package.path(), Platform::Linux)
            .unwrap_err()
            .contains("x64 little-endian Linux executable")
    );
    let package = fixture(Platform::Windows);
    let path = package.path().join("peerlume-daemon.exe");
    let mut bytes = fs::read(&path).unwrap();
    bytes[68] = 0x4c; // PE i386 machine, not AMD64.
    bytes[69] = 0x01;
    fs::write(path, bytes).unwrap();
    write_checksums(package.path(), Platform::Windows);
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("x64 Windows executable")
    );
}

#[test]
fn malformed_identity_and_unsupported_manifest_claims_are_rejected() {
    let package = fixture(Platform::Windows);
    let manifest = package.path().join(MANIFEST);
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for (key, value) in [
        ("version", serde_json::json!("0.5.0\nmalicious")),
        ("commit", serde_json::json!("deadbeef")),
        ("signed", serde_json::json!(true)),
        ("features", serde_json::json!(["tui"])),
        ("profile", serde_json::json!("dev")),
        ("rustc", serde_json::json!("\u{1b}[31m")),
    ] {
        let mut changed = original.clone();
        changed[key] = value;
        fs::write(&manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
        write_checksums(package.path(), Platform::Windows);
        assert!(
            verify_for(package.path(), Platform::Windows).is_err(),
            "{key}"
        );
    }
    fs::write(&manifest, b"{ invalid json").unwrap();
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("Invalid build-info.json")
    );
}

#[test]
fn manifest_itself_is_checked_against_the_original_checksum() {
    let package = fixture(Platform::Windows);
    let manifest = package.path().join(MANIFEST);
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, original.replace("0.1.0", "0.2.0")).unwrap();
    assert_eq!(
        verify_for(package.path(), Platform::Windows).unwrap_err(),
        "SHA-256 mismatch: build-info.json"
    );
}

#[test]
fn oversized_metadata_and_streams_fail_at_fixed_limits() {
    let package = fixture(Platform::Windows);
    fs::write(
        package.path().join(CHECKSUMS),
        vec![0; MAX_METADATA_BYTES as usize + 1],
    )
    .unwrap();
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("metadata exceeds 64 KiB")
    );
    let file = File::open(package.path().join("peerlume.exe")).unwrap();
    assert!(
        hash_file(file, 4)
            .unwrap_err()
            .to_string()
            .contains("size limit")
    );
}

#[test]
fn hard_linked_member_is_rejected_even_with_the_correct_checksum() {
    let package = fixture(Platform::Windows);
    let outside = tempfile::tempdir().unwrap();
    let readme = package.path().join("README.md");
    let linked = outside.path().join("original.md");
    fs::copy(&readme, &linked).unwrap();
    fs::remove_file(&readme).unwrap();
    fs::hard_link(&linked, &readme).unwrap();
    assert!(
        verify_for(package.path(), Platform::Windows)
            .unwrap_err()
            .contains("without links")
    );
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_roots_nested_directories_and_files() {
    use std::os::unix::fs::symlink;
    let package = fixture(Platform::Linux);
    let outside = tempfile::tempdir().unwrap();
    let linked_root = outside.path().join("package");
    symlink(package.path(), &linked_root).unwrap();
    assert!(verify_for(&linked_root, Platform::Linux).is_err());
    let docs = package.path().join("docs");
    let real_docs = outside.path().join("docs");
    fs::rename(&docs, &real_docs).unwrap();
    symlink(&real_docs, &docs).unwrap();
    assert!(
        verify_for(package.path(), Platform::Linux)
            .unwrap_err()
            .contains("links")
    );
    fs::remove_file(&docs).unwrap();
    fs::rename(&real_docs, &docs).unwrap();
    let readme = package.path().join("README.md");
    let real_readme = outside.path().join("readme");
    fs::rename(&readme, &real_readme).unwrap();
    symlink(&real_readme, &readme).unwrap();
    assert!(
        verify_for(package.path(), Platform::Linux)
            .unwrap_err()
            .contains("links")
    );
}
