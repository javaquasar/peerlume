//! One replaceable background path hydration request. Never opens a writable DB.
use super::deferred_files::{FILE_PAGE_ROWS, read_page};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct FileDetailReply {
    pub id: String,
    pub generation: u64,
    pub result: Result<Vec<String>, String>,
}
type Wake = Arc<dyn Fn() + Send + Sync>;
struct Request {
    sequence: u64,
    id: String,
    generation: u64,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
struct State {
    current: u64,
    active: Option<u64>,
    cancel: Option<Arc<AtomicBool>>,
    pending: Option<Request>,
    reply: Option<FileDetailReply>,
    stopped: bool,
    wake: Option<Wake>,
    startup_error: Option<String>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}
pub struct FileDetailLoader {
    shared: Arc<Shared>,
    _worker: Option<thread::JoinHandle<()>>,
}
impl FileDetailLoader {
    pub fn new(database: PathBuf) -> Self {
        let shared = Arc::new(Shared::default());
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("peerlume-file-details".into())
            .spawn(move || {
                loop {
                    let request = {
                        let mut state = worker_shared
                            .state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        while state.pending.is_none() && !state.stopped {
                            state = worker_shared
                                .ready
                                .wait(state)
                                .unwrap_or_else(|error| error.into_inner());
                        }
                        if state.stopped {
                            break;
                        }
                        let Some(request) = state.pending.take() else {
                            continue;
                        };
                        state.active = Some(request.sequence);
                        request
                    };
                    let result = load_paths(&database, &request.id, &request.cancel);
                    let wake = {
                        let mut state = worker_shared
                            .state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        state.active = None;
                        if state.current == request.sequence
                            && !request.cancel.load(Ordering::Relaxed)
                            && !state.stopped
                        {
                            state.reply = Some(FileDetailReply {
                                id: request.id,
                                generation: request.generation,
                                result,
                            });
                            state.wake.clone()
                        } else {
                            None
                        }
                    };
                    if let Some(wake) = wake {
                        wake();
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .startup_error = Some(error.to_string());
                None
            }
        };
        Self {
            shared,
            _worker: worker,
        }
    }
    pub fn request(&self, id: String, generation: u64) -> u64 {
        let (sequence, wake) = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(cancel) = state.cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            state.current = state.current.wrapping_add(1);
            let sequence = state.current;
            let cancel = Arc::new(AtomicBool::new(false));
            state.cancel = Some(cancel.clone());
            state.reply = None;
            let wake = if let Some(error) = state.startup_error.clone() {
                state.reply = Some(FileDetailReply {
                    id,
                    generation,
                    result: Err(format!("Cannot start file-detail worker: {error}")),
                });
                state.wake.clone()
            } else {
                state.pending = Some(Request {
                    sequence,
                    id,
                    generation,
                    cancel,
                });
                None
            };
            (sequence, wake)
        };
        self.shared.ready.notify_one();
        if let Some(wake) = wake {
            wake();
        }
        sequence
    }
    pub fn poll(&self) -> Option<FileDetailReply> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reply
            .take()
    }
    pub fn is_loading(&self) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.pending.is_some() || state.active == Some(state.current)
    }
    pub fn cancel(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(cancel) = state.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        state.current = state.current.wrapping_add(1);
        state.pending = None;
        state.reply = None;
    }
    pub fn set_waker(&self, wake: Wake) {
        let ready = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.wake = Some(wake.clone());
            state.reply.is_some()
        };
        if ready {
            wake();
        }
    }
}
impl Drop for FileDetailLoader {
    fn drop(&mut self) {
        self.cancel();
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopped = true;
        self.shared.ready.notify_one();
        // A native database open/read may be blocked. Never join it on the UI/owner.
    }
}

fn load_paths(database: &Path, id: &str, cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let cancelled = || {
        if cancel.load(Ordering::Relaxed) {
            Err("File detail request cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    cancelled()?;
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|error| error.to_string())?;
    let mut paths = Vec::new();
    let mut offset = 0;
    let mut bytes = 0usize;
    let mut expected_total = None;
    loop {
        cancelled()?;
        let page = read_page(&connection, id, offset, FILE_PAGE_ROWS)
            .map_err(|error| error.to_string())?;
        if expected_total.is_some_and(|total| total != page.total) {
            return Err("File detail structure changed; reload the authoritative snapshot".into());
        }
        expected_total = Some(page.total);
        for file in page.files {
            bytes = bytes
                .checked_add(file.path.len())
                .ok_or("File detail memory budget exceeded")?;
            if bytes > 64 * 1024 * 1024 {
                return Err("File detail memory budget exceeded; use bounded file pages".into());
            }
            paths.push(file.path);
        }
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    cancelled()?;
    crate::limits::validate_relative_paths(
        paths.iter().map(String::as_str),
        &crate::limits::ResourceLimits::default(),
    )?;
    cancelled()?;
    Ok(paths)
}
