use std::{collections::HashMap, path::PathBuf, sync::Arc, thread, time::Duration};

use crate::completion::CompletionStatus;
use crate::{announce_journal::AnnounceJournal, download_mode::DownloadMode};
use librqbit::{
    AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions, TorrentStatsState,
};
#[cfg(test)]
use std::sync::mpsc;
use tokio::sync::mpsc as tokio_mpsc;
mod verification;
use crate::runtime_status::{
    ModeApplication, ModeConfig, RepairBudget, RepairPolicy, RepairStatus,
};
use librqbit::storage::StorageFactoryExt;
use verification::{CheckRun, restart_for_check};
mod controls;
mod events;

/// One validated metainfo snapshot and the authorized output directory identity.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedTorrent {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) directory: crate::storage::sandbox::Directory,
}
impl std::fmt::Debug for PreparedTorrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedTorrent")
            .field("bytes", &self.bytes.len())
            .field("directory", &self.directory)
            .finish()
    }
}

fn read_metainfo_snapshot(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let limit = crate::limits::ResourceLimits::default().max_torrent_bytes;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    crate::metainfo::TorrentMetainfo::parse(&bytes).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid metainfo snapshot")
    })?;
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    SelectStorage {
        id: String,
        directory: String,
        copy: bool,
    },
    CancelStorage {
        id: String,
    },
    PreparedAdd {
        command: Box<SessionCommand>,
        prepared: PreparedTorrent,
    },
    AddTorrent {
        resume: crate::resume::ResumeCounters,
        transfer: crate::transfer_settings::TransferSettings,
        repair_policy: RepairPolicy,
        verify_after_download: bool,
        download_mode: DownloadMode,
        freeze_after_bytes: u64,
        upload_ratio: crate::download_mode::UploadRatioRange,
        full_size: u64,
        id: String,
        source: String,
        output_folder: String,
        only_files: Option<Vec<usize>>,
        trackers: Vec<String>,
        paused: bool,
        peer_limit: Option<usize>,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Remove {
        id: String,
    },
    Recheck {
        id: String,
    },
    RecheckAndRepair {
        id: String,
    },
    SetVerifyAfterDownload {
        id: String,
        enabled: bool,
    },
    SetFiles {
        id: String,
        only_files: Vec<usize>,
    },
    SetDownloadMode {
        id: String,
        config: ModeConfig,
    },
    SetRepairPolicy {
        id: String,
        policy: RepairPolicy,
    },
    Shutdown,
    ConfigureQueue {
        max_active_downloads: usize,
    },
    SetTransferSettings {
        id: String,
        settings: crate::transfer_settings::TransferSettings,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Storage {
        id: String,
        status: crate::storage::relocation::StorageStatus,
    },
    Peers {
        id: String,
        peers: Vec<crate::diagnostics::PeerSnapshot>,
    },
    TransferSettingsApplied {
        id: String,
        settings: crate::transfer_settings::TransferSettings,
    },
    VerifyAfterDownload {
        id: String,
        enabled: bool,
    },
    FilesApplied {
        id: String,
        only_files: Vec<usize>,
    },
    Mode {
        id: String,
        application: ModeApplication,
    },
    RepairPolicy {
        id: String,
        policy: RepairPolicy,
    },
    Repair {
        id: String,
        status: RepairStatus,
    },
    NetworkTotals {
        seeded_seconds: u64,
        id: String,
        uploaded: u64,
        downloaded: u64,
    },
    Verification {
        id: String,
        status: crate::completion::CompletionStatus,
    },
    Added {
        id: String,
    },
    StateChanged {
        id: String,
        state: &'static str,
    },
    Stats {
        id: String,
        downloaded_bytes: u64,
        uploaded_bytes: u64,
        download_rate: u64,
        upload_rate: u64,
        file_progress: Vec<u64>,
        peers: usize,
        finished: bool,
        paused: bool,
    },
    Error {
        id: String,
        message: String,
    },
    Removed {
        id: String,
    },
    Stopped,
}

impl SessionCommand {
    fn target_id(&self) -> Option<&str> {
        match self {
            Self::AddTorrent { id, .. }
            | Self::Pause { id }
            | Self::Resume { id }
            | Self::Remove { id }
            | Self::Recheck { id }
            | Self::RecheckAndRepair { id }
            | Self::SetVerifyAfterDownload { id, .. }
            | Self::SetFiles { id, .. }
            | Self::SetDownloadMode { id, .. }
            | Self::SetRepairPolicy { id, .. }
            | Self::SetTransferSettings { id, .. }
            | Self::SelectStorage { id, .. }
            | Self::CancelStorage { id } => Some(id),
            Self::PreparedAdd { command, .. } => command.target_id(),
            Self::Shutdown | Self::ConfigureQueue { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSendError {
    Unavailable,
    Full,
    Closed,
}

fn send_command(
    sender: &tokio_mpsc::Sender<SessionCommand>,
    command: SessionCommand,
) -> Result<(), SessionSendError> {
    sender.try_send(command).map_err(|error| match error {
        tokio_mpsc::error::TrySendError::Full(_) => SessionSendError::Full,
        tokio_mpsc::error::TrySendError::Closed(_) => SessionSendError::Closed,
    })
}

pub struct SessionRuntime {
    pub journal: AnnounceJournal,
    commands: tokio_mpsc::Sender<SessionCommand>,
    events: events::Receiver,
    thread: Option<thread::JoinHandle<()>>,
    #[cfg(test)]
    test_engine: Arc<std::sync::Mutex<std::sync::Weak<Session>>>,
}

impl SessionRuntime {
    pub fn start(
        capacity: usize,
        default_output_folder: PathBuf,
        disable_trackers: bool,
    ) -> Result<Self, String> {
        if capacity == 0 {
            return Err("channel capacity must be greater than zero".into());
        }
        Self::start_with_journal(
            capacity,
            default_output_folder,
            session_options(disable_trackers),
            AnnounceJournal::open(&crate::settings::data_directory().join("announce-logs")),
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_options(
        capacity: usize,
        default_output_folder: PathBuf,
        options: SessionOptions,
    ) -> Result<Self, String> {
        Self::start_with_journal(
            capacity,
            default_output_folder,
            options,
            AnnounceJournal::memory(),
        )
    }

    fn start_with_journal(
        capacity: usize,
        default_output_folder: PathBuf,
        mut options: SessionOptions,
        journal: AnnounceJournal,
    ) -> Result<Self, String> {
        if capacity == 0 {
            return Err("channel capacity must be greater than zero".into());
        }
        let (commands, mut command_rx) = tokio_mpsc::channel(capacity);
        let (event_tx, events) = events::channel(capacity);
        let (ready, initialized) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
        let thread_journal = journal.clone();
        #[cfg(test)]
        let test_engine = Arc::new(std::sync::Mutex::new(std::sync::Weak::new()));
        #[cfg(test)]
        let observed_engine = test_engine.clone();
        let thread = thread::Builder::new()
            .name("peerlume-session".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build() {
                        Ok(runtime)=>runtime,
                        Err(error)=>{let _=ready.send(Err(format!("Could not create transfer runtime: {error}")));return;}
                    };
                runtime.block_on(async move {
                    // Keep the pinned librqbit 9.0.1 default fingerprint while
                    // knowing the exact session identity before its first announce.
                    let session_peer_id = options.peer_id.unwrap_or_else(||
                        librqbit::generate_azereus_style(*b"rQ", (9, 0, 1, 0)));
                    options.peer_id = Some(session_peer_id);
                    let tracker_enabled = !options.disable_trackers;
                    let session = match tokio::time::timeout(Duration::from_secs(5),Session::new_with_opts(
                        default_output_folder,
                        options,
                    ))
                    .await
                    {
                        Ok(Ok(session)) => session,
                        Ok(Err(error)) => {
                            let _=ready.send(Err(format!("Could not start transfer engine: {error:#}")));
                            let _ = event_tx.send(SessionEvent::Error {
                                id: String::new(),
                                message: format!("Could not start transfer engine: {error:#}"),
                            });
                            return;
                        }
                        Err(_)=>{let _=ready.send(Err("Transfer engine initialization exceeded five seconds".into()));return;}
                    };
                    // The provider samples network payload independently of checked disk pieces.
                    #[cfg(test)]
                    { *observed_engine.lock().unwrap() = Arc::downgrade(&session); }
                    // Carry totals across pause/resume/recheck live-state generations.
                    let weak_session = Arc::downgrade(&session);
                    let counters = Arc::new(std::sync::Mutex::new(HashMap::<[u8;20], TransferCounter>::new()));
                    let observed_counters = counters.clone();
                    let transfers: librqbit_tracker_comms::TransferMetricsProvider = Arc::new(move |hash| {
                        let handle = weak_session.upgrade().and_then(|s| s.with_torrents(|ts| {
                            for (_, t) in ts { if t.info_hash().0 == hash { return Some(t.clone()); } }
                            None
                        }));
                        let mut counters = observed_counters.lock().unwrap_or_else(|e| e.into_inner());
                        let counter = counters.entry(hash).or_default();
                        let mut snapshot = librqbit_tracker_comms::TransferSnapshot::default();
                        if let Some(handle) = handle {
                            snapshot.full_size = handle.with_metadata(|m| m.lengths().total_length()).ok();
                            if let Some(live) = handle.live() {
                            if let Some(hns) = live.get_hns() { snapshot.verified_bytes = Some(hns.have_bytes); snapshot.selected_bytes = Some(hns.selected_bytes); }
                            let stats = live.stats_snapshot();
                            let live: Arc<dyn Send + Sync> = live;
                            counter.sample(Arc::downgrade(&live), stats.fetched_bytes, stats.uploaded_bytes);
                            } else {
                                let state = handle.stats();
                                snapshot.verified_bytes = Some(if matches!(state.state, TorrentStatsState::Initializing { .. }) { 0 } else { state.file_progress.iter().sum() });
                            }
                        }
                        snapshot.downloaded = counter.total.0;
                        snapshot.uploaded = counter.total.1;
                        snapshot
                    });
                    let _observer = match librqbit_tracker_comms::register_observer(session_peer_id.0, thread_journal.observer(), transfers.clone()) {
                        Ok(guard) => guard,
                        Err(message) => {let _=ready.send(Err(message.into()));session.stop().await;let _ = event_tx.send(SessionEvent::Error { id: String::new(), message: message.into() }); return; }
                    };
                    let mut torrents: HashMap<String, Arc<ManagedTorrent>> = HashMap::new();
                    let mut storage_roots: HashMap<String,crate::storage::sandbox::Directory> = HashMap::new();
                    let mut reporting_policies = HashMap::new();
                    let mut added_commands: HashMap<String, SessionCommand> = HashMap::new();
                    let mut checks = HashMap::<String, CheckRun>::new();
                    let mut checked_after_download = std::collections::HashSet::<String>::new();
                    let mut check_failed = std::collections::HashSet::<String>::new();
                    let mut initializing = std::collections::HashSet::<String>::new();
                    let mut active_modes = HashMap::<String, ModeConfig>::new();
                    let mut pending_modes = HashMap::<String, ModeConfig>::new();
                    let mut pending_resume = std::collections::HashSet::<String>::new();
                    let mut repairs = HashMap::<String, RepairBudget>::new();
                    let mut manual_repairs = std::collections::HashSet::<String>::new();
                    let mut seeding_clocks = HashMap::<String, crate::transfer_settings::SeedingClock>::new();
                    let mut copy_jobs = HashMap::<String, crate::storage::relocation::CopyJob>::new();
                    let mut pending_storage = HashMap::<String,(crate::storage::sandbox::Directory,String)>::new();
                    let mut ticker = tokio::time::interval(Duration::from_millis(500));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    if ready.send(Ok(())).is_err(){session.stop().await;return;}

                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                let mut finished=Vec::new();
                                for (id,job) in &copy_jobs {
                                    let _=event_tx.send(SessionEvent::Storage{id:id.clone(),status:crate::storage::relocation::StorageStatus::Copying{bytes:job.bytes.load(std::sync::atomic::Ordering::Relaxed),destination:job.destination.clone()}});
                                    if let Some(result)=job.finished(){finished.push((id.clone(),result));}
                                }
                                for (id,result) in finished {
                                    let job=copy_jobs.remove(&id).expect("known copy");
                                    match result {
                                        Ok(())=>{pending_storage.insert(id,(job.root.clone(),job.destination.clone()));}
                                        Err(message)=>{let _=event_tx.send(SessionEvent::Storage{id,status:crate::storage::relocation::StorageStatus::Failed{message:format!("{message}; originals retained; destination: {}",job.destination)}});}
                                    }
                                }
                                for (id,(root,directory)) in std::mem::take(&mut pending_storage) {
                                    let Some(handle)=torrents.get(&id).cloned() else {continue;};
                                    let Some(mut next)=added_commands.get(&id).cloned() else {continue;};
                                    let previous=next.clone();
                                    let previous_root=storage_roots.get(&id).cloned();
                                    let previous_bytes=handle.with_metadata(|metadata|metadata.torrent_bytes.to_vec()).ok();
                                    if let SessionCommand::AddTorrent{output_folder,..}=&mut next{*output_folder=directory.clone();}
                                    match restart_for_check(&session,&handle,&next,session_peer_id.0,&counters,Some(&root)).await {
                                        Ok(checked)=>{
                                            torrents.insert(id.clone(),checked);storage_roots.insert(id.clone(),root);added_commands.insert(id.clone(),next);
                                            checks.insert(id.clone(),CheckRun{resume:true,require_complete:false}); checked_after_download.remove(&id);check_failed.remove(&id);
                                            let _=event_tx.send(SessionEvent::Storage{id,status:crate::storage::relocation::StorageStatus::Checking{destination:directory}});
                                        }
                                        Err(message)=>{
                                            let mut recovered=false;
                                            if let (Some(bytes),Some(previous_root))=(previous_bytes,previous_root)
                                                && let Ok(restored)=verification::add_for_check(&session,bytes,&previous,previous_root).await {
                                                torrents.insert(id.clone(),restored);checks.insert(id.clone(),CheckRun{resume:false,require_complete:false});recovered=true;
                                            }
                                            let _=event_tx.send(SessionEvent::Storage{id,status:crate::storage::relocation::StorageStatus::Failed{message:format!("{message}; originals and destination retained; new directory not acknowledged; original handle restored paused: {recovered}")}});
                                        }
                                    }
                                }
                                let torrent_ids: Vec<_> = torrents.keys().cloned().collect();
                                for id in torrent_ids {
                                    let Some(handle) = torrents.get(&id).cloned() else { continue; };
                                    if check_failed.contains(&id) && !pending_modes.contains_key(&id) { continue; }
                                    let transfer_snapshot = transfers(handle.info_hash().0);
                                    let _ = event_tx.send(SessionEvent::NetworkTotals { id: id.clone(), uploaded: transfer_snapshot.uploaded, downloaded: transfer_snapshot.downloaded, seeded_seconds: seeding_clocks.get_mut(&id).map(|clock| clock.sample(std::time::Instant::now(), false).as_secs()).unwrap_or(0) });
                                    let mut stats = handle.stats();
                                    if let Some(message) = stats.error.clone() {
                                        if checks.remove(&id).is_some() {
                                            manual_repairs.remove(&id);
                                            check_failed.insert(id.clone());
                                            let _ = event_tx.send(SessionEvent::Verification { id, status: CompletionStatus::Failed { message: "Integrity check could not read or validate the files".into() } });
                                        } else { let _ = event_tx.send(SessionEvent::Error { id, message }); }
                                        continue;
                                    }
                                    if matches!(stats.state, TorrentStatsState::Initializing { .. }) {
                                        initializing.insert(id.clone());
                                        let _ = event_tx.send(SessionEvent::Verification { id, status: CompletionStatus::Checking { checked_bytes: stats.progress_bytes, total_bytes: stats.total_bytes } });
                                        continue;
                                    }
                                    if let Some(config) = pending_modes.remove(&id) {
                                        match controls::apply_mode(&session, &handle, session_peer_id.0, config, reporting_policies.get(&id), &counters).await {
                                            Ok((guard, resume_error)) => {
                                                if let Some(guard) = guard { reporting_policies.insert(id.clone(), guard); }
                                                active_modes.insert(id.clone(), config);
                                                if let Some(SessionCommand::AddTorrent { download_mode, freeze_after_bytes, upload_ratio, .. }) = added_commands.get_mut(&id) { *download_mode = config.mode; *freeze_after_bytes = config.freeze_after_bytes; *upload_ratio = config.upload_ratio; }
                                                let _ = event_tx.send(SessionEvent::Mode { id: id.clone(), application: ModeApplication::Active { config, tracker_enabled } });
                                                if pending_resume.remove(&id) && !checks.contains_key(&id) {
                                                    let result = session.unpause(&handle).await.map_err(|_| "Mode applied, but resuming failed".to_owned());
                                                    emit_state_result(&event_tx, id.clone(), result, "downloading");
                                                }
                                                if let Some(message) = resume_error { let _ = event_tx.send(SessionEvent::Error { id: id.clone(), message }); }
                                                stats = handle.stats();
                                            }
                                            Err(message) => { let _ = event_tx.send(SessionEvent::Mode { id: id.clone(), application: ModeApplication::Failed { requested: config, active: active_modes.get(&id).copied(), message } }); }
                                        }
                                    }
                                    if check_failed.contains(&id) { continue; }
                                    if let Some(run) = checks.remove(&id) {
                                        initializing.remove(&id);
                                        if run.require_complete && !stats.finished {
                                            let policy = controls::recovery_policy(added_commands.get(&id), manual_repairs.contains(&id));
                                            let budget = repairs.entry(id.clone()).or_insert_with(|| RepairBudget::new(std::time::Instant::now()));
                                            if run.resume && budget.start(policy, stats.progress_bytes, std::time::Instant::now()) && session.unpause(&handle).await.is_ok() {
                                                    checked_after_download.remove(&id);
                                                    let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Downloading { attempt: budget.used, limit: policy.max_attempts } });
                                                    let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: CompletionStatus::Pending });
                                                    let _ = event_tx.send(SessionEvent::StateChanged { id: id.clone(), state: "downloading" });
                                                    continue;
                                            }
                                            if policy.enabled && run.resume { manual_repairs.remove(&id); let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Exhausted { attempts: budget.used, limit: policy.max_attempts, message: "Automatic recovery stopped; inspect peers and files before resuming".into() } }); }
                                            check_failed.insert(id.clone());
                                            let _ = event_tx.send(SessionEvent::Verification { id, status: CompletionStatus::Failed { message: "One or more selected pieces are missing or failed their hash check. Repair the files and recheck, or resume to download them again.".into() } });
                                            continue;
                                        }
                                        if stats.finished {
                                            manual_repairs.remove(&id);
                                            checked_after_download.insert(id.clone());
                                            if let Some(budget) = repairs.remove(&id) && budget.used > 0 { let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Succeeded { attempts: budget.used } }); }
                                        }
                                        let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: if stats.finished { CompletionStatus::Verified } else { CompletionStatus::Pending } });
                                        if run.resume {
                                            if session.unpause(&handle).await.is_err() {
                                                let _ = event_tx.send(SessionEvent::Error { id, message: "Integrity check succeeded, but the torrent could not resume".into() });
                                                continue;
                                            }
                                            stats = handle.stats();
                                        }
                                        let _ = event_tx.send(SessionEvent::StateChanged { id: id.clone(), state: if run.resume { "downloading" } else { "paused" } });
                                    } else if initializing.remove(&id) {
                                        let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: CompletionStatus::Pending });
                                    }
                                    if check_failed.contains(&id) { continue; }
                                    let verify_after_download = matches!(added_commands.get(&id), Some(SessionCommand::AddTorrent { verify_after_download: true, .. }));
                                    let repair_stalled = !matches!(stats.state, TorrentStatsState::Paused) && repairs.get_mut(&id).is_some_and(|budget| budget.used > 0 && budget.stalled(stats.progress_bytes, std::time::Instant::now()));
                                    if (stats.finished && (verify_after_download || repairs.contains_key(&id)) && !checked_after_download.contains(&id)) || repair_stalled {
                                        if let Some(budget) = repairs.get(&id) { let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Checking { attempt: budget.used, limit: controls::repair_policy(added_commands.get(&id)).max_attempts } }); }
                                        let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: CompletionStatus::Checking { checked_bytes: 0, total_bytes: stats.total_bytes } });
                                        let resume = !matches!(stats.state, TorrentStatsState::Paused);
                                        match restart_for_check(&session, &handle, &added_commands[&id], session_peer_id.0, &counters, storage_roots.get(&id)).await {
                                            Ok(checked) => { torrents.insert(id.clone(), checked); checks.insert(id, CheckRun { resume, require_complete: true }); }
                                            Err(message) => { manual_repairs.remove(&id); check_failed.insert(id.clone()); let _ = event_tx.send(SessionEvent::Verification { id, status: CompletionStatus::Failed { message } }); }
                                        }
                                        continue;
                                    }
                                    let mut paused = matches!(
                                        stats.state,
                                        TorrentStatsState::Paused
                                            | TorrentStatsState::Initializing { paused: true }
                                    );
                                    let seeded = seeding_clocks.entry(id.clone()).or_default().sample(std::time::Instant::now(), stats.finished && !paused);
                                    if stats.finished && !paused
                                        && let Some(SessionCommand::AddTorrent { transfer, .. }) = added_commands.get(&id)
                                        && transfer.seed_limit_reached(transfer_snapshot.uploaded, stats.total_bytes, seeded)
                                    {
                                        let final_sample = final_transfer_sampler(&handle, &counters);
                                        librqbit_tracker_comms::stop_torrent_announces(handle.info_hash().0, session_peer_id.0).await;
                                        let result = session.pause(&handle).await;
                                        final_sample();
                                        match result {
                                            Ok(()) => { paused = true; seeding_clocks.get_mut(&id).unwrap().sample(std::time::Instant::now(), false); let _ = event_tx.send(SessionEvent::StateChanged { id: id.clone(), state: "paused" }); }
                                            Err(_) => { let _ = event_tx.send(SessionEvent::Error { id: id.clone(), message: "Seeding limit reached, but pausing failed".into() }); }
                                        }
                                    }
                                    let (download_rate, upload_rate, peers) = stats
                                        .live
                                        .as_ref()
                                        .map(|live| {
                                            (
                                                live.download_speed.as_bytes(),
                                                live.upload_speed.as_bytes(),
                                                live.snapshot.peer_stats.live as usize,
                                            )
                                        })
                                        .unwrap_or_default();
                                    let mut peer_details = handle.live().map(|live| live.per_peer_stats_snapshot(Default::default()).peers.into_iter().map(|(address, peer)| crate::diagnostics::PeerSnapshot {
                                        address, state: peer.state.into(), client: peer.client_name.unwrap_or_default().chars().filter(|c| !c.is_control()).take(80).collect(),
                                        downloaded: peer.counters.fetched_bytes, uploaded: peer.counters.uploaded_bytes, errors: peer.counters.errors,
                                    }).collect::<Vec<_>>()).unwrap_or_default();
                                    peer_details.sort_by(|a,b| a.address.cmp(&b.address)); peer_details.truncate(128);
                                    let _ = event_tx.send(SessionEvent::Peers { id: id.clone(), peers: peer_details });
                                    let _ = event_tx.send(SessionEvent::Stats {
                                        id: id.clone(),
                                        downloaded_bytes: stats.progress_bytes,
                                        uploaded_bytes: transfer_snapshot.uploaded,
                                        download_rate,
                                        upload_rate,
                                        file_progress: stats.file_progress,
                                        peers,
                                        finished: stats.finished,
                                        paused,
                                    });
                                }
                            }
                            command = command_rx.recv() => {
                                let Some(command) = command else { break };
                                let (command,prepared)=match command {
                                    SessionCommand::PreparedAdd{command,prepared} if matches!(*command,SessionCommand::AddTorrent{..})=>(*command,Some(prepared)),
                                    other=>(other,None),
                                };
                                if let Some(id)=command.target_id().filter(|id|copy_jobs.contains_key(*id)||pending_storage.contains_key(*id))
                                    && !matches!(command,SessionCommand::CancelStorage{..}) {
                                    let _=event_tx.send(SessionEvent::Storage{id:id.to_owned(),status:crate::storage::relocation::StorageStatus::Failed{message:"Copy is active; cancel it before changing this torrent. Originals retained.".into()}});continue;
                                }
                                let recheck = match &command {
                                    SessionCommand::Recheck { id } => Some((id, false)),
                                    SessionCommand::RecheckAndRepair { id } => Some((id, true)),
                                    _ => None,
                                };
                                if let Some((id, manual)) = recheck {
                                    if checks.contains_key(id) {
                                        if manual { let _ = event_tx.send(SessionEvent::Error { id: id.clone(), message: "An integrity check is already running".into() }); }
                                        continue;
                                    }
                                    let Some(handle) = torrents.get(id).cloned() else {
                                        if manual { let _ = event_tx.send(SessionEvent::Error { id: id.clone(), message: "Torrent is not registered in the transfer engine".into() }); }
                                        continue;
                                    };
                                    let stats = handle.stats();
                                    let resume = manual || !matches!(stats.state, TorrentStatsState::Paused | TorrentStatsState::Initializing { paused: true });
                                    let require_complete = manual || stats.finished || check_failed.contains(id);
                                    transfers(handle.info_hash().0);
                                    let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: CompletionStatus::Checking { checked_bytes: 0, total_bytes: stats.total_bytes } });
                                    let Some(saved) = added_commands.get(id) else { continue; };
                                    match restart_for_check(&session, &handle, saved, session_peer_id.0, &counters, storage_roots.get(id)).await {
                                        Ok(checked) => {
                                            if manual { manual_repairs.insert(id.clone()); repairs.remove(id); let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Idle }); }
                                            else { manual_repairs.remove(id); }
                                            torrents.insert(id.clone(), checked); checks.insert(id.clone(), CheckRun { resume, require_complete }); check_failed.remove(id); checked_after_download.remove(id); let _ = event_tx.send(SessionEvent::Added { id: id.clone() });
                                        }
                                        Err(message) => { manual_repairs.remove(id); check_failed.insert(id.clone()); let _ = event_tx.send(SessionEvent::Verification { id: id.clone(), status: CompletionStatus::Failed { message } }); }
                                    }
                                    continue;
                                }
                                let mut saved_command = command.clone();
                                if let SessionCommand::AddTorrent { repair_policy, verify_after_download, .. } = &mut saved_command { *verify_after_download |= repair_policy.enabled; }
                                match command {
                                    SessionCommand::CancelStorage{id}=>{if let Some(job)=copy_jobs.get(&id){job.cancel();}}
                                    SessionCommand::SelectStorage{id,directory,copy}=>{
                                        let result=(||->Result<(),String>{
                                            let handle=torrents.get(&id).ok_or("Torrent unavailable")?;
                                            if checks.contains_key(&id) || !matches!(handle.stats().state,TorrentStatsState::Paused) {return Err("Pause the torrent and wait for the current check before selecting storage".into());}
                                            let path=std::path::Path::new(&directory);if !path.is_absolute(){return Err("Destination must be absolute".into());}
                                            if copy {
                                                let source=storage_roots.get(&id).ok_or("Pinned source directory unavailable")?.clone();
                                                let files=handle.with_metadata(|metadata|metadata.file_infos.iter().filter(|file|!file.attrs.padding).map(|file|(file.relative_filename.clone(),file.len)).collect()).map_err(|_|"Torrent metadata unavailable")?;
                                                let job=crate::storage::relocation::CopyJob::start(source,path,files).map_err(|error|error.to_string())?;
                                                copy_jobs.insert(id.clone(),job);
                                            }else{
                                                let root=crate::storage::sandbox::Directory::open(path).map_err(|error|error.to_string())?;
                                                pending_storage.insert(id.clone(),(root,directory));
                                            }
                                            Ok(())
                                        })();
                                        if let Err(message)=result{let _=event_tx.send(SessionEvent::Storage{id,status:crate::storage::relocation::StorageStatus::Failed{message}});}
                                    }
                                    SessionCommand::ConfigureQueue { max_active_downloads } => { session.max_active_downloads.store(max_active_downloads, std::sync::atomic::Ordering::Relaxed); }
                                    SessionCommand::SetTransferSettings { id, settings } => {
                                        if let Err(message) = settings.validate() { let _ = event_tx.send(SessionEvent::Error { id, message }); continue; }
                                        if checks.contains_key(&id) { let _ = event_tx.send(SessionEvent::Error { id, message: "Wait for the integrity check before changing transfer settings".into() }); continue; }
                                        let Some(handle) = torrents.get(&id).cloned() else { let _ = event_tx.send(SessionEvent::Error { id, message: "Torrent is unavailable".into() }); continue; };
                                        let Some(saved) = added_commands.get(&id) else { continue; };
                                        let mut next = saved.clone();
                                        let SessionCommand::AddTorrent { transfer, peer_limit, .. } = &mut next else { continue; };
                                        let peer_changed = *peer_limit != Some(usize::from(settings.max_peers));
                                        *transfer = settings;
                                        *peer_limit = Some(usize::from(settings.max_peers));
                                        // Capacity is immutable upstream. Re-add only this torrent with
                                        // pinned storage and resolved metadata in the SAME session.
                                        if peer_changed {
                                            let resume = !matches!(handle.stats().state, TorrentStatsState::Paused | TorrentStatsState::Initializing { paused: true });
                                            match restart_for_check(&session, &handle, &next, session_peer_id.0, &counters, storage_roots.get(&id)).await {
                                                Ok(checked) => { torrents.insert(id.clone(), checked); checks.insert(id.clone(), CheckRun { resume, require_complete: false }); }
                                                Err(message) => { let _ = event_tx.send(SessionEvent::Error { id, message }); continue; }
                                            }
                                        } else { handle.set_transfer_settings(settings.engine()); }
                                        added_commands.insert(id.clone(), next);
                                        let _ = event_tx.send(SessionEvent::TransferSettingsApplied { id, settings });
                                    }
                                    SessionCommand::SetDownloadMode { id, config } => {
                                        let active = active_modes.get(&id).copied();
                                        if !torrents.contains_key(&id) || config.validate().is_err() {
                                            let _ = event_tx.send(SessionEvent::Mode { id, application: ModeApplication::Failed { requested: config, active, message: "Torrent unavailable or invalid freeze threshold".into() } });
                                        } else {
                                            pending_modes.insert(id.clone(), config);
                                            // Failed verification remains paused, but does not block settings.
                                            let _ = event_tx.send(SessionEvent::Mode { id, application: ModeApplication::Applying { requested: config, active } });
                                        }
                                    }
                                    SessionCommand::SetRepairPolicy { id, policy } => {
                                        if policy.validate().is_err() { continue; }
                                        if let Some(SessionCommand::AddTorrent { repair_policy, verify_after_download, .. }) = added_commands.get_mut(&id) {
                                            *repair_policy = policy;
                                            if policy.enabled { *verify_after_download = true; checked_after_download.remove(&id); }
                                            let _ = event_tx.send(SessionEvent::RepairPolicy { id, policy });
                                        }
                                    }
                                    SessionCommand::AddTorrent {
                                        resume,
                                        transfer,
                                        repair_policy,
                                        verify_after_download: _,
                                        download_mode,
                                        freeze_after_bytes,
                                        upload_ratio,
                                        full_size,
                                        id,
                                        source,
                                        output_folder,
                                        only_files,
                                        trackers,
                                        paused,
                                        peer_limit,
                                    } => {
                                        if torrents.contains_key(&id) {
                                            let _ = event_tx.send(SessionEvent::Error { id, message: "Torrent is already registered".into() });
                                            continue;
                                        }
                                        if let Err(message) = repair_policy.validate() { let _ = event_tx.send(SessionEvent::Error { id, message }); continue; }
                                        if let Err(message) = transfer.validate() { let _ = event_tx.send(SessionEvent::Error { id, message }); continue; }
                                        if let Some(hash) = id.split(';').find_map(|part|part.strip_prefix("v1: ")).and_then(|hash| crate::tracker::parse_info_hash_hex(hash).ok()) {
                                            counters.lock().unwrap_or_else(|e| e.into_inner()).entry(hash).or_insert_with(|| TransferCounter { base: (resume.downloaded,resume.uploaded), total: (resume.downloaded,resume.uploaded), ..Default::default() });
                                        }
                                        seeding_clocks.entry(id.clone()).or_insert_with(|| crate::transfer_settings::SeedingClock::restored(resume.seeded_seconds));
                                        let root=match prepared.as_ref().map(|p|Ok(p.directory.clone())).unwrap_or_else(||crate::storage::sandbox::Directory::create_root(std::path::Path::new(&output_folder))) {
                                            Ok(root)=>root,
                                            Err(_)=>{let _=event_tx.send(SessionEvent::Error{id,message:"Could not pin download directory".into()});continue;}
                                        };
                                        let registration = match register_download_mode_with_threshold(&id, session_peer_id.0, download_mode, full_size, freeze_after_bytes, upload_ratio) {
                                            Ok(registration) => registration,
                                            Err(message) => {
                                                let _ = event_tx.send(SessionEvent::Error { id, message });
                                                continue;
                                            }
                                        };
                                        let add = if let Some(prepared)=prepared {
                                            Ok(AddTorrent::from_bytes(prepared.bytes.to_vec()))
                                        } else if source.starts_with("magnet:") {
                                            Ok(AddTorrent::from_url(source))
                                        } else {
                                            read_metainfo_snapshot(std::path::Path::new(&source))
                                                .map(AddTorrent::from_bytes)
                                                .map_err(|_| "Could not read bounded torrent metadata".to_owned())
                                        };
                                        let result = match add {
                                            Ok(add) => session
                                                .add_torrent(
                                                    add,
                                                    Some(AddTorrentOptions {
                                                        storage_factory:Some(crate::storage::sandbox::Factory{root:root.clone()}.boxed()),
                                                        paused,
                                                        only_files,
                                                        overwrite: true,
                                                        output_folder: Some(output_folder),
                                                        peer_limit,
                                                        ratelimits: transfer.engine().limits,
                                                        trackers: (!trackers.is_empty()).then_some(trackers),
                                                        ..Default::default()
                                                    }),
                                                )
                                                .await
                                                .map_err(|error| format!("{error:#}")),
                                            Err(error) => Err(error),
                                        };
                                        match result.and_then(|response| {
                                            response.into_handle().ok_or_else(|| {
                                                "Transfer engine returned metadata without a torrent handle".to_owned()
                                            })
                                        }) {
                                            Ok(handle) => {
                                                handle.set_transfer_settings(transfer.engine());
                                                storage_roots.insert(id.clone(),root);
                                                added_commands.insert(id.clone(), saved_command);
                                                if let Some(registration) = registration {
                                                    reporting_policies.insert(id.clone(), registration);
                                                }
                                                torrents.insert(id.clone(), handle);
                                                let config = ModeConfig { mode: download_mode, freeze_after_bytes, upload_ratio };
                                                active_modes.insert(id.clone(), config);
                                                let _ = event_tx.send(SessionEvent::Mode { id: id.clone(), application: ModeApplication::Active { config, tracker_enabled } });
                                                let _ = event_tx.send(SessionEvent::Added { id });
                                            }
                                            Err(message) => {
                                                let _ = event_tx.send(SessionEvent::Error { id, message });
                                            }
                                        }
                                    }
                                    SessionCommand::Pause { id } => {
                                        pending_resume.remove(&id);
                                        if let Some(run) = checks.get_mut(&id) { run.resume = false; continue; }
                                        let result = if let Some(handle) = torrents.get(&id) {
                                            transfers(handle.info_hash().0);
                                            let final_sample = final_transfer_sampler(handle, &counters);
                                            librqbit_tracker_comms::stop_torrent_announces(handle.info_hash().0, session_peer_id.0).await;
                                            let result = session.pause(handle).await.map_err(|error| format!("{error:#}"));
                                            final_sample();
                                            result
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        emit_state_result(&event_tx, id, result, "paused");
                                    }
                                    SessionCommand::Resume { id } => {
                                        if let Some(budget) = repairs.get_mut(&id) { budget.resume(std::time::Instant::now()); }
                                        if let Some(run) = checks.get_mut(&id) { run.resume = true; continue; }
                                        if check_failed.remove(&id) {
                                            checked_after_download.remove(&id);
                                            let policy = controls::recovery_policy(added_commands.get(&id), manual_repairs.contains(&id));
                                            let mut budget = RepairBudget::new(std::time::Instant::now());
                                            if policy.enabled && budget.start(policy, torrents.get(&id).map(|h| h.stats().progress_bytes).unwrap_or(0), std::time::Instant::now()) {
                                                let _ = event_tx.send(SessionEvent::Repair { id: id.clone(), status: RepairStatus::Downloading { attempt: budget.used, limit: policy.max_attempts } });
                                                repairs.insert(id.clone(), budget);
                                            }
                                        }
                                        if pending_modes.contains_key(&id) { pending_resume.insert(id); continue; }
                                        let result = if let Some(handle) = torrents.get(&id) {
                                            session.unpause(handle).await.map_err(|error| format!("{error:#}"))
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        emit_state_result(&event_tx, id, result, "downloading");
                                    }
                                    SessionCommand::Remove { id } => {
                                        let result = if let Some(handle) = torrents.get(&id).cloned() {
                                            let final_sample = final_transfer_sampler(&handle, &counters);
                                            librqbit_tracker_comms::stop_torrent_announces(handle.info_hash().0, session_peer_id.0).await;
                                            let result = session
                                                .delete(handle.id().into(), false)
                                                .await
                                                .map_err(|error| format!("{error:#}"));
                                            final_sample();
                                            if result.is_ok() {
                                                counters.lock().unwrap_or_else(|e| e.into_inner()).remove(&handle.info_hash().0);
                                                librqbit_tracker_comms::forget_torrent_lifecycle(handle.info_hash().0, session_peer_id.0);
                                            }
                                            result
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        let event = match result {
                                            Ok(()) => {
                                                torrents.remove(&id);
                                                storage_roots.remove(&id);
                                                checks.remove(&id);
                                                checked_after_download.remove(&id);
                                                check_failed.remove(&id);
                                                initializing.remove(&id);
                                                added_commands.remove(&id);
                                                reporting_policies.remove(&id);
                                                active_modes.remove(&id);
                                                pending_modes.remove(&id);
                                                pending_resume.remove(&id);
                                                repairs.remove(&id);
                                                manual_repairs.remove(&id);
                                                seeding_clocks.remove(&id);
                                                SessionEvent::Removed { id }
                                            },
                                            Err(message) => SessionEvent::Error { id, message },
                                        };
                                        let _ = event_tx.send(event);
                                    }
                                    SessionCommand::PreparedAdd { .. } => {
                                        let _=event_tx.send(SessionEvent::Error{id:String::new(),message:"Invalid prepared add command".into()});
                                    }
                                    SessionCommand::Shutdown => {
                                        copy_jobs.clear();
                                        let final_samples: Vec<_> = torrents.values().map(|h| final_transfer_sampler(h, &counters)).collect();
                                        librqbit_tracker_comms::stop_session_announces(session_peer_id.0).await;
                                        session.stop().await;
                                        for sample in final_samples { sample(); }
                                        for (id,handle) in &torrents {
                                            let totals=counters.lock().unwrap_or_else(|error|error.into_inner()).get(&handle.info_hash().0).map(|counter|counter.total).unwrap_or_default();
                                            let seeded_seconds=seeding_clocks.get_mut(id).map(|clock|clock.sample(std::time::Instant::now(),false).as_secs()).unwrap_or_default();
                                            let _=event_tx.send(SessionEvent::NetworkTotals{id:id.clone(),downloaded:totals.0,uploaded:totals.1,seeded_seconds});
                                        }
                                        reporting_policies.clear();
                                        let _ = event_tx.send(SessionEvent::Stopped);
                                        break;
                                    }
                                    SessionCommand::Recheck { .. } | SessionCommand::RecheckAndRepair { .. } => unreachable!("recheck expanded above"),
                                    SessionCommand::SetVerifyAfterDownload { id, enabled } => {
                                        if let Some(SessionCommand::AddTorrent { verify_after_download, repair_policy, .. }) = added_commands.get_mut(&id) {
                                            *verify_after_download = enabled || repair_policy.enabled;
                                            if enabled { checked_after_download.remove(&id); }
                                            let _ = event_tx.send(SessionEvent::VerifyAfterDownload { id, enabled: *verify_after_download });
                                        } else {
                                            let _ = event_tx.send(SessionEvent::Error { id, message: "Torrent is not registered in the transfer engine".into() });
                                        }
                                    }
                                    SessionCommand::SetFiles { id, only_files } => {
                                        if let Some(handle) = torrents.get(&id) {
                                            let selected = only_files.iter().copied().collect();
                                            match session.update_only_files(handle, &selected).await {
                                                Ok(()) => {
                                                    checked_after_download.remove(&id);
                                                    if let Some(SessionCommand::AddTorrent { only_files: saved, .. }) = added_commands.get_mut(&id) { *saved = Some(only_files.clone()); }
                                                    let _ = event_tx.send(SessionEvent::FilesApplied { id, only_files });
                                                },
                                                Err(error) => { let _ = event_tx.send(SessionEvent::Error { id, message: format!("File selection update failed: {error:#}") }); }
                                            }
                                        } else {
                                            let _ = event_tx.send(SessionEvent::Error { id, message: "Torrent is not registered in the transfer engine".into() });
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            })
            .map_err(|error| error.to_string())?;
        let runtime = Self {
            journal,
            commands,
            events,
            thread: Some(thread),
            #[cfg(test)]
            test_engine,
        };
        initialized
            .recv_timeout(Duration::from_secs(6))
            .map_err(|_| {
                "Transfer engine initialization worker stopped or timed out".to_owned()
            })??;
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn start_test_with_journal(
        capacity: usize,
        output: PathBuf,
        options: SessionOptions,
        journal: AnnounceJournal,
    ) -> Result<Self, String> {
        Self::start_with_journal(capacity, output, options, journal)
    }

    #[cfg(test)]
    pub(crate) fn test_engine(&self) -> Option<Arc<Session>> {
        self.test_engine.lock().unwrap().upgrade()
    }

    pub fn try_send(&self, command: SessionCommand) -> Result<(), String> {
        self.commands
            .try_send(command)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn send_checked(&self, command: SessionCommand) -> Result<(), SessionSendError> {
        send_command(&self.commands, command)
    }

    pub fn try_recv(&self) -> Option<SessionEvent> {
        self.events.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SessionEvent, String> {
        self.events
            .recv_timeout(timeout)
            .map_err(|error| error.to_string())
    }

    pub fn shutdown(mut self) -> Result<(), String> {
        // Shutdown consumes the receiver. Release control-event backpressure
        // before joining the worker, even if the UI has stopped polling.
        self.events.close();
        self.commands
            .blocking_send(SessionCommand::Shutdown)
            .map_err(|error| error.to_string())?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "session thread panicked".to_owned())?;
        }
        Ok(())
    }
}

fn session_options(disable_trackers: bool) -> SessionOptions {
    SessionOptions {
        disable_trackers,
        ..Default::default()
    }
}

#[cfg(test)]
fn register_download_mode(
    id: &str,
    peer_id: [u8; 20],
    mode: DownloadMode,
    full_size: u64,
) -> Result<Option<librqbit_tracker_comms::ReportingRegistration>, String> {
    register_download_mode_with_threshold(
        id,
        peer_id,
        mode,
        full_size,
        crate::download_mode::DEFAULT_FREEZE_AFTER_BYTES,
        Default::default(),
    )
}

fn register_download_mode_with_threshold(
    id: &str,
    peer_id: [u8; 20],
    mode: DownloadMode,
    full_size: u64,
    threshold: u64,
    upload_ratio: crate::download_mode::UploadRatioRange,
) -> Result<Option<librqbit_tracker_comms::ReportingRegistration>, String> {
    let config = ModeConfig {
        mode,
        freeze_after_bytes: threshold,
        upload_ratio,
    };
    config.validate()?;
    if mode == DownloadMode::Normal {
        return Ok(None);
    }
    if full_size == 0 {
        return Err(
            "This download mode requires resolved .torrent metadata and a nonzero full size".into(),
        );
    }
    let hash = id
        .split(';')
        .find_map(|hash| hash.strip_prefix("v1: "))
        .ok_or("This download mode requires a v1 or hybrid torrent")?;
    let info_hash = crate::tracker::parse_info_hash_hex(hash)?;
    librqbit_tracker_comms::register_reporting_policy(
        info_hash,
        peer_id,
        config.reporting_policy(full_size)?,
    )
    .map(Some)
    .map_err(str::to_owned)
}

#[derive(Default)]
struct TransferCounter {
    live: Option<std::sync::Weak<dyn Send + Sync>>,
    base: (u64, u64),
    total: (u64, u64),
}

type TransferCounters = Arc<std::sync::Mutex<HashMap<[u8; 20], TransferCounter>>>;
fn final_transfer_sampler(
    handle: &ManagedTorrent,
    counters: &TransferCounters,
) -> impl FnOnce() + use<> {
    let live = handle.live();
    let hash = handle.info_hash().0;
    let counters = counters.clone();
    move || {
        if let Some(live) = live {
            let stats = live.stats_snapshot();
            let live: Arc<dyn Send + Sync> = live;
            counters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(hash)
                .or_default()
                .sample(
                    Arc::downgrade(&live),
                    stats.fetched_bytes,
                    stats.uploaded_bytes,
                );
        }
    }
}
impl TransferCounter {
    fn sample(&mut self, live: std::sync::Weak<dyn Send + Sync>, downloaded: u64, uploaded: u64) {
        if self
            .live
            .as_ref()
            .is_none_or(|previous| !previous.ptr_eq(&live))
        {
            self.base = self.total;
            self.live = Some(live);
        }
        self.total = (
            self.base.0.saturating_add(downloaded),
            self.base.1.saturating_add(uploaded),
        );
    }
}

fn emit_state_result(
    sender: &events::Sender,
    id: String,
    result: Result<(), String>,
    state: &'static str,
) {
    let event = match result {
        Ok(()) => SessionEvent::StateChanged { id, state },
        Err(message) => SessionEvent::Error { id, message },
    };
    let _ = sender.send(event);
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        self.events.close();
        let _ = self.commands.blocking_send(SessionCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod multi_peer_tests;

#[cfg(test)]
mod sandbox_tests;
#[cfg(test)]
mod verification_tests;

#[cfg(test)]
pub(crate) fn reliability_transfer_cycle() {
    multi_peer_tests::different_torrents_are_verified_after_parallel_download_before_done();
    verification_tests::automatic_repair_fetches_only_the_damaged_piece_and_keeps_the_reporting_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_command_transport_reports_full_and_closed_without_dropping_queued_commands() {
        let (sender, mut receiver) = tokio_mpsc::channel(1);
        assert_eq!(send_command(&sender, SessionCommand::Shutdown), Ok(()));
        assert_eq!(
            send_command(&sender, SessionCommand::Pause { id: "a".into() }),
            Err(SessionSendError::Full)
        );
        assert_eq!(receiver.try_recv().unwrap(), SessionCommand::Shutdown);
        drop(receiver);
        assert_eq!(
            send_command(&sender, SessionCommand::Shutdown),
            Err(SessionSendError::Closed)
        );
    }

    #[test]
    fn reporting_registration_is_scoped_to_torrent_and_session() {
        let hash = "v1: 0123456789abcdef0123456789abcdef01234567";
        let guard = register_download_mode(hash, [1; 20], DownloadMode::NoProgress, 100).unwrap();
        assert!(register_download_mode(hash, [1; 20], DownloadMode::UploadRatio15, 100).is_err());
        let other_session =
            register_download_mode(hash, [2; 20], DownloadMode::UploadRatio15, 100).unwrap();
        assert!(
            register_download_mode(hash, [1; 20], DownloadMode::Normal, 100)
                .unwrap()
                .is_none()
        );
        drop(guard);
        assert!(register_download_mode(hash, [1; 20], DownloadMode::UploadRatio15, 100).is_ok());
        drop(other_session);
    }

    #[test]
    fn real_downloads_continue_in_all_modes_with_correct_http_and_udp_metrics() {
        for udp in [false, true] {
            for mode in DownloadMode::ALL {
                download_fixture(mode, udp);
            }
        }
    }

    fn download_fixture(mode: DownloadMode, udp: bool) {
        use crate::protocol::{HANDSHAKE_LENGTH, Handshake, PeerMessage};
        use sha1::{Digest, Sha1};
        use std::{
            fs,
            io::{BufRead, BufReader, Read, Write},
            net::{TcpListener, UdpSocket},
            sync::atomic::{AtomicBool, Ordering},
            time::{Instant, SystemTime, UNIX_EPOCH},
        };

        let payload = vec![b'x'; 16_383];
        let peer_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer_address = peer_listener.local_addr().unwrap();
        peer_listener.set_nonblocking(true).unwrap();
        let tracker_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tracker_address = tracker_listener.local_addr().unwrap();
        tracker_listener.set_nonblocking(true).unwrap();
        let udp_tracker = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp_tracker
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let tracker_url = if udp {
            format!("udp://{}/announce", udp_tracker.local_addr().unwrap())
        } else {
            format!("http://{tracker_address}/announce")
        };
        let mut info = format!(
            "d6:lengthi{}e4:name11:payload.bin12:piece lengthi16384e6:pieces20:",
            payload.len()
        )
        .into_bytes();
        info.extend_from_slice(&Sha1::digest(&payload));
        info.extend_from_slice(b"7:privatei1ee");
        let info_hash = format!("{:x}", Sha1::digest(&info));
        let mut metainfo =
            format!("d8:announce{}:{}4:info", tracker_url.len(), tracker_url).into_bytes();
        metainfo.extend(info);
        metainfo.push(b'e');
        let root = std::env::temp_dir().join(format!(
            "peerlume-download-{}-{}-{}",
            std::process::id(),
            mode.id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("fixture.torrent");
        fs::write(&source, metainfo).unwrap();
        let output = root.join("output");

        let stop = Arc::new(AtomicBool::new(false));
        let tracker_stop = Arc::clone(&stop);
        let (announce_tx, announce_rx) = mpsc::channel();
        let tracker_thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !tracker_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                if udp {
                    let mut buffer = [0; 256];
                    let (count, source) = match udp_tracker.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => panic!("UDP recv: {error}"),
                    };
                    assert!(count >= 16);
                    let action = u32::from_be_bytes(buffer[8..12].try_into().unwrap());
                    let mut response = Vec::new();
                    response.extend(action.to_be_bytes());
                    response.extend_from_slice(&buffer[12..16]);
                    if action == 0 {
                        response.extend(42u64.to_be_bytes());
                    } else {
                        assert_eq!(count, 98);
                        assert_eq!(action, 1);
                        let mut values = HashMap::new();
                        values.insert(
                            "downloaded".into(),
                            u64::from_be_bytes(buffer[56..64].try_into().unwrap()).to_string(),
                        );
                        values.insert(
                            "left".into(),
                            u64::from_be_bytes(buffer[64..72].try_into().unwrap()).to_string(),
                        );
                        values.insert(
                            "uploaded".into(),
                            u64::from_be_bytes(buffer[72..80].try_into().unwrap()).to_string(),
                        );
                        if u32::from_be_bytes(buffer[80..84].try_into().unwrap()) == 1 {
                            values.insert("event".into(), "completed".into());
                        }
                        let _ = announce_tx.send(values);
                        response.extend(1u32.to_be_bytes()); // interval
                        response.extend(0u32.to_be_bytes()); // leechers
                        response.extend(1u32.to_be_bytes()); // seeders
                        response.extend([127, 0, 0, 1]);
                        response.extend(peer_address.port().to_be_bytes());
                    }
                    udp_tracker.send_to(&response, source).unwrap();
                    continue;
                }
                let (mut stream, _) = match tracker_listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("tracker accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                }
                let query = request.split_whitespace().nth(1).unwrap();
                let url = url::Url::parse(&format!("http://localhost{query}")).unwrap();
                let values: HashMap<_, _> = url.query_pairs().into_owned().collect();
                let mut body = b"d8:intervali1e5:peers6:".to_vec();
                body.extend([127, 0, 0, 1]);
                body.extend(peer_address.port().to_be_bytes());
                body.push(b'e');
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                let _ = announce_tx.send(values);
            }
        });
        let peer_stop = Arc::clone(&stop);
        let seed_data = payload.clone();
        let peer_thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut stream = loop {
                assert!(
                    Instant::now() < deadline,
                    "client never connected to the tracker-provided peer"
                );
                if peer_stop.load(Ordering::Relaxed) {
                    return;
                }
                match peer_listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("peer accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut handshake = [0; HANDSHAKE_LENGTH];
            stream.read_exact(&mut handshake).unwrap();
            let incoming = Handshake::decode(&handshake).unwrap();
            let outgoing = Handshake {
                reserved: [0; 8],
                info_hash: incoming.info_hash,
                peer_id: *b"-PLTEST-seeder000001",
            };
            stream.write_all(&outgoing.encode()).unwrap();
            stream
                .write_all(&PeerMessage::Bitfield(vec![0x80]).encode())
                .unwrap();
            stream.write_all(&PeerMessage::Unchoke.encode()).unwrap();
            loop {
                let mut prefix = [0; 4];
                stream.read_exact(&mut prefix).unwrap();
                let len = u32::from_be_bytes(prefix) as usize;
                assert!(len <= 65536);
                let mut message = prefix.to_vec();
                message.resize(len + 4, 0);
                stream.read_exact(&mut message[4..]).unwrap();
                if let Ok(PeerMessage::Request {
                    index,
                    begin,
                    length,
                }) = PeerMessage::decode(&message, 65536)
                {
                    let block = seed_data[begin as usize..(begin + length) as usize].to_vec();
                    stream
                        .write_all(
                            &PeerMessage::Piece {
                                index,
                                begin,
                                block,
                            }
                            .encode(),
                        )
                        .unwrap();
                    break;
                }
            }
        });
        let runtime = SessionRuntime::start_with_options(
            64,
            output.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..session_options(false)
            },
        )
        .unwrap();
        runtime
            .try_send(SessionCommand::AddTorrent {
                resume: Default::default(),
                repair_policy: Default::default(),
                verify_after_download: false,
                id: format!("v1: {info_hash}"),
                source: source.display().to_string(),
                output_folder: output.display().to_string(),
                only_files: None,
                trackers: vec![tracker_url],
                paused: false,
                transfer: Default::default(),
                peer_limit: Some(5),
                download_mode: mode,
                freeze_after_bytes: 8192,
                upload_ratio: crate::download_mode::UploadRatioRange {
                    min_milli: 1750,
                    max_milli: 1750,
                },
                full_size: payload.len() as u64,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut finished = false;
        while Instant::now() < deadline {
            match runtime.recv_timeout(Duration::from_secs(1)) {
                Ok(SessionEvent::Stats {
                    finished: true,
                    downloaded_bytes,
                    ..
                }) => {
                    assert_eq!(downloaded_bytes, payload.len() as u64);
                    finished = true;
                    break;
                }
                Ok(SessionEvent::Error { message, .. }) => panic!("{mode:?}: {message}"),
                _ => {}
            }
        }
        assert!(finished, "download did not complete in mode {mode:?}");
        // Discard pre-completion announces, then inspect the next actual request.
        while announce_rx.try_recv().is_ok() {}
        // The upstream UDP client enforces a minimum five-second interval.
        let reported = announce_rx
            .recv_timeout(Duration::from_secs(8))
            .unwrap_or_else(|error| panic!("{mode:?}, UDP={udp}: {error}"));
        let total = payload.len() as u64;
        let expected = ModeConfig {
            mode,
            upload_ratio: crate::download_mode::UploadRatioRange {
                min_milli: 1750,
                max_milli: 1750,
            },
            ..Default::default()
        }
        .reporting_policy(total)
        .unwrap()
        .apply(librqbit_tracker_comms::TrackerCommsStats {
            downloaded_bytes: total,
            total_bytes: total,
            uploaded_bytes: 0,
            torrent_state: librqbit_tracker_comms::TrackerCommsStatsState::Live,
        });
        assert_eq!(
            reported["uploaded"],
            expected.uploaded_bytes.to_string(),
            "{mode:?}"
        );
        assert_eq!(
            reported["downloaded"],
            expected.downloaded_bytes.to_string(),
            "{mode:?}"
        );
        assert_eq!(
            reported["left"],
            expected.get_left_to_download_bytes().to_string(),
            "{mode:?}"
        );
        if mode == DownloadMode::NoProgress {
            assert_ne!(reported.get("event").map(String::as_str), Some("completed"));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let journal = runtime.journal.clone();
        loop {
            let entries = journal.entries(Some(&info_hash));
            if let Some(entry) = entries
                .iter()
                .find(|e| e.announce.actual.downloaded == total && e.announce.outcome == "ok")
            {
                assert_eq!(entry.announce.verified_bytes, total);
                assert_eq!(entry.announce.reply.peers, vec![peer_address]);
                assert_eq!(
                    entry.announce.sent.downloaded.to_string(),
                    reported["downloaded"]
                );
                assert_eq!(entry.announce.request_transmitted, Some(true));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Missing real response in announce journal"
            );
            thread::sleep(Duration::from_millis(5));
        }
        runtime.shutdown().unwrap();
        let entries = journal.entries(Some(&info_hash));
        let completed = entries
            .iter()
            .filter(|entry| {
                entry.announce.event.as_deref() == Some("completed")
                    && entry.announce.outcome == "ok"
            })
            .count();
        if mode == DownloadMode::Normal {
            assert_eq!(
                completed, 1,
                "a fresh download must report completed exactly once"
            );
        }
        if mode == DownloadMode::NoProgress {
            assert_eq!(
                completed, 0,
                "no-progress must continue suppressing completed"
            );
        }
        let last = &entries.last().unwrap().announce;
        assert_eq!(last.event.as_deref(), Some("stopped"));
        assert_eq!(
            last.outcome, "ok",
            "shutdown must wait for the real tracker reply"
        );
        assert_eq!(last.mode, mode.id());
        stop.store(true, Ordering::Relaxed);
        tracker_thread.join().unwrap();
        peer_thread.join().unwrap();
        assert_eq!(fs::read(output.join("payload.bin")).unwrap(), payload);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zero_capacity_is_rejected_before_starting_a_thread() {
        assert!(SessionRuntime::start(0, PathBuf::from("downloads"), false).is_err());
    }

    #[test]
    fn shutdown_releases_full_control_queue_without_a_polling_frontend() {
        let root = tempfile::tempdir().unwrap();
        let runtime = SessionRuntime::start_with_options(
            1,
            root.path().into(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..session_options(true)
            },
        )
        .unwrap();
        runtime
            .try_send(SessionCommand::Pause {
                id: "missing-one".into(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while runtime.events.pending_controls() != 1 {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        runtime
            .try_send(SessionCommand::Pause {
                id: "missing-two".into(),
            })
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(runtime.shutdown()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn disabled_reporting_is_applied_to_the_whole_transfer_session() {
        assert!(session_options(true).disable_trackers);
        assert!(!session_options(false).disable_trackers);
    }

    #[test]
    fn state_result_reports_success_and_failure() {
        let (tx, rx) = events::channel(2);
        emit_state_result(&tx, "ok".into(), Ok(()), "paused");
        emit_state_result(&tx, "bad".into(), Err("failure".into()), "paused");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            SessionEvent::StateChanged {
                id: "ok".into(),
                state: "paused"
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            SessionEvent::Error {
                id: "bad".into(),
                message: "failure".into()
            }
        );
    }

    #[test]
    fn network_totals_survive_live_state_replacement_without_counting_disk_progress() {
        let live: Arc<dyn Send + Sync> = Arc::new(1_u8);
        let mut counter = TransferCounter::default();
        counter.sample(Arc::downgrade(&live), 12, 7);
        counter.sample(Arc::downgrade(&live), 15, 8);
        assert_eq!(counter.total, (15, 8));
        let next: Arc<dyn Send + Sync> = Arc::new(2_u8);
        counter.sample(Arc::downgrade(&next), 0, 0);
        assert_eq!(counter.total, (15, 8));
        counter.sample(Arc::downgrade(&next), 9, 3);
        assert_eq!(counter.total, (24, 11));
    }

    #[test]
    fn partial_existing_files_and_recheck_do_not_become_new_network_downloads() {
        use sha1::{Digest, Sha1};
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            sync::atomic::{AtomicBool, Ordering},
            time::Instant,
        };
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("out");
        std::fs::create_dir_all(&output).unwrap();
        let existing = vec![b'a'; 16384];
        std::fs::write(output.join("first.bin"), &existing).unwrap();
        let missing = vec![b'b'; 16384];
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let tracker = format!(
            "http://{}/private-key/announce?passkey=secret",
            server.local_addr().unwrap()
        );
        let mut info = b"d5:filesld6:lengthi16384e4:pathl9:first.bineed6:lengthi16384e4:pathl10:second.bineee4:name7:partial12:piece lengthi16384e6:pieces40:".to_vec();
        info.extend(Sha1::digest(&existing));
        info.extend(Sha1::digest(&missing));
        info.extend(b"7:privatei1ee");
        let hash = format!("{:x}", Sha1::digest(&info));
        let mut meta = format!("d8:announce{}:{}4:info", tracker.len(), tracker).into_bytes();
        meta.extend(info);
        meta.push(b'e');
        let source = root.path().join("partial.torrent");
        std::fs::write(&source, meta).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let thread = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match server.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                let body = b"d8:intervali1e5:peers0:e";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let runtime = SessionRuntime::start_with_options(
            64,
            output.clone(),
            SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ..session_options(false)
            },
        )
        .unwrap();
        let id = format!("v1: {hash}");
        runtime
            .try_send(SessionCommand::AddTorrent {
                resume: Default::default(),
                repair_policy: Default::default(),
                verify_after_download: false,
                id: id.clone(),
                source: source.display().to_string(),
                output_folder: output.display().to_string(),
                only_files: Some(vec![0]),
                trackers: vec![],
                paused: false,
                transfer: Default::default(),
                peer_limit: Some(2),
                download_mode: DownloadMode::Normal,
                freeze_after_bytes: 1,
                upload_ratio: Default::default(),
                full_size: 32768,
            })
            .unwrap();
        let wait_for_partial = |after: u64| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let entries = runtime.journal.entries(Some(&hash));
                if let Some(entry) = entries.iter().find(|e| {
                    e.sequence > after
                        && e.announce.verified_bytes == 16384
                        && e.announce.outcome == "ok"
                }) {
                    assert_eq!(entry.announce.actual.downloaded, 0);
                    assert_eq!(entry.announce.sent.downloaded, 0);
                    assert_eq!(
                        entry.announce.sent.left, 16384,
                        "Unselected content must remain missing in tracker metrics"
                    );
                    assert_eq!(entry.announce.full_size, 32768);
                    assert_eq!(entry.announce.selected_bytes, Some(16384));
                    assert!(!serde_json::to_string(entry).unwrap().contains("secret"));
                    assert!(!entry.announce.tracker.contains("private-key"));
                    return entry.sequence;
                }
                assert!(
                    Instant::now() < deadline,
                    "No checked partial-file announce: {:?}",
                    entries
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        let before = wait_for_partial(0);
        let wait_added = || loop {
            match runtime.recv_timeout(Duration::from_secs(5)).unwrap() {
                SessionEvent::Added { .. } => break,
                SessionEvent::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        };
        wait_added();
        runtime
            .try_send(SessionCommand::Recheck { id: id.clone() })
            .unwrap();
        wait_added();
        let after_recheck = runtime
            .journal
            .entries(None)
            .last()
            .map(|e| e.sequence)
            .unwrap_or(before);
        wait_for_partial(after_recheck);
        runtime
            .try_send(SessionCommand::SetFiles {
                id: id.clone(),
                only_files: vec![0, 1],
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if runtime
                .journal
                .entries(None)
                .iter()
                .any(|e| e.announce.selected_bytes == Some(32768) && e.announce.outcome == "ok")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "File selection was not applied to the real engine"
            );
            thread::sleep(Duration::from_millis(10));
        }
        runtime
            .try_send(SessionCommand::Pause { id: id.clone() })
            .unwrap();
        loop {
            if let SessionEvent::StateChanged {
                state: "paused", ..
            } = runtime.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                break;
            }
        }
        runtime.try_send(SessionCommand::Recheck { id }).unwrap();
        wait_added();
        loop {
            match runtime.recv_timeout(Duration::from_secs(5)).unwrap() {
                SessionEvent::Stats {
                    paused: true,
                    file_progress,
                    ..
                } if file_progress.first() == Some(&16384) => break,
                SessionEvent::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        runtime.shutdown().unwrap();
        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();
        assert_eq!(std::fs::read(output.join("first.bin")).unwrap(), existing);
    }

    #[test]
    fn http_journal_captures_errors_warnings_cancellation_and_redacts_credentials() {
        use sha1::{Digest, Sha1};
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            sync::atomic::{AtomicBool, Ordering},
            time::Instant,
        };
        let message = "passkey secret rejected";
        let failure = format!("d14:failure reason{}:{}e", message.len(), message).into_bytes();
        let warning = format!(
            "d8:intervali60e5:peers0:15:warning message{}:{}e",
            message.len(),
            message
        )
        .into_bytes();
        for (status, body, expected) in [
            (Some(403), Vec::new(), "http_error"),
            (Some(200), b"not bencode".to_vec(), "invalid_response"),
            (
                Some(200),
                vec![b'x'; librqbit_tracker_comms::MAX_TRACKER_RESPONSE_BYTES + 1],
                "response_read_error",
            ),
            (Some(200), failure, "tracker_failure"),
            (Some(200), warning, "ok"),
            (None, Vec::new(), "cancelled"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let tracker = format!(
                "http://{}/private-path/announce?passkey=secret",
                listener.local_addr().unwrap()
            );
            let mut info = b"d6:lengthi1e4:name7:one.bin12:piece lengthi16384e6:pieces20:".to_vec();
            info.extend(Sha1::digest(b"x"));
            info.extend(b"7:privatei1ee");
            let hash = format!("{:x}", Sha1::digest(&info));
            let mut meta = format!("d8:announce{}:{}4:info", tracker.len(), tracker).into_bytes();
            meta.extend(info);
            meta.push(b'e');
            let source = dir.path().join("one.torrent");
            std::fs::write(&source, meta).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let server_stop = stop.clone();
            let (seen_tx, seen_rx) = mpsc::channel();
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    assert!(Instant::now() < deadline, "No HTTP request");
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                seen_tx.send(()).unwrap();
                if let Some(status) = status {
                    write!(
                        stream,
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    let _ = stream.write_all(&body);
                } else {
                    while !server_stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            let runtime = SessionRuntime::start_with_options(
                32,
                dir.path().join("out"),
                SessionOptions {
                    dht: None,
                    disable_local_service_discovery: true,
                    ..session_options(false)
                },
            )
            .unwrap();
            runtime
                .try_send(SessionCommand::AddTorrent {
                    resume: Default::default(),
                    repair_policy: Default::default(),
                    verify_after_download: false,
                    id: format!("v1: {hash}"),
                    source: source.display().to_string(),
                    output_folder: dir.path().join("out").display().to_string(),
                    only_files: None,
                    trackers: vec![],
                    paused: false,
                    transfer: Default::default(),
                    peer_limit: Some(1),
                    download_mode: DownloadMode::Normal,
                    freeze_after_bytes: 1,
                    upload_ratio: Default::default(),
                    full_size: 1,
                })
                .unwrap();
            seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let journal = runtime.journal.clone();
            if status.is_some() {
                let deadline = Instant::now() + Duration::from_secs(5);
                while journal.entries(None).is_empty() {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
            }
            runtime.shutdown().unwrap();
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap();
            let entries = journal.entries(Some(&hash));
            let starts: Vec<_> = entries
                .iter()
                .filter(|e| e.announce.event.as_deref() == Some("started"))
                .collect();
            assert_eq!(starts.len(), 1, "{expected}");
            // This fixture accepts exactly one connection. Orderly shutdown now
            // makes a real stopped attempt after completed initial requests;
            // the closed fixture must not turn that attempt into a fake success.
            if status.is_some() {
                assert_eq!(entries.len(), 2, "{expected}");
                let stopped = &entries[1].announce;
                assert_eq!(stopped.event.as_deref(), Some("stopped"));
                // Linux commonly reports refusal immediately; Windows may
                // still be connecting when the two-second stop deadline cancels
                // the attempt. Neither path may claim tracker acceptance.
                assert!(matches!(
                    stopped.outcome.as_str(),
                    "transport_error" | "cancelled"
                ));
                assert_eq!(stopped.reply.http_status, None);
                assert_eq!(stopped.request_transmitted, None);
            } else {
                // The initial request still holds the transport lock. The
                // bounded shutdown cancels it rather than inventing a reply.
                assert_eq!(entries.len(), 1);
            }
            let record = &starts[0].announce;
            assert_eq!(record.outcome, expected);
            assert_eq!(record.reply.http_status, status);
            assert_eq!(record.request_transmitted, status.map(|_| true));
            assert_eq!(record.sent.downloaded, 0);
            assert_eq!(record.sent.left, 1);
            let json = serde_json::to_string(&entries).unwrap();
            assert!(!json.contains("secret"));
            assert!(!json.contains("private-path"));
            if expected == "tracker_failure" {
                assert!(
                    record
                        .reply
                        .failure
                        .as_ref()
                        .unwrap()
                        .contains("[redacted]")
                );
            }
            if expected == "ok" {
                assert!(
                    record
                        .reply
                        .warning
                        .as_ref()
                        .unwrap()
                        .contains("[redacted]")
                );
                assert_eq!(record.reply.seeders, None);
            }
        }
    }

    #[test]
    fn udp_journal_distinguishes_connect_failure_from_a_transmitted_announce() {
        use sha1::{Digest, Sha1};
        use std::{net::UdpSocket, time::Instant};
        for fail_connect in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let tracker = format!(
                "udp://{}/announce?passkey=secret",
                socket.local_addr().unwrap()
            );
            let mut info = b"d6:lengthi1e4:name7:one.bin12:piece lengthi16384e6:pieces20:".to_vec();
            info.extend(Sha1::digest(b"x"));
            info.extend(b"7:privatei1ee");
            let hash = format!("{:x}", Sha1::digest(&info));
            let mut meta = format!("d8:announce{}:{}4:info", tracker.len(), tracker).into_bytes();
            meta.extend(info);
            meta.push(b'e');
            let source = dir.path().join("one.torrent");
            std::fs::write(&source, meta).unwrap();
            let server = thread::spawn(move || {
                let mut buf = [0; 256];
                let (len, addr) = socket.recv_from(&mut buf).unwrap();
                assert_eq!(len, 16);
                let mut tid = buf[12..16].to_vec();
                if !fail_connect {
                    let mut response = 0_u32.to_be_bytes().to_vec();
                    response.extend(&tid);
                    response.extend(42_u64.to_be_bytes());
                    socket.send_to(&response, addr).unwrap();
                    let (len, _) = socket.recv_from(&mut buf).unwrap();
                    assert_eq!(len, 98);
                    tid = buf[12..16].to_vec();
                }
                let mut error = 3_u32.to_be_bytes().to_vec();
                error.extend(tid);
                error.extend(b"denied secret");
                socket.send_to(&error, addr).unwrap();
            });
            let runtime = SessionRuntime::start_with_options(
                32,
                dir.path().join("out"),
                SessionOptions {
                    dht: None,
                    disable_local_service_discovery: true,
                    ..session_options(false)
                },
            )
            .unwrap();
            runtime
                .try_send(SessionCommand::AddTorrent {
                    resume: Default::default(),
                    repair_policy: Default::default(),
                    verify_after_download: false,
                    id: format!("v1: {hash}"),
                    source: source.display().to_string(),
                    output_folder: dir.path().join("out").display().to_string(),
                    only_files: None,
                    trackers: vec![],
                    paused: false,
                    transfer: Default::default(),
                    peer_limit: Some(1),
                    download_mode: DownloadMode::Normal,
                    freeze_after_bytes: 1,
                    upload_ratio: Default::default(),
                    full_size: 1,
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while runtime.journal.entries(None).is_empty() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            let entries = runtime.journal.entries(None);
            assert_eq!(entries[0].announce.outcome, "udp_error");
            assert_eq!(entries[0].announce.request_transmitted, Some(!fail_connect));
            assert!(
                entries[0]
                    .announce
                    .reply
                    .failure
                    .as_ref()
                    .unwrap()
                    .contains("[redacted]")
            );
            assert!(
                !serde_json::to_string(&entries[0])
                    .unwrap()
                    .contains("secret")
            );
            runtime.shutdown().unwrap();
            server.join().unwrap();
        }
    }
}
