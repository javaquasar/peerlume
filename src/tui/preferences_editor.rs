//! Every serialized field in the nine shared preference sections is editable.
use crate::{settings::ClientSettings, ui_schema::PreferencesTab};
use serde_json::Value;

pub(super) fn section(tab: PreferencesTab) -> &'static str {
    match tab {
        PreferencesTab::Downloads => "download",
        PreferencesTab::Queue => "queue",
        PreferencesTab::Speed => "speed",
        PreferencesTab::Connection => "connection",
        PreferencesTab::BitTorrent => "bittorrent",
        PreferencesTab::Seeding => "seeding",
        PreferencesTab::Privacy => "privacy",
        PreferencesTab::Storage => "storage",
        PreferencesTab::Automation => "automation",
    }
}

pub(super) fn fields(settings: &ClientSettings, tab: PreferencesTab) -> Vec<(String, Value)> {
    serde_json::to_value(settings).expect("settings serialize")[section(tab)]
        .as_object()
        .expect("preference section is an object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub(super) fn choices(field: &str) -> &'static [&'static str] {
    match field {
        "content_layout" => &["original", "subfolder", "no_subfolder"],
        "preallocation" => &["off", "fast", "full"],
        "verify_on_add" => &["fast", "full"],
        "piece_strategy" => &["rarest_first", "sequential"],
        "encryption" => &["prefer", "require", "allow_plaintext"],
        "tracker_reporting" => &["accurate", "disabled"],
        "choking_algorithm" => &["tit_for_tat", "fixed_slots", "rate_based"],
        "no_space_action" => &["pause_torrent", "warn_only"],
        _ => &[],
    }
}

pub(super) fn secret(field: &str) -> bool {
    field.ends_with("_url") || field.starts_with("script_on_")
}

pub(super) fn display(field: &str, value: &Value) -> String {
    if secret(field) && value.as_str().is_some_and(|text| !text.is_empty()) {
        "[hidden; Enter replaces, Esc cancels]".into()
    } else {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    }
}

pub(super) fn application(tab: PreferencesTab, field: &str) -> &'static str {
    // A field in the shared GUI schema does not imply engine support.
    match (tab, field) {
        (
            PreferencesTab::Downloads,
            "download_directory" | "start_when_added" | "select_all_files" | "sequential_download",
        )
        | (PreferencesTab::Queue, "enabled" | "max_active_downloads")
        | (PreferencesTab::Speed, "download_limit_kib" | "upload_limit_kib")
        | (PreferencesTab::Connection, "max_connections_per_torrent")
        | (PreferencesTab::Privacy, "tracker_reporting") => "next start / new torrents",
        _ => "stored only; engine support not guaranteed",
    }
}

/// Parse against the existing serialized type, then round-trip through the shared
/// Rust schema. Invalid drafts never replace the original settings.
pub(super) fn edit(
    settings: &ClientSettings,
    tab: PreferencesTab,
    key: &str,
    text: &str,
) -> Result<ClientSettings, String> {
    let mut document =
        serde_json::to_value(settings).map_err(|_| "Could not serialize settings")?;
    let current = document[section(tab)]
        .get(key)
        .ok_or("Unknown preference field")?;
    let value = if current.is_string() {
        if text.starts_with('"') {
            Value::String(
                serde_json::from_str::<String>(text).map_err(|_| "Invalid quoted string")?,
            )
        } else {
            Value::String(text.to_owned())
        }
    } else {
        serde_json::from_str::<Value>(text).map_err(|_| "Enter a valid number or true/false")?
    };
    if (current.is_boolean() && !value.is_boolean()) || (current.is_number() && !value.is_number())
    {
        return Err("Preference value has the wrong type".into());
    }
    document[section(tab)][key] = value;
    serde_json::from_value(document)
        .map_err(|_| "Invalid value for this preference (check type, range or enum choices)".into())
}

pub(super) fn validate(settings: &ClientSettings) -> Result<(), String> {
    let issues = settings.validate();
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues
            .iter()
            .map(|issue| format!("{}: {}", issue.field, issue.message))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_in_every_section_round_trips_and_is_not_a_read_only_debug_dump() {
        let settings = ClientSettings::default();
        let mut total = 0;
        for tab in PreferencesTab::ALL {
            for (key, value) in fields(&settings, tab) {
                let text = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                assert_eq!(
                    edit(&settings, tab, &key, &text).unwrap(),
                    settings,
                    "{key}"
                );
                if let Some(boolean) = value.as_bool() {
                    let changed = edit(&settings, tab, &key, &(!boolean).to_string()).unwrap();
                    assert_ne!(changed, settings, "{key}");
                }
                total += 1;
            }
        }
        assert!(total > 80);
    }

    #[test]
    fn numeric_string_enum_and_cross_field_validation_preserve_the_original() {
        let settings = ClientSettings::default();
        let changed = edit(
            &settings,
            PreferencesTab::Connection,
            "listen_port",
            "50000",
        )
        .unwrap();
        assert_eq!(changed.connection.listen_port, 50000);
        assert!(
            edit(
                &settings,
                PreferencesTab::Connection,
                "listen_port",
                "70000"
            )
            .is_err()
        );
        assert!(edit(&settings, PreferencesTab::Connection, "listen_port", "-1").is_err());
        assert!(
            edit(
                &settings,
                PreferencesTab::Connection,
                "listen_port",
                "\"6000\""
            )
            .is_err()
        );
        assert!(edit(&settings, PreferencesTab::Connection, "unknown", "true").is_err());
        assert!(edit(&settings, PreferencesTab::Privacy, "encryption", "typo").is_err());
        let invalid = edit(
            &settings,
            PreferencesTab::Connection,
            "max_connections_per_torrent",
            "0",
        )
        .unwrap();
        assert!(validate(&invalid).is_err());
        assert!(validate(&settings).is_ok());
        assert_eq!(
            edit(
                &settings,
                PreferencesTab::Downloads,
                "download_directory",
                "folder with spaces"
            )
            .unwrap()
            .download
            .download_directory,
            "folder with spaces"
        );
        assert!(
            !display(
                "proxy_url",
                &Value::String("http://user:secret@example.test".into())
            )
            .contains("secret")
        );
    }
}
