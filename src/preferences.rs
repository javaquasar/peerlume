use eframe::egui;

use crate::settings::{
    ChokingAlgorithm, ClientSettings, ContentLayout, EncryptionMode, NoSpaceAction, PieceStrategy,
    PreallocationMode, TrackerReportingMode, VerifyMode,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreferencesTab {
    #[default]
    Downloads,
    Queue,
    Speed,
    Connection,
    BitTorrent,
    Seeding,
    Privacy,
    Storage,
    Automation,
}

impl PreferencesTab {
    pub const ALL: [Self; 9] = [
        Self::Downloads,
        Self::Queue,
        Self::Speed,
        Self::Connection,
        Self::BitTorrent,
        Self::Seeding,
        Self::Privacy,
        Self::Storage,
        Self::Automation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Downloads => "Downloads",
            Self::Queue => "Queue",
            Self::Speed => "Speed",
            Self::Connection => "Connection",
            Self::BitTorrent => "BitTorrent",
            Self::Seeding => "Seeding",
            Self::Privacy => "Privacy",
            Self::Storage => "Disk and cache",
            Self::Automation => "Automation",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }
}

#[derive(Debug, Default)]
pub struct PreferencesActions {
    pub update_blocklist: bool,
    pub check_blocklist_ip: bool,
}

pub struct PrivacyRuntime<'a> {
    pub blocklist_status: &'a str,
    pub blocklist_rules: usize,
    pub blocklist_ranges: usize,
    pub blocklist_updating: bool,
    pub probe: &'a mut String,
    pub probe_result: &'a str,
}

pub fn show_tab(
    ui: &mut egui::Ui,
    tab: PreferencesTab,
    settings: &mut ClientSettings,
    privacy_runtime: &mut PrivacyRuntime<'_>,
) -> PreferencesActions {
    let mut actions = PreferencesActions::default();
    egui::ScrollArea::vertical()
        .id_salt("preferences_tab_scroll")
        .show(ui, |ui| match tab {
            PreferencesTab::Downloads => show_downloads(ui, settings),
            PreferencesTab::Queue => show_queue(ui, settings),
            PreferencesTab::Speed => show_speed(ui, settings),
            PreferencesTab::Connection => show_connection(ui, settings),
            PreferencesTab::BitTorrent => show_bittorrent(ui, settings),
            PreferencesTab::Seeding => show_seeding(ui, settings),
            PreferencesTab::Privacy => {
                actions = show_privacy(ui, settings, privacy_runtime);
            }
            PreferencesTab::Storage => show_storage(ui, settings),
            PreferencesTab::Automation => show_automation(ui, settings),
        });
    actions
}

fn show_downloads(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.download;
    ui.heading("Downloads");
    group(ui, "Locations", |ui| {
        text_row(
            ui,
            "Default download directory",
            &mut value.download_directory,
        );
        ui.checkbox(
            &mut value.incomplete_directory_enabled,
            "Use a separate directory for incomplete files",
        );
        ui.add_enabled_ui(value.incomplete_directory_enabled, |ui| {
            text_row(ui, "Incomplete directory", &mut value.incomplete_directory);
        });
        ui.checkbox(
            &mut value.rename_partial_files,
            "Append .part to incomplete files",
        );
    });
    group(ui, "When adding a torrent", |ui| {
        ui.checkbox(
            &mut value.show_add_dialog,
            "Show torrent content and options",
        );
        ui.checkbox(&mut value.select_all_files, "Select all files by default");
        ui.checkbox(&mut value.start_when_added, "Start torrent immediately");
        combo_row(
            ui,
            "Content layout",
            "content_layout",
            match value.content_layout {
                ContentLayout::Original => "Original",
                ContentLayout::Subfolder => "Create subfolder",
                ContentLayout::NoSubfolder => "Do not create subfolder",
            },
            |ui| {
                ui.selectable_value(
                    &mut value.content_layout,
                    ContentLayout::Original,
                    "Original",
                );
                ui.selectable_value(
                    &mut value.content_layout,
                    ContentLayout::Subfolder,
                    "Create subfolder",
                );
                ui.selectable_value(
                    &mut value.content_layout,
                    ContentLayout::NoSubfolder,
                    "Do not create subfolder",
                );
            },
        );
        combo_row(
            ui,
            "Preallocation",
            "preallocation",
            match value.preallocation {
                PreallocationMode::Off => "Off",
                PreallocationMode::Fast => "Fast",
                PreallocationMode::Full => "Full",
            },
            |ui| {
                ui.selectable_value(&mut value.preallocation, PreallocationMode::Off, "Off");
                ui.selectable_value(&mut value.preallocation, PreallocationMode::Fast, "Fast");
                ui.selectable_value(&mut value.preallocation, PreallocationMode::Full, "Full");
            },
        );
    });
    group(ui, "Verification and piece order", |ui| {
        combo_row(
            ui,
            "Verify existing data",
            "verify_mode",
            match value.verify_on_add {
                VerifyMode::Fast => "Fast",
                VerifyMode::Full => "Full",
            },
            |ui| {
                ui.selectable_value(&mut value.verify_on_add, VerifyMode::Fast, "Fast");
                ui.selectable_value(&mut value.verify_on_add, VerifyMode::Full, "Full");
            },
        );
        ui.checkbox(
            &mut value.verify_on_completion,
            "Verify torrent after download completes",
        );
        ui.checkbox(
            &mut value.sequential_download,
            "Download pieces sequentially by default",
        );
    });
}

fn show_queue(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.queue;
    ui.heading("Queue");
    ui.checkbox(&mut value.enabled, "Enable download queue");
    ui.add_enabled_ui(value.enabled, |ui| {
        number_row(
            ui,
            "Maximum active downloads",
            &mut value.max_active_downloads,
            1..=1000,
        );
        number_row(
            ui,
            "Maximum simultaneous checks",
            &mut value.max_active_checks,
            1..=64,
        );
        ui.checkbox(&mut value.add_to_top, "Add new torrents to top of queue");
        ui.checkbox(
            &mut value.start_next_automatically,
            "Start next torrent automatically",
        );
        ui.checkbox(&mut value.prefer_downloads, "Prefer downloads over seeds");
        ui.checkbox(&mut value.stalled_enabled, "Detect stalled torrents");
        ui.add_enabled_ui(value.stalled_enabled, |ui| {
            u64_row(
                ui,
                "Stalled after minutes",
                &mut value.stalled_minutes,
                1..=10080,
            );
            ui.checkbox(
                &mut value.ignore_inactive,
                "Do not count inactive torrents against queue limits",
            );
        });
    });
}

fn show_speed(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.speed;
    ui.heading("Speed");
    group(ui, "Global limits", |ui| {
        u64_row(
            ui,
            "Download limit KiB/s (0 = unlimited)",
            &mut value.download_limit_kib,
            0..=10_000_000,
        );
        u64_row(
            ui,
            "Upload limit KiB/s (0 = unlimited)",
            &mut value.upload_limit_kib,
            0..=10_000_000,
        );
        ui.checkbox(
            &mut value.limit_local_peers,
            "Apply limits to local-network peers",
        );
    });
    group(ui, "Alternative speed profile", |ui| {
        ui.checkbox(
            &mut value.alternate_enabled,
            "Enable alternative speed limits",
        );
        ui.add_enabled_ui(value.alternate_enabled, |ui| {
            u64_row(
                ui,
                "Alternative download KiB/s",
                &mut value.alternate_download_kib,
                1..=10_000_000,
            );
            u64_row(
                ui,
                "Alternative upload KiB/s",
                &mut value.alternate_upload_kib,
                1..=10_000_000,
            );
        });
        ui.checkbox(&mut value.schedule_enabled, "Schedule alternative limits");
        ui.add_enabled_ui(value.schedule_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Start minute of day");
                ui.add(egui::DragValue::new(&mut value.schedule_begin_minutes).range(0..=1439));
                ui.label(format_minutes(value.schedule_begin_minutes));
            });
            ui.horizontal(|ui| {
                ui.label("End minute of day");
                ui.add(egui::DragValue::new(&mut value.schedule_end_minutes).range(0..=1439));
                ui.label(format_minutes(value.schedule_end_minutes));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Days");
                for (bit, label) in [
                    (1, "Sun"),
                    (2, "Mon"),
                    (4, "Tue"),
                    (8, "Wed"),
                    (16, "Thu"),
                    (32, "Fri"),
                    (64, "Sat"),
                ] {
                    let mut enabled = value.schedule_days & bit != 0;
                    if ui.checkbox(&mut enabled, label).changed() {
                        if enabled {
                            value.schedule_days |= bit;
                        } else {
                            value.schedule_days &= !bit;
                        }
                    }
                }
            });
        });
    });
}

fn show_connection(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.connection;
    ui.heading("Connection");
    group(ui, "Peer limits", |ui| {
        number_row(
            ui,
            "Global maximum connections",
            &mut value.max_connections_global,
            1..=100_000,
        );
        number_row(
            ui,
            "Maximum per torrent",
            &mut value.max_connections_per_torrent,
            1..=100_000,
        );
        u64_row(
            ui,
            "Connection timeout seconds",
            &mut value.connection_timeout_seconds,
            1..=600,
        );
        number_row(ui, "Retry count", &mut value.retry_count, 0..=100);
    });
    group(ui, "Incoming connections", |ui| {
        ui.horizontal(|ui| {
            ui.label("Listen port");
            ui.add(egui::DragValue::new(&mut value.listen_port).range(1..=65535));
        });
        ui.checkbox(
            &mut value.random_port_on_start,
            "Choose random port on startup",
        );
        ui.checkbox(
            &mut value.port_forwarding,
            "Enable UPnP / NAT-PMP port forwarding",
        );
    });
    group(ui, "Transport", |ui| {
        ui.checkbox(&mut value.tcp_enabled, "Enable TCP");
        ui.checkbox(&mut value.utp_enabled, "Enable uTP");
        ui.add_enabled_ui(value.utp_enabled, |ui| {
            ui.checkbox(&mut value.prefer_utp, "Prefer uTP over TCP");
        });
    });
    group(ui, "Binding and proxy", |ui| {
        text_row(
            ui,
            "Network interface (empty = any)",
            &mut value.network_interface,
        );
        text_row(ui, "Bind address (empty = any)", &mut value.bind_address);
        text_row(ui, "Proxy URL", &mut value.proxy_url);
        ui.checkbox(
            &mut value.proxy_peer_connections,
            "Proxy peer connections as well as tracker traffic",
        );
    });
}

fn show_bittorrent(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.bittorrent;
    ui.heading("BitTorrent behavior");
    group(ui, "Client identity", |ui| {
        text_row(
            ui,
            "Peer ID prefix (1-8 ASCII bytes)",
            &mut value.peer_id_prefix,
        );
        ui.label("A new 20-byte peer ID is generated when a torrent is added.");
    });
    group(ui, "Piece selection", |ui| {
        ui.radio_value(
            &mut value.piece_strategy,
            PieceStrategy::RarestFirst,
            "Rarest first",
        );
        ui.radio_value(
            &mut value.piece_strategy,
            PieceStrategy::Sequential,
            "Sequential",
        );
        ui.checkbox(
            &mut value.prioritize_first_last,
            "Prioritize first and last piece of each selected file",
        );
        ui.checkbox(&mut value.endgame, "Enable endgame mode");
    });
    group(ui, "Peer discovery", |ui| {
        ui.checkbox(&mut value.dht, "Enable DHT");
        ui.checkbox(&mut value.pex, "Enable peer exchange (PEX)");
        ui.checkbox(&mut value.lsd, "Enable local service discovery (LSD)");
        ui.label("Private torrents always disable DHT, PEX and LSD.");
    });
    group(ui, "Requests and trackers", |ui| {
        number_row(
            ui,
            "Outstanding block requests",
            &mut value.max_outstanding_requests,
            1..=100_000,
        );
        number_row(
            ui,
            "Outgoing connections per second",
            &mut value.outgoing_connections_per_second,
            1..=10_000,
        );
        ui.label("Additional trackers for public torrents, one URL per line");
        ui.add(egui::TextEdit::multiline(&mut value.public_extra_trackers).desired_rows(5));
    });
}

fn show_seeding(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.seeding;
    ui.heading("Seeding");
    number_row(
        ui,
        "Maximum active seeds",
        &mut value.max_active_seeds,
        1..=10_000,
    );
    number_row(
        ui,
        "Upload slots per torrent",
        &mut value.upload_slots_per_torrent,
        1..=10_000,
    );
    ui.checkbox(
        &mut value.prefer_seeds,
        "Prefer seeds when the global active limit is reached",
    );
    group(ui, "Stop conditions", |ui| {
        ui.checkbox(&mut value.ratio_limit_enabled, "Stop at ratio");
        ui.add_enabled_ui(value.ratio_limit_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Ratio");
                ui.add(
                    egui::DragValue::new(&mut value.ratio_limit)
                        .range(0.01..=1000.0)
                        .speed(0.1),
                );
            });
        });
        ui.checkbox(&mut value.time_limit_enabled, "Stop after seeding time");
        ui.add_enabled_ui(value.time_limit_enabled, |ui| {
            u64_row(ui, "Minutes", &mut value.time_limit_minutes, 1..=10_000_000);
        });
        ui.checkbox(&mut value.idle_limit_enabled, "Stop after idle time");
        ui.add_enabled_ui(value.idle_limit_enabled, |ui| {
            u64_row(
                ui,
                "Idle minutes",
                &mut value.idle_limit_minutes,
                1..=10_000_000,
            );
        });
    });
    combo_row(
        ui,
        "Choking algorithm",
        "choking_algorithm",
        match value.choking_algorithm {
            ChokingAlgorithm::TitForTat => "Tit for tat",
            ChokingAlgorithm::FixedSlots => "Fixed slots",
            ChokingAlgorithm::RateBased => "Rate based",
        },
        |ui| {
            ui.selectable_value(
                &mut value.choking_algorithm,
                ChokingAlgorithm::TitForTat,
                "Tit for tat",
            );
            ui.selectable_value(
                &mut value.choking_algorithm,
                ChokingAlgorithm::FixedSlots,
                "Fixed slots",
            );
            ui.selectable_value(
                &mut value.choking_algorithm,
                ChokingAlgorithm::RateBased,
                "Rate based",
            );
        },
    );
}

fn show_privacy(
    ui: &mut egui::Ui,
    settings: &mut ClientSettings,
    runtime: &mut PrivacyRuntime<'_>,
) -> PreferencesActions {
    let value = &mut settings.privacy;
    let mut actions = PreferencesActions::default();
    ui.heading("Privacy");
    combo_row(
        ui,
        "Encryption mode",
        "encryption_mode",
        match value.encryption {
            EncryptionMode::Prefer => "Prefer encryption",
            EncryptionMode::Require => "Require encryption",
            EncryptionMode::AllowPlaintext => "Allow plaintext",
        },
        |ui| {
            ui.selectable_value(
                &mut value.encryption,
                EncryptionMode::Prefer,
                "Prefer encryption",
            );
            ui.selectable_value(
                &mut value.encryption,
                EncryptionMode::Require,
                "Require encryption",
            );
            ui.selectable_value(
                &mut value.encryption,
                EncryptionMode::AllowPlaintext,
                "Allow plaintext",
            );
        },
    );
    ui.checkbox(
        &mut value.redact_tracker_credentials,
        "Redact tracker passkeys and credentials in logs",
    );
    group(ui, "Tracker reporting", |ui| {
        combo_row(
            ui,
            "Announce mode",
            "tracker_reporting_mode",
            match value.tracker_reporting {
                TrackerReportingMode::Accurate => "Accurate counters",
                TrackerReportingMode::Disabled => "Disabled",
            },
            |ui| {
                ui.selectable_value(
                    &mut value.tracker_reporting,
                    TrackerReportingMode::Accurate,
                    "Accurate counters",
                );
                ui.selectable_value(
                    &mut value.tracker_reporting,
                    TrackerReportingMode::Disabled,
                    "Disabled",
                );
            },
        );
        ui.label(
            "Disabled prevents all announce and scrape requests. Private torrents then cannot discover peers.",
        );
        ui.label("Changing this setting requires an application restart.");
    });
    group(ui, "IP blocklist", |ui| {
        ui.checkbox(&mut value.blocklist_enabled, "Enable blocklist");
        ui.add_enabled_ui(value.blocklist_enabled, |ui| {
            text_row(ui, "Blocklist URL (.gz)", &mut value.blocklist_url);
            ui.checkbox(
                &mut value.blocklist_auto_update,
                "Update automatically on startup",
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!runtime.blocklist_updating, egui::Button::new("Update now"))
                    .clicked()
                {
                    actions.update_blocklist = true;
                }
                ui.label(runtime.blocklist_status);
            });
            ui.label(format!(
                "Active rules: {} ({} merged ranges)",
                runtime.blocklist_rules, runtime.blocklist_ranges
            ));
            ui.horizontal(|ui| {
                ui.label("Test IP");
                ui.text_edit_singleline(runtime.probe);
                if ui.button("Check").clicked() {
                    actions.check_blocklist_ip = true;
                }
                ui.label(runtime.probe_result);
            });
        });
    });
    actions
}

fn show_storage(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.storage;
    ui.heading("Disk and cache");
    group(ui, "Cache", |ui| {
        u64_row(
            ui,
            "Memory cache MiB",
            &mut value.cache_size_mib,
            0..=1_048_576,
        );
        u64_row(
            ui,
            "Maximum queued writes MiB",
            &mut value.max_queued_write_mib,
            1..=1_048_576,
        );
        u64_row(
            ui,
            "Save resume data every seconds",
            &mut value.resume_save_interval_seconds,
            10..=86_400,
        );
        u64_row(
            ui,
            "Verification pause milliseconds",
            &mut value.verify_sleep_milliseconds,
            0..=60_000,
        );
        ui.label("Cache size changes require an application restart.");
    });
    group(ui, "Free space", |ui| {
        ui.checkbox(
            &mut value.check_free_space,
            "Check free space before and during downloads",
        );
        ui.add_enabled_ui(value.check_free_space, |ui| {
            u64_row(
                ui,
                "Minimum free space MiB",
                &mut value.minimum_free_space_mib,
                0..=100_000_000,
            );
            combo_row(
                ui,
                "When space is low",
                "no_space",
                match value.no_space_action {
                    NoSpaceAction::PauseTorrent => "Pause torrent",
                    NoSpaceAction::WarnOnly => "Warn only",
                },
                |ui| {
                    ui.selectable_value(
                        &mut value.no_space_action,
                        NoSpaceAction::PauseTorrent,
                        "Pause torrent",
                    );
                    ui.selectable_value(
                        &mut value.no_space_action,
                        NoSpaceAction::WarnOnly,
                        "Warn only",
                    );
                },
            );
        });
    });
    group(ui, "Completed downloads", |ui| {
        ui.checkbox(&mut value.move_completed, "Move completed downloads");
        ui.add_enabled_ui(value.move_completed, |ui| {
            text_row(ui, "Completed directory", &mut value.completed_directory);
        });
    });
}

fn show_automation(ui: &mut egui::Ui, settings: &mut ClientSettings) {
    let value = &mut settings.automation;
    ui.heading("Automation");
    group(ui, "Watch directory", |ui| {
        ui.checkbox(
            &mut value.watch_directory_enabled,
            "Watch a directory for .torrent files",
        );
        ui.add_enabled_ui(value.watch_directory_enabled, |ui| {
            text_row(ui, "Watch directory", &mut value.watch_directory);
            ui.checkbox(
                &mut value.remove_original_torrent,
                "Remove original .torrent after adding",
            );
        });
    });
    group(ui, "Scripts", |ui| {
        text_row(ui, "Run when torrent is added", &mut value.script_on_added);
        text_row(
            ui,
            "Run when download completes",
            &mut value.script_on_completed,
        );
        text_row(
            ui,
            "Run when seeding completes",
            &mut value.script_on_seeding_completed,
        );
    });
    group(ui, "Torrent backups", |ui| {
        ui.checkbox(
            &mut value.backup_torrents_enabled,
            "Back up added .torrent files",
        );
        ui.add_enabled_ui(value.backup_torrents_enabled, |ui| {
            text_row(ui, "Backup directory", &mut value.backup_directory);
        });
    });
}

fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.group(|ui| {
        ui.strong(title);
        ui.add_space(4.0);
        add(ui);
    });
    ui.add_space(8.0);
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
    });
}

fn number_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut usize,
    range: std::ops::RangeInclusive<usize>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(range));
    });
}

fn u64_row(ui: &mut egui::Ui, label: &str, value: &mut u64, range: std::ops::RangeInclusive<u64>) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(range));
    });
}

fn combo_row(
    ui: &mut egui::Ui,
    label: &str,
    id: &'static str,
    selected: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(selected)
            .show_ui(ui, add);
    });
}

fn format_minutes(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_indices_round_trip() {
        for tab in PreferencesTab::ALL {
            assert_eq!(PreferencesTab::from_index(tab.index()), tab);
        }
        assert_eq!(PreferencesTab::from_index(100), PreferencesTab::Downloads);
    }
}
