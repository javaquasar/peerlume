//! Tracker credential providers. Errors never include URLs or provider details.
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretUnavailable;
impl std::fmt::Display for SecretUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tracker credential store unavailable; unlock the OS secret store or re-enter the tracker URL")
    }
}
impl std::error::Error for SecretUnavailable {}

pub trait TrackerSecrets: Send + Sync {
    fn protect(&self, value: &str) -> Result<Vec<u8>, SecretUnavailable>;
    fn restore(&self, reference: &[u8]) -> Result<String, SecretUnavailable>;
}

/// Ephemeral store for in-memory databases and explicitly injected test profiles.
/// References, not plaintext URLs, go into SQLite. Dropping this store loses keys.
#[derive(Default)]
pub struct MemorySecrets(Mutex<BTreeMap<Vec<u8>, String>>);
impl TrackerSecrets for MemorySecrets {
    fn protect(&self, value: &str) -> Result<Vec<u8>, SecretUnavailable> {
        let reference = crate::control::access::digest(value.as_bytes()).into_bytes();
        self.0
            .lock()
            .map_err(|_| SecretUnavailable)?
            .insert(reference.clone(), value.into());
        Ok(reference)
    }
    fn restore(&self, reference: &[u8]) -> Result<String, SecretUnavailable> {
        self.0
            .lock()
            .map_err(|_| SecretUnavailable)?
            .get(reference)
            .cloned()
            .ok_or(SecretUnavailable)
    }
}

pub struct PlatformSecrets;
impl TrackerSecrets for PlatformSecrets {
    fn protect(&self, value: &str) -> Result<Vec<u8>, SecretUnavailable> {
        #[cfg(windows)]
        {
            crate::state::dpapi_protect(value.as_bytes()).map_err(|_| SecretUnavailable)
        }
        #[cfg(target_os = "linux")]
        {
            let reference = format!(
                "secret-service-v1:{}",
                crate::control::access::digest(value.as_bytes())
            );
            keyring::Entry::new("PeerLume tracker credentials", &reference)
                .map_err(|_| SecretUnavailable)?
                .set_password(value)
                .map_err(|_| SecretUnavailable)?;
            Ok(reference.into_bytes())
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = value;
            Err(SecretUnavailable)
        }
    }
    fn restore(&self, reference: &[u8]) -> Result<String, SecretUnavailable> {
        #[cfg(windows)]
        {
            String::from_utf8(
                crate::state::dpapi_unprotect(reference).map_err(|_| SecretUnavailable)?,
            )
            .map_err(|_| SecretUnavailable)
        }
        #[cfg(target_os = "linux")]
        {
            let reference = std::str::from_utf8(reference).map_err(|_| SecretUnavailable)?;
            let hash = reference
                .strip_prefix("secret-service-v1:")
                .ok_or(SecretUnavailable)?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(SecretUnavailable);
            }
            keyring::Entry::new("PeerLume tracker credentials", reference)
                .map_err(|_| SecretUnavailable)?
                .get_password()
                .map_err(|_| SecretUnavailable)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = reference;
            Err(SecretUnavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ephemeral_store_round_trips_and_loses_no_secrets_into_its_reference() {
        let store = MemorySecrets::default();
        let url = "https://tracker.test/announce?passkey=SYNTHETIC_SECRET";
        let reference = store.protect(url).unwrap();
        assert!(!String::from_utf8_lossy(&reference).contains("SYNTHETIC_SECRET"));
        assert_eq!(store.restore(&reference).unwrap(), url);
        assert_eq!(
            MemorySecrets::default().restore(&reference),
            Err(SecretUnavailable)
        );
        assert_eq!(store.protect(url).unwrap(), reference);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_unknown_reference_fails_without_contacting_a_secret_service() {
        assert_eq!(
            PlatformSecrets.restore(b"plaintext-secret"),
            Err(SecretUnavailable)
        );
        assert_eq!(
            PlatformSecrets.restore(b"secret-service-v1:bad"),
            Err(SecretUnavailable)
        );
    }
}
