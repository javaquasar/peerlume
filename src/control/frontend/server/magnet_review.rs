//! Short-lived metadata review; payload admission requires a separate confirmation.
use super::*;
use crate::metainfo::TorrentMetainfo;
use librqbit::dht::Id20;
use std::{collections::HashMap, io::Write, sync::Weak, time::Instant};

const MAX_RETAINED: usize = 8;
const MAX_URI: usize = 16 * 1024;
const MAX_TRACKERS: usize = 8;
const MAX_PAGE_ROWS: usize = 128;
const PAGE_FILE_BYTES: usize = 128 * 1024;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
const REVIEW_TTL: Duration = Duration::from_secs(5 * 60);

struct Magnet {
    v1: Id20,
    v2: Option<String>,
    trackers: Vec<url::Url>,
}

impl Magnet {
    fn parse(uri: &str) -> Result<Self, Error> {
        if uri.len() > MAX_URI {
            return Err(Error::TooLarge);
        }
        let uri = url::Url::parse(uri).map_err(|_| Error::Failed("Invalid magnet link".into()))?;
        if uri.scheme() != "magnet" {
            return Err(Error::Failed("Expected a magnet link".into()));
        }
        let mut v1 = None;
        let mut v2 = None;
        let mut trackers = Vec::new();
        for (key, value) in uri.query_pairs() {
            match key.as_ref() {
                "xt" => {
                    if let Some(hash) = value.strip_prefix("urn:btih:") {
                        let hash = hash
                            .to_ascii_uppercase()
                            .parse::<Id20>()
                            .map_err(|_| Error::Failed("Invalid v1 magnet identity".into()))?;
                        if v1.replace(hash).is_some_and(|previous| previous != hash) {
                            return Err(Error::Failed("Conflicting v1 magnet identities".into()));
                        }
                    } else if let Some(hash) = value.strip_prefix("urn:btmh:1220") {
                        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                            return Err(Error::Failed("Invalid v2 magnet identity".into()));
                        }
                        let hash = hash.to_ascii_lowercase();
                        if v2
                            .replace(hash.clone())
                            .is_some_and(|previous| previous != hash)
                        {
                            return Err(Error::Failed("Conflicting v2 magnet identities".into()));
                        }
                    }
                }
                "tr" => {
                    let tracker = url::Url::parse(&value)
                        .ok()
                        .filter(|url| {
                            matches!(url.scheme(), "http" | "https" | "udp")
                                && url.host().is_some()
                                && (url.scheme() != "udp" || url.port().is_some())
                                && url.as_str().len() <= 8192
                        })
                        .ok_or_else(|| {
                            Error::Failed("Invalid or unsupported explicit magnet tracker".into())
                        })?;
                    if !trackers.contains(&tracker) {
                        if trackers.len() == MAX_TRACKERS {
                            return Err(Error::TooLarge);
                        }
                        trackers.push(tracker);
                    }
                }
                // A magnet may contain untrusted file ranges or extra peer URLs.
                // Only the reviewed selection and configured discovery are used.
                _ => {}
            }
        }
        Ok(Self {
            v1: v1.ok_or_else(|| {
                Error::Failed("Metadata review requires a BitTorrent v1 topic".into())
            })?,
            v2,
            trackers,
        })
    }
}

struct Resolved {
    metadata: TorrentMetainfo,
    hashes: Vec<String>,
    trackers: Vec<String>,
    bytes: Arc<[u8]>,
}

impl Resolved {
    fn parse(raw_info: &[u8], request: Magnet) -> Result<Self, String> {
        // Preserve the exact authenticated info bytes. Tracker credentials
        // belong in the existing protected state store, not this cache file.
        let mut bytes = Vec::with_capacity(raw_info.len() + 8);
        bytes.extend_from_slice(b"d4:info");
        bytes.extend_from_slice(raw_info);
        bytes.push(b'e');
        let metadata = TorrentMetainfo::parse_v1_review(&bytes)
            .map_err(|_| "Peer metadata is malformed or exceeds application limits".to_owned())?;
        if metadata.info_hash_v1_hex.as_deref() != Some(request.v1.as_string().as_str())
            || request
                .v2
                .as_ref()
                .is_some_and(|hash| metadata.info_hash_v2_hex.as_ref() != Some(hash))
        {
            return Err("Peer metadata does not match the magnet identity".into());
        }
        if metadata.is_private && request.trackers.is_empty() {
            return Err("Private metadata requires an explicit tracker in the magnet link".into());
        }
        let checked = librqbit::torrent_from_bytes(&bytes)
            .map_err(|_| "The transfer engine does not support this metadata".to_owned())?;
        checked
            .info
            .data
            .validate()
            .map_err(|_| "The transfer engine does not support this metadata".to_owned())?;
        Ok(Self {
            hashes: metainfo_hashes(&metadata),
            metadata,
            trackers: request
                .trackers
                .into_iter()
                .map(|tracker| tracker.to_string())
                .collect(),
            bytes: bytes.into(),
        })
    }
}

enum State {
    Resolving(tokio::task::AbortHandle),
    Ready(Arc<Resolved>),
    Failed(String),
    Cancelled,
}

struct Entry {
    client: String,
    hash: Id20,
    state: State,
    since: Instant,
    confirming: bool,
}

pub(super) struct Reviews {
    entries: Mutex<HashMap<String, Entry>>,
    admission: Arc<tokio::sync::Semaphore>,
}

impl Default for Reviews {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            admission: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}

impl Reviews {
    fn prune(entries: &mut HashMap<String, Entry>) {
        entries.retain(|_, entry| {
            entry.confirming
                || matches!(entry.state, State::Resolving(_))
                || entry.since.elapsed() < REVIEW_TTL
        });
    }

    fn begin(
        self: &Arc<Self>,
        client: String,
        request: Magnet,
        engine: Arc<librqbit::Session>,
    ) -> Result<MagnetReviewPage, Error> {
        let permit = self
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::prune(&mut entries);
        if entries.values().any(|entry| {
            entry.hash == request.v1 && matches!(entry.state, State::Resolving(_) | State::Ready(_))
        }) {
            return Err(Error::Failed(
                "A review for this torrent is already open".into(),
            ));
        }
        if entries.len() >= MAX_RETAINED {
            let oldest = entries
                .iter()
                .filter(|(_, entry)| {
                    !entry.confirming && !matches!(entry.state, State::Resolving(_))
                })
                .min_by_key(|(_, entry)| entry.since)
                .map(|(id, _)| id.clone());
            let Some(oldest) = oldest else {
                return Err(Error::Busy);
            };
            entries.remove(&oldest);
        }
        let id = crate::control::access::random_id().map_err(|_| Error::Busy)?;
        let hash = request.v1;
        let weak = Arc::downgrade(self);
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                RESOLVE_TIMEOUT,
                engine.review_magnet_metadata(request.v1, request.trackers.clone()),
            )
            .await
            {
                Ok(Ok(raw)) => tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    Resolved::parse(&raw, request)
                })
                .await
                .unwrap_or_else(|_| Err("Metadata validation could not finish".into())),
                Ok(Err(_)) => {
                    Err("No peer supplied valid metadata using the enabled discovery routes".into())
                }
                Err(_) => Err("Metadata discovery timed out".into()),
            };
            Self::finish(weak, &task_id, result);
        });
        entries.insert(
            id.clone(),
            Entry {
                client,
                hash,
                state: State::Resolving(task.abort_handle()),
                since: Instant::now(),
                confirming: false,
            },
        );
        Ok(empty_page(id, MagnetReviewState::Resolving))
    }

    fn finish(reviews: Weak<Self>, id: &str, result: Result<Resolved, String>) {
        let Some(reviews) = reviews.upgrade() else {
            return;
        };
        let mut entries = reviews
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = entries.get_mut(id)
            && matches!(entry.state, State::Resolving(_))
        {
            entry.state = match result {
                Ok(value) => State::Ready(Arc::new(value)),
                Err(message) => State::Failed(message),
            };
            entry.since = Instant::now();
        }
    }

    fn page(&self, client: &str, id: &str, offset: usize) -> Result<MagnetReviewPage, Error> {
        let data = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Self::prune(&mut entries);
            let entry = entries
                .get(id)
                .filter(|entry| entry.client == client)
                .ok_or(Error::Stale)?;
            match &entry.state {
                State::Resolving(_) => {
                    return Ok(empty_page(id.into(), MagnetReviewState::Resolving));
                }
                State::Failed(message) => {
                    return Ok(empty_page(
                        id.into(),
                        MagnetReviewState::Failed {
                            message: message.clone(),
                        },
                    ));
                }
                State::Cancelled => return Ok(empty_page(id.into(), MagnetReviewState::Cancelled)),
                State::Ready(data) => data.clone(),
            }
        };
        let total_files = data.metadata.files.len();
        if offset >= total_files {
            return Err(Error::Stale);
        }
        let mut files = Vec::new();
        let mut bytes = 0;
        for file in data.metadata.files.iter().skip(offset).take(MAX_PAGE_ROWS) {
            // Validated paths contain no control characters. Quotes and
            // backslashes are bounded by doubling their UTF-8 byte length.
            let upper_bound = file.path.len() * 2 + 64;
            if !files.is_empty() && bytes + upper_bound > PAGE_FILE_BYTES {
                break;
            }
            bytes += upper_bound;
            files.push(MagnetReviewFile {
                path: file.path.clone(),
                size_bytes: file.size_bytes,
            });
        }
        let next = offset + files.len();
        Ok(MagnetReviewPage {
            id: id.into(),
            state: MagnetReviewState::Ready,
            name: Some(data.metadata.name.clone()),
            hashes: data.hashes.clone(),
            trackers: data.trackers.clone(),
            is_private: data.metadata.is_private,
            total_files,
            next_offset: (next < total_files).then_some(next),
            files,
        })
    }

    fn cancel(&self, client: &str, id: &str) -> Result<(), Error> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::prune(&mut entries);
        let entry = entries
            .get_mut(id)
            .filter(|entry| entry.client == client)
            .ok_or(Error::Stale)?;
        if entry.confirming {
            return Err(Error::Busy);
        }
        if let State::Resolving(task) = &entry.state {
            task.abort();
        }
        entry.state = State::Cancelled;
        entry.since = Instant::now();
        Ok(())
    }

    fn confirm(self: &Arc<Self>, client: &str, id: &str) -> Result<Confirmation, Error> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::prune(&mut entries);
        let entry = entries
            .get_mut(id)
            .filter(|entry| entry.client == client)
            .ok_or(Error::Stale)?;
        if entry.confirming {
            return Err(Error::Busy);
        }
        let State::Ready(data) = &entry.state else {
            return Err(Error::Stale);
        };
        entry.confirming = true;
        Ok(Confirmation {
            reviews: self.clone(),
            id: id.into(),
            hash: entry.hash,
            data: data.clone(),
        })
    }
}

struct Confirmation {
    reviews: Arc<Reviews>,
    id: String,
    hash: Id20,
    data: Arc<Resolved>,
}
impl Confirmation {
    fn consume(self) {
        self.reviews
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.id);
    }
}
impl Drop for Confirmation {
    fn drop(&mut self) {
        let mut entries = self
            .reviews
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = entries.get_mut(&self.id) {
            entry.confirming = false;
        }
    }
}

fn empty_page(id: String, state: MagnetReviewState) -> MagnetReviewPage {
    MagnetReviewPage {
        id,
        state,
        name: None,
        hashes: vec![],
        trackers: vec![],
        is_private: false,
        total_files: 0,
        next_offset: None,
        files: vec![],
    }
}

fn already_added(owner: &mut Controller, hash: Id20) -> bool {
    owner
        .torrent_index(&format!("v1: {}", hash.as_string()))
        .is_some()
        || owner.torrents.iter().any(|item| {
            item.info_hashes.iter().any(|identity| {
                identity
                    .strip_prefix("urn:btih:")
                    .and_then(|value| value.to_ascii_uppercase().parse::<Id20>().ok())
                    == Some(hash)
            })
        })
}

impl Service {
    pub(super) fn begin_magnet_review(&self, client: String, uri: &str) -> Result<Reply, Error> {
        let request = Magnet::parse(uri)?;
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        if already_added(&mut owner, request.v1) {
            return Err(Error::Failed("Torrent is already added".into()));
        }
        let engine = owner
            .backend
            .metadata_engine()
            .ok_or_else(|| Error::Failed("Transfer engine unavailable".into()))?;
        drop(owner);
        self.reviews
            .begin(client, request, engine)
            .map(Reply::MagnetReview)
    }

    pub(super) fn magnet_review_page(
        &self,
        client: &str,
        id: &str,
        offset: usize,
    ) -> Result<Reply, Error> {
        self.reviews
            .page(client, id, offset)
            .map(Reply::MagnetReview)
    }

    pub(super) fn cancel_magnet_review(&self, client: &str, id: &str) -> Result<Reply, Error> {
        self.reviews.cancel(client, id)?;
        Ok(Reply::Done("Metadata review cancelled".into()))
    }

    pub(super) async fn confirm_magnet_review(
        &self,
        client: &str,
        id: &str,
        request: NewTorrent,
        ticket: Option<Ticket>,
    ) -> Result<Reply, Error> {
        let _permit = self.imports.try_acquire().map_err(|_| Error::Busy)?;
        let confirmation = self.reviews.confirm(client, id)?;
        {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            if already_added(&mut owner, confirmation.hash) {
                return Err(Error::Failed("Torrent is already added".into()));
            }
        }
        let data = confirmation.data.clone();
        let profile = self.directory.clone();
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let worker_settings = settings.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_confirmation(&profile, &worker_settings, &data, request)
        })
        .await
        .map_err(|_| Error::Busy)??;
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        if already_added(&mut owner, confirmation.hash) {
            return Err(Error::Failed("Torrent is already added".into()));
        }
        let canonical = match prepared.admit(&mut owner, &settings) {
            Ok(id) => id,
            Err(error @ Error::Transport(_)) => {
                drop(owner);
                confirmation.consume();
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if let Some(index) = owner.torrent_index(&canonical) {
            owner.torrents[index].files.defer_details();
        }
        self.record_acceptance(ticket, &mut owner, &canonical);
        owner.owner_wake().notify_one();
        drop(owner);
        confirmation.consume();
        Ok(Reply::Done(
            "Reviewed torrent queued; awaiting engine confirmation".into(),
        ))
    }
}

struct PreparedConfirmation {
    item: TorrentItem,
    audit: Option<crate::state::StoredTrackerAuditProfile>,
    prepared: crate::session::PreparedTorrent,
    cache: tempfile::NamedTempFile,
}

impl PreparedConfirmation {
    fn admit(self, owner: &mut Controller, settings: &ClientSettings) -> Result<String, Error> {
        let Self {
            item,
            audit,
            prepared,
            mut cache,
        } = self;
        let id = item.info_hashes.join(";");
        let source = item.source.clone();
        match owner.add_prepared_with_audit(item, settings, prepared, audit) {
            Ok(()) => {
                cache.disable_cleanup(true);
                Ok(id)
            }
            Err(error)
                if owner.backend.persisted_torrent_uses_source(&id, &source) == Ok(false) =>
            {
                Err(Error::Failed(error))
            }
            Err(_) => {
                // The add writes a row before admitting the engine command.
                // If its rollback did not remove that row, preserve the only
                // authoritative source and require outcome reconciliation.
                cache.disable_cleanup(true);
                Err(Error::Transport("Torrent admission could not be confirmed; reviewed metadata retained for recovery".into()))
            }
        }
    }
}

fn prepare_confirmation(
    profile: &std::path::Path,
    settings: &ClientSettings,
    data: &Resolved,
    mut request: NewTorrent,
) -> Result<PreparedConfirmation, Error> {
    if request.hashes != data.hashes {
        return Err(Error::Stale);
    }
    if request.selected.indices()?.is_empty() {
        return Err(Error::Failed(
            "Select at least one file from the reviewed torrent".into(),
        ));
    }
    // The frontend's source is display-only; it cannot choose what bytes
    // are registered or where this trusted metadata is stored.
    request.source.clear();
    request.trackers = data.trackers.clone();
    let metadata = TorrentMetainfo::parse_v1_review(&data.bytes)
        .map_err(|_| Error::Failed("Reviewed metadata is no longer valid".into()))?;
    let (mut item, audit) = prepare_resolved_metadata(request, settings, metadata)?;
    let destination = item
        .overrides
        .download_directory
        .as_deref()
        .ok_or(Error::Stale)?;
    let directory =
        crate::storage::sandbox::Directory::create_root(std::path::Path::new(destination))
            .map_err(|_| Error::Failed("Could not pin the selected download directory".into()))?;
    let mut cache = tempfile::Builder::new()
        .prefix("magnet-")
        .suffix(".torrent")
        .tempfile_in(profile)
        .map_err(|_| Error::Failed("Could not save reviewed torrent metadata".into()))?;
    cache
        .write_all(&data.bytes)
        .and_then(|()| cache.as_file().sync_all())
        .map_err(|_| Error::Failed("Could not save reviewed torrent metadata".into()))?;
    #[cfg(unix)]
    std::fs::File::open(profile)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| Error::Failed("Could not sync reviewed torrent metadata".into()))?;
    item.source = cache.path().to_str().ok_or(Error::Stale)?.into();
    Ok(PreparedConfirmation {
        item,
        audit,
        prepared: crate::session::PreparedTorrent {
            bytes: data.bytes.clone(),
            directory,
        },
        cache,
    })
}

#[cfg(test)]
mod tests;
