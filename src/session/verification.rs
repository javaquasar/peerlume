use super::*;

pub(super) struct CheckRun {
    pub resume: bool,
    pub require_complete: bool,
}

pub(super) async fn restart_for_check(
    session: &Arc<Session>,
    handle: &Arc<ManagedTorrent>,
    saved: &SessionCommand,
    peer_id: [u8; 20],
    counters: &Arc<std::sync::Mutex<HashMap<[u8; 20], TransferCounter>>>,
    root: Option<&crate::storage::sandbox::Directory>,
) -> Result<Arc<ManagedTorrent>, String> {
    let root = root
        .ok_or_else(|| "Pinned payload directory is unavailable".to_owned())?
        .clone();
    let SessionCommand::AddTorrent { .. } = saved else {
        return Err("Missing saved torrent settings".into());
    };
    // Reuse resolved metadata, including for magnets and removed source .torrent files.
    // A paused re-add forces the backend to read and hash the files without peers.
    let bytes = handle
        .with_metadata(|metadata| metadata.torrent_bytes.clone())
        .map_err(|_| "Torrent metadata is unavailable for verification".to_owned())?;
    let final_sample = final_transfer_sampler(handle, counters);
    librqbit_tracker_comms::stop_torrent_announces(handle.info_hash().0, peer_id).await;
    let deleted = session.delete(handle.id().into(), false).await;
    final_sample();
    deleted.map_err(|_| "Could not stop the torrent for verification".to_owned())?;
    add_for_check(session, bytes.to_vec(), saved, root).await
}

pub(super) async fn add_for_check(
    session: &Arc<Session>,
    bytes: Vec<u8>,
    saved: &SessionCommand,
    root: crate::storage::sandbox::Directory,
) -> Result<Arc<ManagedTorrent>, String> {
    let SessionCommand::AddTorrent {
        transfer,
        output_folder,
        only_files,
        peer_limit,
        trackers,
        ..
    } = saved
    else {
        return Err("Missing saved torrent settings".into());
    };
    let checked = session
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(crate::storage::sandbox::Factory { root }.boxed()),
                paused: true,
                only_files: only_files.clone(),
                overwrite: true,
                output_folder: Some(output_folder.clone()),
                peer_limit: *peer_limit,
                ratelimits: transfer.engine().limits,
                trackers: (!trackers.is_empty()).then(|| trackers.clone()),
                ..Default::default()
            }),
        )
        .await
        .map_err(|_| "Could not start the on-disk integrity check".to_owned())?
        .into_handle()
        .ok_or_else(|| "Verification returned no torrent handle".to_owned())?;
    checked.set_transfer_settings(transfer.engine());
    Ok(checked)
}
