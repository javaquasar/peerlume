use super::{
    MAX_CONNECTIONS, MAX_FRAME_BYTES, PROTOCOL_VERSION,
    operations::{History, Intent, SharedHistory, Ticket},
    transport,
    wire::*,
};
use crate::{
    control::Controller,
    file_selection::FileSelectionModel,
    settings::{
        ClientSettings, SpeedSettings,
        speed_policy::{SpeedPlan, SpeedView},
    },
    torrent::{TorrentItem, TorrentState},
};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
mod trash_plan;
use trash_plan::PreparedTrash;
#[cfg(test)]
mod file_revision_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod workflow_tests;

pub struct Server {
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicBool>,
    request_wake: Arc<tokio::sync::Notify>,
    wake: Arc<tokio::sync::Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}
struct Service {
    owner: Arc<Mutex<Controller>>,
    directory: PathBuf,
    settings: Mutex<ClientSettings>,
    saved_speed: Mutex<Option<(SpeedSettings, bool, usize)>>,
    history: Option<SharedHistory>,
    instance: String,
    token_hash: String,
    requested: Arc<AtomicBool>,
    request_wake: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    imports: tokio::sync::Semaphore,
    plans: Mutex<std::collections::BTreeMap<String, PreparedTrash>>,
}
impl Server {
    pub fn start(
        owner: Arc<Mutex<Controller>>,
        directory: PathBuf,
        settings: ClientSettings,
    ) -> io::Result<Self> {
        let instance = crate::control::access::random_id()?;
        let token = crate::control::access::random_id()?;
        let address = transport::endpoint(&directory)?;
        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicBool::new(false));
        let request_wake = Arc::new(tokio::sync::Notify::new());
        let wake = Arc::new(tokio::sync::Notify::new());
        let saved_speed = ClientSettings::load_from(&directory.join("settings.toml"))
            .ok()
            .map(|saved| {
                (
                    saved.speed,
                    saved.queue.enabled,
                    saved.queue.max_active_downloads,
                )
            });
        let history = History::open(&directory.join("native-operations.sqlite3"))
            .ok()
            .map(|history| Arc::new(Mutex::new(history)));
        let service = Arc::new(Service {
            owner: owner.clone(),
            directory: directory.clone(),
            settings: Mutex::new(settings),
            saved_speed: Mutex::new(saved_speed),
            history: history.clone(),
            instance: instance.clone(),
            token_hash: crate::control::access::digest(token.as_bytes()),
            requested: requested.clone(),
            request_wake: request_wake.clone(),
            stop: stop.clone(),
            wake: wake.clone(),
            imports: tokio::sync::Semaphore::new(1),
            plans: Default::default(),
        });
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("peerlume-native-server".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match result {
                    Ok(runtime) => runtime.block_on(listen(address, service, ready)),
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })?;
        let server = Self {
            stop,
            requested,
            request_wake,
            wake,
            thread: Some(thread),
        };
        started
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| io::Error::other("Native server initialization timed out"))??;
        owner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .native_operations = history;
        transport::write_credentials(&directory, instance, &token)?;
        Ok(server)
    }
    pub fn stop_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
    pub async fn stopped(&self) {
        while !self.stop_requested() {
            self.request_wake.notified().await;
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
async fn listen(
    address: String,
    service: Arc<Service>,
    ready: std::sync::mpsc::SyncSender<io::Result<()>>,
) {
    let first = crate::control::host::windows_pipe(&address, true);
    let mut pipe = match first {
        Ok(pipe) => pipe,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        if service.stop.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {result=pipe.connect()=>{
            if result.is_err(){return;}
            let next=match crate::control::host::windows_pipe(&address,false){Ok(pipe)=>pipe,Err(_)=>return};
            let connected=std::mem::replace(&mut pipe,next);
            if let Ok(permit)=permits.clone().try_acquire_owned(){let service=service.clone();tokio::spawn(async move{let _permit=permit;connection(connected,service).await;});}
        },_ = service.wake.notified()=>{if service.stop.load(Ordering::Acquire){break;}}}
    }
}
#[cfg(unix)]
async fn listen(
    address: String,
    service: Arc<Service>,
    ready: std::sync::mpsc::SyncSender<io::Result<()>>,
) {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(&address) {
        if !meta.file_type().is_socket() {
            let _ = ready.send(Err(io::Error::other("Native socket path is not a socket")));
            return;
        }
        let _ = std::fs::remove_file(&address);
    }
    let listener = match tokio::net::UnixListener::bind(&address) {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if let Err(error) = std::fs::set_permissions(&address, std::fs::Permissions::from_mode(0o600)) {
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(()));
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        if service.stop.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {result=listener.accept()=>{if let Ok((stream,_))=result && let Ok(permit)=permits.clone().try_acquire_owned(){let service=service.clone();tokio::spawn(async move{let _permit=permit;connection(stream,service).await;});}},_ = service.wake.notified()=>{if service.stop.load(Ordering::Acquire){break;}}}
    }
    let _ = std::fs::remove_file(address);
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin>(stream: S, service: Arc<Service>) {
    let mut stream = BufReader::new(stream);
    let request =
        match tokio::time::timeout(Duration::from_secs(5), transport::read(&mut stream)).await {
            Ok(Ok(bytes)) => serde_json::from_slice::<Request>(&bytes).ok(),
            _ => None,
        };
    let Some(request) = request else {
        return;
    };
    let valid_request_id = !request.request_id.is_empty() && request.request_id.len() <= 128;
    let request_id = if valid_request_id {
        request.request_id.clone()
    } else {
        String::new()
    };
    let requests_stop = matches!(&request.action, Action::Stop);
    let (operation_id, result) = if !valid_request_id {
        (None, Err(Error::TooLarge))
    } else if request.protocol != PROTOCOL_VERSION {
        (None, Err(Error::Incompatible))
    } else if request.client_id.len() != 64
        || !crate::control::access::matches_token(&service.token_hash, &request.token)
    {
        (None, Err(Error::Unauthorized))
    } else if request
        .instance_id
        .as_ref()
        .is_some_and(|id| *id != service.instance)
    {
        (None, Err(Error::Stale))
    } else {
        service
            .dispatch_tracked(request.client_id, &request_id, request.action)
            .await
    };
    let mut response = Response {
        protocol: PROTOCOL_VERSION,
        request_id,
        instance_id: service.instance.clone(),
        operation_id,
        result,
    };
    let mut bytes = serde_json::to_vec(&response).unwrap_or_default();
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        response.result = Err(Error::TooLarge);
        bytes = serde_json::to_vec(&response).unwrap_or_default();
    }
    let accepted_stop = requests_stop && response.result.is_ok();
    bytes.push(b'\n');
    let transmitted =
        tokio::time::timeout(Duration::from_secs(5), stream.get_mut().write_all(&bytes)).await;
    if accepted_stop && matches!(transmitted, Ok(Ok(()))) {
        // The daemon must not abort this connection while its accepted stop
        // receipt is still being encoded/transmitted.
        service.requested.store(true, Ordering::Release);
        service.request_wake.notify_one();
    }
}

impl Service {
    fn record_saved_speed(&self, settings: &ClientSettings) {
        *self.saved_speed.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            settings.speed.clone(),
            settings.queue.enabled,
            settings.queue.max_active_downloads,
        ));
    }

    fn speed_view(&self, owner: &Controller) -> SpeedView {
        let desired = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let saved = self.saved_speed.lock().unwrap_or_else(|e| e.into_inner());
        let (applied, pending) = owner.backend.speed_readback();
        SpeedView {
            desired: desired.speed.clone(),
            saved: saved.as_ref().map(|(speed, _, _)| speed.clone()),
            desired_queue_enabled: desired.queue.enabled,
            saved_queue_enabled: saved.as_ref().map(|(_, enabled, _)| *enabled),
            desired_max_active_downloads: desired.queue.max_active_downloads,
            saved_max_active_downloads: saved.as_ref().map(|(_, _, slots)| *slots),
            applied,
            pending,
        }
    }

    async fn dispatch_tracked(
        self: &Arc<Self>,
        client: String,
        request_id: &str,
        action: Action,
    ) -> (Option<String>, Result<Reply, Error>) {
        let Some(intent) = Intent::from_action(&action) else {
            return (None, self.dispatch(client, action, None).await);
        };
        let Some(history) = &self.history else {
            return (None, self.dispatch(client, action, None).await);
        };
        let opened = history
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .begin(&self.instance, request_id, intent);
        let (ticket, id) = match opened {
            Ok(value) => value,
            Err(Error::Busy) => return (None, Err(Error::Busy)),
            Err(_) => {
                history
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .degrade();
                return (None, self.dispatch(client, action, None).await);
            }
        };
        let result = self.dispatch(client, action, Some(ticket)).await;
        let recorded = history
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finish_rpc(ticket, &result);
        (recorded.then_some(id), result)
    }

    fn record_acceptance(&self, ticket: Option<Ticket>, owner: &mut Controller, id: &str) {
        let (Some(ticket), Some(history)) = (ticket, &self.history) else {
            return;
        };
        let Some(index) = owner.torrent_index(id) else {
            return;
        };
        let item = &owner.torrents[index];
        let id = item.info_hashes.join(";");
        let Ok(revision) = owner.command_revision(&id) else {
            return;
        };
        let fresh = item.state != TorrentState::Checking
            && !matches!(
                item.runtime.storage,
                crate::storage::relocation::StorageStatus::Copying { .. }
                    | crate::storage::relocation::StorageStatus::Checking { .. }
            );
        history
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .accepted(ticket, &id, revision, fresh);
    }

    async fn dispatch(
        self: &Arc<Self>,
        client: String,
        action: Action,
        ticket: Option<Ticket>,
    ) -> Result<Reply, Error> {
        if let Action::Operations { before, limit } = action {
            return self
                .history
                .as_ref()
                .ok_or_else(super::operations::unavailable)?
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .page(before, limit)
                .map(Reply::Operations);
        }
        if let Action::Add(request) = action {
            let _permit = self.imports.try_acquire().map_err(|_| Error::Busy)?;
            let settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let item = tokio::task::spawn_blocking(move || prepare_new(request, &settings))
                .await
                .map_err(|_| Error::Busy)??;
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            let id = item.0.info_hashes.join(";");
            owner
                .add(
                    item.0,
                    &self
                        .settings
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()),
                    item.1,
                )
                .map_err(Error::Failed)?;
            if let Some(index) = owner.torrent_index(&id) {
                owner.torrents[index].files.defer_details();
            }
            self.record_acceptance(ticket, &mut owner, &id);
            owner.owner_wake().notify_one();
            return Ok(Reply::Done(
                "Torrent queued; awaiting engine confirmation".into(),
            ));
        }
        if let Action::Export { id, destination } = action {
            let (journal, hash) = {
                let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
                let hash = journal_hash(&mut owner, id.as_deref())?;
                let journal = owner
                    .backend
                    .journal()
                    .cloned()
                    .ok_or_else(|| Error::Failed("Journal unavailable".into()))?;
                (journal, hash)
            };
            return tokio::task::spawn_blocking(move || {
                journal
                    .export(&destination, hash.as_deref())
                    .map(|count| Reply::Done(format!("Exported {count} records")))
                    .map_err(Error::Failed)
            })
            .await
            .map_err(|_| Error::Busy)?;
        }
        if let Action::PrepareTrash {
            id,
            revision,
            scope,
        } = action
        {
            let service = self.clone();
            return tokio::task::spawn_blocking(move || {
                service.prepare_trash(client, id, revision, scope)
            })
            .await
            .map_err(|_| Error::Busy)?;
        }
        if let Action::Trash { plan_id, digest } = action {
            let service = self.clone();
            return tokio::task::spawn_blocking(move || {
                service.commit_trash(client, plan_id, digest)
            })
            .await
            .map_err(|_| Error::Busy)?;
        }
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        let save_preferences = matches!(&action, Action::SaveSettings(_));
        let mutating = !matches!(
            action,
            Action::Hello
                | Action::Snapshot { .. }
                | Action::SpeedState
                | Action::Files { .. }
                | Action::Journal { .. }
                | Action::Access(AccessAction::View)
                | Action::Interest { .. }
                | Action::ConfigureAgents { .. }
                | Action::Notices { .. }
                | Action::TrackerAudit { .. }
        );
        let result = match action {
            Action::Hello => Ok(Reply::Hello {
                settings: Box::new(
                    self.settings
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                ),
                instance_id: self.instance.clone(),
            }),
            Action::Notices { after, limit } => {
                if limit == 0 || limit > 128 {
                    return Err(Error::TooLarge);
                }
                let (cursor, resync, entries) = owner.native_notices(after, limit);
                Ok(Reply::Notices {
                    cursor,
                    resync,
                    entries,
                })
            }
            Action::TrackerAudit { id } => {
                if owner.torrent_index(&id).is_none() {
                    return Err(Error::Stale);
                }
                let audit = owner
                    .backend
                    .load_tracker_audit(&id)
                    .map_err(Error::Failed)?;
                Ok(Reply::TrackerAudit { id, audit })
            }
            Action::Snapshot {
                offset,
                limit,
                revision,
                detail,
            } => {
                // A matching engine ACK may have removed a row since the last
                // pump publication. Pin the actual structural epoch before
                // accepting offsets from any previous native page.
                owner.publish();
                if limit == 0 || limit > 128 || offset > owner.torrents.len() {
                    return Err(Error::TooLarge);
                }
                if revision.is_some_and(|epoch| epoch != owner.command_clock) {
                    return Err(Error::Stale);
                }
                let end = offset.saturating_add(limit).min(owner.torrents.len());
                let mut torrents = Vec::new();
                let mut observations = Vec::new();
                for item in &owner.torrents[offset..end] {
                    let id = item.info_hashes.join(";");
                    let observed = owner.observed.get(&id).ok_or(Error::Stale)?;
                    torrents.push(WireTorrent::capture(
                        item,
                        observed.command_revision,
                        !owner.unpersisted_updates.contains_key(&id),
                        detail.as_ref() == Some(&id),
                    ));
                    observations.push(observed.clone());
                }
                Ok(Reply::Snapshot(Page {
                    revision: owner.frontend_revision(),
                    epoch: owner.command_clock,
                    total: owner.torrents.len(),
                    next_offset: (end < owner.torrents.len()).then_some(end),
                    torrents,
                    observations,
                }))
            }
            Action::Files {
                id,
                offset,
                limit,
                revision,
                file_revision,
                paths,
            } => {
                check_file_revision(&mut owner, &id, revision, file_revision)?;
                let index = owner.torrent_index(&id).ok_or(Error::Stale)?;
                let item = &owner.torrents[index];
                let total = item.files.file_count();
                if offset > total || limit == 0 || limit > if paths { 128 } else { 8192 } {
                    return Err(Error::TooLarge);
                }
                let mut page = if paths {
                    owner
                        .backend
                        .load_file_page(&id, offset, limit)
                        .map_err(Error::Failed)?
                } else {
                    crate::state::FilePage {
                        offset,
                        total,
                        next_offset: Some(offset.saturating_add(limit).min(total))
                            .filter(|end| *end < total),
                        files: (offset..offset.saturating_add(limit).min(total))
                            .filter_map(|index| {
                                item.files
                                    .file_state(index)
                                    .map(|state| crate::state::StoredFile {
                                        index: index as u32,
                                        path: String::new(),
                                        size_bytes: state.size_bytes,
                                        downloaded_bytes: state.downloaded_bytes,
                                        selected: state.selected,
                                        priority: i32::from(state.high_priority),
                                    })
                            })
                            .collect(),
                    }
                };
                let maximum_end = offset.saturating_add(limit).min(total);
                let end = offset.saturating_add(page.files.len());
                if page.total != total
                    || page.offset != offset
                    || end > maximum_end
                    || (end == offset && offset < total)
                    || (!paths && end != maximum_end)
                    || page.next_offset != (end < total).then_some(end)
                {
                    return Err(Error::Stale);
                }
                for (index, file) in (offset..end).zip(&mut page.files) {
                    if file.index as usize != index {
                        return Err(Error::Stale);
                    }
                    let state = item.files.file_state(index).ok_or(Error::Stale)?;
                    file.size_bytes = state.size_bytes;
                    file.downloaded_bytes = state.downloaded_bytes;
                    file.selected = state.selected;
                    file.priority = i32::from(state.high_priority);
                }
                // The owner mutex spans SQL path loading and canonical overlay.
                // Never return pages under a generation other than the caller's.
                check_file_revision(&mut owner, &id, revision, file_revision)?;
                Ok(Reply::Files(FilePage {
                    id,
                    revision,
                    file_revision,
                    total: page.total,
                    next_offset: page.next_offset,
                    files: page.files,
                }))
            }
            Action::Mutate {
                id,
                command,
                revision,
            } => {
                if matches!(command, crate::control::TorrentCommand::SetFiles(_)) {
                    return Err(Error::Failed(
                        "File selection requires a native file generation fence".into(),
                    ));
                }
                let remove = matches!(command, crate::control::TorrentCommand::Remove);
                let accepted = owner
                    .submit(&id, command, revision)
                    .map_err(Error::Command)?;
                if remove {
                    owner.pending_removals.insert(accepted.torrent_id.clone());
                }
                self.record_acceptance(ticket, &mut owner, &accepted.torrent_id);
                Ok(Reply::Accepted(accepted))
            }
            Action::Selection {
                id,
                files,
                revision,
                file_revision,
            } => {
                check_file_revision(&mut owner, &id, revision, file_revision)?;
                check_file_count(&mut owner, &id, &files)?;
                let accepted = owner
                    .submit(
                        &id,
                        crate::control::TorrentCommand::SetFiles(files.indices()?),
                        Some(revision),
                    )
                    .map_err(Error::Command)?;
                self.record_acceptance(ticket, &mut owner, &accepted.torrent_id);
                Ok(Reply::Accepted(accepted))
            }
            Action::Priorities {
                id,
                files,
                revision,
                file_revision,
            } => {
                check_file_revision(&mut owner, &id, revision, file_revision)?;
                check_file_count(&mut owner, &id, &files)?;
                let next = next_native_revision(&owner)?;
                owner
                    .set_file_priorities(&id, files.indices()?)
                    .map_err(Error::Failed)?;
                publish_native_revision(&mut owner, &id, next)?;
                self.record_acceptance(ticket, &mut owner, &id);
                Ok(Reply::Done("File priorities queued".into()))
            }
            Action::Retry { id, revision } => {
                check_revision(&owner, &id, revision)?;
                owner
                    .retry_start(
                        &id,
                        &self
                            .settings
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()),
                    )
                    .map_err(Error::Failed)?;
                self.record_acceptance(ticket, &mut owner, &id);
                Ok(Reply::Done("Start queued".into()))
            }
            Action::Storage {
                id,
                action,
                revision,
            } => {
                check_revision(&owner, &id, revision)?;
                let next = next_native_revision(&owner)?;
                match action {
                    StorageAction::Select { directory, copy } => {
                        owner.select_storage(&id, directory, copy)
                    }
                    StorageAction::Cancel => owner.cancel_storage(&id),
                    StorageAction::Priorities { .. } => {
                        return Err(Error::Failed(
                            "File priorities require a native file generation fence".into(),
                        ));
                    }
                    StorageAction::Cleanup => owner.remove_copy_originals(&id),
                    StorageAction::Move { parent } => owner.move_storage(&id, parent),
                }
                .map_err(Error::Failed)?;
                publish_native_revision(&mut owner, &id, next)?;
                self.record_acceptance(ticket, &mut owner, &id);
                Ok(Reply::Done("Storage command queued".into()))
            }
            Action::Interest { id, files, peers } => {
                owner
                    .backend
                    .set_detail_interest_for(
                        &client,
                        id.map(|id| (id, crate::session::DetailInterest { files, peers })),
                    )
                    .map_err(Error::Failed)?;
                Ok(Reply::Done(String::new()))
            }
            Action::ConfigureAgents { download_root } => {
                let agent = owner
                    .agent
                    .as_ref()
                    .ok_or_else(|| Error::Failed("Owner access store unavailable".into()))?;
                if agent.download_root != download_root {
                    return Err(Error::Stale);
                }
                Ok(Reply::Done(String::new()))
            }
            Action::AgentSettings(settings) => owner
                .set_agent_settings(&settings)
                .map(|()| Reply::Done(String::new()))
                .map_err(|error| Error::Failed(format!("{error:?}"))),
            Action::Access(action) => access(&mut owner, action).map(Reply::Access),
            Action::SpeedState => Ok(Reply::SpeedState(self.speed_view(&owner))),
            Action::SetSpeedPolicy(policy) => {
                // Persist only the policy flags. Other applied-only changes
                // and another frontend's unsaved drafts must remain separate.
                let mut desired = self
                    .settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                desired.speed.set_policy(policy);
                let plan = SpeedPlan::from_settings(&desired).map_err(Error::Failed)?;
                let path = self.directory.join("settings.toml");
                let mut persisted = ClientSettings::load_from(&path)
                    .map_err(|error| Error::Failed(error.to_string()))?;
                persisted.speed.set_policy(policy);
                SpeedPlan::from_settings(&persisted).map_err(Error::Failed)?;
                persisted
                    .save_to(&path)
                    .map_err(|error| Error::Failed(error.to_string()))?;
                self.record_saved_speed(&persisted);
                if let Some(agent) = owner.agent.as_mut() {
                    agent.client_settings.speed.set_policy(policy);
                }
                *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = desired;
                owner.backend.request_live_speed(plan);
                Ok(Reply::SpeedState(self.speed_view(&owner)))
            }
            Action::SaveView(ui) => {
                let path = self.directory.join("settings.toml");
                let mut persisted = match ClientSettings::load_from(&path) {
                    Ok(settings) => settings,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => self
                        .settings
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                    Err(error) => return Err(Error::Failed(error.to_string())),
                };
                persisted.ui = ui.clone();
                if !persisted.validate().is_empty() {
                    return Err(Error::Failed("Invalid saved view".into()));
                }
                persisted
                    .save_to(&path)
                    .map_err(|error| Error::Failed(error.to_string()))?;
                self.record_saved_speed(&persisted);
                self.settings
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .ui = ui.clone();
                if let Some(agent) = owner.agent.as_mut() {
                    agent.client_settings.ui = ui;
                }
                Ok(Reply::Done("View saved".into()))
            }
            Action::SaveSettings(settings) | Action::ApplySettings(settings) => {
                if !settings.validate().is_empty() {
                    return Err(Error::Failed("Invalid preferences".into()));
                }
                if !std::path::Path::new(&settings.download.download_directory).is_absolute() {
                    return Err(Error::Failed(
                        "Background ownership requires an absolute download directory".into(),
                    ));
                }
                crate::network_settings::options(&settings).map_err(Error::Failed)?;
                let speed_plan = SpeedPlan::from_settings(&settings).map_err(Error::Failed)?;
                // Changing a default directory applies to new registrations.
                // Persist the actual inherited location of existing engines
                // before replacing that default, even if the TOML write fails.
                let previous = self
                    .settings
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if previous.download.download_directory != settings.download.download_directory {
                    let next = next_native_revision(&owner)?;
                    for index in 0..owner.torrents.len() {
                        if owner.torrents[index].overrides.download_directory.is_none() {
                            owner.torrents[index].overrides.download_directory =
                                Some(previous.download.download_directory.clone());
                            let Controller {
                                backend, torrents, ..
                            } = &mut *owner;
                            if let Err(error) = backend.persist_torrent_update(
                                &torrents[index],
                                crate::state::PersistenceIntent::STORAGE,
                                None,
                            ) {
                                torrents[index].overrides.download_directory = None;
                                return Err(Error::Failed(error));
                            }
                            let id = torrents[index].info_hashes.join(";");
                            publish_native_revision(&mut owner, &id, next)?;
                        }
                    }
                }
                if save_preferences {
                    settings
                        .save_to(&self.directory.join("settings.toml"))
                        .map_err(|error| Error::Failed(error.to_string()))?;
                    self.record_saved_speed(&settings);
                }
                owner
                    .set_agent_settings(&settings)
                    .map_err(|error| Error::Failed(format!("{error:?}")))?;
                *self
                    .settings
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = settings;
                owner.backend.request_live_speed(speed_plan);
                Ok(Reply::Done(if save_preferences {
                    "Preferences saved. Check effective speed for engine confirmation.".into()
                } else {
                    "Preferences accepted. Check effective speed for engine confirmation.".into()
                }))
            }
            Action::SaveEditable {
                id,
                revision,
                trackers,
                audit,
            } => {
                check_revision(&owner, &id, revision)?;
                let next = next_native_revision(&owner)?;
                owner
                    .backend
                    .save_tracker_configuration(&id, &trackers, audit.as_ref())
                    .map_err(Error::Failed)?;
                if let Some(index) = owner.torrent_index(&id) {
                    owner.torrents[index].trackers = trackers;
                }
                publish_native_revision(&mut owner, &id, next)?;
                Ok(Reply::Done("Tracker presentation saved".into()))
            }
            Action::SaveAudit {
                id,
                revision,
                audit,
            } => {
                check_revision(&owner, &id, revision)?;
                let next = next_native_revision(&owner)?;
                owner
                    .backend
                    .save_tracker_audit(&id, audit.as_ref())
                    .map_err(Error::Failed)?;
                publish_native_revision(&mut owner, &id, next)?;
                Ok(Reply::Done("Tracker audit settings saved".into()))
            }
            Action::Journal { id, offset, limit } => {
                if limit == 0 || limit > 32 {
                    return Err(Error::TooLarge);
                }
                let hash = journal_hash(&mut owner, id.as_deref())?;
                let journal = owner
                    .backend
                    .journal()
                    .ok_or_else(|| Error::Failed("Journal unavailable".into()))?;
                let page = journal.page(hash.as_deref(), offset, limit);
                Ok(Reply::Journal {
                    entries: page
                        .entries
                        .into_iter()
                        .map(|entry| (*entry).clone())
                        .collect(),
                    total: page.total,
                    path: journal.path(),
                    error: journal.error(),
                })
            }
            Action::Stop => Ok(Reply::Done(
                "Background shutdown requested; waiting for final persistence".into(),
            )),
            Action::Add(_)
            | Action::Export { .. }
            | Action::Trash { .. }
            | Action::PrepareTrash { .. }
            | Action::Operations { .. } => unreachable!(),
        };
        if mutating {
            owner.owner_wake().notify_one();
        }
        result
    }
}

fn check_revision(owner: &Controller, id: &str, revision: u64) -> Result<(), Error> {
    if owner.command_revision(id).map_err(Error::Command)? != revision {
        return Err(Error::Stale);
    }
    if owner.is_busy(id) {
        return Err(Error::Busy);
    }
    Ok(())
}

fn check_file_revision(
    owner: &mut Controller,
    id: &str,
    revision: u64,
    file_revision: u64,
) -> Result<(), Error> {
    check_revision(owner, id, revision)?;
    let index = owner.torrent_index(id).ok_or(Error::Stale)?;
    if file_revision == 0
        || file_revision == u64::MAX
        || owner.torrents[index].files.metadata_revision() != file_revision
    {
        return Err(Error::Stale);
    }
    Ok(())
}

fn check_file_count(owner: &mut Controller, id: &str, files: &PackedIndices) -> Result<(), Error> {
    let index = owner.torrent_index(id).ok_or(Error::Stale)?;
    if files.count != owner.torrents[index].files.file_count() {
        return Err(Error::Stale);
    }
    Ok(())
}

fn next_native_revision(owner: &Controller) -> Result<u64, Error> {
    owner.command_clock.checked_add(1).ok_or(Error::Command(
        crate::control::CommandError::RevisionExhausted,
    ))
}

fn journal_hash(owner: &mut Controller, id: Option<&str>) -> Result<Option<String>, Error> {
    let Some(id) = id else {
        return Ok(None);
    };
    owner.reconcile();
    let index = owner
        .torrent_indices
        .get(id)
        .copied()
        .or_else(|| {
            owner.torrents.iter().position(|item| {
                item.info_hashes.iter().any(|hash| {
                    hash.strip_prefix("v1:")
                        .is_some_and(|hash| hash.trim().eq_ignore_ascii_case(id))
                })
            })
        })
        .ok_or(Error::Stale)?;
    let hash = owner.torrents[index]
        .info_hashes
        .iter()
        .find_map(|hash| {
            hash.strip_prefix("v1:")
                .map(|hash| hash.trim().to_ascii_lowercase())
        })
        .ok_or_else(|| {
            Error::Failed("Per-torrent tracker journals require a v1 or hybrid torrent".into())
        })?;
    Ok(Some(hash))
}
fn publish_native_revision(owner: &mut Controller, id: &str, next: u64) -> Result<(), Error> {
    let index = owner.torrent_index(id).ok_or(Error::Stale)?;
    let canonical = owner.torrents[index].info_hashes.join(";");
    owner.command_clock = next;
    owner
        .observed
        .get_mut(&canonical)
        .ok_or(Error::Stale)?
        .command_revision = next;
    owner.dirty = true;
    owner.publish();
    Ok(())
}

fn prepare_new(
    request: NewTorrent,
    settings: &ClientSettings,
) -> Result<(TorrentItem, Option<crate::state::StoredTrackerAuditProfile>), Error> {
    let (name, hashes, _trackers, files, is_private) = if request.source.starts_with("magnet:") {
        let magnet = crate::magnet::MagnetLink::parse(&request.source).map_err(Error::Failed)?;
        (
            magnet
                .display_name
                .unwrap_or_else(|| "Magnet metadata".into()),
            magnet.exact_topics,
            magnet.trackers,
            FileSelectionModel::default(),
            false,
        )
    } else {
        let metadata =
            crate::metainfo::TorrentMetainfo::load(std::path::Path::new(&request.source))
                .map_err(Error::Failed)?;
        let hashes = [
            metadata.info_hash_v1_hex.map(|hash| format!("v1: {hash}")),
            metadata.info_hash_v2_hex.map(|hash| format!("v2: {hash}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        (
            metadata.name,
            hashes,
            metadata.trackers,
            FileSelectionModel::from_metainfo_files(metadata.files),
            metadata.is_private,
        )
    };
    if hashes != request.hashes || files.file_count() != request.selected.count {
        return Err(Error::Stale);
    }
    crate::metainfo::validate_peer_id(&request.peer_id).map_err(Error::Failed)?;
    if request.trackers.len() > crate::limits::ResourceLimits::default().max_trackers
        || request.trackers.iter().any(|url| url.len() > 8192)
    {
        return Err(Error::TooLarge);
    }
    let mut item = TorrentItem {
        name,
        source: request.source,
        info_hashes: hashes,
        trackers: request.trackers,
        peer_id: request.peer_id,
        files,
        downloaded_bytes: 0,
        uploaded_bytes: 0,
        download_rate: 0,
        upload_rate: 0,
        peers: 0,
        seeds: 0,
        state: if request.start {
            TorrentState::WaitingForEngine
        } else {
            TorrentState::Paused
        },
        is_private,
        overrides: request.overrides,
        runtime: Default::default(),
        completion: Default::default(),
    };
    let effective = crate::torrent::EffectiveTorrentSettings::resolve(
        settings,
        &item.overrides,
        item.is_private,
    );
    if !std::path::Path::new(&effective.download_directory).is_absolute() {
        return Err(Error::Failed(
            "Background owner requires an absolute torrent download directory".into(),
        ));
    }
    item.overrides.download_directory = Some(effective.download_directory);
    item.files.select_all(false);
    for index in request.selected.indices()? {
        item.files.set_selected(index, true);
    }
    item.files.set_high_priority(&request.high.indices()?);
    Ok((item, request.audit))
}

fn access(owner: &mut Controller, action: AccessAction) -> Result<AccessView, Error> {
    let agent = owner
        .agent_access()
        .ok_or_else(|| Error::Failed("Owner access store unavailable".into()))?;
    let mut token = None;
    match action {
        AccessAction::View => {}
        AccessAction::Enable(enabled) => {
            if enabled {
                for root in [&agent.import_root, &agent.export_root] {
                    std::fs::create_dir_all(root)
                        .map_err(|error| Error::Failed(error.to_string()))?;
                }
            }
            agent.enabled = enabled;
        }
        AccessAction::Grant(grants) => {
            token = Some(
                agent
                    .grant(grants)
                    .map_err(|error| Error::Failed(format!("{error:?}")))?,
            )
        }
        AccessAction::Revoke(id) => agent
            .revoke(&id)
            .map_err(|error| Error::Failed(format!("{error:?}")))?,
        AccessAction::RevokeAll => {
            for client in agent.clients.clone() {
                agent
                    .revoke(&client.id)
                    .map_err(|error| Error::Failed(format!("{error:?}")))?;
            }
        }
        AccessAction::Approve { operation, allow } => agent
            .approve(&operation, allow)
            .map_err(|error| Error::Failed(format!("{error:?}")))?,
    }
    let approvals = agent
        .approvals()
        .into_iter()
        .map(|operation| {
            let description = agent
                .approval_description(&operation.id)
                .unwrap_or_default();
            (operation, description)
        })
        .collect();
    Ok(AccessView {
        enabled: agent.enabled,
        clients: agent.clients.clone(),
        approvals,
        import_root: agent.import_root.clone(),
        download_root: agent.download_root.clone(),
        export_root: agent.export_root.clone(),
        token,
    })
}
