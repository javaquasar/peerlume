//! Expiring frontend interest; absence never disables aggregate accounting.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetailInterest {
    pub files: bool,
    pub peers: bool,
}

#[derive(Clone, Default)]
pub(super) struct Shared(Arc<Mutex<BTreeMap<String, Lease>>>);
struct Lease {
    id: String,
    detail: DetailInterest,
    until: Instant,
}
impl Shared {
    pub fn set(&self, interest: Option<(String, DetailInterest)>, now: Instant) {
        let _ = self.set_for("desktop", interest, now);
    }

    pub fn set_for(
        &self,
        consumer: &str,
        interest: Option<(String, DetailInterest)>,
        now: Instant,
    ) -> Result<(), String> {
        if consumer.is_empty() || consumer.len() > 128 {
            return Err("Invalid detail consumer identity".into());
        }
        let mut leases = self.0.lock().unwrap_or_else(|error| error.into_inner());
        leases.retain(|_, lease| lease.until > now);
        if let Some((id, detail)) = interest {
            if !leases.contains_key(consumer) && leases.len() >= 8 {
                return Err("Too many active detail consumers".into());
            }
            leases.insert(
                consumer.to_owned(),
                Lease {
                    id,
                    detail,
                    until: now + Duration::from_secs(3),
                },
            );
        } else {
            leases.remove(consumer);
        }
        Ok(())
    }

    pub fn for_torrent(&self, id: &str, now: Instant) -> DetailInterest {
        let mut leases = self.0.lock().unwrap_or_else(|error| error.into_inner());
        leases.retain(|_, lease| lease.until > now);
        leases.values().filter(|lease| lease.id == id).fold(
            DetailInterest::default(),
            |mut detail, lease| {
                detail.files |= lease.detail.files;
                detail.peers |= lease.detail.peers;
                detail
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interests_expire_replace_and_release_without_retaining_a_torrent_map() {
        let shared = Shared::default();
        let now = Instant::now();
        shared.set(
            Some((
                "a".into(),
                DetailInterest {
                    files: true,
                    peers: false,
                },
            )),
            now,
        );
        assert!(shared.for_torrent("a", now).files);
        assert_eq!(shared.for_torrent("b", now), DetailInterest::default());
        shared.set(
            Some((
                "b".into(),
                DetailInterest {
                    files: false,
                    peers: true,
                },
            )),
            now,
        );
        assert_eq!(shared.for_torrent("a", now), DetailInterest::default());
        assert!(shared.for_torrent("b", now + Duration::from_secs(2)).peers);
        assert_eq!(
            shared.for_torrent("b", now + Duration::from_secs(3)),
            DetailInterest::default()
        );
        assert!(shared.0.lock().unwrap().is_empty());
        shared.set(
            Some((
                "a".into(),
                DetailInterest {
                    files: true,
                    peers: true,
                },
            )),
            now,
        );
        shared.set(None, now);
        assert_eq!(shared.for_torrent("a", now), DetailInterest::default());
    }

    #[test]
    fn independent_frontends_combine_bounded_leases_and_disconnect_individually() {
        let shared = Shared::default();
        let now = Instant::now();
        shared
            .set_for(
                "gui",
                Some((
                    "a".into(),
                    DetailInterest {
                        files: true,
                        peers: false,
                    },
                )),
                now,
            )
            .unwrap();
        shared
            .set_for(
                "tui",
                Some((
                    "a".into(),
                    DetailInterest {
                        files: false,
                        peers: true,
                    },
                )),
                now,
            )
            .unwrap();
        assert_eq!(
            shared.for_torrent("a", now),
            DetailInterest {
                files: true,
                peers: true
            }
        );
        shared.set_for("gui", None, now).unwrap();
        assert_eq!(
            shared.for_torrent("a", now),
            DetailInterest {
                files: false,
                peers: true
            }
        );
        for index in 0..7 {
            shared
                .set_for(
                    &index.to_string(),
                    Some(("b".into(), DetailInterest::default())),
                    now,
                )
                .unwrap();
        }
        assert!(
            shared
                .set_for("ninth", Some(("c".into(), DetailInterest::default())), now)
                .is_err()
        );
        assert_eq!(
            shared.for_torrent("a", now + Duration::from_secs(3)),
            DetailInterest::default()
        );
        assert!(shared.0.lock().unwrap().is_empty());
    }
}
