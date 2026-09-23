use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use librqbit::{
    AddTorrent, AddTorrentOptions, ManagedTorrent, Session, SessionOptions, TorrentStatsState,
};
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    AddTorrent {
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
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
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

pub struct SessionRuntime {
    commands: tokio_mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    thread: Option<thread::JoinHandle<()>>,
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
        let (commands, mut command_rx) = tokio_mpsc::channel(capacity);
        let (event_tx, events) = mpsc::sync_channel(capacity);
        let thread = thread::Builder::new()
            .name("peerlume-session".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("Tokio runtime creation failed");
                runtime.block_on(async move {
                    let session = match Session::new_with_opts(
                        default_output_folder,
                        session_options(disable_trackers),
                    )
                    .await
                    {
                        Ok(session) => session,
                        Err(error) => {
                            let _ = event_tx.send(SessionEvent::Error {
                                id: String::new(),
                                message: format!("Could not start transfer engine: {error:#}"),
                            });
                            return;
                        }
                    };
                    let mut torrents: HashMap<String, Arc<ManagedTorrent>> = HashMap::new();
                    let mut ticker = tokio::time::interval(Duration::from_millis(500));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                for (id, handle) in &torrents {
                                    let stats = handle.stats();
                                    if let Some(message) = stats.error.clone() {
                                        let _ = event_tx.try_send(SessionEvent::Error {
                                            id: id.clone(),
                                            message,
                                        });
                                        continue;
                                    }
                                    let paused = matches!(
                                        stats.state,
                                        TorrentStatsState::Paused
                                            | TorrentStatsState::Initializing { paused: true }
                                    );
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
                                    let _ = event_tx.try_send(SessionEvent::Stats {
                                        id: id.clone(),
                                        downloaded_bytes: stats.progress_bytes,
                                        uploaded_bytes: stats.uploaded_bytes,
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
                                match command {
                                    SessionCommand::AddTorrent {
                                        id,
                                        source,
                                        output_folder,
                                        only_files,
                                        trackers,
                                        paused,
                                        peer_limit,
                                    } => {
                                        let add = if source.starts_with("magnet:") {
                                            Ok(AddTorrent::from_url(source))
                                        } else {
                                            std::fs::read(&source)
                                                .map(AddTorrent::from_bytes)
                                                .map_err(|error| format!("Could not read torrent file: {error}"))
                                        };
                                        let result = match add {
                                            Ok(add) => session
                                                .add_torrent(
                                                    add,
                                                    Some(AddTorrentOptions {
                                                        paused,
                                                        only_files,
                                                        overwrite: true,
                                                        output_folder: Some(output_folder),
                                                        peer_limit,
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
                                                torrents.insert(id.clone(), handle);
                                                let _ = event_tx.try_send(SessionEvent::Added { id });
                                            }
                                            Err(message) => {
                                                let _ = event_tx.try_send(SessionEvent::Error { id, message });
                                            }
                                        }
                                    }
                                    SessionCommand::Pause { id } => {
                                        let result = if let Some(handle) = torrents.get(&id) {
                                            session.pause(handle).await.map_err(|error| format!("{error:#}"))
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        emit_state_result(&event_tx, id, result, "paused");
                                    }
                                    SessionCommand::Resume { id } => {
                                        let result = if let Some(handle) = torrents.get(&id) {
                                            session.unpause(handle).await.map_err(|error| format!("{error:#}"))
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        emit_state_result(&event_tx, id, result, "downloading");
                                    }
                                    SessionCommand::Remove { id } => {
                                        let result = if let Some(handle) = torrents.remove(&id) {
                                            session
                                                .delete(handle.id().into(), false)
                                                .await
                                                .map_err(|error| format!("{error:#}"))
                                        } else {
                                            Err("Torrent is not registered in the transfer engine".into())
                                        };
                                        let event = match result {
                                            Ok(()) => SessionEvent::Removed { id },
                                            Err(message) => SessionEvent::Error { id, message },
                                        };
                                        let _ = event_tx.try_send(event);
                                    }
                                    SessionCommand::Shutdown => {
                                        session.stop().await;
                                        let _ = event_tx.send(SessionEvent::Stopped);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands,
            events,
            thread: Some(thread),
        })
    }

    pub fn try_send(&self, command: SessionCommand) -> Result<(), String> {
        self.commands
            .try_send(command)
            .map_err(|error| error.to_string())
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

fn emit_state_result(
    sender: &mpsc::SyncSender<SessionEvent>,
    id: String,
    result: Result<(), String>,
    state: &'static str,
) {
    let event = match result {
        Ok(()) => SessionEvent::StateChanged { id, state },
        Err(message) => SessionEvent::Error { id, message },
    };
    let _ = sender.try_send(event);
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        let _ = self.commands.blocking_send(SessionCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_capacity_is_rejected_before_starting_a_thread() {
        assert!(SessionRuntime::start(0, PathBuf::from("downloads"), false).is_err());
    }

    #[test]
    fn disabled_reporting_is_applied_to_the_whole_transfer_session() {
        assert!(session_options(true).disable_trackers);
        assert!(!session_options(false).disable_trackers);
    }

    #[test]
    fn state_result_reports_success_and_failure() {
        let (tx, rx) = mpsc::sync_channel(2);
        emit_state_result(&tx, "ok".into(), Ok(()), "paused");
        emit_state_result(&tx, "bad".into(), Err("failure".into()), "paused");
        assert_eq!(
            rx.recv().unwrap(),
            SessionEvent::StateChanged {
                id: "ok".into(),
                state: "paused"
            }
        );
        assert_eq!(
            rx.recv().unwrap(),
            SessionEvent::Error {
                id: "bad".into(),
                message: "failure".into()
            }
        );
    }
}
