use super::*;
use crate::{file_selection::FileSelectionModel, torrent::TorrentState};

fn item(id: &str) -> TorrentItem {
    let mut item = crate::client::tests::torrent();
    item.info_hashes = vec![id.into()];
    item.state = TorrentState::Downloading;
    item.files = FileSelectionModel::from_files([("synthetic".into(), 1000)]);
    item
}
fn sorted_ids(torrents: &[TorrentItem], sort: Sort, descending: bool) -> Vec<&str> {
    let view = UiSettings {
        sort,
        descending,
        ..Default::default()
    };
    indices(torrents, StatusFilter::All, &view)
        .into_iter()
        .map(|i| torrents[i].info_hashes[0].as_str())
        .collect()
}

#[test]
fn all_new_metrics_sort_in_both_directions_and_ties_use_identity() {
    let mut torrents = vec![item("z"), item("a"), item("m")];
    torrents[0].state = TorrentState::Paused;
    torrents[1].state = TorrentState::Error;
    torrents[2].state = TorrentState::Downloading;
    assert_eq!(sorted_ids(&torrents, Sort::Status, false), ["a", "m", "z"]);
    assert_eq!(sorted_ids(&torrents, Sort::Status, true), ["z", "m", "a"]);
    torrents[0].peers = 2;
    torrents[0].seeds = 1;
    torrents[1].peers = 2;
    torrents[1].seeds = 1;
    torrents[2].peers = 2;
    torrents[2].seeds = 2;
    assert_eq!(sorted_ids(&torrents, Sort::Peers, false), ["a", "z", "m"]);
    assert_eq!(sorted_ids(&torrents, Sort::Peers, true), ["m", "a", "z"]);
    torrents.swap(0, 2);
    assert_eq!(sorted_ids(&torrents, Sort::Peers, false), ["a", "z", "m"]);
    assert_eq!(
        torrents[0].info_hashes,
        ["m"],
        "presentation cannot reorder canonical records"
    );
}

#[test]
fn ratio_order_uses_exact_cross_products_at_u64_limits_and_defined_zero_denominator() {
    let max = u64::MAX;
    let mut a = item("near-a");
    a.uploaded_bytes = max;
    a.downloaded_bytes = max - 1;
    let mut b = item("near-b");
    b.uploaded_bytes = max - 1;
    b.downloaded_bytes = max - 2;
    assert_eq!(
        a.ratio(),
        b.ratio(),
        "fixture must expose a float precision tie"
    );
    let mut zero = item("zero");
    zero.uploaded_bytes = max;
    zero.downloaded_bytes = 0;
    let mut tie_z = item("tie-z");
    tie_z.uploaded_bytes = 3;
    tie_z.downloaded_bytes = 3;
    let mut tie_a = item("tie-a");
    tie_a.uploaded_bytes = 2;
    tie_a.downloaded_bytes = 2;
    let mut two = item("two");
    two.uploaded_bytes = 2;
    two.downloaded_bytes = 1;
    let torrents = [b, tie_z, zero, two, a, tie_a];
    assert_eq!(
        sorted_ids(&torrents, Sort::Ratio, false),
        ["zero", "tie-a", "tie-z", "near-a", "near-b", "two"]
    );
    assert_eq!(
        sorted_ids(&torrents, Sort::Ratio, true),
        ["two", "near-b", "near-a", "tie-a", "tie-z", "zero"]
    );
}

#[test]
fn eta_unknowns_remain_last_in_both_directions_including_zero_rate_and_paused() {
    let mut slow = item("slow");
    slow.downloaded_bytes = 0;
    slow.download_rate = 10;
    let mut fast = item("fast");
    fast.downloaded_bytes = 0;
    fast.download_rate = 100;
    let mut stopped = item("unknown-a");
    stopped.download_rate = 0;
    let mut paused = item("unknown-z");
    paused.state = TorrentState::Paused;
    paused.download_rate = 100;
    let torrents = [paused, slow, stopped, fast];
    assert_eq!(
        sorted_ids(&torrents, Sort::Eta, false),
        ["fast", "slow", "unknown-a", "unknown-z"]
    );
    assert_eq!(
        sorted_ids(&torrents, Sort::Eta, true),
        ["slow", "fast", "unknown-a", "unknown-z"]
    );
}

#[test]
fn live_new_sort_keys_invalidate_cache_but_unrelated_data_reuses_it() {
    for sort in [Sort::Status, Sort::Peers, Sort::Eta, Sort::Ratio] {
        let mut torrents = vec![item("a"), item("b")];
        for t in &mut torrents {
            t.download_rate = 10;
            t.downloaded_bytes = 1;
            t.uploaded_bytes = 1;
        }
        let view = UiSettings {
            sort,
            ..Default::default()
        };
        let mut cache = ViewCache::default();
        let first = cache.indices(&torrents, StatusFilter::All, &view);
        torrents[0].upload_rate = 99;
        assert!(Arc::ptr_eq(
            &first,
            &cache.indices(&torrents, StatusFilter::All, &view)
        ));
        match sort {
            Sort::Status => torrents[0].state = TorrentState::Paused,
            Sort::Peers => torrents[0].peers += 1,
            Sort::Eta => torrents[0].download_rate = 1,
            Sort::Ratio => torrents[0].uploaded_bytes = 2,
            _ => unreachable!(),
        }
        let second = cache.indices(&torrents, StatusFilter::All, &view);
        assert!(!Arc::ptr_eq(&first, &second), "{sort:?}");
        assert_eq!(second.as_slice(), [1, 0], "{sort:?}");
    }
}
