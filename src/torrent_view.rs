//! Presentation indices never reorder authoritative torrent identities.
use crate::{settings::UiSettings, torrent::TorrentItem, torrent_filter::StatusFilter};
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
}
impl Sort {
    pub const ALL: [Self; 6] = [
        Self::Added,
        Self::Name,
        Self::Progress,
        Self::Download,
        Self::Upload,
        Self::Size,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Name => "Name",
            Self::Progress => "Progress",
            Self::Download => "Download",
            Self::Upload => "Upload",
            Self::Size => "Size",
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
    let query = query.to_lowercase();
    let mut result: Vec<_> = torrents
        .iter()
        .enumerate()
        .filter_map(|(index, torrent)| {
            let id = torrent.info_hashes.join(";");
            let labels = view.labels.get(&id).map(String::as_str).unwrap_or_default();
            let category = view
                .categories
                .get(&id)
                .map(String::as_str)
                .unwrap_or_default();
            (filter.matches(torrent)
                && (view.category_filter.is_empty() || category == view.category_filter)
                && (query.is_empty()
                    || torrent.name.to_lowercase().contains(&query)
                    || labels.to_lowercase().contains(&query)
                    || category.to_lowercase().contains(&query)))
            .then_some(index)
        })
        .collect();
    result.sort_by(|&a, &b| {
        let (left, right) = (&torrents[a], &torrents[b]);
        let order = match view.sort {
            Sort::Added => a.cmp(&b),
            Sort::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            Sort::Progress => (u128::from(left.files.selected_downloaded())
                * u128::from(right.selected_size().max(1)))
            .cmp(
                &(u128::from(right.files.selected_downloaded())
                    * u128::from(left.selected_size().max(1))),
            ),
            Sort::Download => left.download_rate.cmp(&right.download_rate),
            Sort::Upload => left.upload_rate.cmp(&right.upload_rate),
            Sort::Size => left.selected_size().cmp(&right.selected_size()),
        }
        .then_with(|| left.info_hashes.cmp(&right.info_hashes));
        if view.descending {
            order.reverse()
        } else {
            order
        }
    });
    result
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
}
