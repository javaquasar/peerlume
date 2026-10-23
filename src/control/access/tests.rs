use super::*;

#[test]
fn tokens_have_cryptographic_length_and_unique_random_values() {
    let tokens: std::collections::BTreeSet<_> = (0..128).map(|_| random_id().unwrap()).collect();
    assert_eq!(tokens.len(), 128);
    assert!(tokens.iter().all(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }));
}
#[test]
fn token_digest_matches_a_known_sha256_vector() {
    assert_eq!(
        digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
#[test]
fn token_authentication_rejects_prefixes_suffixes_and_changed_case() {
    let token = "AbC-client-token";
    let hash = digest(token.as_bytes());
    assert!(matches_token(&hash, token));
    for other in [
        "",
        "AbC-client-toke",
        "AbC-client-tokenx",
        "abc-client-token",
    ] {
        assert!(!matches_token(&hash, other));
    }
    assert!(!matches_token(&hash[..63], token));
    assert!(!matches_token("", token));
}
#[test]
fn all_grant_combinations_round_trip_without_gaining_permissions() {
    for mask in 0..16 {
        let grants = Grants {
            manage: mask & 1 != 0,
            reporting: mask & 2 != 0,
            export: mask & 4 != 0,
            remove: mask & 8 != 0,
        };
        let encoded = serde_json::to_string(&grants).unwrap();
        assert_eq!(serde_json::from_str::<Grants>(&encoded).unwrap(), grants);
    }
    assert_eq!(
        Grants::default(),
        Grants {
            manage: false,
            reporting: false,
            export: false,
            remove: false
        }
    );
}
#[test]
fn nested_existing_file_is_canonical_and_stays_under_root() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/a.torrent"), b"data").unwrap();
    let resolved = confined_existing(root.path(), "nested/a.torrent").unwrap();
    assert_eq!(
        resolved,
        root.path().join("nested/a.torrent").canonicalize().unwrap()
    );
}
#[test]
fn nonexistent_roots_and_descendants_fail_without_creating_them() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("absent");
    assert!(confined_existing(&missing, "a").is_err());
    assert!(confined_new(&missing, "a").is_err());
    assert!(!missing.exists());
    assert!(confined_existing(root.path(), "absent/file").is_err());
    assert!(!root.path().join("absent").exists());
}
#[test]
fn export_names_reject_nested_paths_devices_streams_and_aliases() {
    let root = tempfile::tempdir().unwrap();
    for value in [
        "a/b",
        "a\\b",
        "a:b",
        ".",
        "..",
        "CON",
        "con.txt",
        "AUX.jsonl",
        "PRN",
        "NUL",
        "COM9.bin",
        "LPT1",
        "trailing.",
        "trailing ",
        "a\0b",
    ] {
        assert_eq!(
            confined_new(root.path(), value).err(),
            Some(ErrorCode::PathDenied),
            "{value:?}"
        );
    }
}
#[test]
fn export_name_boundary_does_not_create_output_as_a_side_effect() {
    let root = tempfile::tempdir().unwrap();
    let name = "a".repeat(200);
    let path = confined_new(root.path(), &name).unwrap();
    assert!(!path.exists());
    assert!(confined_new(root.path(), &"a".repeat(201)).is_err());
}
#[test]
fn existing_directory_is_not_a_valid_new_export_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("occupied")).unwrap();
    assert!(confined_new(root.path(), "occupied").is_err());
}
#[test]
fn existing_payload_is_never_overwritten_by_path_validation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("existing.jsonl");
    std::fs::write(&path, b"keep me").unwrap();
    assert!(confined_new(root.path(), "existing.jsonl").is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"keep me");
}
#[cfg(unix)]
#[test]
fn dangling_symlinks_and_linked_directories_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("file"), b"secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("missing"), root.path().join("dangling"))
        .unwrap();
    assert!(confined_existing(root.path(), "link/file").is_err());
    assert!(confined_new(root.path(), "dangling").is_err());
}
proptest::proptest! {
    #[test]
    fn traversal_fragments_never_escape_a_root(suffix in "[a-z]{1,20}") {
        let root=tempfile::tempdir().unwrap();
        for value in [format!("../{suffix}"),format!("a/../../{suffix}"),format!("/{suffix}"),format!("{suffix}:stream")] {
            proptest::prop_assert!(confined_existing(root.path(),&value).is_err());proptest::prop_assert!(confined_new(root.path(),&value).is_err());
        }
    }
}
