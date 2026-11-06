use super::*;
use crate::{client::tests::torrent, file_selection::FileSelectionModel};

fn item() -> TorrentItem {
    let mut item = torrent();
    item.files = FileSelectionModel::from_files([("first".into(), 100), ("second".into(), 100)]);
    item.files.set_selected(1, false);
    item
}
fn summary(item: &TorrentItem) -> frontend::NativeTorrent {
    frontend::NativeTorrent {
        item: TorrentItem {
            files: FileSelectionModel::from_summary(item.files.summary()),
            ..item.clone()
        },
        command_revision: 1,
        file_revision: item.files.metadata_revision(),
        durable: true,
        file_updates: item.files.persistence_snapshot(true),
    }
}

#[test]
fn matching_file_generation_preserves_loaded_paths_and_applies_progress() {
    let mut previous = item();
    let mut latest = previous.clone();
    latest.files.set_file_downloaded(0, 75);
    let mut row = summary(&latest);
    Controller::preserve_native_file_details(&mut row, &mut previous);
    assert!(row.item.files.details_loaded());
    assert_eq!(row.item.files.files()[0].path, "first");
    assert_eq!(row.item.files.files()[0].downloaded_bytes, 75);
    assert_eq!(
        row.item.files.metadata_revision(),
        latest.files.metadata_revision()
    );
}

#[test]
fn same_count_selection_and_priority_changes_never_retain_stale_loaded_details() {
    let previous = item();
    let mut latest = previous.clone();
    latest.files.set_selected(0, false);
    latest.files.set_selected(1, true);
    assert_eq!(
        previous.files.selected_count(),
        latest.files.selected_count()
    );
    let mut old = previous.clone();
    let mut row = summary(&latest);
    Controller::preserve_native_file_details(&mut row, &mut old);
    assert!(!row.item.files.details_loaded());
    let mut latest = previous.clone();
    latest.files.set_high_priority(&[1]);
    let mut old = previous;
    let mut row = summary(&latest);
    Controller::preserve_native_file_details(&mut row, &mut old);
    assert!(!row.item.files.details_loaded());
}

#[test]
fn mismatched_bitmap_cannot_be_blessed_by_a_matching_header_generation() {
    let mut previous = item();
    let mut row = summary(&previous);
    row.file_updates.as_mut().unwrap().selected_bits[0] = 2;
    Controller::preserve_native_file_details(&mut row, &mut previous);
    assert!(
        !row.item.files.details_loaded(),
        "re-fetch authoritative complete pages instead of adopting changed bits"
    );
}

fn native_snapshot(owner: &str, revision: u64, durable: bool) -> frontend::Snapshot {
    let item = item();
    let mut observed = TorrentObservation::pending(&item);
    observed.command_revision = revision;
    let mut row = summary(&item);
    row.command_revision = revision;
    row.durable = durable;
    frontend::Snapshot {
        instance_id: owner.into(),
        revision: 1,
        torrents: vec![row],
        observations: vec![observed],
        notices: vec![],
        detail_torrent: None,
    }
}

#[test]
fn imported_owner_durability_stays_false_until_a_matching_saved_snapshot_arrives() {
    let mut mirror = Controller::new(ClientBackend::offline(), vec![]);
    assert_eq!(mirror.native_command_evidence("v1: abc"), None);
    mirror.apply_native_snapshot(native_snapshot("owner-a", 7, false));
    assert!(
        !mirror.has_confirmed_update("v1: abc"),
        "a frontend has no local SQLite writes"
    );
    assert_eq!(
        mirror.native_command_evidence("v1: abc"),
        Some((7, false)),
        "empty local work must not fabricate saved-state proof"
    );
    mirror.apply_native_snapshot(native_snapshot("owner-a", 7, true));
    assert_eq!(mirror.native_command_evidence("v1: abc"), Some((7, true)));
    mirror.apply_native_snapshot(native_snapshot("owner-b", 7, false));
    assert_eq!(
        mirror.native_command_evidence("v1: abc"),
        Some((7, false)),
        "replacement owner never inherits the previous owner's durable bit"
    );
    let mut inconsistent = native_snapshot("owner-b", 8, true);
    inconsistent.observations[0].command_revision = 9;
    mirror.apply_native_snapshot(inconsistent);
    assert_eq!(
        mirror.native_command_evidence("v1: abc"),
        None,
        "durability cannot cross command revisions"
    );
    mirror.apply_native_snapshot(frontend::Snapshot {
        instance_id: "owner-b".into(),
        revision: 2,
        torrents: vec![],
        observations: vec![],
        notices: vec![],
        detail_torrent: None,
    });
    assert!(mirror.native_durability.is_empty());
    assert_eq!(mirror.native_command_evidence("v1: abc"), None);
}
