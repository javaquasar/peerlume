//! Read-only inspection of the portable desktop package produced by package_build.py.
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt as LinkMetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

const MANIFEST: &str = "build-info.json";
const CHECKSUMS: &str = "SHA256SUMS";
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 768 * 1024 * 1024;
const BINARIES: [&str; 5] = [
    "peerlume",
    "peerlume-daemon",
    "peerlume-tui",
    "peerlume-control",
    "peerlume-mcp",
];
const DOCUMENTS: [&str; 6] = [
    "README.md",
    "docs/tui.md",
    "docs/agent-control.md",
    "docs/runtime-controls.md",
    "config/peerlume.example.toml",
    "config/tracker-behaviors.example.toml",
];
const LINUX_DESKTOP_FILES: [&str; 2] = [
    "share/applications/peerlume.desktop",
    "share/icons/hicolor/256x256/apps/peerlume.png",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Windows,
    Linux,
}

impl Platform {
    fn current() -> Result<Self, String> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => Ok(Self::Windows),
            ("linux", "x86_64") => Ok(Self::Linux),
            _ => Err("Portable desktop packages are available for Windows and Linux x64".into()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Windows => "windows-x64",
            Self::Linux => "linux-x64",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Self::Windows => "x86_64-pc-windows-msvc",
            Self::Linux => "x86_64-unknown-linux-gnu",
        }
    }

    fn binary_path(self, name: &str) -> String {
        match self {
            Self::Windows => format!("{name}.exe"),
            Self::Linux => format!("bin/{name}"),
        }
    }

    fn members(self) -> BTreeMap<String, bool> {
        let mut files = BTreeMap::from([(MANIFEST.to_owned(), false)]);
        files.extend(BINARIES.map(|name| (self.binary_path(name), true)));
        files.extend(DOCUMENTS.map(|path| (path.to_owned(), false)));
        if self == Self::Linux {
            files.extend(LINUX_DESKTOP_FILES.map(|path| (path.to_owned(), false)));
        }
        files
    }
}

#[derive(Deserialize)]
struct Manifest {
    application: String,
    version: String,
    commit: String,
    platform: String,
    target: String,
    profile: String,
    features: Vec<String>,
    rustc: String,
    signed: bool,
}

#[derive(Debug)]
pub(super) struct VerifiedPackage {
    pub folder: PathBuf,
    pub version: String,
    pub commit: String,
    pub platform: String,
    pub target: String,
    pub rustc: String,
    pub hashed_files: usize,
    pub hashed_bytes: u64,
}

pub(super) fn verify(folder: &Path) -> Result<VerifiedPackage, String> {
    verify_for(folder, Platform::current()?)
}

fn verify_for(folder: &Path, platform: Platform) -> Result<VerifiedPackage, String> {
    if !folder.is_absolute() || folder.components().any(|part| part == Component::ParentDir) {
        return Err("Choose an absolute extracted package folder".into());
    }
    let parent = folder
        .parent()
        .ok_or("Choose a package folder below a filesystem root")?;
    let name = folder
        .file_name()
        .ok_or("Choose an extracted package folder")?;
    let parent = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|error| format!("Cannot open package parent: {error}"))?;
    let dir = open_directory(&parent, Path::new(name))
        .map_err(|error| format!("Cannot open package folder: {error}"))?;
    let manifest_bytes = read_metadata(&dir, MANIFEST)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Invalid build-info.json: {error}"))?;
    validate_manifest(&manifest, platform)?;

    let expected = platform.members();
    let checksum_bytes = read_metadata(&dir, CHECKSUMS)?;
    let checksums = parse_checksums(&checksum_bytes, &expected)?;
    validate_members(&dir, &expected)?;

    let mut total = (manifest_bytes.len() + checksum_bytes.len()) as u64;
    for (name, &executable) in &expected {
        let digest = if name == MANIFEST {
            format!("{:x}", Sha256::digest(&manifest_bytes))
        } else {
            let mut file = open_member(&dir, name)?;
            let length = file
                .metadata()
                .map_err(|error| member_error(name, error))?
                .len();
            let remaining = (MAX_PACKAGE_BYTES - total).min(MAX_FILE_BYTES);
            if length > remaining {
                return Err(format!("Package size limit exceeded at {name}"));
            }
            if executable {
                check_executable(&mut file, length, platform)
                    .map_err(|error| member_error(name, error))?;
            }
            let (digest, bytes) =
                hash_file(file, remaining).map_err(|error| member_error(name, error))?;
            if bytes != length {
                return Err(format!(
                    "Package member changed during verification: {name}"
                ));
            }
            total += bytes;
            digest
        };
        if digest != checksums[name.as_str()] {
            return Err(format!("SHA-256 mismatch: {name}"));
        }
    }
    Ok(VerifiedPackage {
        folder: folder.to_owned(),
        version: manifest.version,
        commit: manifest.commit,
        platform: manifest.platform,
        target: manifest.target,
        rustc: manifest.rustc,
        hashed_files: expected.len(),
        hashed_bytes: total - checksum_bytes.len() as u64,
    })
}

fn validate_manifest(manifest: &Manifest, platform: Platform) -> Result<(), String> {
    if manifest.application != "PeerLume" || manifest.profile != "release" {
        return Err("Expected a PeerLume release-profile package".into());
    }
    if manifest.platform != platform.label() || manifest.target != platform.target() {
        return Err(format!(
            "Choose a package for {} ({})",
            platform.label(),
            platform.target()
        ));
    }
    if manifest.features != ["gui", "tui"] {
        return Err("Choose the desktop package containing both GUI and TUI".into());
    }
    if !valid_version(&manifest.version) || !lower_hex(&manifest.commit, 40) {
        return Err("The build manifest needs a valid version and full source commit".into());
    }
    if manifest.rustc.is_empty()
        || manifest.rustc.len() > 200
        || manifest.rustc.chars().any(char::is_control)
    {
        return Err("The build manifest contains invalid compiler information".into());
    }
    if manifest.signed {
        return Err("This preview supports the current unsigned package format".into());
    }
    Ok(())
}

fn valid_version(version: &str) -> bool {
    if version.len() > 64 {
        return false;
    }
    let (core, suffix) = version.find(['-', '+']).map_or((version, None), |index| {
        (&version[..index], Some(&version[index + 1..]))
    });
    core.split('.').count() == 3
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && suffix.is_none_or(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
        })
}

fn lower_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_checksums<'a>(
    bytes: &'a [u8],
    expected: &BTreeMap<String, bool>,
) -> Result<BTreeMap<&'a str, &'a str>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "SHA256SUMS must contain UTF-8 text")?;
    let mut checksums = BTreeMap::new();
    for line in text.lines() {
        let (digest, name) = line.split_once("  ").ok_or("Invalid SHA256SUMS entry")?;
        if !lower_hex(digest, 64)
            || !expected.contains_key(name)
            || checksums.insert(name, digest).is_some()
        {
            return Err("SHA256SUMS has an invalid, unexpected, or duplicate entry".into());
        }
    }
    if checksums.len() != expected.len() {
        return Err("SHA256SUMS is missing required package members".into());
    }
    Ok(checksums)
}

fn validate_members(dir: &Dir, expected: &BTreeMap<String, bool>) -> Result<(), String> {
    let mut files: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
    files.insert(CHECKSUMS);
    let parents = expected
        .keys()
        .flat_map(|path| {
            path.match_indices('/')
                .map(move |(index, _)| &path[..index])
        })
        .collect();
    scan_members(dir, "", &mut files, &parents)?;
    if !files.is_empty() {
        return Err("The extracted package is missing required files".into());
    }
    Ok(())
}

fn scan_members(
    dir: &Dir,
    prefix: &str,
    files: &mut BTreeSet<&str>,
    parents: &BTreeSet<&str>,
) -> Result<(), String> {
    let entries = dir
        .entries()
        .map_err(|error| format!("Cannot list package folder: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Cannot read package folder entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Invalid package member name")?;
        let relative = format!("{prefix}{name}");
        let metadata = dir
            .symlink_metadata(&name)
            .map_err(|error| member_error(&relative, error))?;
        if is_link(&metadata) {
            return Err("Package links are not allowed".into());
        }
        if metadata.is_dir() && parents.contains(relative.as_str()) {
            let child = open_directory(dir, Path::new(&name))
                .map_err(|error| member_error(&relative, error))?;
            scan_members(&child, &format!("{relative}/"), files, parents)?;
        } else if !metadata.is_file() || !files.remove(relative.as_str()) {
            return Err("The extracted folder contains an unexpected package member".into());
        }
    }
    Ok(())
}

fn open_directory(parent: &Dir, name: &Path) -> io::Result<Dir> {
    let metadata = parent.symlink_metadata(name)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(io::Error::other("Expected a regular package directory"));
    }
    let directory = parent.open_dir_nofollow(name)?;
    if is_link(&directory.dir_metadata()?) {
        return Err(io::Error::other("Package directory links are not allowed"));
    }
    Ok(directory)
}

fn open_member(root: &Dir, path: &str) -> Result<File, String> {
    // `path` comes only from our fixed allowlist; checksum strings are never opened.
    let result = (|| -> io::Result<File> {
        let mut directory = root.try_clone()?;
        let (parents, name) = path.rsplit_once('/').unwrap_or(("", path));
        for parent in parents.split('/').filter(|part| !part.is_empty()) {
            directory = open_directory(&directory, Path::new(parent))?;
        }
        let metadata = directory.symlink_metadata(name)?;
        if !metadata.is_file() || is_link(&metadata) {
            return Err(io::Error::other("Expected a regular package file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(target_os = "linux")]
        {
            use cap_std::fs::OpenOptionsExt;
            // Linux x86-64 O_NONBLOCK prevents a replaced FIFO from blocking open.
            options.custom_flags(0o4000);
        }
        let file = directory.open_with(name, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_link(&metadata) || LinkMetadataExt::nlink(&metadata) != 1 {
            return Err(io::Error::other(
                "Expected a regular package file without links",
            ));
        }
        Ok(file.into_std())
    })();
    result.map_err(|error| member_error(path, error))
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn member_error(path: &str, error: io::Error) -> String {
    format!("Cannot verify {path}: {error}")
}

fn read_metadata(dir: &Dir, path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    open_member(dir, path)?
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| member_error(path, error))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(format!("Package metadata exceeds 64 KiB: {path}"));
    }
    Ok(bytes)
}

fn check_executable(file: &mut File, length: u64, platform: Platform) -> io::Result<()> {
    let mut header = [0; 64];
    file.read_exact(&mut header)?;
    match platform {
        Platform::Windows => {
            let offset = u64::from(u32::from_le_bytes(header[60..64].try_into().unwrap()));
            if &header[..2] != b"MZ" || offset < 64 || offset + 6 > length {
                return Err(io::Error::other("Invalid Windows executable header"));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut signature = [0; 6];
            file.read_exact(&mut signature)?;
            if signature != *b"PE\0\0\x64\x86" {
                return Err(io::Error::other("Expected an x64 Windows executable"));
            }
        }
        Platform::Linux => {
            if &header[..6] != b"\x7fELF\x02\x01"
                || u16::from_le_bytes([header[18], header[19]]) != 62
            {
                return Err(io::Error::other(
                    "Expected an x64 little-endian Linux executable",
                ));
            }
        }
    }
    file.rewind()
}

fn hash_file(file: File, limit: u64) -> io::Result<(String, u64)> {
    let mut reader = file.take(limit + 1);
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    let mut total = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            return Err(io::Error::other(
                "Package size limit exceeded while reading",
            ));
        }
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:x}", digest.finalize()), total))
}

#[cfg(test)]
mod tests;
