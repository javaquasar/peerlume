//! Metadata-only discovery on the existing session's connector and discovery settings.
use super::*;
use crate::peer_info_reader::{MetadataPeer, read_metadata_bytes_from_peer};

const MAX_PEERS: usize = 64;
const PARALLEL_PEERS: usize = 4;
const MAX_METADATA: u32 = 8 * 1024 * 1024;

impl Session {
    /// Discover and authenticate the raw v1 info dictionary without registering
    /// a torrent, creating storage, joining its transfer lifecycle, or
    /// advertising through LSD or PEX. Callers must bound elapsed time and
    /// validate the returned bencode before materializing it.
    pub async fn review_magnet_metadata(
        self: &Arc<Self>,
        info_hash: Id20,
        trackers: Vec<url::Url>,
    ) -> anyhow::Result<Bytes> {
        if trackers.len() > 8 {
            bail!("too many metadata discovery trackers");
        }
        let peer_id = crate::generate_azereus_style(*b"rQ", (9, 0, 1, 0));
        let candidates: PeerStream = if trackers.is_empty() {
            self.dht
                .as_ref()
                .context("trackerless metadata review requires enabled DHT")?
                .get_peers(info_hash, None)
                .boxed()
        } else {
            if self.disable_trackers {
                bail!("tracker discovery is disabled");
            }
            let session = self.clone();
            let proxy_only = self.proxy_only;
            futures::stream::iter(trackers)
                .filter(move |tracker| {
                    futures::future::ready(
                        !proxy_only || matches!(tracker.scheme(), "http" | "https"),
                    )
                })
                .map(move |tracker| {
                    let session = session.clone();
                    async move {
                        TrackerComms::metadata_peers(
                            info_hash,
                            peer_id,
                            &tracker,
                            session.metadata_http_client.clone(),
                            &session.udp_tracker_client,
                        )
                        .await
                        .unwrap_or_default()
                    }
                })
                .buffer_unordered(PARALLEL_PEERS)
                .flat_map(futures::stream::iter)
                .boxed()
        };
        let mut seen = HashSet::new();
        let session = self.clone();
        let readers = candidates
            .take(MAX_PEERS)
            .filter(move |addr| {
                futures::future::ready(
                    seen.insert(*addr)
                        && !session.blocklist.has(addr.ip())
                        && session
                            .allowlist
                            .as_ref()
                            .is_none_or(|list| list.has(addr.ip())),
                )
            })
            .map(|addr| {
                read_metadata_bytes_from_peer(
                    MetadataPeer {
                        addr,
                        peer_id,
                        info_hash,
                        max_bytes: MAX_METADATA,
                        preview: true,
                    },
                    Some(self.peer_opts),
                    BlockingSpawner::new(1),
                    self.connector.clone(),
                    self.client_name_and_version.clone(),
                )
            })
            .buffer_unordered(PARALLEL_PEERS)
            .filter_map(|result| futures::future::ready(result.ok()));
        tokio::pin!(readers);
        tokio::select! {
            _ = self.cancellation_token.cancelled() => bail!("transfer session stopped"),
            result = readers.next() => result.map(|bytes| bytes.0)
                .context("no peer supplied valid magnet metadata"),
        }
    }
}
