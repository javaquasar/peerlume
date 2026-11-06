//! Authenticated current-user native frontend channel. This is intentionally
//! separate from the restricted external-agent protocol and its observations.
mod client;
mod server;
mod transport;
mod wire;

pub use client::{Client, ConnectionStatus, ConnectionView, Receipt, finish_pending_actions};
pub use server::Server;
pub use wire::{
    AccessAction, AccessView, NativeTorrent, Snapshot, StorageAction, TrashPlan, TrashScope,
};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_CONNECTIONS: usize = 8;

/// CLI/package smoke probe. Connects through the same trusted native backend as
/// the real frontend, then disconnects without stopping the session owner.
pub fn attach_check(directory: &std::path::Path) -> std::io::Result<()> {
    let client = Client::connect_or_launch(directory).map_err(std::io::Error::other)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while std::time::Instant::now() < deadline {
        if let Some(snapshot) = client.sync() {
            println!(
                "{}",
                serde_json::json!({"owner_instance":snapshot.instance_id,"torrents":snapshot.torrents.len(),"revision":snapshot.revision})
            );
            drop(client);
            return finish_pending_actions(std::time::Duration::from_secs(8))
                .map_err(std::io::Error::other);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Native frontend snapshot timed out",
    ))
}
