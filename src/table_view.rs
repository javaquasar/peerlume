//! Persisted table presentation. This module has no GUI or transfer dependency.
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Name,
    Progress,
    Status,
    Download,
    Upload,
    Peers,
    Eta,
    Ratio,
}
impl Column {
    pub const ALL: [Self; 8] = [
        Self::Name,
        Self::Progress,
        Self::Status,
        Self::Download,
        Self::Upload,
        Self::Peers,
        Self::Eta,
        Self::Ratio,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Progress => "Progress",
            Self::Status => "Status",
            Self::Download => "Down",
            Self::Upload => "Up",
            Self::Peers => "Peers",
            Self::Eta => "ETA",
            Self::Ratio => "Ratio",
        }
    }
    pub const fn minimum(self) -> u32 {
        match self {
            Self::Name => 160,
            Self::Progress => 80,
            _ => 48,
        }
    }
    pub const fn numeric(self) -> bool {
        matches!(
            self,
            Self::Download | Self::Upload | Self::Peers | Self::Eta | Self::Ratio
        )
    }
    pub const fn sort(self) -> crate::torrent_view::Sort {
        use crate::torrent_view::Sort;
        match self {
            Self::Name => Sort::Name,
            Self::Progress => Sort::Progress,
            Self::Status => Sort::Status,
            Self::Download => Sort::Download,
            Self::Upload => Sort::Upload,
            Self::Peers => Sort::Peers,
            Self::Eta => Sort::Eta,
            Self::Ratio => Sort::Ratio,
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "name" => Self::Name,
            "progress" => Self::Progress,
            "status" => Self::Status,
            "download" | "down" => Self::Download,
            "upload" | "up" => Self::Upload,
            "peers" => Self::Peers,
            "eta" => Self::Eta,
            "ratio" => Self::Ratio,
            _ => return None,
        })
    }
}

pub const MAX_COLUMN_WIDTH: u32 = 4096;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TableColumns {
    pub order: Vec<Column>,
    pub hidden: BTreeSet<Column>,
    /// Name keeps using UiSettings::name_gap_pixels so it grows with its window.
    pub widths: BTreeMap<Column, u32>,
}
impl Default for TableColumns {
    fn default() -> Self {
        Self {
            order: Column::ALL.to_vec(),
            hidden: BTreeSet::new(),
            widths: BTreeMap::new(),
        }
    }
}
impl TableColumns {
    pub fn normalize(&mut self) {
        let mut seen = BTreeSet::new();
        self.order.retain(|column| seen.insert(*column));
        self.order.extend(
            Column::ALL
                .into_iter()
                .filter(|column| !seen.contains(column)),
        );
        self.hidden.remove(&Column::Name);
        self.widths.retain(|column, width| {
            if *column == Column::Name {
                return false;
            }
            *width = (*width).clamp(column.minimum(), MAX_COLUMN_WIDTH);
            true
        });
    }
    pub fn visible(&self) -> impl Iterator<Item = Column> + '_ {
        self.order
            .iter()
            .copied()
            .filter(|column| !self.hidden.contains(column) || *column == Column::Name)
    }
    pub fn is_visible(&self, column: Column) -> bool {
        column == Column::Name || !self.hidden.contains(&column)
    }
    pub fn set_visible(&mut self, column: Column, visible: bool) -> bool {
        if column == Column::Name {
            return false;
        }
        if visible {
            self.hidden.remove(&column)
        } else {
            self.hidden.insert(column)
        }
    }
    pub fn set_width(&mut self, column: Column, width: u32) -> bool {
        if column == Column::Name {
            return false;
        }
        let width = width.clamp(column.minimum(), MAX_COLUMN_WIDTH);
        self.widths.insert(column, width) != Some(width)
    }
    pub fn reset_width(&mut self, column: Column) -> bool {
        self.widths.remove(&column).is_some()
    }
    pub fn move_to(&mut self, column: Column, index: usize) -> bool {
        self.normalize();
        let previous = self
            .order
            .iter()
            .position(|current| *current == column)
            .expect("normalized columns");
        let index = index.min(self.order.len() - 1);
        if previous == index {
            return false;
        }
        self.order.remove(previous);
        self.order.insert(index, column);
        true
    }
    pub fn move_before(&mut self, column: Column, target: Column) -> bool {
        self.normalize();
        if column == target {
            return false;
        }
        let previous = self
            .order
            .iter()
            .position(|current| *current == column)
            .expect("normalized columns");
        let target = self
            .order
            .iter()
            .position(|current| *current == target)
            .expect("normalized columns");
        self.move_to(
            column,
            if previous < target {
                target - 1
            } else {
                target
            },
        )
    }
}
impl<'de> Deserialize<'de> for TableColumns {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Raw {
            order: Vec<String>,
            hidden: Vec<String>,
            widths: BTreeMap<String, serde_json::Value>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let mut columns = Self {
            order: raw
                .order
                .iter()
                .filter_map(|column| Column::parse(column))
                .collect(),
            hidden: raw
                .hidden
                .iter()
                .filter_map(|column| Column::parse(column))
                .collect(),
            widths: BTreeMap::new(),
        };
        for (name, value) in raw.widths {
            let Some(column) = Column::parse(&name) else {
                continue;
            };
            let width = value
                .as_u64()
                .map(|value| value.min(u64::from(MAX_COLUMN_WIDTH)) as u32)
                .or_else(|| {
                    value
                        .as_i64()
                        .map(|value| value.clamp(0, i64::from(MAX_COLUMN_WIDTH)) as u32)
                })
                .or_else(|| {
                    value
                        .as_f64()
                        .filter(|value| value.is_finite())
                        .map(|value| value.round().clamp(0.0, f64::from(MAX_COLUMN_WIDTH)) as u32)
                });
            if let Some(width) = width {
                columns.widths.insert(column, width);
            }
        }
        columns.normalize();
        Ok(columns)
    }
}

#[cfg(test)]
mod tests;
