//! One conservative capability declaration for both frontends. Serialized does
//! not mean implemented. Existing transfers use acknowledged per-torrent controls.
use crate::ui_schema::PreferencesTab;

pub fn application(tab: PreferencesTab, field: &str) -> &'static str {
    match (tab, field) {
        (
            PreferencesTab::Downloads,
            "download_directory" | "start_when_added" | "select_all_files" | "sequential_download",
        )
        | (PreferencesTab::Speed, "download_limit_kib" | "upload_limit_kib")
        | (PreferencesTab::Connection, "max_connections_per_torrent")
        | (
            PreferencesTab::Seeding,
            "ratio_limit" | "time_limit_minutes" | "ratio_limit_enabled" | "time_limit_enabled",
        ) => "new torrents only; existing transfers unchanged",
        (PreferencesTab::Queue, "enabled" | "max_active_downloads")
        | (PreferencesTab::Privacy, "tracker_reporting") => "engine restart required",
        (PreferencesTab::BitTorrent, "peer_id_prefix") => {
            "standalone audit identity only; transfer engine unchanged"
        }
        (PreferencesTab::Privacy, "redact_tracker_credentials") => "frontend display only",
        _ => "stored only / not implemented in the transfer engine",
    }
}
