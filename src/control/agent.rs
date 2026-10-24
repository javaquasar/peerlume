//! Authorization and operation coordination inside the authoritative controller.
use super::{
    Controller, MAX_PAGE_SIZE, TorrentCommand,
    access::{self, ClientGrant, Grants},
    operations::{ControlStore, Operation, OperationState, now_ms},
    wire::{ErrorCode, Method, Request, Response},
};
use crate::session::SessionEvent;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

struct Pending {
    operation: Operation,
    method: Method,
    dispatched: bool,
    checking: bool,
}

pub struct AgentService {
    pub enabled: bool,
    pub instance_id: String,
    pub clients: Vec<ClientGrant>,
    pub store: ControlStore,
    pending: BTreeMap<String, Pending>,
    export_jobs: BTreeMap<String, super::export::ExportJob>,
    pub import_root: PathBuf,
    pub export_root: PathBuf,
    pub download_root: PathBuf,
    download_cap: Option<crate::storage::sandbox::Directory>,
    pub(crate) client_settings: crate::settings::ClientSettings,
}

impl AgentService {
    pub fn open(directory: &Path, download_root: PathBuf) -> Result<Self, ErrorCode> {
        std::fs::create_dir_all(directory).map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let store = ControlStore::open(&directory.join("control.sqlite3"))?;
        let clients = {
            let mut query = store
                .connection
                .prepare("SELECT record FROM agent_clients LIMIT 65")
                .map_err(|_| ErrorCode::PersistenceUnavailable)?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|_| ErrorCode::PersistenceUnavailable)?;
            let mut clients = Vec::new();
            for row in rows {
                clients.push(
                    serde_json::from_str(&row.map_err(|_| ErrorCode::PersistenceUnavailable)?)
                        .map_err(|_| ErrorCode::PersistenceUnavailable)?,
                );
            }
            if clients.len() > 64 {
                return Err(ErrorCode::PersistenceUnavailable);
            }
            clients
        };
        Ok(Self {
            enabled: false,
            instance_id: access::random_id().map_err(|_| ErrorCode::PersistenceUnavailable)?,
            clients,
            store,
            pending: BTreeMap::new(),
            export_jobs: BTreeMap::new(),
            import_root: directory.join("imports"),
            export_root: directory.join("exports"),
            download_root,
            download_cap: None,
            client_settings: Default::default(),
        })
    }

    /// Trusted UI only. The raw token is displayed once and never persisted.
    pub fn grant(&mut self, grants: Grants) -> Result<String, ErrorCode> {
        if self.clients.len() >= 64 {
            return Err(ErrorCode::Busy);
        }
        let token = access::random_id().map_err(|_| ErrorCode::PersistenceUnavailable)?;
        let client = ClientGrant {
            id: access::random_id().map_err(|_| ErrorCode::PersistenceUnavailable)?,
            grants,
            token_hash: access::digest(token.as_bytes()),
        };
        self.store
            .connection
            .execute(
                "INSERT INTO agent_clients(id,record) VALUES(?,?)",
                rusqlite::params![
                    client.id,
                    serde_json::to_string(&client)
                        .map_err(|_| ErrorCode::PersistenceUnavailable)?
                ],
            )
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        self.clients.push(client);
        Ok(token)
    }

    pub fn revoke(&mut self, id: &str) -> Result<(), ErrorCode> {
        self.store
            .connection
            .execute("DELETE FROM agent_clients WHERE id=?", [id])
            .map_err(|_| ErrorCode::PersistenceUnavailable)?;
        self.clients.retain(|client| client.id != id);
        for pending in self
            .pending
            .values_mut()
            .filter(|p| p.operation.client_id == id && !p.dispatched)
        {
            pending.operation.state = OperationState::Cancelled;
            self.store.save(&pending.operation)?;
        }
        self.pending
            .retain(|_, p| p.operation.state != OperationState::Cancelled);
        Ok(())
    }

    pub fn approvals(&self) -> Vec<Operation> {
        self.pending
            .values()
            .filter(|p| p.operation.approval_required && !p.dispatched)
            .map(|p| p.operation.clone())
            .collect()
    }

    pub fn approval_description(&self, id: &str) -> Option<String> {
        self.pending.get(id).map(|p| match &p.method {
            Method::Export { destination, .. } => format!(
                "Export redacted disk journal to new file: {}",
                destination.escape_default()
            ),
            Method::Mutate {
                command: TorrentCommand::Remove,
                ..
            } => "Remove from application only; keep all downloaded files".into(),
            _ => "No destructive payload action is supported".into(),
        })
    }

    pub fn approve(&mut self, operation: &str, allow: bool) -> Result<(), ErrorCode> {
        let pending = self
            .pending
            .get_mut(operation)
            .ok_or(ErrorCode::UnknownOperation)?;
        if pending.dispatched || !pending.operation.approval_required {
            return Err(ErrorCode::Conflict);
        }
        if pending.operation.deadline_unix_ms <= now_ms() {
            return Err(ErrorCode::DeadlineExceeded);
        }
        if allow {
            pending.operation.approval_required = false;
        } else {
            pending.operation.state = OperationState::Cancelled;
        }
        self.store.save(&pending.operation)?;
        if !allow {
            self.pending.remove(operation);
        }
        Ok(())
    }

    pub fn busy(&self, id: &str) -> bool {
        self.pending.values().any(|p| {
            p.operation.torrent_id == id
                && p.dispatched
                && !matches!(p.method, Method::Export { .. })
        })
    }

    pub fn request(&mut self, control: &mut Controller, request: Request) -> Response {
        let result = self.authorized(control, &request);
        let mut response = match result {
            Ok(value) => Response::success(value),
            Err(code) => Response::error(code),
        };
        response.request_id = request.request_id;
        response.instance_id.clone_from(&self.instance_id);
        response
    }

    fn authorized(
        &mut self,
        control: &mut Controller,
        request: &Request,
    ) -> Result<Value, ErrorCode> {
        if !self.enabled {
            return Err(ErrorCode::Disabled);
        }
        let client = self
            .clients
            .iter()
            .find(|c| access::matches_token(&c.token_hash, &request.token))
            .cloned()
            .ok_or(ErrorCode::Unauthorized)?;
        if request
            .instance_id
            .as_ref()
            .is_some_and(|id| *id != self.instance_id)
        {
            return Err(ErrorCode::Conflict);
        }
        match &request.method {
            Method::Discover => Ok(json!({"protocol_version": super::PROTOCOL_VERSION,
                "capabilities": ["list", "details", "journal", "tracker_peers", "operations", "mutate", "preview", "add", "export"],
                "grants": client.grants, "limits": {"page_size": MAX_PAGE_SIZE, "request_bytes": super::wire::MAX_REQUEST_BYTES,
                    "response_bytes": super::wire::MAX_RESPONSE_BYTES, "operation_records": super::wire::MAX_OPERATIONS,
                    "archived_operation_records": super::operations::MAX_ARCHIVED_OPERATIONS},
                "units": {"bytes": "bytes", "timestamps": "unix_ms"},
                "payload_deletion": false, "connected_peer_addresses": false,
                "subscriptions": "poll_latest_snapshot_with_revision_and_gap_detection"})),
            Method::List {
                offset,
                limit,
                revision,
                after_revision,
            } => {
                let page = control
                    .read_handle()
                    .page(*offset, *limit, *revision, *after_revision)
                    .map_err(|_| ErrorCode::InvalidPage)?;
                Ok(json!(page))
            }
            Method::Details {
                torrent_id,
                offset,
                limit,
            } => {
                let item = find(control, torrent_id)?;
                let (end, next) = page(*offset, *limit, item.files.files().len())?;
                let files: Vec<_> = item.files.files()[*offset..end].iter().enumerate().map(|(index,file)| json!({
                    "index": offset+index, "size_bytes": file.size_bytes, "downloaded_bytes": file.downloaded_bytes,
                    "selected": file.selected})).collect();
                Ok(
                    json!({"torrent_id": item.info_hashes.join(";"), "command_revision": control.command_revision(torrent_id).ok(),
                    "total_files": item.files.files().len(), "next_offset": next, "files": files,
                    "verify_after_download": item.overrides.verify_after_download,
                    "repair_policy": item.overrides.repair_policy,
                    "repair_state": match item.runtime.repair { crate::runtime_status::RepairStatus::Idle => "idle",
                        crate::runtime_status::RepairStatus::Downloading{..} => "downloading", crate::runtime_status::RepairStatus::Checking{..} => "checking",
                        crate::runtime_status::RepairStatus::Succeeded{..} => "succeeded", crate::runtime_status::RepairStatus::Exhausted{..} => "exhausted" }}),
                )
            }
            Method::Journal {
                torrent_id,
                offset,
                limit,
            } => {
                let item = find(control, torrent_id)?;
                if *limit == 0 || *limit > 32 {
                    return Err(ErrorCode::InvalidPage);
                }
                let hash = crate::announce_journal::torrent_hash(&item.info_hashes)
                    .ok_or(ErrorCode::Unsupported)?;
                let journal = control.backend.journal().ok_or(ErrorCode::Unsupported)?;
                let data = journal.page(Some(hash), *offset, *limit);
                if *offset > data.total {
                    return Err(ErrorCode::InvalidPage);
                }
                let entries: Vec<_> = data
                    .entries
                    .iter()
                    .map(|entry| safe_announce(entry))
                    .collect();
                Ok(json!({"entries": entries, "retained_total": data.total,
                    "history": "bounded_live_tail_newest_first", "cursor": "run_id_and_sequence",
                    "next_offset": (*offset+entries.len() < data.total).then_some(*offset+entries.len())}))
            }
            Method::Peers {
                torrent_id,
                offset,
                limit,
            } => {
                let item = find(control, torrent_id)?;
                let hash = crate::announce_journal::torrent_hash(&item.info_hashes)
                    .ok_or(ErrorCode::Unsupported)?;
                let latest = control.backend.journal().and_then(|j| j.latest(hash));
                let peers = latest
                    .as_ref()
                    .map(|entry| entry.announce.reply.peers.as_slice())
                    .unwrap_or(&[]);
                let (end, next) = page(*offset, *limit, peers.len())?;
                Ok(
                    json!({"source": "latest_tracker_reply", "tracker_returned": &peers[*offset..end],
                    "connected_count": item.peers, "connected_addresses": null, "next_offset": next,
                    "reply_sequence": latest.as_ref().map(|entry| entry.sequence)}),
                )
            }
            Method::Operations { offset, limit } => {
                let (operations, total) = self.store.page(&client.id, *offset, *limit)?;
                let next =
                    (*offset + operations.len() < total).then_some(*offset + operations.len());
                Ok(
                    json!({"operations": operations.iter().map(operation_json).collect::<Vec<_>>(), "total":total, "next_offset": next}),
                )
            }
            Method::Operation { operation_id } => {
                let op = self
                    .store
                    .operation(&client.id, operation_id)?
                    .ok_or(ErrorCode::UnknownOperation)?;
                let mut value = operation_json(&op);
                value["exported_entries"] = json!(
                    self.export_jobs
                        .get(operation_id)
                        .map(|job| job.entries.load(std::sync::atomic::Ordering::Relaxed))
                );
                value["verification_progress"]=control.observed.get(&op.torrent_id).map(|row|json!({"checked_bytes":row.checked_bytes,"total_bytes":row.check_total_bytes,"completion":row.completion})).unwrap_or(Value::Null);
                Ok(value)
            }
            Method::Cancel { operation_id } => {
                let pending = self
                    .pending
                    .get(operation_id)
                    .ok_or(ErrorCode::UnknownOperation)?;
                if pending.operation.client_id != client.id {
                    return Err(ErrorCode::UnknownOperation);
                }
                if pending.dispatched {
                    return Err(ErrorCode::ReconciliationRequired);
                }
                let mut op = pending.operation.clone();
                op.state = OperationState::Cancelled;
                self.store.save(&op)?;
                self.pending.remove(operation_id);
                Ok(operation_json(&op))
            }
            Method::Preview { source } => {
                if !client.grants.manage {
                    return Err(ErrorCode::PermissionDenied);
                }
                let meta =
                    super::filesystem::Root::open(&self.import_root)?.read_metainfo(source)?;
                Ok(
                    json!({"total_bytes": meta.total_size(), "file_count": meta.files.len(), "private": meta.is_private,
                    "info_hash_v1": meta.info_hash_v1_hex, "info_hash_v2": meta.info_hash_v2_hex,
                    "tracker_count": meta.trackers.len()}),
                )
            }
            method => {
                if request.instance_id.as_deref() != Some(&self.instance_id) {
                    return Err(ErrorCode::Conflict);
                }
                let (target, key, deadline, approval, allowed, name) = match method {
                    Method::Mutate {
                        torrent_id,
                        command,
                        idempotency_key,
                        deadline_unix_ms,
                        ..
                    } => {
                        let allowed = match command {
                            TorrentCommand::SetMode(_) => client.grants.reporting,
                            TorrentCommand::Remove => client.grants.remove,
                            _ => client.grants.manage,
                        };
                        (
                            torrent_id.as_str(),
                            idempotency_key.as_str(),
                            *deadline_unix_ms,
                            matches!(command, TorrentCommand::Remove),
                            allowed,
                            command_name(command),
                        )
                    }
                    Method::Add {
                        idempotency_key,
                        deadline_unix_ms,
                        ..
                    } => (
                        "",
                        idempotency_key.as_str(),
                        *deadline_unix_ms,
                        false,
                        client.grants.manage,
                        "add",
                    ),
                    Method::Export {
                        torrent_id,
                        idempotency_key,
                        deadline_unix_ms,
                        ..
                    } => (
                        torrent_id.as_str(),
                        idempotency_key.as_str(),
                        *deadline_unix_ms,
                        true,
                        client.grants.export,
                        "export",
                    ),
                    _ => return Err(ErrorCode::Unsupported),
                };
                if !allowed {
                    self.store
                        .audit(&client.id, &request.request_id, "denied")?;
                    return Err(ErrorCode::PermissionDenied);
                }
                if key.is_empty() || key.len() > 128 {
                    return Err(ErrorCode::InvalidRequest);
                }
                if matches!(method,Method::Mutate{command:TorrentCommand::SetFiles(files),..} if files.len()>4096)
                {
                    return Err(ErrorCode::TooLarge);
                }
                let fingerprint = access::digest(
                    &serde_json::to_vec(method).map_err(|_| ErrorCode::InvalidRequest)?,
                );
                if let Some(previous) = self.store.lookup(&client.id, key)? {
                    if previous.fingerprint != fingerprint {
                        return Err(ErrorCode::IdempotencyConflict);
                    }
                    return Ok(operation_json(&previous));
                }
                if deadline <= now_ms() || deadline > now_ms().saturating_add(24 * 60 * 60 * 1000) {
                    return Err(ErrorCode::DeadlineExceeded);
                }
                if !target.is_empty() {
                    let item = find(control, target)?;
                    if item.info_hashes.join(";") != target {
                        return Err(ErrorCode::InvalidRequest);
                    }
                    if self
                        .pending
                        .values()
                        .any(|p| p.operation.torrent_id == target)
                    {
                        return Err(ErrorCode::Busy);
                    }
                }
                if let Method::Mutate {
                    torrent_id,
                    expected_revision,
                    ..
                } = method
                    && control
                        .command_revision(torrent_id)
                        .map_err(|_| ErrorCode::UnknownTorrent)?
                        != *expected_revision
                {
                    return Err(ErrorCode::Conflict);
                }
                self.store
                    .audit(&client.id, &request.request_id, "authorized")?;
                let op = self.store.create(
                    (&client.id, target, name),
                    key,
                    &fingerprint,
                    deadline,
                    approval,
                )?;
                self.pending.insert(
                    op.id.clone(),
                    Pending {
                        operation: op.clone(),
                        method: method.clone(),
                        dispatched: false,
                        checking: false,
                    },
                );
                Ok(operation_json(&op))
            }
        }
    }

    pub fn tick(&mut self, control: &mut Controller) {
        let completed: Vec<_> = self
            .export_jobs
            .iter()
            .filter_map(|(id, job)| match job.receiver.try_recv() {
                Ok(result) => Some((id.clone(), result)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some((id.clone(), Err(ErrorCode::ReconciliationRequired)))
                }
                _ => None,
            })
            .collect();
        for (id, result) in completed {
            self.export_jobs.remove(&id);
            if let Some(pending) = self.pending.get_mut(&id) {
                pending.operation.state = if result.is_ok() {
                    OperationState::Succeeded
                } else {
                    OperationState::Failed
                };
                pending.operation.error = result.err();
            }
        }
        let ids: Vec<_> = self.pending.keys().cloned().collect();
        for id in ids {
            let mut pending = self.pending.remove(&id).expect("key exists");
            if !matches!(
                pending.operation.state,
                OperationState::Queued | OperationState::Running
            ) {
                if self.store.save(&pending.operation).is_err() {
                    self.pending.insert(id, pending);
                }
                continue;
            }
            if pending.operation.deadline_unix_ms <= now_ms() {
                pending.operation.state = if pending.dispatched {
                    OperationState::ReconciliationRequired
                } else {
                    OperationState::Cancelled
                };
                pending.operation.error = Some(if pending.dispatched {
                    ErrorCode::ReconciliationRequired
                } else {
                    ErrorCode::DeadlineExceeded
                });
                if self.store.save(&pending.operation).is_err() {
                    self.pending.insert(id, pending);
                }
                continue;
            }
            if pending.dispatched || pending.operation.approval_required || !self.enabled {
                self.pending.insert(id, pending);
                continue;
            }
            // Write the ambiguous boundary BEFORE enqueueing. A crash can never trigger replay.
            pending.operation.state = OperationState::Running;
            if self.store.save(&pending.operation).is_err() {
                self.pending.insert(id, pending);
                continue;
            }
            pending.dispatched = true;
            let result = self.dispatch(control, &mut pending);
            match result {
                Ok(true) => {
                    pending.dispatched = true;
                    self.pending.insert(id, pending);
                }
                Ok(false) => {
                    pending.operation.state = OperationState::Succeeded;
                    if self.store.save(&pending.operation).is_err() {
                        self.pending.insert(id, pending);
                    }
                }
                Err(error) => {
                    pending.operation.state = OperationState::Failed;
                    pending.operation.error = Some(error);
                    if self.store.save(&pending.operation).is_err() {
                        self.pending.insert(id, pending);
                    }
                }
            }
        }
    }

    fn dispatch(
        &mut self,
        control: &mut Controller,
        pending: &mut Pending,
    ) -> Result<bool, ErrorCode> {
        match &pending.method {
            Method::Mutate {
                torrent_id,
                command,
                expected_revision,
                ..
            } => {
                control
                    .submit(torrent_id, command.clone(), Some(*expected_revision))
                    .map_err(|error| match error {
                        super::CommandError::RevisionConflict { .. } => ErrorCode::Conflict,
                        super::CommandError::PersistenceUnavailable => {
                            ErrorCode::PersistenceUnavailable
                        }
                        _ => ErrorCode::EngineRejected,
                    })?;
                if matches!(command, TorrentCommand::Remove) {
                    control.pending_removals.insert(torrent_id.clone());
                }
                Ok(true)
            }
            Method::Add {
                source,
                destination,
                start,
                ..
            } => {
                let (bytes, meta) = super::filesystem::Root::open(&self.import_root)?
                    .read_metainfo_bytes(source)?;
                if self.download_cap.is_none() {
                    self.download_cap = Some(
                        crate::storage::sandbox::Directory::open(&self.download_root)
                            .map_err(|_| ErrorCode::PathDenied)?,
                    );
                }
                let directory = self
                    .download_cap
                    .as_ref()
                    .ok_or(ErrorCode::PathDenied)?
                    .descendant(destination)
                    .map_err(|_| ErrorCode::PathDenied)?;
                let path = self.import_root.join(source);
                let output = self.download_root.join(destination);
                let settings = self.client_settings.clone();
                let mut item = crate::torrent::TorrentItem {
                    runtime: Default::default(),
                    completion: crate::completion::CompletionStatus::Pending,
                    name: meta.name,
                    source: path.display().to_string(),
                    info_hashes: [
                        meta.info_hash_v1_hex.map(|h| format!("v1: {h}")),
                        meta.info_hash_v2_hex.map(|h| format!("v2: {h}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                    trackers: meta.trackers,
                    peer_id: crate::metainfo::generate_peer_id(&settings.bittorrent.peer_id_prefix),
                    files: crate::file_selection::FileSelectionModel::from_metainfo_files(
                        meta.files,
                    ),
                    downloaded_bytes: 0,
                    uploaded_bytes: 0,
                    download_rate: 0,
                    upload_rate: 0,
                    peers: 0,
                    seeds: 0,
                    state: crate::torrent::TorrentState::Paused,
                    is_private: meta.is_private,
                    overrides: crate::torrent::TorrentOverrides {
                        download_directory: Some(output.display().to_string()),
                        ..Default::default()
                    },
                };
                if *start {
                    item.state = crate::torrent::TorrentState::WaitingForEngine;
                }
                if !settings.download.select_all_files {
                    item.files.select_all(false);
                }
                pending.operation.torrent_id = item.info_hashes.join(";");
                self.store.save(&pending.operation)?;
                control
                    .add_prepared(
                        item,
                        &settings,
                        crate::session::PreparedTorrent {
                            bytes: bytes.into(),
                            directory,
                        },
                    )
                    .map_err(|_| ErrorCode::EngineRejected)?;
                Ok(true)
            }
            Method::Export {
                torrent_id,
                destination,
                ..
            } => {
                let item = find(control, torrent_id)?;
                let hash = crate::announce_journal::torrent_hash(&item.info_hashes)
                    .ok_or(ErrorCode::Unsupported)?;
                let journal = control.backend.journal().ok_or(ErrorCode::Unsupported)?;
                if self.export_jobs.len() >= 2 {
                    return Err(ErrorCode::Busy);
                }
                if journal.path().is_none() {
                    return Err(ErrorCode::Unsupported);
                }
                let file =
                    super::filesystem::Root::open(&self.export_root)?.create_export(destination)?;
                let job = super::export::start(
                    journal,
                    hash.into(),
                    file,
                    pending.operation.deadline_unix_ms,
                )?;
                self.export_jobs.insert(pending.operation.id.clone(), job);
                Ok(true)
            }
            _ => Err(ErrorCode::Unsupported),
        }
    }

    /// Called only for actual engine acknowledgements, never telemetry guesses.
    pub fn event(&mut self, event: &SessionEvent, control: &mut Controller) {
        let mut complete = Vec::new();
        for (id, pending) in &mut self.pending {
            if !pending.dispatched {
                continue;
            }
            let target = &pending.operation.torrent_id;
            let mut failure = false;
            let confirmed = match (&pending.method, event) {
                (_, SessionEvent::Error { id, .. })
                    if id == target && !matches!(pending.method, Method::Export { .. }) =>
                {
                    failure = true;
                    true
                }
                (Method::Add { .. }, SessionEvent::Added { id }) => id == target,
                (
                    Method::Mutate {
                        command: TorrentCommand::Remove,
                        ..
                    },
                    SessionEvent::Removed { id },
                ) => id == target,
                (
                    Method::Mutate {
                        command: TorrentCommand::Pause,
                        ..
                    },
                    SessionEvent::StateChanged { id, state },
                ) => id == target && *state == "paused",
                (
                    Method::Mutate {
                        command: TorrentCommand::Resume,
                        ..
                    },
                    SessionEvent::StateChanged { id, state },
                ) => id == target && *state == "downloading",
                (
                    Method::Mutate {
                        command: TorrentCommand::SetFiles(files),
                        ..
                    },
                    SessionEvent::FilesApplied { id, only_files },
                ) => {
                    id == target && {
                        let mut files = files.clone();
                        files.sort_unstable();
                        files == *only_files
                    }
                }
                (
                    Method::Mutate {
                        command: TorrentCommand::SetVerifyAfterDownload(enabled),
                        ..
                    },
                    SessionEvent::VerifyAfterDownload {
                        id,
                        enabled: actual,
                    },
                ) => {
                    id == target
                        && (enabled == actual
                            || (*actual
                                && control.torrents.iter().any(|torrent| {
                                    torrent.info_hashes.join(";") == *target
                                        && torrent.overrides.repair_policy.enabled
                                })))
                }
                (
                    Method::Mutate {
                        command: TorrentCommand::SetRepairPolicy(policy),
                        ..
                    },
                    SessionEvent::RepairPolicy { id, policy: actual },
                ) => id == target && policy == actual,
                (
                    Method::Mutate {
                        command: TorrentCommand::SetMode(config),
                        ..
                    },
                    SessionEvent::Mode {
                        id,
                        application:
                            crate::runtime_status::ModeApplication::Failed { requested, .. },
                    },
                ) if id == target && config == requested => {
                    failure = true;
                    true
                }
                (
                    Method::Mutate {
                        command: TorrentCommand::SetMode(config),
                        ..
                    },
                    SessionEvent::Mode {
                        id,
                        application:
                            crate::runtime_status::ModeApplication::Active { config: actual, .. },
                    },
                ) => id == target && config == actual,
                (
                    Method::Mutate {
                        command: TorrentCommand::Recheck,
                        ..
                    },
                    SessionEvent::Verification { id, status },
                ) if id == target => match status {
                    crate::completion::CompletionStatus::Checking { .. } => {
                        pending.checking = true;
                        false
                    }
                    crate::completion::CompletionStatus::Verified => pending.checking,
                    crate::completion::CompletionStatus::Failed { .. } => {
                        failure = true;
                        true
                    }
                    _ => false,
                },
                _ => false,
            };
            if confirmed {
                complete.push((id.clone(), failure));
            }
        }
        for (id, failure) in complete {
            let mut pending = self.pending.remove(&id).expect("exists");
            let persisted = if matches!(
                pending.method,
                Method::Mutate {
                    command: TorrentCommand::Remove,
                    ..
                }
            ) && control
                .pending_removals
                .contains(&pending.operation.torrent_id)
            {
                false
            } else if control
                .unpersisted_updates
                .contains(&pending.operation.torrent_id)
            {
                control
                    .persist_preserving_audit(&pending.operation.torrent_id)
                    .is_ok()
            } else {
                true
            };
            pending.operation.state = if !persisted {
                OperationState::ReconciliationRequired
            } else if failure {
                OperationState::Failed
            } else {
                OperationState::Succeeded
            };
            pending.operation.error = if !persisted {
                Some(ErrorCode::PersistenceUnavailable)
            } else if failure {
                Some(ErrorCode::EngineRejected)
            } else {
                None
            };
            if self.store.save(&pending.operation).is_err() {
                self.pending.insert(id, pending);
            }
        }
    }
}

fn find<'a>(
    control: &'a Controller,
    id: &str,
) -> Result<&'a crate::torrent::TorrentItem, ErrorCode> {
    control
        .torrents
        .iter()
        .find(|item| item.info_hashes.join(";") == id)
        .ok_or(ErrorCode::UnknownTorrent)
}
fn page(offset: usize, limit: usize, total: usize) -> Result<(usize, Option<usize>), ErrorCode> {
    if limit == 0 || limit > MAX_PAGE_SIZE || offset > total {
        return Err(ErrorCode::InvalidPage);
    }
    let end = offset.saturating_add(limit).min(total);
    Ok((end, (end < total).then_some(end)))
}
pub(crate) fn operation_json(op: &Operation) -> Value {
    json!({"operation_id":op.id,"torrent_id":op.torrent_id,"command":op.command,"created_unix_ms":op.created_unix_ms,
        "deadline_unix_ms":op.deadline_unix_ms,"state":op.state,"approval_required":op.approval_required,"error":op.error})
}
pub(crate) fn safe_announce(entry: &crate::announce_journal::JournalEntry) -> Value {
    let a = &entry.announce;
    json!({"run_id":entry.run_id,"sequence":entry.sequence,"started_unix_ms":a.started_unix_ms,"elapsed_ms":a.elapsed_ms,
        "actual":a.actual,"sent":a.sent,"verified_bytes":a.verified_bytes,"full_size":a.full_size,
        "request_transmitted":a.request_transmitted,"event":match a.event.as_deref(){Some("started")=>Some("started"),Some("completed")=>Some("completed"),Some("stopped")=>Some("stopped"),_=>None},
        "http_status":a.reply.http_status,"interval_seconds":a.reply.interval,"min_interval_seconds":a.reply.min_interval,
        "seeders":a.reply.seeders,"leechers":a.reply.leechers,"tracker_returned_peer_count":a.reply.peers.len(),
        "warning_present":a.reply.warning.is_some(),"failure_present":a.reply.failure.is_some()})
}
fn command_name(command: &TorrentCommand) -> &'static str {
    match command {
        TorrentCommand::Pause => "pause",
        TorrentCommand::Resume => "resume",
        TorrentCommand::Recheck => "recheck",
        TorrentCommand::SetMode(_) => "set_mode",
        TorrentCommand::SetRepairPolicy(_) => "set_repair_policy",
        TorrentCommand::SetVerifyAfterDownload(_) => "set_verify_after_download",
        TorrentCommand::SetFiles(_) => "set_files",
        TorrentCommand::Remove => "remove",
    }
}

#[cfg(test)]
mod tests;
