//! Review metadata off the render thread and keep confirmation bound to the original owner.
use super::{
    Client, ConnectionStatus,
    wire::{Action, Error, MagnetReviewPage, MagnetReviewState, NewTorrent, PackedIndices, Reply},
};
use crate::{
    file_selection::FileSelectionModel,
    limits::ResourceLimits,
    magnet::MagnetLink,
    metainfo::{MetainfoFile, TorrentMetainfo},
    state::StoredTrackerAuditProfile,
    torrent::TorrentOverrides,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

const MAX_MAGNET_URI_BYTES: usize = 16 * 1024;
const MAX_PREVIEW_PATH_BYTES: usize = 64 * 1024 * 1024;
const REVIEW_DEADLINE: Duration = Duration::from_secs(75);
const STATUS_INTERVAL: Duration = Duration::from_millis(200);

pub struct MagnetReviewTask {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<MagnetReview, String>>,
}

pub struct MagnetReview {
    pub token: MagnetReviewToken,
    pub metadata: TorrentMetainfo,
}

pub struct MagnetReviewToken {
    client: Client,
    owner: String,
    id: String,
    hashes: Vec<String>,
    trackers: Vec<String>,
    confirmed: bool,
}

pub struct MagnetSelection<'a> {
    pub files: &'a FileSelectionModel,
    pub peer_id: String,
    pub overrides: TorrentOverrides,
    pub start: bool,
    pub audit: Option<StoredTrackerAuditProfile>,
}

impl MagnetReviewTask {
    pub fn start(client: &Client, magnet: String) -> Result<Self, String> {
        if magnet.len() > MAX_MAGNET_URI_BYTES {
            return Err("Magnet URI exceeds the review size limit".into());
        }
        MagnetLink::parse(&magnet)?;
        let connection = client.connection();
        let owner = connection
            .owner_instance
            .filter(|_| connection.status == ConnectionStatus::Connected)
            .ok_or("Wait for the background client before reviewing a magnet")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let client = client.clone();
        std::thread::Builder::new()
            .name("peerlume-magnet-review".into())
            .spawn(move || {
                let result = fetch_review(client, owner, magnet, &worker_cancel);
                let _ = sender.send(result);
            })
            .map_err(|_| "Could not start the magnet review worker")?;
        Ok(Self { cancel, receiver })
    }

    pub fn poll(&self) -> Option<Result<MagnetReview, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("The magnet review worker ended without a result".into()))
            }
        }
    }
}

impl Drop for MagnetReviewTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl MagnetReviewToken {
    pub fn confirm(&self, selection: MagnetSelection<'_>) -> Result<u64, String> {
        if self.confirmed {
            return Err("This magnet review was already confirmed".into());
        }
        let count = selection.files.file_count();
        let selected = selection
            .files
            .selected_indices()
            .ok_or("Complete magnet selection is unavailable")?;
        if selected.is_empty() {
            return Err("Select at least one file before confirming".into());
        }
        let request = NewTorrent {
            source: String::new(),
            hashes: self.hashes.clone(),
            trackers: self.trackers.clone(),
            peer_id: selection.peer_id,
            overrides: selection.overrides,
            selected: PackedIndices::new(count, &selected).map_err(|error| error.to_string())?,
            high: PackedIndices::new(count, &selection.files.high_priority())
                .map_err(|error| error.to_string())?,
            start: selection.start,
            audit: selection.audit,
        };
        self.client.confirm_magnet(&self.owner, &self.id, request)
    }

    pub fn confirmed(&mut self) {
        self.confirmed = true;
    }
}

impl Drop for MagnetReviewToken {
    fn drop(&mut self) {
        if !self.confirmed {
            self.client.cancel_magnet(&self.owner, &self.id);
        }
    }
}

fn fetch_review(
    client: Client,
    owner: String,
    magnet: String,
    cancel: &AtomicBool,
) -> Result<MagnetReview, String> {
    let deadline = Instant::now() + REVIEW_DEADLINE;
    check_active(cancel, deadline)?;
    let mut page = request_page(&client, &owner, Action::BeginMagnetReview { magnet })?;
    let mut token = MagnetReviewToken {
        client,
        owner,
        id: page.id.clone(),
        hashes: Vec::new(),
        trackers: Vec::new(),
        confirmed: false,
    };
    loop {
        check_active(cancel, deadline)?;
        match page.state {
            MagnetReviewState::Ready => break,
            MagnetReviewState::Cancelled => return Err("Magnet review cancelled".into()),
            MagnetReviewState::Failed { message } => return Err(message),
            MagnetReviewState::Resolving => {
                std::thread::sleep(STATUS_INTERVAL);
                check_active(cancel, deadline)?;
                page = next_page(&token, 0)?;
            }
        }
    }
    let name = page.name.take().ok_or("Magnet review returned no name")?;
    let total = page.total_files;
    if total == 0 || total > ResourceLimits::default().max_files {
        return Err("Magnet review returned an invalid file count".into());
    }
    token.hashes = std::mem::take(&mut page.hashes);
    token.trackers = std::mem::take(&mut page.trackers);
    let is_private = page.is_private;
    let mut files = Vec::new();
    let mut path_bytes = 0usize;
    loop {
        check_active(cancel, deadline)?;
        if page.state != MagnetReviewState::Ready
            || page.total_files != total
            || page.files.is_empty()
            || files.len().saturating_add(page.files.len()) > total
        {
            return Err("Magnet review returned an inconsistent file page".into());
        }
        for file in page.files {
            path_bytes = path_bytes.saturating_add(file.path.len());
            if path_bytes > MAX_PREVIEW_PATH_BYTES {
                return Err("Magnet review exceeds the file path budget".into());
            }
            files.push(MetainfoFile {
                path: file.path,
                size_bytes: file.size_bytes,
            });
        }
        match page.next_offset {
            Some(offset) if offset == files.len() && offset < total => {
                page = next_page(&token, offset)?
            }
            None if files.len() == total => break,
            _ => return Err("Magnet review returned an inconsistent page cursor".into()),
        }
    }
    let metadata = TorrentMetainfo {
        name,
        files,
        trackers: token.trackers.clone(),
        is_private,
        info_hash_v1_hex: token
            .hashes
            .iter()
            .find_map(|hash| hash.strip_prefix("v1: ").map(str::to_owned)),
        info_hash_v2_hex: token
            .hashes
            .iter()
            .find_map(|hash| hash.strip_prefix("v2: ").map(str::to_owned)),
    };
    Ok(MagnetReview { token, metadata })
}

fn next_page(token: &MagnetReviewToken, offset: usize) -> Result<MagnetReviewPage, String> {
    let page = request_page(
        &token.client,
        &token.owner,
        Action::MagnetReview {
            id: token.id.clone(),
            offset,
        },
    )?;
    if page.id != token.id {
        return Err("Background client returned a different magnet review".into());
    }
    Ok(page)
}

fn request_page(client: &Client, owner: &str, action: Action) -> Result<MagnetReviewPage, String> {
    match client.review_request(owner, action) {
        Ok(Reply::MagnetReview(page)) => Ok(page),
        Ok(_) => Err("Unexpected magnet review response".into()),
        Err(Error::Transport(_)) => {
            Err("The background client could not read the magnet review".into())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn check_active(cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Magnet review cancelled".into())
    } else if Instant::now() >= deadline {
        Err("Magnet review timed out".into())
    } else {
        Ok(())
    }
}
