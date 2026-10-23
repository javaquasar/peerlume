//! Trusted-frontend access management. No grant/revoke/approve wire methods exist.
use super::wire::ErrorCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    pub manage: bool,
    pub reporting: bool,
    pub export: bool,
    pub remove: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientGrant {
    pub id: String,
    pub grants: Grants,
    pub(crate) token_hash: String,
}

pub fn random_id() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn matches_token(hash: &str, token: &str) -> bool {
    let candidate = digest(token.as_bytes());
    hash.len() == candidate.len()
        && hash
            .bytes()
            .zip(candidate.bytes())
            .fold(0u8, |a, (b, c)| a | (b ^ c))
            == 0
}

/// Existing files only, no links/reparse points. Never accept absolute, ADS or traversal paths.
/// The configured root is trusted; descendants must remain under its canonical path.
pub fn confined_existing(root: &Path, relative: &str) -> Result<PathBuf, ErrorCode> {
    if relative.is_empty() || relative.len() > 1024 || relative.contains(['\\', ':']) {
        return Err(ErrorCode::PathDenied);
    }
    let path = Path::new(relative);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ErrorCode::PathDenied);
    }
    crate::limits::validate_relative_paths([relative], &crate::limits::ResourceLimits::default())
        .map_err(|_| ErrorCode::PathDenied)?;
    let root = root.canonicalize().map_err(|_| ErrorCode::PathDenied)?;
    let mut target = root.clone();
    for part in path.components() {
        target.push(part.as_os_str());
        let metadata = std::fs::symlink_metadata(&target).map_err(|_| ErrorCode::PathDenied)?;
        if is_link(&metadata) {
            return Err(ErrorCode::PathDenied);
        }
    }
    let resolved = target.canonicalize().map_err(|_| ErrorCode::PathDenied)?;
    if !resolved.starts_with(&root) {
        return Err(ErrorCode::PathDenied);
    }
    Ok(resolved)
}

pub fn confined_new(root: &Path, relative: &str) -> Result<PathBuf, ErrorCode> {
    if relative.contains(['/', '\\', ':']) || relative.len() > 200 {
        return Err(ErrorCode::PathDenied);
    }
    crate::limits::validate_relative_paths([relative], &crate::limits::ResourceLimits::default())
        .map_err(|_| ErrorCode::PathDenied)?;
    let root = root.canonicalize().map_err(|_| ErrorCode::PathDenied)?;
    let target = root.join(relative);
    if target.exists() || std::fs::symlink_metadata(&target).is_ok() {
        return Err(ErrorCode::PathDenied);
    }
    Ok(target)
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests;
