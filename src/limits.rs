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
        let mut normalized = String::new();
        let components: Vec<_> = path.split(['/', '\\']).collect();
        for (index, component) in components.iter().enumerate() {
            validate_windows_component(component, limits)?;
            if index > 0 {
                normalized.push('/');
            }
            normalized.push_str(&component.to_lowercase());
            if index + 1 < components.len() {
                if normalized_files.contains(&normalized) {
                    return Err(format!("file/directory path collision: {path:?}"));
                }
                directories.insert(normalized.clone());
            }
        }
        if directories.contains(&normalized) || !normalized_files.insert(normalized.clone()) {
            return Err(format!(
                "duplicate or file/directory path collision: {path:?}"
            ));
        }
        if normalized_files
            .iter()
            .any(|file| file.starts_with(&(normalized.clone() + "/")))
        {
            return Err(format!("file/directory path collision: {path:?}"));
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
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
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
}
