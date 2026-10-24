use super::*;
fn payload(root: &Directory, path: &str, len: u64) -> Payload {
    let file = root.open_payload(Path::new(path), true).unwrap();
    file.set_len(len).unwrap();
    Payload {
        root: root.clone(),
        files: RwLock::new(vec![Entry {
            file: Some(file),
            path: path.into(),
            length: len,
        }]),
    }
}
#[test]
fn opened_payload_is_not_truncated_and_creation_can_refuse_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("payload.bin"), b"verified").unwrap();
    let root = Directory::open(temp.path()).unwrap();
    drop(root.open_payload(Path::new("payload.bin"), true).unwrap());
    assert_eq!(
        std::fs::read(temp.path().join("payload.bin")).unwrap(),
        b"verified"
    );
    assert!(root.open_payload(Path::new("payload.bin"), false).is_err());
}
#[test]
fn aliases_absolute_paths_and_traversal_never_create_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    for path in [
        "../escape",
        "/absolute",
        "a/../../escape",
        "NUL",
        "CON.txt",
        "a:b",
        "a\\b",
        "",
    ] {
        assert!(root.open_payload(Path::new(path), true).is_err(), "{path}");
    }
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
#[test]
fn bounds_checked_io_never_extends_a_file_or_overflows_offsets() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = payload(&root, "payload", 8);
    p.pwrite_all(0, 0, b"12345678").unwrap();
    let mut bytes = [0; 8];
    p.pread_exact(0, 0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"12345678");
    for offset in [8, u64::MAX] {
        assert!(p.pwrite_all(0, offset, b"x").is_err());
        assert!(p.pread_exact(0, offset, &mut [0]).is_err());
    }
    assert!(p.ensure_file_length(0, 9).is_err());
    assert!(p.pwrite_all(9, 0, b"x").is_err());
    assert_eq!(
        std::fs::metadata(temp.path().join("payload"))
            .unwrap()
            .len(),
        8
    );
}
#[test]
fn taking_storage_moves_handles_and_invalidates_the_old_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = payload(&root, "payload", 8);
    let moved = p.take().unwrap();
    assert!(p.pwrite_all(0, 0, b"x").is_err());
    moved.pwrite_all(0, 0, b"verified").unwrap();
    let mut b = [0; 8];
    moved.pread_exact(0, 0, &mut b).unwrap();
    assert_eq!(&b, b"verified");
}
#[test]
fn padding_is_zero_filled_without_creating_a_physical_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = Payload {
        root,
        files: RwLock::new(vec![Entry {
            file: None,
            path: ".pad/16".into(),
            length: 16,
        }]),
    };
    p.pwrite_all(0, 0, b"ignored").unwrap();
    let mut b = [255; 16];
    p.pread_exact(0, 0, &mut b).unwrap();
    assert_eq!(b, [0; 16]);
    p.ensure_file_length(0, 16).unwrap();
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
#[test]
fn parallel_positioned_writes_preserve_each_disjoint_block() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = Arc::new(payload(&root, "parallel", 8 * 1024));
    let jobs: Vec<_> = (0..8)
        .map(|index| {
            let p = p.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    p.pwrite_all(0, index * 1024, &vec![index as u8; 1024])
                        .unwrap();
                }
            })
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let mut bytes = vec![0; 8192];
    p.pread_exact(0, 0, &mut bytes).unwrap();
    for (index, block) in bytes.chunks_exact(1024).enumerate() {
        assert!(block.iter().all(|b| *b == index as u8));
    }
}
#[test]
fn removal_is_confined_and_requires_the_matching_file_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = payload(&root, "nested/payload", 8);
    assert!(p.remove_file(0, Path::new("other")).is_err());
    assert!(
        p.remove_directory_if_empty(Path::new("../outside"))
            .is_err()
    );
    assert!(root.remove(Path::new("nested"), true).is_err());
}
#[cfg(unix)]
#[test]
fn replacing_an_ancestor_cannot_redirect_an_open_payload_handle() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = Directory::open(temp.path()).unwrap();
    let p = payload(&root, "folder/payload", 8);
    std::fs::rename(temp.path().join("folder"), temp.path().join("original")).unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("folder")).unwrap();
    p.pwrite_all(0, 0, b"verified").unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("original/payload")).unwrap(),
        b"verified"
    );
    assert!(!outside.path().join("payload").exists());
    assert!(root.open_payload(Path::new("folder/escape"), true).is_err());
}
#[cfg(unix)]
#[test]
fn symlink_leaf_and_directory_are_rejected_before_read_or_write() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"untouched").unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), temp.path().join("leaf")).unwrap();
    let root = Directory::open(temp.path()).unwrap();
    assert!(root.open_payload(Path::new("link/secret"), true).is_err());
    assert!(root.open_payload(Path::new("leaf"), true).is_err());
    assert_eq!(
        std::fs::read(outside.path().join("secret")).unwrap(),
        b"untouched"
    );
}
#[cfg(windows)]
#[test]
fn windows_junction_cannot_redirect_backend_payload_creation() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(temp.path().join("junction"))
        .arg(outside.path())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let root = Directory::open(temp.path()).unwrap();
    assert!(
        root.open_payload(Path::new("junction/escape"), true)
            .is_err()
    );
    assert!(!outside.path().join("escape").exists());
}
