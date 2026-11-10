use super::*;
fn contrast(a: Rgb8, b: Rgb8) -> f64 {
    let luminance = |value: Rgb8| {
        [value.0, value.1, value.2]
            .into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(value, weight)| {
                let value = f64::from(value) / 255.0;
                weight
                    * if value <= 0.04045 {
                        value / 12.92
                    } else {
                        ((value + 0.055) / 1.055).powf(2.4)
                    }
            })
            .sum::<f64>()
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
#[test]
fn legacy_defaults_roundtrip_and_clamps_are_additive() {
    assert_eq!(
        toml::from_str::<AppearanceSettings>("").unwrap(),
        AppearanceSettings::default()
    );
    for (value, expected) in [
        (0, 75),
        (74, 75),
        (75, 75),
        (150, 150),
        (200, 200),
        (65535, 200),
    ] {
        let parsed: AppearanceSettings = toml::from_str(&format!("zoom_percent={value}")).unwrap();
        assert_eq!(parsed.zoom_percent, expected);
        assert_eq!(
            toml::from_str::<AppearanceSettings>(&toml::to_string(&parsed).unwrap()).unwrap(),
            parsed
        );
    }
    for invalid in [
        "zoom_percent=-1",
        "zoom_percent=\"large\"",
        "theme=\"unknown\"",
    ] {
        assert!(toml::from_str::<AppearanceSettings>(invalid).is_err());
    }
    let mut settings = AppearanceSettings {
        zoom_percent: 0,
        ..Default::default()
    };
    assert!(settings.normalize());
    assert!(!settings.normalize());
    settings.set_zoom_percent(u16::MAX);
    assert_eq!(settings.zoom_factor(), 2.0);
}
#[test]
fn every_status_selection_and_hover_pair_has_readable_text_and_noncolor_meaning() {
    use crate::torrent::TorrentState as S;
    for state in [
        S::Downloading,
        S::WaitingForEngine,
        S::Paused,
        S::Seeding,
        S::Checking,
        S::VerificationFailed,
        S::Error,
    ] {
        let badge = StatusPresentation::for_state(state);
        assert!(!badge.label.is_empty());
        for dark in [false, true] {
            for selected in [false, true] {
                for hovered in [false, true] {
                    let colors = row_colors(state, dark, selected, hovered);
                    assert!(
                        contrast(colors.background, colors.text) >= 4.5,
                        "{state:?} dark={dark} selected={selected} hovered={hovered}"
                    );
                }
            }
        }
    }
    assert!((contrast(Rgb8(0, 0, 0), Rgb8(255, 255, 255)) - 21.0).abs() < 0.0001);
}
