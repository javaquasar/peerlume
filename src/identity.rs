use url::Url;

/// URL-declared identity candidates, not a verified server-side account lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackerIdentityKind {
    UserId,
    AccountKey,
    UrlUsername,
    UrlPassword,
    PossiblePathKey,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TrackerIdentityField {
    pub name: String,
    pub kind: TrackerIdentityKind,
    value: String,
}

impl std::fmt::Debug for TrackerIdentityField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackerIdentityField")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("value", &"[hidden]")
            .finish()
    }
}

impl TrackerIdentityField {
    pub fn label(&self) -> String {
        let kind = match self.kind {
            TrackerIdentityKind::UserId => "Declared user ID",
            TrackerIdentityKind::AccountKey => "Account/authentication key",
            TrackerIdentityKind::UrlUsername => "URL username",
            TrackerIdentityKind::UrlPassword => "URL password",
            TrackerIdentityKind::PossiblePathKey => "Possible account key in URL path",
        };
        format!("{kind} ({})", self.name)
    }

    pub fn display_value(&self, reveal: bool) -> &str {
        if reveal { &self.value } else { "[hidden]" }
    }
}

pub const TRACKER_IDENTITY_HINT: &str = "URL account/authentication fields are not the client's Peer ID. The actual server-side account ID cannot be verified locally.";

pub fn tracker_identity_fields(input: &str) -> Vec<TrackerIdentityField> {
    let Ok(url) = Url::parse(input) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    if !url.username().is_empty() {
        fields.push(TrackerIdentityField {
            name: "userinfo".into(),
            kind: TrackerIdentityKind::UrlUsername,
            value: url.username().into(),
        });
    }
    if let Some(value) = url.password().filter(|value| !value.is_empty()) {
        fields.push(TrackerIdentityField {
            name: "password".into(),
            kind: TrackerIdentityKind::UrlPassword,
            value: value.into(),
        });
    }
    for (name, value) in url.query_pairs() {
        if value.is_empty() {
            continue;
        }
        let kind = match name.to_ascii_lowercase().as_str() {
            "uid" | "user_id" | "userid" => TrackerIdentityKind::UserId,
            "uk" | "passkey" | "authkey" | "auth_key" | "token" | "access_token" | "api_key"
            | "apikey" | "key" | "userkey" | "user_key" | "auth" => TrackerIdentityKind::AccountKey,
            _ => continue,
        };
        fields.push(TrackerIdentityField {
            name: name.into_owned(),
            kind,
            value: value.into_owned(),
        });
    }
    let segments: Vec<_> = url.path_segments().into_iter().flatten().collect();
    for (index, value) in segments.iter().enumerate() {
        let named_key = index > 0 && is_key_segment_name(segments[index - 1]);
        if !value.is_empty() && (named_key || looks_like_passkey(value)) {
            fields.push(TrackerIdentityField {
                name: format!("path segment {}", index + 1),
                kind: TrackerIdentityKind::PossiblePathKey,
                value: (*value).into(),
            });
        }
    }
    fields
}

pub fn tracker_identity_summary(input: &str, reveal: bool) -> Vec<String> {
    let fields = tracker_identity_fields(input);
    if fields.is_empty() {
        return vec!["Tracker account: no recognized account ID/key in this URL".into()];
    }
    fields
        .iter()
        .map(|field| format!("{}: {}", field.label(), field.display_value(reveal)))
        .collect()
}

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
    if !segments.is_empty() {
        let mut redacted = segments.clone();
        for (index, segment) in segments.iter().enumerate() {
            if looks_like_passkey(segment)
                || (index > 0 && is_key_segment_name(&segments[index - 1]))
            {
                redacted[index] = "REDACTED".into();
            }
        }
        url.set_path(&redacted.join("/"));
    }
    url.to_string()
}

fn looks_like_passkey(value: &str) -> bool {
    value.len() >= 24
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

fn is_key_segment_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "uk" | "passkey" | "authkey" | "token" | "key"
    )
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

    #[test]
    fn uk_is_an_account_key_not_a_peer_id_or_a_verified_numeric_user_id() {
        let fields = tracker_identity_fields("http://tracker.test/ann?uk=test-account-key");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].kind, TrackerIdentityKind::AccountKey);
        assert_eq!(fields[0].name, "uk");
        assert_eq!(fields[0].display_value(false), "[hidden]");
        assert_eq!(fields[0].display_value(true), "test-account-key");
        assert!(!format!("{fields:?}").contains("test-account-key"));
    }

    #[test]
    fn identity_candidates_preserve_multiple_values_and_decode_query_encoding() {
        let fields = tracker_identity_fields(
            "https://tracker.test/announce?UID=42&passkey=abc%2B123%3D&passkey=second&uploaded=10&peer_id=client",
        );
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].kind, TrackerIdentityKind::UserId);
        assert_eq!(fields[0].display_value(true), "42");
        assert_eq!(fields[1].display_value(true), "abc+123=");
        assert_eq!(fields[2].display_value(true), "second");
    }

    #[test]
    fn identity_is_derived_from_current_url_and_does_not_invent_a_missing_account() {
        assert!(tracker_identity_fields("not a URL").is_empty());
        assert!(tracker_identity_fields("https://tracker.test/announce?left=0&uk=").is_empty());
        assert!(
            tracker_identity_summary("https://tracker.test/announce", false)[0]
                .contains("no recognized")
        );
        let first = tracker_identity_fields("https://tracker.test/announce?uk=first");
        let changed = tracker_identity_fields("https://tracker.test/announce?uk=changed");
        assert_eq!(first[0].display_value(true), "first");
        assert_eq!(changed[0].display_value(true), "changed");
    }

    #[test]
    fn url_userinfo_and_path_keys_are_separate_candidates() {
        let fields = tracker_identity_fields(
            "https://user:password@tracker.test/0123456789abcdef01234567/announce",
        );
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].kind, TrackerIdentityKind::UrlUsername);
        assert_eq!(fields[1].kind, TrackerIdentityKind::UrlPassword);
        assert_eq!(fields[2].kind, TrackerIdentityKind::PossiblePathKey);
        let named = tracker_identity_fields("https://tracker.test/uk/short-key/announce");
        assert_eq!(named[0].display_value(true), "short-key");
        assert!(
            !redact_tracker_url("https://tracker.test/uk/short-key/announce").contains("short-key")
        );
    }
}
