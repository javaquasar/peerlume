//! Expiring frontend interest; absence never disables aggregate accounting.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetailInterest {
    pub files: bool,
    pub peers: bool,
}

#[derive(Clone, Default)]
pub(super) struct Shared(Arc<Mutex<Option<Lease>>>);
struct Lease {
    id: String,
    detail: DetailInterest,
    until: Instant,
}
impl Shared {
    pub fn set(&self, interest: Option<(String, DetailInterest)>, now: Instant) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) =
            interest.map(|(id, detail)| Lease {
                id,
                detail,
                until: now + Duration::from_secs(3),
            });
    }

    pub fn for_torrent(&self, id: &str, now: Instant) -> DetailInterest {
        let mut lease = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if lease.as_ref().is_some_and(|lease| lease.until <= now) {
            *lease = None;
        }
        lease
            .as_ref()
            .filter(|lease| lease.id == id)
            .map(|lease| lease.detail)
            .unwrap_or_default()
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
        assert!(shared.0.lock().unwrap().is_none());
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
}
