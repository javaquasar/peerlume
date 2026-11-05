use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_torrent_bytes: u64,
    pub max_bencode_depth: usize,
    pub max_bencode_nodes: usize,
    pub max_files: usize,
    pub max_trackers: usize,
    pub max_component_bytes: usize,
    pub max_path_bytes: usize,
    pub max_total_content_bytes: u64,
    pub max_blocklist_compressed_bytes: usize,
    pub max_blocklist_decompressed_bytes: usize,
    pub max_blocklist_rules: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_torrent_bytes: 32 * 1024 * 1024,
            max_bencode_depth: 128,
            max_bencode_nodes: 2_000_000,
            max_files: 500_000,
            max_trackers: 256,
            max_component_bytes: 255,
            max_path_bytes: 32_767,
            max_total_content_bytes: 16 * 1024 * 1024 * 1024 * 1024,
            max_blocklist_compressed_bytes: 64 * 1024 * 1024,
            max_blocklist_decompressed_bytes: 256 * 1024 * 1024,
            max_blocklist_rules: 2_000_000,
        }
    }
}

pub fn validate_relative_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    limits: &ResourceLimits,
) -> Result<(), String> {
    let mut normalized_files = HashSet::new();
    let mut directories = HashSet::new();
    for path in paths {
        if path.is_empty() || path.len() > limits.max_path_bytes {
            return Err(format!("invalid path length: {path:?}"));
        }
        if path.starts_with(['/', '\\']) || has_drive_prefix(path) {
            return Err(format!("absolute paths are not allowed: {path:?}"));
        }
        let mut normalized = String::with_capacity(path.len());
        let mut components = path.split(['/', '\\']).peekable();
        while let Some(component) = components.next() {
            validate_windows_component(component, limits)?;
            if !normalized.is_empty() {
                normalized.push('/');
            }
            normalized.push_str(&component.to_lowercase());
            if components.peek().is_some() {
                if normalized_files.contains(&normalized) {
                    return Err(format!("file/directory path collision: {path:?}"));
                }
                directories.insert(normalized.clone());
            }
        }
        // Every proper directory prefix was indexed while processing its child.
        // This covers child-before-parent collisions without scanning all files.
        if directories.contains(&normalized) || !normalized_files.insert(normalized) {
            return Err(format!(
                "duplicate or file/directory path collision: {path:?}"
            ));
        }
        if normalized_files.len() > limits.max_files {
            return Err("too many files".into());
        }
    }
    Ok(())
}

fn validate_windows_component(component: &str, limits: &ResourceLimits) -> Result<(), String> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.len() > limits.max_component_bytes
        || component.ends_with([' ', '.'])
        || component.bytes().any(|byte| byte < 32)
        || component.contains(['\0', ':', '*', '?', '"', '<', '>', '|'])
    {
        return Err(format!("unsafe path component: {component:?}"));
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(
                    number,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
    if reserved {
        return Err(format!("reserved Windows path component: {component:?}"));
    }
    Ok(())
}

fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_windows_aliases_duplicates_and_collisions() {
        let limits = ResourceLimits::default();
        assert!(validate_relative_paths(["dir/a.txt", "DIR/A.TXT"], &limits).is_err());
        assert!(validate_relative_paths(["folder", "folder/a.txt"], &limits).is_err());
        assert!(validate_relative_paths(["CON.txt"], &limits).is_err());
        assert!(validate_relative_paths(["file.txt:secret"], &limits).is_err());
        assert!(validate_relative_paths(["C:/escape.txt"], &limits).is_err());
    }

    #[test]
    fn accepts_distinct_portable_relative_paths() {
        let limits = ResourceLimits::default();
        validate_relative_paths(["dir/a.txt", "dir/b.txt", "other/c.bin"], &limits).unwrap();
    }

    #[test]
    fn indexed_prefixes_reject_both_orders_and_preserve_unicode_aliases() {
        let limits = ResourceLimits::default();
        for pair in [
            ["folder", "folder/a.txt"],
            ["A/B", "a\\b\\child.bin"],
            ["É/FILE.bin", "é\\file.BIN"],
            ["İ/file.bin", "i\u{0307}/file.bin"],
            ["ΟΣ/file.bin", "ος/file.bin"],
        ] {
            assert!(validate_relative_paths(pair, &limits).is_err(), "{pair:?}");
            assert!(
                validate_relative_paths(pair.into_iter().rev(), &limits).is_err(),
                "{pair:?}"
            );
        }
        validate_relative_paths(["a/b", "a/bb/c", "a/bb/d"], &limits).unwrap();
    }

    #[test]
    fn resource_bounds_and_windows_device_aliases_remain_rejected() {
        let limits = ResourceLimits {
            max_files: 1,
            ..Default::default()
        };
        assert!(validate_relative_paths(["a", "b"], &limits).is_err());
        for path in [
            "COM¹.txt",
            "LPT²",
            "COM³.bin",
            "a\u{001f}b",
            "a/../b",
            "a//b",
            "a\\.\\b",
            "name.",
            "name ",
            "/absolute",
            "\\server\\file",
            "C:relative",
        ] {
            assert!(
                validate_relative_paths([path], &ResourceLimits::default()).is_err(),
                "accepted {path:?}"
            );
        }
        let short = ResourceLimits {
            max_component_bytes: 2,
            max_path_bytes: 4,
            ..Default::default()
        };
        assert!(validate_relative_paths(["abc"], &short).is_err());
        assert!(validate_relative_paths(["ab/cd"], &short).is_err());
        validate_relative_paths(["ab/c"], &short).unwrap();
    }

    proptest! {
        #[test]
        fn generated_prefix_sets_match_a_pairwise_collision_oracle(paths in prop::collection::vec(prop::collection::vec("x[a-zA-Z0-9_-]{1,6}", 1..5), 0..40)) {
            let normalized: Vec<_> = paths.iter().map(|parts| parts.join("/").to_lowercase()).collect();
            let collision = normalized.iter().enumerate().any(|(index,left)| normalized[index+1..].iter().any(|right| left==right || left.starts_with(&(right.clone()+"/")) || right.starts_with(&(left.clone()+"/"))));
            let paths: Vec<_> = paths.iter().enumerate().map(|(index,parts)| parts.join(if index%2==0 {"/"} else {"\\"})).collect();
            prop_assert_eq!(validate_relative_paths(paths.iter().map(String::as_str), &ResourceLimits::default()).is_err(), collision);
            prop_assert_eq!(validate_relative_paths(paths.iter().rev().map(String::as_str), &ResourceLimits::default()).is_err(), collision);
        }

        #[test]
        fn generated_parent_child_collisions_are_order_independent(parts in prop::collection::vec("[a-zA-Z0-9_-]{1,6}",1..8)) {
            let parent=parts.join("/");
            let child=format!("{}\\child.bin",parts.join("\\").to_uppercase());
            prop_assert!(validate_relative_paths([parent.as_str(),child.as_str()],&ResourceLimits::default()).is_err());
            prop_assert!(validate_relative_paths([child.as_str(),parent.as_str()],&ResourceLimits::default()).is_err());
        }
    }
}
