//! One bounded metainfo worker per frontend. Loading never adds a torrent or
//! waits for filesystem I/O while a controller owner is locked.
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
};

use crate::{limits::ResourceLimits, metainfo::TorrentMetainfo};

type Reader = dyn Fn(&Path, &AtomicBool) -> Result<TorrentMetainfo, String> + Send + Sync;

#[derive(Clone)]
struct Request {
    id: u64,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
}

pub struct LoadedMetainfo {
    pub path: PathBuf,
    pub result: Result<TorrentMetainfo, String>,
}

struct Completed {
    id: u64,
    value: LoadedMetainfo,
}

struct Worker {
    requests: SyncSender<Request>,
    results: Receiver<Completed>,
}

#[derive(Default)]
pub struct MetainfoLoader {
    worker: Option<Worker>,
    active: Option<Request>,
    pending: Option<Request>,
    next_id: u64,
    #[cfg(test)]
    reader: Option<Arc<Reader>>,
}

impl MetainfoLoader {
    pub fn is_loading(&self) -> bool {
        self.active.is_some()
    }

    pub fn active_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|request| request.path.as_path())
    }

    pub fn cancel(&mut self) {
        for request in [self.active.take(), self.pending.take()]
            .into_iter()
            .flatten()
        {
            request.cancel.store(true, Ordering::Release);
        }
    }

    /// Replace the wanted import, retaining at most one running read and one
    /// queued request. Cancelled/stale results cannot open a review dialog.
    pub fn start(&mut self, path: PathBuf) -> Result<(), String> {
        self.ensure_worker()?;
        self.cancel();
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Import generation exhausted")?;
        let request = Request {
            id: self.next_id,
            path,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.active = Some(request.clone());
        match self.worker.as_ref().unwrap().requests.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => self.pending = Some(request),
            Err(TrySendError::Disconnected(_)) => {
                self.active = None;
                return Err("Metainfo worker is unavailable".into());
            }
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<LoadedMetainfo> {
        let worker = self.worker.as_ref()?;
        if let Some(request) = self.pending.take() {
            match worker.requests.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => self.pending = Some(request),
                Err(TrySendError::Disconnected(request)) => {
                    if self
                        .active
                        .as_ref()
                        .is_some_and(|active| active.id == request.id)
                    {
                        self.active = None;
                        return Some(LoadedMetainfo {
                            path: request.path,
                            result: Err("Metainfo worker is unavailable".into()),
                        });
                    }
                }
            }
        }
        // The result channel holds one item; a bounded drain also tolerates the
        // worker handing off a stale item concurrently with this poll.
        for _ in 0..2 {
            let completed = match worker.results.try_recv() {
                Ok(completed) => completed,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return self.active.take().map(|request| LoadedMetainfo {
                        path: request.path,
                        result: Err("Metainfo worker stopped unexpectedly".into()),
                    });
                }
            };
            if self.active.as_ref().is_some_and(|active| {
                active.id == completed.id && !active.cancel.load(Ordering::Acquire)
            }) {
                self.active = None;
                return Some(completed.value);
            }
        }
        None
    }

    fn ensure_worker(&mut self) -> Result<(), String> {
        if self.worker.is_some() {
            return Ok(());
        }
        let (requests, input) = mpsc::sync_channel::<Request>(1);
        let (output, results) = mpsc::sync_channel(1);
        #[cfg(test)]
        let reader = self
            .reader
            .clone()
            .unwrap_or_else(|| Arc::new(read_metainfo));
        #[cfg(not(test))]
        let reader: Arc<Reader> = Arc::new(read_metainfo);
        std::thread::Builder::new()
            .name("peerlume-metainfo".into())
            .spawn(move || {
                while let Ok(request) = input.recv() {
                    if request.cancel.load(Ordering::Acquire) {
                        continue;
                    }
                    let result = reader(&request.path, &request.cancel);
                    if request.cancel.load(Ordering::Acquire) {
                        continue;
                    }
                    if output
                        .send(Completed {
                            id: request.id,
                            value: LoadedMetainfo {
                                path: request.path,
                                result,
                            },
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Could not start metainfo worker: {error}"))?;
        self.worker = Some(Worker { requests, results });
        Ok(())
    }
}

impl Drop for MetainfoLoader {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn read_metainfo(path: &Path, cancel: &AtomicBool) -> Result<TorrentMetainfo, String> {
    let limits = ResourceLimits::default();
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Torrent input must be a regular file".into());
    }
    if metadata.len() > limits.max_torrent_bytes {
        return Err("Torrent exceeds configured input limit".into());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 32 * 1024];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err("Import cancelled".into());
        }
        let count = file.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) as u64 > limits.max_torrent_bytes {
            return Err("Torrent exceeds configured input limit".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if cancel.load(Ordering::Acquire) {
        return Err("Import cancelled".into());
    }
    TorrentMetainfo::parse_with_limits(&bytes, &limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(loader: &mut MetainfoLoader) -> LoadedMetainfo {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = loader.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "bounded worker did not complete");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn blocked_import_can_be_cancelled_and_replaced_without_waiting_or_stale_review() {
        let (started, observed) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let gate = std::sync::Mutex::new(gate);
        let mut loader = MetainfoLoader {
            worker: None,
            active: None,
            pending: None,
            next_id: 0,
            reader: Some(Arc::new(move |path, _| {
                if path == Path::new("slow.torrent") {
                    started.send(()).unwrap();
                    gate.lock().unwrap().recv().unwrap();
                }
                Err(format!("{} failed", path.display()))
            })),
        };
        loader.start("slow.torrent".into()).unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        for index in 0..100 {
            loader
                .start(format!("replacement-{index}.torrent").into())
                .unwrap();
        }
        assert_eq!(
            loader.active_path(),
            Some(Path::new("replacement-99.torrent"))
        );
        assert!(loader.poll().is_none());
        release.send(()).unwrap();
        let value = wait(&mut loader);
        assert_eq!(value.path, Path::new("replacement-99.torrent"));
        assert_eq!(value.result.unwrap_err(), "replacement-99.torrent failed");
        assert!(!loader.is_loading());
    }

    #[test]
    fn cancellation_discards_completed_results_and_a_later_import_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.torrent");
        std::fs::write(&path, b"d4:infod6:lengthi1e4:name1:xee").unwrap();
        let mut loader = MetainfoLoader::default();
        loader.start(path.clone()).unwrap();
        loader.cancel();
        assert!(loader.poll().is_none());
        loader.start(path.clone()).unwrap();
        let value = wait(&mut loader);
        assert_eq!(value.path, path);
        assert_eq!(value.result.unwrap().name, "x");
    }
}
