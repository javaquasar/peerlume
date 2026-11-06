//! Presentation indices never reorder authoritative torrent identities.
use crate::{settings::UiSettings, torrent::TorrentItem, torrent_filter::StatusFilter};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Added,
    Name,
    Progress,
    Download,
    Upload,
    Size,
    Status,
    Peers,
    Eta,
    Ratio,
}
impl Sort {
    pub const ALL: [Self; 10] = [
        Self::Added,
        Self::Name,
        Self::Progress,
        Self::Download,
        Self::Upload,
        Self::Size,
        Self::Status,
        Self::Peers,
        Self::Eta,
        Self::Ratio,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Name => "Name",
            Self::Progress => "Progress",
            Self::Download => "Download",
            Self::Upload => "Upload",
            Self::Size => "Size",
            Self::Status => "Status",
            Self::Peers => "Peers",
            Self::Eta => "ETA",
            Self::Ratio => "Ratio",
        }
    }
}
pub fn indices(torrents: &[TorrentItem], filter: StatusFilter, view: &UiSettings) -> Vec<usize> {
    indices_with_query(torrents, filter, view, &view.search)
}
pub fn indices_with_query(
    torrents: &[TorrentItem],
    filter: StatusFilter,
    view: &UiSettings,
    query: &str,
) -> Vec<usize> {
    ViewCache::default()
        .indices_with_query(torrents, filter, view, query)
        .as_ref()
        .clone()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SortMetric {
    #[default]
    None,
    Number(u64),
    Fraction(u64, u64),
    OptionalNumber(Option<u64>),
    Pair(u64, u64),
}

#[derive(Default)]
struct CachedRow {
    hashes: Vec<String>,
    id: String,
    name: String,
    normalized_name: String,
    labels: String,
    normalized_labels: String,
    category: String,
    normalized_category: String,
    visible: bool,
    metric: SortMetric,
}

/// A frontend-owned presentation cache. Canonical rows are compared without
/// cloning their file trees; unchanged views retain the same shared index vector.
/// Only inputs used by the current filter/sort invalidate that vector.
#[derive(Default)]
pub struct ViewCache {
    rows: Vec<CachedRow>,
    filter: Option<StatusFilter>,
    raw_query: String,
    query: String,
    category_filter: String,
    sort: Sort,
    descending: bool,
    result: Arc<Vec<usize>>,
    rebuilds: u64,
}

impl ViewCache {
    pub fn indices(
        &mut self,
        torrents: &[TorrentItem],
        filter: StatusFilter,
        view: &UiSettings,
    ) -> Arc<Vec<usize>> {
        self.indices_with_query(torrents, filter, view, &view.search)
    }

    pub fn rebuild_count(&self) -> u64 {
        self.rebuilds
    }

    pub fn indices_with_query(
        &mut self,
        torrents: &[TorrentItem],
        filter: StatusFilter,
        view: &UiSettings,
        query: &str,
    ) -> Arc<Vec<usize>> {
        let mut dirty = self.filter != Some(filter)
            || self.category_filter != view.category_filter
            || self.sort != view.sort
            || self.descending != view.descending
            || self.rows.len() != torrents.len();
        if self.raw_query != query {
            self.raw_query = query.into();
            self.query = query.to_lowercase();
            dirty = true;
        }
        self.filter = Some(filter);
        self.sort = view.sort;
        self.descending = view.descending;
        if self.category_filter != view.category_filter {
            self.category_filter.clone_from(&view.category_filter);
        }
        self.rows.truncate(torrents.len());
        for (index, torrent) in torrents.iter().enumerate() {
            if index == self.rows.len() {
                self.rows.push(CachedRow::default());
            }
            let cached = &mut self.rows[index];
            if cached.hashes != torrent.info_hashes {
                *cached = CachedRow {
                    hashes: torrent.info_hashes.clone(),
                    id: torrent.info_hashes.join(";"),
                    ..Default::default()
                };
                dirty = true;
            }
            if cached.name != torrent.name {
                cached.name.clone_from(&torrent.name);
                cached.normalized_name = torrent.name.to_lowercase();
                dirty |= view.sort == Sort::Name;
            }
            let labels = view
                .labels
                .get(&cached.id)
                .map(String::as_str)
                .unwrap_or_default();
            if cached.labels != labels {
                cached.labels = labels.into();
                cached.normalized_labels = labels.to_lowercase();
            }
            let category = view
                .categories
                .get(&cached.id)
                .map(String::as_str)
                .unwrap_or_default();
            if cached.category != category {
                cached.category = category.into();
                cached.normalized_category = category.to_lowercase();
            }
            let visible = filter.matches(torrent)
                && (self.category_filter.is_empty() || cached.category == self.category_filter)
                && (self.query.is_empty()
                    || cached.normalized_name.contains(&self.query)
                    || cached.normalized_labels.contains(&self.query)
                    || cached.normalized_category.contains(&self.query));
            let metric = match view.sort {
                Sort::Added | Sort::Name => SortMetric::None,
                Sort::Progress => SortMetric::Fraction(
                    torrent.files.selected_downloaded(),
                    torrent.selected_size().max(1),
                ),
                Sort::Download => SortMetric::Number(torrent.download_rate),
                Sort::Upload => SortMetric::Number(torrent.upload_rate),
                Sort::Size => SortMetric::Number(torrent.selected_size()),
                Sort::Status => SortMetric::Number(status_rank(torrent.state)),
                Sort::Peers => SortMetric::Pair(torrent.peers as u64, torrent.seeds as u64),
                Sort::Eta => SortMetric::OptionalNumber(torrent.eta_seconds()),
                Sort::Ratio => SortMetric::Fraction(
                    if torrent.downloaded_bytes == 0 {
                        0
                    } else {
                        torrent.uploaded_bytes
                    },
                    torrent.downloaded_bytes.max(1),
                ),
            };
            dirty |= cached.visible != visible || (visible && cached.metric != metric);
            cached.visible = visible;
            cached.metric = metric;
        }
        if dirty {
            let mut result: Vec<_> = self
                .rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| row.visible.then_some(index))
                .collect();
            result.sort_by(|&a, &b| {
                let (left, right) = (&self.rows[a], &self.rows[b]);
                let order = match view.sort {
                    Sort::Added => a.cmp(&b),
                    Sort::Name => left.normalized_name.cmp(&right.normalized_name),
                    _ => match (left.metric, right.metric) {
                        (SortMetric::Number(a), SortMetric::Number(b)) => a.cmp(&b),
                        (SortMetric::Fraction(ad, az), SortMetric::Fraction(bd, bz)) => {
                            (u128::from(ad) * u128::from(bz))
                                .cmp(&(u128::from(bd) * u128::from(az)))
                        }
                        (SortMetric::OptionalNumber(a), SortMetric::OptionalNumber(b)) => {
                            match (a, b) {
                                (Some(a), Some(b)) => a.cmp(&b),
                                (Some(_), None) => std::cmp::Ordering::Less,
                                (None, Some(_)) => std::cmp::Ordering::Greater,
                                (None, None) => std::cmp::Ordering::Equal,
                            }
                        }
                        (SortMetric::Pair(a, az), SortMetric::Pair(b, bz)) => (a, az).cmp(&(b, bz)),
                        _ => std::cmp::Ordering::Equal,
                    },
                };
                // Unknown ETA stays at the end in both directions. Equal values
                // always use canonical identity, not a transient row position.
                let unknown_eta = matches!(
                    (left.metric, right.metric),
                    (
                        SortMetric::OptionalNumber(None),
                        SortMetric::OptionalNumber(Some(_))
                    ) | (
                        SortMetric::OptionalNumber(Some(_)),
                        SortMetric::OptionalNumber(None)
                    )
                );
                let order = if view.descending && !unknown_eta {
                    order.reverse()
                } else {
                    order
                };
                order.then_with(|| left.hashes.cmp(&right.hashes))
            });
            self.result = Arc::new(result);
            self.rebuilds = self.rebuilds.saturating_add(1);
        }
        self.result.clone()
    }
}

fn status_rank(state: crate::torrent::TorrentState) -> u64 {
    use crate::torrent::TorrentState;
    match state {
        TorrentState::Error => 0,
        TorrentState::VerificationFailed => 1,
        TorrentState::WaitingForEngine => 2,
        TorrentState::Checking => 3,
        TorrentState::Downloading => 4,
        TorrentState::Seeding => 5,
        TorrentState::Paused => 6,
    }
}

/// Active progress and command feedback need a live clock. A quiet, unchanged
/// view can sleep longer; native input still wakes the frontend immediately.
pub fn needs_live_refresh(torrents: &[TorrentItem]) -> bool {
    torrents.iter().any(|torrent| {
        torrent.download_rate > 0
            || torrent.upload_rate > 0
            || torrent.runtime.command.pending_action().is_some()
            || matches!(
                torrent.state,
                crate::torrent::TorrentState::Downloading
                    | crate::torrent::TorrentState::Checking
                    | crate::torrent::TorrentState::WaitingForEngine
            )
            || matches!(
                torrent.runtime.storage,
                crate::storage::relocation::StorageStatus::Copying { .. }
            )
    })
}

pub fn refresh_interval(
    torrents: &[TorrentItem],
    pending: bool,
    minimized: bool,
) -> std::time::Duration {
    std::time::Duration::from_millis(if minimized {
        2000
    } else if pending
        || torrents
            .iter()
            .any(|torrent| torrent.runtime.command.pending_action().is_some())
    {
        50
    } else if needs_live_refresh(torrents) {
        250
    } else {
        2000
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sorting_filtering_keep_canonical_indices_and_do_not_mutate_torrents() {
        let mut a = crate::client::tests::torrent();
        a.name = "Zulu".into();
        let mut b = a.clone();
        b.name = "Alpha".into();
        b.info_hashes = vec!["v1: other".into()];
        let torrents = vec![a, b];
        let mut view = UiSettings {
            sort: Sort::Name,
            ..Default::default()
        };
        assert_eq!(
            indices(&torrents, StatusFilter::default(), &view),
            vec![1, 0]
        );
        view.labels
            .insert(torrents[0].info_hashes.join(";"), "Archive".into());
        view.search = "archive".into();
        assert_eq!(indices(&torrents, StatusFilter::default(), &view), vec![0]);
        assert_eq!(torrents[0].name, "Zulu");
    }

    #[test]
    fn retained_views_reuse_indices_until_an_input_used_by_the_view_changes() {
        let mut a = crate::client::tests::torrent();
        a.name = "Zulu".into();
        let mut b = a.clone();
        b.name = "Alpha".into();
        b.info_hashes = vec!["v1: other".into()];
        let mut torrents = vec![a, b];
        let mut view = UiSettings {
            sort: Sort::Name,
            ..Default::default()
        };
        let mut cache = ViewCache::default();
        let first = cache.indices(&torrents, StatusFilter::All, &view);
        assert_eq!(first.as_slice(), [1, 0]);
        torrents[0].uploaded_bytes += 1;
        torrents[0].download_rate += 10;
        assert!(
            Arc::ptr_eq(&first, &cache.indices(&torrents, StatusFilter::All, &view)),
            "Unrelated telemetry must not allocate or sort a new name view"
        );
        assert_eq!(cache.rebuild_count(), 1);
        torrents[0].name = "Aardvark".into();
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .as_slice(),
            [0, 1]
        );
        view.search = "archive".into();
        assert!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .is_empty()
        );
        view.labels
            .insert(torrents[1].info_hashes.join(";"), "ARCHIVE".into());
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .as_slice(),
            [1]
        );
        view.search.clear();
        view.category_filter = "Movies".into();
        view.categories
            .insert(torrents[0].info_hashes.join(";"), "Movies".into());
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .as_slice(),
            [0]
        );
        torrents.remove(0);
        assert!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .is_empty()
        );
        view.category_filter.clear();
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .as_slice(),
            [0]
        );
        torrents.clear();
        assert!(
            cache
                .indices(&torrents, StatusFilter::All, &view)
                .is_empty()
        );
    }

    #[test]
    fn live_status_rate_and_fractional_progress_sorting_invalidate_without_retargeting_ids() {
        let mut a = crate::client::tests::torrent();
        a.info_hashes = vec!["v1: first".into()];
        a.state = crate::torrent::TorrentState::Downloading;
        a.files = crate::file_selection::FileSelectionModel::from_files([("a".into(), 3)]);
        a.files.set_total_downloaded(1);
        a.download_rate = 10;
        let mut b = a.clone();
        b.info_hashes = vec!["v1: second".into()];
        b.files = crate::file_selection::FileSelectionModel::from_files([("b".into(), 7)]);
        b.files.set_total_downloaded(2);
        b.download_rate = 20;
        let mut torrents = vec![a, b];
        let mut view = UiSettings {
            sort: Sort::Progress,
            ..Default::default()
        };
        let mut cache = ViewCache::default();
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::Downloading, &view)
                .as_slice(),
            [1, 0]
        );
        torrents[1].files.set_total_downloaded(3);
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::Downloading, &view)
                .as_slice(),
            [0, 1]
        );
        view.sort = Sort::Download;
        view.descending = true;
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::Downloading, &view)
                .as_slice(),
            [1, 0]
        );
        torrents[0].download_rate = 30;
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::Downloading, &view)
                .as_slice(),
            [0, 1]
        );
        torrents[0].state = crate::torrent::TorrentState::Paused;
        assert_eq!(
            cache
                .indices(&torrents, StatusFilter::Downloading, &view)
                .as_slice(),
            [1]
        );
        torrents.swap(0, 1);
        let result = cache.indices(&torrents, StatusFilter::Downloading, &view);
        assert_eq!(result.as_slice(), [0]);
        assert_eq!(torrents[result[0]].info_hashes, ["v1: second"]);
    }

    #[test]
    fn idle_refresh_slows_down_but_pending_commands_and_minimized_views_have_bounded_delays() {
        let mut torrent = crate::client::tests::torrent();
        torrent.state = crate::torrent::TorrentState::Paused;
        assert_eq!(
            refresh_interval(&[torrent.clone()], false, false),
            std::time::Duration::from_secs(2)
        );
        torrent.state = crate::torrent::TorrentState::Seeding;
        torrent.upload_rate = 1;
        assert_eq!(
            refresh_interval(&[torrent.clone()], false, false),
            std::time::Duration::from_millis(250)
        );
        torrent.upload_rate = 0;
        assert_eq!(
            refresh_interval(&[torrent.clone()], false, false),
            std::time::Duration::from_secs(2)
        );
        torrent.state = crate::torrent::TorrentState::Checking;
        assert_eq!(
            refresh_interval(&[torrent.clone()], false, false),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            refresh_interval(&[torrent.clone()], true, false),
            std::time::Duration::from_millis(50)
        );
        assert_eq!(
            refresh_interval(&[torrent], true, true),
            std::time::Duration::from_secs(2)
        );
    }
}

#[cfg(test)]
#[path = "torrent_view/ui04_tests.rs"]
mod ui04_tests;
