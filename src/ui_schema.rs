//! Shared navigation and component contract for every PeerLume frontend.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainPage {
    #[default]
    Torrents,
    Preferences,
}

impl MainPage {
    pub const ALL: [Self; 2] = [Self::Torrents, Self::Preferences];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Torrents => "Torrents",
            Self::Preferences => "Preferences",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TorrentDetailsTab {
    #[default]
    Files,
    Info,
    Settings,
    Peers,
    Trackers,
    Pieces,
    Log,
    Status,
}

impl TorrentDetailsTab {
    pub const ALL: [Self; 8] = [
        Self::Files,
        Self::Info,
        Self::Settings,
        Self::Peers,
        Self::Trackers,
        Self::Pieces,
        Self::Log,
        Self::Status,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Info => "Info",
            Self::Settings => "Torrent settings",
            Self::Peers => "Peers",
            Self::Trackers => "Trackers",
            Self::Pieces => "Pieces",
            Self::Log => "Log",
            Self::Status => "Status",
        }
    }

    pub fn next(self) -> Self {
        cycle(&Self::ALL, self, 1)
    }

    pub fn previous(self) -> Self {
        cycle(&Self::ALL, self, -1)
    }
}

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

    pub const fn label(self) -> &'static str {
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

    pub fn next(self) -> Self {
        cycle(&Self::ALL, self, 1)
    }

    pub fn previous(self) -> Self {
        cycle(&Self::ALL, self, -1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComponentId {
    RuntimeStatus,
    TorrentTable,
    AddTorrent,
    AddMagnet,
    PauseAll,
    ResumeAll,
    RemoveTorrent,
    FileSelection,
    TorrentInfo,
    TorrentSettings,
    PeerList,
    TrackerAudit,
    PieceProgress,
    EventLog,
    Preferences,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentSpec {
    pub id: ComponentId,
    pub label: &'static str,
    pub interactive: bool,
}

pub const COMPONENTS: [ComponentSpec; 15] = [
    ComponentSpec {
        id: ComponentId::RuntimeStatus,
        label: "Runtime status",
        interactive: false,
    },
    ComponentSpec {
        id: ComponentId::TorrentTable,
        label: "Torrent table",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::AddTorrent,
        label: "Add torrent",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::AddMagnet,
        label: "Add magnet",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::PauseAll,
        label: "Pause all",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::ResumeAll,
        label: "Resume all",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::RemoveTorrent,
        label: "Remove torrent",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::FileSelection,
        label: "File selection",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::TorrentInfo,
        label: "Torrent info",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::TorrentSettings,
        label: "Torrent settings",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::PeerList,
        label: "Peers",
        interactive: false,
    },
    ComponentSpec {
        id: ComponentId::TrackerAudit,
        label: "Tracker audit",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::PieceProgress,
        label: "Pieces",
        interactive: false,
    },
    ComponentSpec {
        id: ComponentId::EventLog,
        label: "Log",
        interactive: true,
    },
    ComponentSpec {
        id: ComponentId::Preferences,
        label: "Preferences",
        interactive: true,
    },
];

fn cycle<T: Copy + PartialEq>(values: &[T], current: T, direction: isize) -> T {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = (index as isize + direction).rem_euclid(values.len() as isize) as usize;
    values[next]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn shared_component_ids_are_unique() {
        let ids: HashSet<_> = COMPONENTS.iter().map(|component| component.id).collect();
        assert_eq!(ids.len(), COMPONENTS.len());
    }

    #[test]
    fn all_navigation_groups_cycle_without_diverging() {
        for tab in TorrentDetailsTab::ALL {
            assert_eq!(tab.next().previous(), tab);
            assert!(!tab.label().is_empty());
        }
        for tab in PreferencesTab::ALL {
            assert_eq!(tab.next().previous(), tab);
            assert_eq!(PreferencesTab::from_index(tab.index()), tab);
            assert!(!tab.label().is_empty());
        }
    }
}
