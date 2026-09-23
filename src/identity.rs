use url::Url;

pub fn generate_peer_id(prefix: &str) -> Result<String, String> {
    let prefix = crate::metainfo::normalize_peer_id_prefix(prefix)?;
    let mut random = [0_u8; 20];
    getrandom::fill(&mut random).map_err(|error| error.to_string())?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(20);
    result.push_str(&prefix);
    for byte in random {
        if result.len() >= 20 {
            break;
        }
        result.push(HEX[(byte >> 4) as usize] as char);
        if result.len() < 20 {
            result.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    Ok(result)
}

pub fn redact_tracker_url(input: &str) -> String {
    let Ok(mut url) = Url::parse(input) else {
        return "<invalid tracker URL>".into();
    };
    if !url.username().is_empty() {
        let _ = url.set_username("REDACTED");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("REDACTED"));
    }
    if url.query().is_some() {
        url.set_query(Some("REDACTED"));
    }
    let segments: Vec<_> = url
        .path_segments()
        .map(|segments| segments.map(str::to_owned).collect())
        .unwrap_or_default();
    if segments.len() >= 2 {
        let mut redacted = segments;
        for segment in &mut redacted {
            if looks_like_passkey(segment) {
                *segment = "REDACTED".into();
            }
        }
        url.set_path(&redacted.join("/"));
    }
    url.to_string()
}

fn looks_like_passkey(value: &str) -> bool {
    value.len() >= 24 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_uses_prefix_and_has_protocol_length() {
        let first = generate_peer_id("-PL0001-").unwrap();
        let second = generate_peer_id("-PL0001-").unwrap();
        assert_eq!(first.len(), 20);
        assert!(first.starts_with("-PL0001-"));
        assert_ne!(first, second);
    }

    #[test]
    fn tracker_redaction_removes_common_secret_locations() {
        let redacted = redact_tracker_url(
            "https://user:password@tracker.test/0123456789abcdef01234567/announce?token=secret",
        );
        assert!(!redacted.contains("password"));
        assert!(!redacted.contains("0123456789abcdef01234567"));
        assert!(!redacted.contains("token=secret"));
        assert!(redacted.contains("REDACTED"));
    }
}
