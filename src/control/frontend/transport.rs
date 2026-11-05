use super::{
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
    wire::{Error, Request, Response},
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub protocol: u32,
    pub instance_id: String,
    token: Vec<u8>,
}

pub fn endpoint(directory: &Path) -> io::Result<String> {
    let path = directory.canonicalize()?;
    #[cfg(windows)]
    return Ok(format!(
        r"\\.\pipe\peerlume-native-{}",
        &crate::control::access::digest(path.to_string_lossy().to_lowercase().as_bytes())[..32]
    ));
    #[cfg(unix)]
    return Ok(path.join("frontend.sock").to_string_lossy().into_owned());
}

pub fn write_credentials(directory: &Path, instance_id: String, token: &str) -> io::Result<()> {
    let path = directory.join("frontend-auth.json");
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(io::Error::other(
            "Native authentication path is not a regular file",
        ));
    }
    #[cfg(windows)]
    let protected = {
        use crate::secrets::TrackerSecrets;
        crate::secrets::PlatformSecrets
            .protect(token)
            .map_err(io::Error::other)?
    };
    #[cfg(unix)]
    let protected = token.as_bytes().to_vec();
    let value = Credentials {
        protocol: PROTOCOL_VERSION,
        instance_id,
        token: protected,
    };
    let temporary = directory.join(format!(
        ".frontend-auth-{}.tmp",
        crate::control::access::random_id()?
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary)?;
    use std::io::Write;
    output.write_all(&serde_json::to_vec(&value).map_err(io::Error::other)?)?;
    output.sync_all()?;
    drop(output);
    // The profile owner holds InstanceLock. A stale descriptor may be replaced
    // only here, never by a frontend attempting to connect to a legacy owner.
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(temporary, path)
}

pub fn read_credentials(directory: &Path) -> Result<(String, String), String> {
    let path = directory.join("frontend-auth.json");
    let meta =
        std::fs::symlink_metadata(&path).map_err(|_| "Native owner is not ready".to_owned())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
        return Err("Invalid native owner authentication descriptor".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Native owner authentication descriptor is not private".into());
        }
    }
    let value: Credentials = serde_json::from_slice(
        &std::fs::read(path).map_err(|_| "Cannot read native owner authentication descriptor")?,
    )
    .map_err(|_| "Invalid native owner authentication descriptor")?;
    if value.protocol != PROTOCOL_VERSION {
        return Err("The profile owner uses an incompatible native protocol".into());
    }
    #[cfg(windows)]
    let token = {
        use crate::secrets::TrackerSecrets;
        crate::secrets::PlatformSecrets
            .restore(&value.token)
            .map_err(|_| "Cannot unlock native owner authentication")?
    };
    #[cfg(unix)]
    let token =
        String::from_utf8(value.token).map_err(|_| "Invalid native owner authentication")?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid native owner authentication".into());
    }
    Ok((value.instance_id, token))
}

pub async fn read<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| Error::Failed("Native connection closed".into()))?;
    if count > MAX_FRAME_BYTES {
        return Err(Error::TooLarge);
    }
    if bytes.pop() != Some(b'\n') {
        return Err(Error::Failed("Native frame is incomplete".into()));
    }
    Ok(bytes)
}

pub async fn call(directory: &Path, request: &Request) -> Result<Response, Error> {
    let address =
        endpoint(directory).map_err(|_| Error::Failed("Background owner is unavailable".into()))?;
    let mut bytes = serde_json::to_vec(request).map_err(|_| Error::TooLarge)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::TooLarge);
    }
    #[cfg(windows)]
    let mut stream = {
        let mut result = Err(Error::Failed("Background owner is unavailable".into()));
        for _ in 0..20 {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&address) {
                Ok(stream) => {
                    result = Ok(stream);
                    break;
                }
                Err(error) if error.raw_os_error() == Some(231) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                Err(_) => break,
            }
        }
        result?
    };
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(&address)
        .await
        .map_err(|_| Error::Failed("Background owner is unavailable".into()))?;
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| Error::Failed("Native request could not be transmitted".into()))?;
        let response: Response = serde_json::from_slice(&read(&mut BufReader::new(stream)).await?)
            .map_err(|_| Error::Incompatible)?;
        if response.protocol != PROTOCOL_VERSION {
            return Err(Error::Incompatible);
        }
        if response.request_id != request.request_id
            || request
                .instance_id
                .as_ref()
                .is_some_and(|id| *id != response.instance_id)
        {
            return Err(Error::Stale);
        }
        Ok(response)
    })
    .await
    .map_err(|_| Error::Failed("Native request timed out; refresh before retrying".into()))??;
    Ok(response)
}

pub fn call_blocking(directory: PathBuf, request: Request) -> Result<Response, Error> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    if ACTIVE
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < super::MAX_CONNECTIONS).then_some(active + 1)
        })
        .is_err()
    {
        return Err(Error::Busy);
    }
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
    let _permit = Permit;
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Busy)?
            .block_on(call(&directory, &request))
    })
    .join()
    .map_err(|_| Error::Failed("Native request worker stopped".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_oversize_and_incomplete_input_without_unbounded_reads() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bytes = vec![b'x'; MAX_FRAME_BYTES + 100];
            let mut reader = BufReader::new(bytes.as_slice());
            assert!(matches!(read(&mut reader).await, Err(Error::TooLarge)));
            let mut reader = BufReader::new(b"unfinished".as_slice());
            assert!(read(&mut reader).await.is_err());
            let mut reader = BufReader::new(b"{}\nignored\n".as_slice());
            assert_eq!(read(&mut reader).await.unwrap(), b"{}");
        });
    }
}
