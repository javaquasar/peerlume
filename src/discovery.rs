#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerSource {
    Tracker,
    Dht,
    Pex,
    Lsd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoveryPolicy {
    private: bool,
    dht_enabled: bool,
    pex_enabled: bool,
    lsd_enabled: bool,
}

impl DiscoveryPolicy {
    pub fn new(private: bool, dht_enabled: bool, pex_enabled: bool, lsd_enabled: bool) -> Self {
        Self {
            private,
            dht_enabled,
            pex_enabled,
            lsd_enabled,
        }
    }

    pub fn allows(self, source: PeerSource) -> bool {
        match source {
            PeerSource::Tracker => true,
            PeerSource::Dht => !self.private && self.dht_enabled,
            PeerSource::Pex => !self.private && self.pex_enabled,
            PeerSource::Lsd => !self.private && self.lsd_enabled,
        }
    }

    pub fn sources_to_evict_after_private_metadata(
        peers: impl IntoIterator<Item = (String, PeerSource)>,
    ) -> Vec<String> {
        peers
            .into_iter()
            .filter(|(_, source)| *source != PeerSource::Tracker)
            .map(|(peer, _)| peer)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_torrent_accepts_only_tracker_peers() {
        let policy = DiscoveryPolicy::new(true, true, true, true);
        assert!(policy.allows(PeerSource::Tracker));
        assert!(!policy.allows(PeerSource::Dht));
        assert!(!policy.allows(PeerSource::Pex));
        assert!(!policy.allows(PeerSource::Lsd));
    }

    #[test]
    fn late_private_metadata_identifies_non_tracker_peers_for_eviction() {
        let evicted = DiscoveryPolicy::sources_to_evict_after_private_metadata([
            ("tracker-peer".into(), PeerSource::Tracker),
            ("dht-peer".into(), PeerSource::Dht),
            ("pex-peer".into(), PeerSource::Pex),
        ]);
        assert_eq!(evicted, ["dht-peer", "pex-peer"]);
    }
}
