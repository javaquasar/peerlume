use super::*;
use crate::{
    appearance::{AppearanceSettings, Density, ThemePreference},
    table_view::{Column, MAX_COLUMN_WIDTH, TableColumns},
    torrent_view::Sort,
};

#[test]
fn old_and_empty_profiles_get_additive_ui_defaults_without_losing_existing_fields() {
    for content in [
        "",
        "version=2\n[ui]\nsearch='retained'\nname_gap_pixels=80\nsort='name'\n",
    ] {
        let settings = ClientSettings::from_toml(content).unwrap();
        assert_eq!(settings.ui.table_columns, TableColumns::default());
        assert_eq!(settings.ui.appearance, AppearanceSettings::default());
        if !content.is_empty() {
            assert_eq!(settings.ui.search, "retained");
            assert_eq!(settings.ui.name_gap_pixels, 80);
            assert_eq!(settings.ui.sort, Sort::Name);
        }
    }
}

#[test]
fn malformed_layout_normalizes_unknown_duplicate_columns_and_defensive_width_zoom_bounds() {
    let source = "version=2\n[ui]\nsearch='kept'\nname_gap_pixels=99999\ntorrent_pane_percent=999\n[ui.table_columns]\norder=['ratio','unknown','name','ratio']\nhidden=['name','peers']\n[ui.table_columns.widths]\ndownload=-20\nprogress=999999\neta='invalid'\n[ui.appearance]\nzoom_percent=65000\n";
    let settings = ClientSettings::from_toml(source).unwrap();
    let ui = settings.ui;
    assert_eq!(ui.search, "kept");
    assert_eq!(ui.name_gap_pixels, 10000);
    assert_eq!(ui.torrent_pane_percent, 80);
    assert_eq!(ui.table_columns.order[..2], [Column::Ratio, Column::Name]);
    assert_eq!(ui.table_columns.order.len(), 8);
    assert!(ui.table_columns.is_visible(Column::Name));
    assert!(!ui.table_columns.is_visible(Column::Peers));
    assert_eq!(ui.table_columns.widths[&Column::Download], 48);
    assert_eq!(ui.table_columns.widths[&Column::Progress], MAX_COLUMN_WIDTH);
    assert!(!ui.table_columns.widths.contains_key(&Column::Eta));
    assert_eq!(ui.appearance.zoom_percent, 200);
    assert_eq!(
        ClientSettings::from_toml("version=2\n[ui.appearance]\nzoom_percent=0\n")
            .unwrap()
            .ui
            .appearance
            .zoom_percent,
        75
    );
}

#[test]
fn edited_layout_appearance_and_new_sorts_persist_through_real_temp_file_round_trips() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    let mut settings = ClientSettings::default();
    settings.download.download_directory = "synthetic-root".into();
    settings.queue.max_active_downloads = 7;
    settings.ui.search = "retain query".into();
    settings.ui.name_gap_pixels = 40;
    settings.ui.table_columns.set_visible(Column::Upload, false);
    settings.ui.table_columns.set_width(Column::Progress, 240);
    settings.ui.table_columns.move_to(Column::Ratio, 0);
    settings.ui.appearance = AppearanceSettings {
        theme: ThemePreference::Dark,
        density: Density::Compact,
        zoom_percent: 150,
    };
    for sort in [Sort::Status, Sort::Peers, Sort::Eta, Sort::Ratio] {
        settings.ui.sort = sort;
        settings.ui.descending = true;
        settings.save_to(&path).unwrap();
        let restored = ClientSettings::load_from(&path).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(restored.download.download_directory, "synthetic-root");
        assert_eq!(restored.queue.max_active_downloads, 7);
    }
}
