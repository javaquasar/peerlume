//! Local-only IPC and independent owner pumping. No transfer runtime in adapters.
#[cfg(windows)]
use super::access::digest;
use super::{
    Controller,
    wire::{self, ErrorCode, Request, Response},
};
use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot},
};

pub struct InstanceLock(File);
impl InstanceLock {
    /// Acquire before starting ANY transfer engine in this profile.
    pub fn acquire(directory: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("instance.lock"))?;
        file.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Another instance owns this profile",
            )
        })?;
        Ok(Self(file))
    }
}
impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn endpoint(directory: &Path) -> io::Result<String> {
    #[cfg(windows)]
    {
        let canonical = directory.canonicalize()?;
        Ok(format!(
            r"\\.\pipe\peerlume-{}",
            &digest(canonical.to_string_lossy().to_lowercase().as_bytes())[..32]
        ))
    }
    #[cfg(unix)]
    {
        Ok(directory
            .canonicalize()?
            .join("control.sock")
            .to_string_lossy()
            .into_owned())
    }
}

struct Envelope {
    request: Request,
    reply: oneshot::Sender<Response>,
}
pub struct Host {
    stop: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    pump_count: Arc<std::sync::atomic::AtomicU64>,
    thread: Option<thread::JoinHandle<()>>,
    pub error: Arc<Mutex<Option<ErrorCode>>>,
}

impl Host {
    pub fn start<T: Send + 'static>(
        owner: Arc<Mutex<T>>,
        controller: fn(&mut T) -> &mut Controller,
        directory: PathBuf,
    ) -> io::Result<Self> {
        let address = endpoint(&directory)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (enabled, enabled_changes) = tokio::sync::watch::channel(false);
        let error = Arc::new(Mutex::new(None));
        let worker_error = error.clone();
        let wake = {
            let mut owner = owner
                .lock()
                .map_err(|_| io::Error::other("Owner lock poisoned"))?;
            controller(&mut owner).owner_wake()
        };
        let worker_wake = wake.clone();
        #[cfg(test)]
        let pump_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let worker_pump_count = pump_count.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread = thread::Builder::new()
            .name("peerlume-control".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let (tx, mut rx) = mpsc::channel::<Envelope>(64);
                    let listener = tokio::spawn(listen(address, tx, enabled_changes, worker_error));
                    let mut incoming = None;
                    while !worker_stop.load(Ordering::Relaxed) {
                        // Register before draining. A producer at the boundary
                        // either completes this future or leaves a Notify permit.
                        let notified = worker_wake.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        let mut delay = Duration::from_millis(10);
                        let mut owner_available = false;
                        if let Ok(mut owner) = owner.try_lock() {
                            owner_available = true;
                            let control = controller(&mut owner);
                            #[cfg(test)]
                            worker_pump_count.fetch_add(1, Ordering::Relaxed);
                            control.tick();
                            let next_enabled = control.agent.as_ref().is_some_and(|a| a.enabled);
                            enabled.send_if_modified(|current| {
                                let changed = *current != next_enabled;
                                *current = next_enabled;
                                changed
                            });
                            for _ in 0..16 {
                                let Some(envelope) = incoming.take().or_else(|| rx.try_recv().ok())
                                else {
                                    break;
                                };
                                if !envelope.reply.is_closed() {
                                    let _ = envelope
                                        .reply
                                        .send(control.handle_request(envelope.request));
                                }
                            }
                            delay = control.next_wake_delay();
                        }
                        if owner_available && !rx.is_empty() {
                            continue;
                        }
                        tokio::select! {
                            _ = &mut notified => {},
                            envelope = rx.recv(), if incoming.is_none() => { incoming = envelope; },
                            _ = tokio::time::sleep(delay) => {},
                        }
                    }
                    listener.abort();
                    let _ = listener.await;
                })
            })?;
        Ok(Self {
            stop,
            wake,
            #[cfg(test)]
            pump_count,
            thread: Some(thread),
            error,
        })
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin>(stream: S, tx: mpsc::Sender<Envelope>) {
    let mut stream = BufReader::new(stream);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        read_frame(&mut stream, wire::MAX_REQUEST_BYTES),
    )
    .await;
    let response = match result {
        Ok(Ok(bytes)) => match wire::parse_request(&bytes) {
            Ok(request) => {
                let (reply, answer) = oneshot::channel();
                match tx.try_send(Envelope { request, reply }) {
                    Ok(()) => match tokio::time::timeout(Duration::from_secs(5), answer).await {
                        Ok(Ok(response)) => response,
                        _ => Response::error(ErrorCode::Busy),
                    },
                    Err(_) => Response::error(ErrorCode::Busy),
                }
            }
            Err(error) => Response::error(error),
        },
        Ok(Err(error)) => Response::error(error),
        _ => return,
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        stream
            .get_mut()
            .write_all(&wire::encode_response(&response)),
    )
    .await;
}

pub async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Vec<u8>, ErrorCode> {
    let mut bytes = Vec::new();
    let count = reader
        .take(max as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| ErrorCode::InvalidRequest)?;
    if count > max {
        return Err(ErrorCode::TooLarge);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(ErrorCode::InvalidRequest);
    }
    bytes.pop();
    Ok(bytes)
}

#[cfg(windows)]
async fn listen(
    address: String,
    tx: mpsc::Sender<Envelope>,
    mut enabled: tokio::sync::watch::Receiver<bool>,
    error: Arc<Mutex<Option<ErrorCode>>>,
) {
    let permits = Arc::new(tokio::sync::Semaphore::new(wire::MAX_CONNECTIONS));
    let mut listening = None;
    loop {
        if !*enabled.borrow() {
            listening = None;
            if enabled.changed().await.is_err() {
                return;
            }
            continue;
        }
        let pipe = match listening
            .take()
            .map(Ok)
            .unwrap_or_else(|| windows_pipe(&address, true))
        {
            Ok(pipe) => pipe,
            Err(_) => {
                *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(ErrorCode::Busy);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        *error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore open");
        loop {
            tokio::select! {
                result = pipe.connect() => match result {
                Ok(()) => {
                    // Keep at least one pipe instance alive continuously. The first
                    // creation is exclusive, so a pre-existing endpoint is never trusted.
                    match windows_pipe(&address, false) {
                        Ok(next) => listening = Some(next),
                        Err(_) => {
                            *error.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(ErrorCode::Busy);
                            break;
                        }
                    }
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        connection(pipe, tx).await;
                    });
                    break;
                }
                Err(_) => break,
                },
                changed = enabled.changed() => {
                    if changed.is_err() { return; }
                    if !*enabled.borrow() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn windows_pipe(
    address: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = std::ptr::null_mut();
    // SAFETY: each API receives correctly sized/aligned buffers. Allocations and
    // handles are released before return; Tokio copies security attributes at create.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid_text})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                address,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            );
        LocalFree(descriptor);
        result
    }
}

#[cfg(all(test, windows))]
#[test]
fn windows_endpoint_rejects_an_existing_server_before_accepting_credentials() {
    let root = tempfile::tempdir().unwrap();
    let address = endpoint(root.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let first = windows_pipe(&address, true).unwrap();
        assert!(windows_pipe(&address, true).is_err());
        let additional = windows_pipe(&address, false).unwrap();
        drop(additional);
        drop(first);
    });
}

#[cfg(unix)]
async fn listen(
    address: String,
    tx: mpsc::Sender<Envelope>,
    mut enabled: tokio::sync::watch::Receiver<bool>,
    error: Arc<Mutex<Option<ErrorCode>>>,
) {
    use std::os::unix::fs::PermissionsExt;
    let permits = Arc::new(tokio::sync::Semaphore::new(wire::MAX_CONNECTIONS));
    loop {
        if !*enabled.borrow() {
            if enabled.changed().await.is_err() {
                return;
            }
            continue;
        }
        // InstanceLock is held by the owner; only this fixed stale socket is removed.
        if let Ok(meta) = std::fs::symlink_metadata(&address) {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(ErrorCode::PathDenied);
                return;
            }
            let _ = std::fs::remove_file(&address);
        }
        let listener = match tokio::net::UnixListener::bind(&address) {
            Ok(listener) => listener,
            Err(_) => {
                *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(ErrorCode::Busy);
                return;
            }
        };
        if std::fs::set_permissions(&address, std::fs::Permissions::from_mode(0o600)).is_err() {
            return;
        }
        while *enabled.borrow() {
            let permit = permits
                .clone()
                .acquire_owned()
                .await
                .expect("open semaphore");
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    connection(stream, tx).await;
                });
                },
                changed = enabled.changed() => {
                    if changed.is_err() { return; }
                },
            }
        }
        drop(listener);
        let _ = std::fs::remove_file(&address);
    }
}

pub async fn call(directory: &Path, request: &Request) -> Result<Response, ErrorCode> {
    let address = endpoint(directory).map_err(|_| ErrorCode::NotRunning)?;
    let mut bytes = serde_json::to_vec(request).map_err(|_| ErrorCode::InvalidRequest)?;
    if bytes.len() + 1 > wire::MAX_REQUEST_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    bytes.push(b'\n');
    #[cfg(windows)]
    let mut stream = {
        let mut result = Err(ErrorCode::NotRunning);
        for _ in 0..50 {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&address) {
                Ok(stream) => {
                    result = Ok(stream);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(231) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(_) => break,
            }
        }
        result?
    };
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(&address)
        .await
        .map_err(|_| ErrorCode::NotRunning)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| ErrorCode::NotRunning)?;
        let data = read_frame(&mut BufReader::new(stream), wire::MAX_RESPONSE_BYTES + 1).await?;
        let response: Response =
            serde_json::from_slice(&data).map_err(|_| ErrorCode::InvalidRequest)?;
        if response.protocol_version != super::PROTOCOL_VERSION {
            return Err(ErrorCode::IncompatibleProtocol);
        }
        if response.error.is_none()
            && (response.request_id != request.request_id
                || request
                    .instance_id
                    .as_ref()
                    .is_some_and(|id| *id != response.instance_id))
        {
            return Err(ErrorCode::Conflict);
        }
        Ok(response)
    })
    .await
    .map_err(|_| ErrorCode::ReconciliationRequired)?
}

#[cfg(test)]
mod tests;
