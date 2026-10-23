use super::*;

/// Replace the policy only while the transport is paused. Tracker workers capture
/// their policy at startup; resuming creates workers using the new registration.
pub(super) async fn apply_mode(
    session: &Arc<Session>,
    handle: &Arc<ManagedTorrent>,
    peer_id: [u8; 20],
    config: ModeConfig,
    registration: Option<&librqbit_tracker_comms::ReportingRegistration>,
    counters: &TransferCounters,
) -> Result<
    (
        Option<librqbit_tracker_comms::ReportingRegistration>,
        Option<String>,
    ),
    String,
> {
    config.mode.validate_threshold(config.freeze_after_bytes)?;
    let full_size = handle
        .with_metadata(|m| m.lengths().total_length())
        .map_err(|_| "Resolve torrent metadata before changing the mode".to_owned())?;
    config.mode.validate_metadata(true, full_size)?;
    let resume = matches!(handle.stats().state, TorrentStatsState::Live);
    if resume {
        let sample = final_transfer_sampler(handle, counters);
        let result = session.pause(handle).await;
        sample();
        result.map_err(|_| "Could not pause the torrent to apply the mode".to_owned())?;
    }
    let policy = config
        .mode
        .reporting_policy_with_threshold(full_size, config.freeze_after_bytes);
    let result = match registration {
        Some(guard) => guard.replace(policy).map(|()| None),
        None => {
            librqbit_tracker_comms::register_reporting_policy(handle.info_hash().0, peer_id, policy)
                .map(Some)
        }
    };
    match result {
        Ok(guard) => {
            let resume_error = if resume && session.unpause(handle).await.is_err() {
                Some("Mode applied, but the torrent could not resume; use Resume".into())
            } else {
                None
            };
            Ok((guard, resume_error))
        }
        Err(message) => {
            if resume {
                let _ = session.unpause(handle).await;
            }
            Err(message.into())
        }
    }
}

pub(super) fn repair_policy(saved: Option<&SessionCommand>) -> RepairPolicy {
    match saved {
        Some(SessionCommand::AddTorrent { repair_policy, .. }) => *repair_policy,
        _ => RepairPolicy::default(),
    }
}
