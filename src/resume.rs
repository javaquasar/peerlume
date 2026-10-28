//! Durable *network* totals, kept distinct from bytes found on disk.
mod cache;
pub(crate) use cache::Cache;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResumeCounters {
    pub downloaded: u64,
    pub uploaded: u64,
    pub seeded_seconds: u64,
}
