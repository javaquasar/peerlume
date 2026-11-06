//! Bounded admission, single disk owner, durable barriers and complete-history segments.
use super::JournalEntry;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) const EXPORT_TOO_LARGE: &str = "Disk journal exceeds export size limit";
const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEGMENTS: usize = 16_384;

#[derive(Clone, Debug)]
pub struct JournalOptions {
    pub segment_bytes: u64,
    pub queue_capacity: usize,
    pub queue_bytes: usize,
    pub barrier_timeout: Duration,
    #[cfg(test)]
    pub(super) gate: Option<Arc<TestGate>>,
    #[cfg(test)]
    pub(super) fail_sequence: Option<u64>,
}
impl Default for JournalOptions {
    fn default() -> Self {
        Self {
            segment_bytes: 32 * 1024 * 1024,
            queue_capacity: 2048,
            queue_bytes: 8 * 1024 * 1024,
            barrier_timeout: Duration::from_secs(5),
            #[cfg(test)]
            gate: None,
            #[cfg(test)]
            fail_sequence: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JournalStatus {
    pub accepted_sequence: u64,
    pub written_sequence: u64,
    pub flushed_sequence: u64,
    pub durable_sequence: u64,
    pub queued_entries: usize,
    pub queued_bytes: usize,
    pub rejected_records: u64,
    pub segments: usize,
    pub closed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Announce,
    Audit,
}
impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Announce => "announces",
            Self::Audit => "audit",
        }
    }
}

pub(crate) trait JsonRecord: Send + Sync {
    fn json(&self, output: &mut dyn Write) -> io::Result<()>;
}
impl<T: Serialize + Send + Sync> JsonRecord for T {
    fn json(&self, output: &mut dyn Write) -> io::Result<()> {
        serde_json::to_writer(output, self).map_err(io::Error::other)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Segment {
    file_name: String,
    first_sequence: u64,
    last_sequence: u64,
    length: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    kind: Kind,
    run_id: String,
    segments: Vec<Segment>,
    clean_shutdown: bool,
    incomplete: bool,
    error: Option<String>,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    directory: PathBuf,
    manifest: Manifest,
    sealed: bool,
}
impl Snapshot {
    pub(crate) fn length(&self) -> Result<u64, String> {
        self.manifest
            .segments
            .iter()
            .try_fold(0u64, |total, segment| {
                total
                    .checked_add(segment.length)
                    .ok_or_else(|| "Journal size overflow".into())
            })
    }
    pub(crate) fn first_path(&self) -> PathBuf {
        self.directory.join(&self.manifest.segments[0].file_name)
    }
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.directory.join(format!(
            "{}-{}.manifest.json",
            self.manifest.kind.prefix(),
            self.manifest.run_id
        ))
    }
    pub(crate) fn run_id(&self) -> &str {
        &self.manifest.run_id
    }
    pub(crate) fn status(&self) -> JournalStatus {
        let last = self
            .manifest
            .segments
            .last()
            .map_or(0, |segment| segment.last_sequence);
        JournalStatus {
            accepted_sequence: last,
            written_sequence: last,
            flushed_sequence: last,
            durable_sequence: last,
            segments: self.manifest.segments.len(),
            closed: self.sealed,
            ..Default::default()
        }
    }
    pub(crate) fn reopen(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|_| "Journal manifest is unavailable")?;
        if file
            .metadata()
            .map_err(|_| "Cannot inspect journal manifest")?
            .len()
            > MAX_MANIFEST_BYTES
        {
            return Err("Journal manifest exceeds its size limit".into());
        }
        let manifest: Manifest =
            serde_json::from_reader(file).map_err(|_| "Journal manifest is invalid")?;
        if manifest.schema_version != 1
            || manifest.kind != Kind::Announce
            || manifest.run_id.is_empty()
            || manifest.run_id.len() > 128
            || !manifest
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || manifest.segments.is_empty()
            || manifest.segments.len() > MAX_SEGMENTS
        {
            return Err("Journal manifest identity or bounds are invalid".into());
        }
        if !manifest.clean_shutdown || manifest.incomplete || manifest.error.is_some() {
            return Err(
                "Previous journal did not close completely; full history cannot be verified".into(),
            );
        }
        let mut expected = 1u64;
        for (index, segment) in manifest.segments.iter().enumerate() {
            let name = segment_name(manifest.kind, &manifest.run_id, index);
            if segment.file_name != name
                || segment.first_sequence != expected
                || segment.last_sequence.saturating_add(1) < expected
            {
                return Err("Journal manifest segment order is invalid".into());
            }
            expected = segment.last_sequence.saturating_add(1);
        }
        Ok(Self {
            directory: path
                .parent()
                .ok_or("Journal manifest has no directory")?
                .to_path_buf(),
            manifest,
            sealed: true,
        })
    }
    pub(crate) fn stream(
        &self,
        output: &mut dyn Write,
        hash: Option<&str>,
        mut observed: impl FnMut(JournalEntry),
    ) -> Result<u64, String> {
        let mut expected = 1u64;
        let mut count = 0;
        for (index, segment) in self.manifest.segments.iter().enumerate() {
            let path = self.directory.join(&segment.file_name);
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| "Disk journal is unavailable")?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() < segment.length
                || ((self.sealed || index + 1 < self.manifest.segments.len())
                    && metadata.len() != segment.length)
            {
                return Err("Journal segment is missing, changed or incomplete".into());
            }
            let file = File::open(path).map_err(|_| "Disk journal is unavailable")?;
            let mut reader = BufReader::new(file.take(segment.length));
            loop {
                let mut line = Vec::new();
                let read = reader
                    .by_ref()
                    .take(MAX_RECORD_BYTES as u64 + 1)
                    .read_until(b'\n', &mut line)
                    .map_err(|_| "Cannot read journal segment")?;
                if read == 0 {
                    break;
                }
                if read > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
                    return Err("Journal record is oversized or interrupted".into());
                }
                if self.manifest.kind == Kind::Announce {
                    let entry: JournalEntry =
                        serde_json::from_slice(&line).map_err(|_| "Journal record is invalid")?;
                    if entry.schema_version != 1
                        || entry.run_id != self.manifest.run_id
                        || entry.sequence != expected
                    {
                        return Err("Journal sequence or run identity is invalid".into());
                    }
                    if hash.is_none_or(|hash| entry.announce.info_hash == hash) {
                        output.write_all(&line).map_err(|error| error.to_string())?;
                        count += 1;
                    }
                    observed(entry);
                } else {
                    serde_json::from_slice::<serde_json::Value>(&line)
                        .map_err(|_| "Audit journal record is invalid")?;
                    output.write_all(&line).map_err(|error| error.to_string())?;
                    count += 1;
                }
                expected = expected.checked_add(1).ok_or("Journal sequence overflow")?;
            }
            if expected != segment.last_sequence.saturating_add(1) {
                return Err("Journal segment record count is incomplete".into());
            }
        }
        Ok(count)
    }
    pub(crate) fn file(&self, max_bytes: u64) -> Result<(File, u64), String> {
        if self.length()? > max_bytes {
            return Err(EXPORT_TOO_LARGE.into());
        }
        let mut output = tempfile::tempfile().map_err(|_| "Cannot create journal snapshot")?;
        self.stream(&mut output, None, |_| {})?;
        let length = output
            .stream_position()
            .map_err(|_| "Cannot inspect journal snapshot")?;
        output
            .rewind()
            .map_err(|_| "Cannot rewind journal snapshot")?;
        Ok((output, length))
    }
}

enum Command {
    Record {
        sequence: u64,
        value: Arc<dyn JsonRecord>,
        bytes: usize,
    },
    Fence {
        limit: Option<u64>,
        reply: mpsc::SyncSender<Result<Snapshot, String>>,
    },
}
struct Admission {
    sender: Option<mpsc::SyncSender<Command>>,
    accepted: u64,
}
#[derive(Default)]
struct State {
    accepted: AtomicU64,
    written: AtomicU64,
    flushed: AtomicU64,
    durable: AtomicU64,
    entries: AtomicUsize,
    bytes: AtomicUsize,
    rejected: AtomicU64,
    segments: AtomicUsize,
    closing: AtomicBool,
    error: Mutex<Option<String>>,
    done: Mutex<bool>,
    completed: Condvar,
}
impl State {
    fn fail(&self, message: String) {
        let mut error = self.error.lock().unwrap_or_else(|error| error.into_inner());
        if error.is_none() {
            *error = Some(message);
        }
    }
    fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    fn status(&self) -> JournalStatus {
        // Read older watermarks first so concurrent forward progress cannot
        // expose flushed > written or written > accepted in one observation.
        let durable_sequence = self.durable.load(Ordering::Acquire);
        let flushed_sequence = self.flushed.load(Ordering::Acquire);
        let written_sequence = self.written.load(Ordering::Acquire);
        let accepted_sequence = self.accepted.load(Ordering::Acquire);
        JournalStatus {
            accepted_sequence,
            written_sequence,
            flushed_sequence,
            durable_sequence,
            queued_entries: self.entries.load(Ordering::Acquire),
            queued_bytes: self.bytes.load(Ordering::Acquire),
            rejected_records: self.rejected.load(Ordering::Relaxed),
            segments: self.segments.load(Ordering::Relaxed),
            closed: *self.done.lock().unwrap_or_else(|error| error.into_inner()),
            error: self.error(),
        }
    }
}
pub(crate) struct DiskWriter {
    admission: Mutex<Admission>,
    state: Arc<State>,
    options: JournalOptions,
    path: PathBuf,
    manifest: PathBuf,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}
impl DiskWriter {
    pub(crate) fn start(
        directory: &Path,
        run_id: &str,
        kind: Kind,
        options: JournalOptions,
    ) -> Result<Arc<Self>, String> {
        if options.segment_bytes == 0
            || options.segment_bytes > 1024 * 1024 * 1024
            || !(1..=65536).contains(&options.queue_capacity)
            || !(1..=128 * 1024 * 1024).contains(&options.queue_bytes)
            || options.barrier_timeout.is_zero()
            || options.barrier_timeout > Duration::from_secs(30)
        {
            return Err("Journal options are outside supported bounds".into());
        }
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("JSONL logging unavailable: {error}"))?;
        let first = Segment {
            file_name: segment_name(kind, run_id, 0),
            first_sequence: 1,
            last_sequence: 0,
            length: 0,
        };
        let path = directory.join(&first.file_name);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("JSONL logging unavailable: {error}"))?;
        let manifest = Manifest {
            schema_version: 1,
            kind,
            run_id: run_id.into(),
            segments: vec![first],
            clean_shutdown: false,
            incomplete: false,
            error: None,
        };
        let snapshot = Snapshot {
            directory: directory.into(),
            manifest,
            sealed: false,
        };
        persist_manifest(&snapshot)?;
        let manifest_path = snapshot.manifest_path();
        let state = Arc::new(State::default());
        state.segments.store(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(options.queue_capacity);
        let (worker_state, worker_options) = (state.clone(), options.clone());
        let worker = thread::Builder::new()
            .name("peerlume-journal".into())
            .spawn(move || {
                run_writer(
                    receiver,
                    worker_state,
                    worker_options,
                    snapshot,
                    BufWriter::new(file),
                )
            })
            .map_err(|_| "Cannot start journal writer")?;
        Ok(Arc::new(Self {
            admission: Mutex::new(Admission {
                sender: Some(sender),
                accepted: 0,
            }),
            state,
            options,
            path,
            manifest: manifest_path,
            worker: Mutex::new(Some(worker)),
        }))
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.manifest.clone()
    }
    pub(crate) fn status(&self) -> JournalStatus {
        // Admission publishes its reserved sequence before try_send so the
        // worker can progress immediately. Hide a failed reservation/rollback
        // from readers until the nonblocking admission attempt has completed.
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.state.status()
    }
    pub(crate) fn enqueue(
        &self,
        sequence: Option<u64>,
        value: Arc<dyn JsonRecord>,
        bytes: usize,
    ) -> Result<u64, String> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.state.closing.load(Ordering::Acquire) || self.state.error().is_some() {
            self.state.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(self
                .state
                .error()
                .unwrap_or_else(|| "Journal writer is closed".into()));
        }
        let next = admission
            .accepted
            .checked_add(1)
            .ok_or("Journal sequence overflow")?;
        if sequence.is_some_and(|sequence| sequence != next) {
            let message = "Journal admission sequence is not contiguous".to_owned();
            self.state.fail(message.clone());
            return Err(message);
        }
        let pending = self.state.bytes.load(Ordering::Acquire);
        if bytes > self.options.queue_bytes
            || pending.saturating_add(bytes) > self.options.queue_bytes
        {
            let message = format!(
                "Journal admission byte budget exceeded at sequence {next}; disk history is incomplete"
            );
            self.state.fail(message.clone());
            self.state.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(message);
        }
        self.state.entries.fetch_add(1, Ordering::Release);
        self.state.bytes.fetch_add(bytes, Ordering::Release);
        self.state.accepted.store(next, Ordering::Release);
        let result = admission
            .sender
            .as_ref()
            .ok_or("Journal writer is closed")?
            .try_send(Command::Record {
                sequence: next,
                value,
                bytes,
            });
        if result.is_err() {
            self.state.entries.fetch_sub(1, Ordering::Release);
            self.state.bytes.fetch_sub(bytes, Ordering::Release);
            self.state
                .accepted
                .store(admission.accepted, Ordering::Release);
            self.state.rejected.fetch_add(1, Ordering::Relaxed);
            let message = format!(
                "Journal admission queue is full or unavailable at sequence {next}; disk history is incomplete"
            );
            self.state.fail(message.clone());
            return Err(message);
        }
        admission.accepted = next;
        Ok(next)
    }
    pub(crate) fn snapshot(&self, limit: Option<u64>) -> Result<Snapshot, String> {
        self.snapshot_until(limit, Instant::now() + self.options.barrier_timeout)
    }
    pub(super) fn snapshot_until(
        &self,
        limit: Option<u64>,
        deadline: Instant,
    ) -> Result<Snapshot, String> {
        if let Some(error) = self.state.error() {
            return Err(error);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let mut command = Command::Fence { limit, reply };
        loop {
            let admission = self
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(sender) = admission.sender.as_ref() else {
                drop(admission);
                return self.closed_snapshot(limit);
            };
            match sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => command = returned,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err("Journal writer stopped before its barrier".into());
                }
            }
            drop(admission);
            if Instant::now() >= deadline {
                return Err("Journal flush barrier timed out".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Journal flush barrier timed out".to_owned())?
    }
    fn closed_snapshot(&self, limit: Option<u64>) -> Result<Snapshot, String> {
        self.finish()?;
        let snapshot = Snapshot::reopen(&self.manifest)?;
        if limit.is_some_and(|limit| snapshot.length().map_or(true, |length| length > limit)) {
            return Err(EXPORT_TOO_LARGE.into());
        }
        Ok(snapshot)
    }
    pub(crate) fn finish(&self) -> Result<(), String> {
        self.finish_with_timeout(self.options.barrier_timeout)
    }
    pub(crate) fn finish_with_timeout(&self, timeout: Duration) -> Result<(), String> {
        self.state.closing.store(true, Ordering::Release);
        self.admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sender
            .take();
        let done = self
            .state
            .done
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (done, _) = self
            .state
            .completed
            .wait_timeout_while(done, timeout.min(self.options.barrier_timeout), |done| {
                !*done
            })
            .unwrap_or_else(|error| error.into_inner());
        if !*done {
            let message =
                "Journal writer shutdown timed out; completeness is not confirmed".to_owned();
            self.state.fail(message.clone());
            return Err(message);
        }
        drop(done);
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            worker
                .join()
                .map_err(|_| "Journal writer stopped unexpectedly")?;
        }
        self.state.error().map_or(Ok(()), Err)
    }
}
impl Drop for DiskWriter {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn segment_name(kind: Kind, run: &str, index: usize) -> String {
    if index == 0 {
        format!("{}-{run}.jsonl", kind.prefix())
    } else {
        format!("{}-{run}.part{index:06}.jsonl", kind.prefix())
    }
}
fn persist_manifest(snapshot: &Snapshot) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = snapshot.manifest_path();
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("Cannot create journal manifest: {error}"))?;
    let mut output = BufWriter::new(file);
    serde_json::to_writer(&mut output, &snapshot.manifest).map_err(|error| error.to_string())?;
    output
        .flush()
        .and_then(|()| output.get_ref().sync_all())
        .map_err(|error| format!("Cannot flush journal manifest: {error}"))?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("Cannot replace journal manifest: {error}"))?;
    #[cfg(unix)]
    File::open(&snapshot.directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot sync journal directory: {error}"))?;
    Ok(())
}

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Journal record size overflow"))?;
        if length > MAX_RECORD_BYTES {
            return Err(io::Error::other(
                "Journal record exceeds serialization bound",
            ));
        }
        if length > self.0.capacity() {
            let capacity = length
                .checked_next_power_of_two()
                .unwrap_or(MAX_RECORD_BYTES)
                .min(MAX_RECORD_BYTES);
            self.0
                .try_reserve_exact(capacity - self.0.len())
                .map_err(io::Error::other)?;
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn barrier(
    snapshot: &mut Snapshot,
    output: &mut BufWriter<File>,
    state: &State,
    limit: Option<u64>,
) -> Result<Snapshot, String> {
    output
        .flush()
        .and_then(|()| output.get_ref().sync_all())
        .map_err(|error| format!("Journal flush failed: {error}"))?;
    let current = snapshot.manifest.segments.last().unwrap();
    let actual = output
        .get_ref()
        .metadata()
        .map_err(|_| "Cannot inspect journal segment")?
        .len();
    if limit.is_some_and(|limit| {
        actual > limit || snapshot.length().map_or(true, |length| length > limit)
    }) {
        return Err(EXPORT_TOO_LARGE.into());
    }
    if actual != current.length {
        return Err("Journal segment changed outside its writer".into());
    }
    state
        .durable
        .store(state.flushed.load(Ordering::Acquire), Ordering::Release);
    persist_manifest(snapshot)?;
    Ok(snapshot.clone())
}
fn run_writer(
    receiver: mpsc::Receiver<Command>,
    state: Arc<State>,
    options: JournalOptions,
    mut snapshot: Snapshot,
    mut output: BufWriter<File>,
) {
    let mut failed = false;
    for command in receiver {
        match command {
            Command::Record {
                sequence,
                value,
                bytes,
            } => {
                state.entries.fetch_sub(1, Ordering::Release);
                state.bytes.fetch_sub(bytes, Ordering::Release);
                #[cfg(test)]
                if let Some(gate) = &options.gate {
                    gate.wait();
                }
                if failed {
                    continue;
                }
                let result = (|| -> Result<(), String> {
                    let mut encoded = Limited(Vec::new());
                    value
                        .json(&mut encoded)
                        .map_err(|error| format!("Journal serialization failed: {error}"))?;
                    encoded
                        .write_all(b"\n")
                        .map_err(|error| error.to_string())?;
                    let current = snapshot.manifest.segments.last().unwrap();
                    if current.length > 0
                        && current.length.saturating_add(encoded.0.len() as u64)
                            > options.segment_bytes
                    {
                        barrier(&mut snapshot, &mut output, &state, None)?;
                        if snapshot.manifest.segments.len() >= MAX_SEGMENTS {
                            return Err("Journal segment manifest limit reached".into());
                        }
                        let file_name = segment_name(
                            snapshot.manifest.kind,
                            &snapshot.manifest.run_id,
                            snapshot.manifest.segments.len(),
                        );
                        let file = OpenOptions::new()
                            .create_new(true)
                            .write(true)
                            .open(snapshot.directory.join(&file_name))
                            .map_err(|error| format!("Cannot rotate journal: {error}"))?;
                        output = BufWriter::new(file);
                        snapshot.manifest.segments.push(Segment {
                            file_name,
                            first_sequence: sequence,
                            last_sequence: sequence - 1,
                            length: 0,
                        });
                        state
                            .segments
                            .store(snapshot.manifest.segments.len(), Ordering::Release);
                        persist_manifest(&snapshot)?;
                    }
                    #[cfg(test)]
                    if options.fail_sequence == Some(sequence) {
                        output
                            .write_all(&encoded.0[..encoded.0.len() / 2])
                            .and_then(|()| output.flush())
                            .map_err(|error| error.to_string())?;
                        return Err("Injected synthetic partial journal write".into());
                    }
                    output
                        .write_all(&encoded.0)
                        .map_err(|error| format!("Journal write failed: {error}"))?;
                    state.written.store(sequence, Ordering::Release);
                    output
                        .flush()
                        .map_err(|error| format!("Journal flush failed: {error}"))?;
                    let segment = snapshot.manifest.segments.last_mut().unwrap();
                    segment.length = segment.length.saturating_add(encoded.0.len() as u64);
                    segment.last_sequence = sequence;
                    state.flushed.store(sequence, Ordering::Release);
                    Ok(())
                })();
                if let Err(error) = result {
                    state.fail(format!("{error}; disk history is incomplete"));
                    failed = true;
                }
            }
            Command::Fence { limit, reply } => {
                let result = state
                    .error()
                    .map_or_else(|| barrier(&mut snapshot, &mut output, &state, limit), Err);
                if let Err(error) = &result
                    && error != EXPORT_TOO_LARGE
                {
                    state.fail(error.clone());
                }
                let _ = reply.send(result);
            }
        }
    }
    if !failed && let Err(error) = barrier(&mut snapshot, &mut output, &state, None) {
        state.fail(error);
    }
    // Closing the file is part of completion; a blocked close must not leave
    // finish() performing an unbounded join after it observes the done flag.
    drop(output);
    snapshot.manifest.error = state.error();
    snapshot.manifest.incomplete = snapshot.manifest.error.is_some();
    snapshot.manifest.clean_shutdown = !snapshot.manifest.incomplete;
    if let Err(error) = persist_manifest(&snapshot) {
        state.fail(error);
    }
    *state.done.lock().unwrap_or_else(|error| error.into_inner()) = true;
    state.completed.notify_all();
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TestGate {
    pub entered: AtomicBool,
    pub released: Mutex<bool>,
    pub condition: Condvar,
}
#[cfg(test)]
impl TestGate {
    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let released = self.released.lock().unwrap();
        drop(
            self.condition
                .wait_while(released, |released| !*released)
                .unwrap(),
        );
    }
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.condition.notify_all();
    }
}
