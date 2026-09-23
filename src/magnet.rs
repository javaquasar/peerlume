use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MagnetLink {
    pub exact_topics: Vec<String>,
    pub display_name: Option<String>,
    pub trackers: Vec<String>,
}

impl MagnetLink {
    pub fn parse(input: &str) -> Result<Self, String> {
        let url = Url::parse(input).map_err(|_| "invalid magnet URI".to_owned())?;
        if url.scheme() != "magnet" {
            return Err("URI does not use the magnet scheme".into());
        }
        let mut exact_topics = Vec::new();
        let mut display_name = None;
        let mut trackers = Vec::new();
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "xt" if value.starts_with("urn:btih:") || value.starts_with("urn:btmh:") => {
                    exact_topics.push(value.into_owned())
                }
                "dn" if display_name.is_none() => display_name = Some(value.into_owned()),
                "tr" if trackers.len() < 256 => trackers.push(value.into_owned()),
                _ => {}
            }
        }
        if exact_topics.is_empty() {
            return Err("magnet URI has no supported exact topic".into());
        }
        trackers.sort();
        trackers.dedup();
        Ok(Self {
            exact_topics,
            display_name,
            trackers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_magnet_and_deduplicates_trackers() {
        let link = MagnetLink::parse("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Example&tr=https%3A%2F%2Ftracker.test%2Fannounce&tr=https%3A%2F%2Ftracker.test%2Fannounce").unwrap();
        assert_eq!(link.display_name.as_deref(), Some("Example"));
        assert_eq!(link.exact_topics.len(), 1);
        assert_eq!(link.trackers.len(), 1);
    }
}
