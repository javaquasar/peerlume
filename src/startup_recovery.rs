//! Local startup evidence for trusted frontends. Never stores underlying errors.
use crate::control::{access::random_id, frontend::PROTOCOL_VERSION, host::InstanceLock};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const FILE_NAME: &str = "startup-failure.json";
const SCHEMA: u8 = 1;
const MAX_BYTES: u64 = 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupFailureCode {
    SettingsUnavailable,
    InvalidSettings,
    SettingsSaveFailed,
    StateUnavailable,
    CredentialStoreUnavailable,
    StorageRecoveryFailed,
    InvalidRestoredPath,
    TransferEngineUnavailable,
    ListenerAddressInUse,
    ControlStoreUnavailable,
    ControlEndpointUnavailable,
    NativeEndpointUnavailable,
    RuntimeUnavailable,
}

impl StartupFailureCode {
    pub fn message(self) -> &'static str {
        match self {
            Self::SettingsUnavailable => {
                "Settings could not be read. Check profile permissions or restore settings.toml from a backup."
            }
            Self::InvalidSettings => {
                "The saved settings are invalid or unsupported. Check the download directory and connection settings with the daemon configuration check."
            }
            Self::SettingsSaveFailed => {
                "The initial settings could not be saved. Check profile permissions and available disk space."
            }
            Self::StateUnavailable => {
                "Saved torrent state could not be opened or read. Check the profile drive, permissions, and available disk space."
            }
            Self::CredentialStoreUnavailable => {
                "Tracker credentials could not be restored. Unlock the operating system credential store and retry with the same user account."
            }
            Self::StorageRecoveryFailed => {
                "The owner could not recover saved storage state. Reconnect the data drive and inspect any pending storage move before retrying."
            }
            Self::InvalidRestoredPath => {
                "A saved torrent has a relative download directory. Correct it to an absolute path before starting the background client."
            }
            Self::TransferEngineUnavailable => {
                "The transfer engine could not start. Check the download directory and connection settings, then retry."
            }
            Self::ListenerAddressInUse => {
                "A listening address is in use. Choose a different listening port or close the program using that address."
            }
            Self::ControlStoreUnavailable => {
                "The local control store could not be initialized. Check profile permissions and available disk space."
            }
            Self::ControlEndpointUnavailable => {
                "The local control endpoint could not start. Close other client processes using this profile and retry."
            }
            Self::NativeEndpointUnavailable => {
                "The background client could not open its native connection. Check profile permissions and use the matching GUI and daemon package."
            }
            Self::RuntimeUnavailable => {
                "The background runtime could not start. Close unused applications and retry."
            }
        }
    }

    /// Map an actual SQLite credential conversion error, without parsing text.
    pub(crate) fn from_state_error(error: &rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::FromSqlConversionFailure(_, _, cause)
            | rusqlite::Error::ToSqlConversionFailure(cause)
                if cause.is::<crate::secrets::SecretUnavailable>() =>
            {
                Self::CredentialStoreUnavailable
            }
            _ => Self::StateUnavailable,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupFailure {
    schema: u8,
    pub code: StartupFailureCode,
    pub recorded_unix_ms: u64,
    pub native_protocol: u32,
    attempt: String,
}

/// The diagnostic writer and the engine share this exact profile lock.
pub struct StartupRecovery {
    directory: PathBuf,
    _lock: InstanceLock,
}

impl StartupRecovery {
    pub fn acquire(directory: &Path) -> io::Result<Self> {
        if !directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Profile directory must be absolute",
            ));
        }
        let lock = InstanceLock::acquire(directory)?;
        Ok(Self {
            directory: directory.canonicalize()?,
            _lock: lock,
        })
    }

    pub fn record_failure(&self, code: StartupFailureCode) -> io::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?;
        let failure = StartupFailure {
            schema: SCHEMA,
            code,
            recorded_unix_ms: u64::try_from(timestamp.as_millis()).map_err(io::Error::other)?,
            native_protocol: PROTOCOL_VERSION,
            attempt: random_id()?,
        };
        let path = self.directory.join(FILE_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || std_link(&metadata) => {
                return Err(invalid_record());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut temporary, &failure).map_err(io::Error::other)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        #[cfg(unix)]
        fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub fn clear_failure(&self) -> io::Result<()> {
        let path = self.directory.join(FILE_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !std_link(&metadata) => {
                fs::remove_file(path)?;
                #[cfg(unix)]
                fs::File::open(&self.directory)?.sync_all()?;
                Ok(())
            }
            Ok(_) => Err(invalid_record()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Read only the last completed startup failure. Compare records before and
/// after spawning to identify fresh evidence. Callers check native_protocol
/// before attaching it to the current launch attempt.
pub fn read_failure(directory: &Path) -> io::Result<Option<StartupFailure>> {
    let directory = match Dir::open_ambient_dir(directory, cap_std::ambient_authority()) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match directory.symlink_metadata(FILE_NAME) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(target_os = "linux")]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(0o4000); // Linux O_NONBLOCK, including a replaced FIFO.
    }
    let file = match directory.open_with(FILE_NAME, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    validate_file(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid_record());
    }
    let failure: StartupFailure = serde_json::from_slice(&bytes).map_err(|_| invalid_record())?;
    if failure.schema != SCHEMA
        || failure.native_protocol == 0
        || failure.recorded_unix_ms == 0
        || failure.attempt.len() != 64
        || !failure
            .attempt
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_record());
    }
    Ok(Some(failure))
}

fn validate_file(metadata: &Metadata) -> io::Result<()> {
    if !metadata.is_file() || metadata.len() > MAX_BYTES || metadata.file_type().is_symlink() {
        return Err(invalid_record());
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(invalid_record());
        }
    }
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_record());
        }
    }
    Ok(())
}

fn std_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn invalid_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid startup recovery record",
    )
}

#[cfg(test)]
mod tests;
