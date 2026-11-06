//! Identity selection is presentation state; it never dispatches transfer actions.
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionCounts {
    pub visible: usize,
    pub hidden: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorrentSelection {
    selected: BTreeSet<String>,
    focus: Option<String>,
    anchor: Option<String>,
    initialized: bool,
}
impl TorrentSelection {
    pub fn selected_ids(&self) -> impl Iterator<Item = &str> {
        self.selected.iter().map(String::as_str)
    }
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }
    pub fn focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }
    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }
    pub fn len(&self) -> usize {
        self.selected.len()
    }
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
    pub fn initialize(&mut self, id: &str) {
        if !self.initialized {
            self.replace(id);
        }
    }
    pub fn replace(&mut self, id: &str) {
        self.selected.clear();
        self.selected.insert(id.into());
        self.focus = Some(id.into());
        self.anchor = Some(id.into());
        self.initialized = true;
    }
    pub fn click(&mut self, id: &str, visible: &[String], modifiers: Modifiers) {
        self.initialized = true;
        if modifiers.shift {
            let start = self
                .anchor
                .as_ref()
                .and_then(|anchor| visible.iter().position(|current| current == anchor));
            let end = visible.iter().position(|current| current == id);
            if let (Some(start), Some(end)) = (start, end) {
                if !modifiers.ctrl {
                    self.selected.clear();
                }
                self.selected
                    .extend(visible[start.min(end)..=start.max(end)].iter().cloned());
                self.focus = Some(id.into());
                return;
            }
            self.replace(id);
            return;
        }
        if modifiers.ctrl {
            if !self.selected.remove(id) {
                self.selected.insert(id.into());
            }
            self.focus = Some(id.into());
            self.anchor = Some(id.into());
        } else {
            self.replace(id);
        }
    }
    pub fn context(&mut self, id: &str) {
        if self.is_selected(id) {
            self.focus = Some(id.into());
        } else {
            self.replace(id);
        }
    }
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.initialized = true;
    }
    pub fn select_visible(&mut self, visible: &[String]) {
        self.selected = visible.iter().cloned().collect();
        self.focus = visible.first().cloned();
        self.anchor = self.focus.clone();
        self.initialized = true;
    }
    pub fn visible_targets(&self, visible: impl IntoIterator<Item = String>) -> Vec<String> {
        visible
            .into_iter()
            .filter(|id| self.is_selected(id))
            .collect()
    }
    pub fn counts(&self, visible: impl IntoIterator<Item = impl AsRef<str>>) -> SelectionCounts {
        let visible: HashSet<_> = visible
            .into_iter()
            .filter_map(|id| self.selected.get(id.as_ref()).map(String::as_str))
            .collect();
        SelectionCounts {
            visible: visible.len(),
            hidden: self.selected.len().saturating_sub(visible.len()),
        }
    }
    pub fn clear_hidden(&mut self, visible: impl IntoIterator<Item = impl AsRef<str>>) {
        let visible: HashSet<String> = visible
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
        self.selected.retain(|id| visible.contains(id));
        if self.anchor.as_ref().is_some_and(|id| !visible.contains(id)) {
            self.anchor = None;
        }
    }
    pub fn remove(&mut self, id: &str) {
        self.selected.remove(id);
        if self.focus.as_deref() == Some(id) {
            self.focus = None;
        }
        if self.anchor.as_deref() == Some(id) {
            self.anchor = None;
        }
    }
    pub fn retain_live(&mut self, live: impl IntoIterator<Item = impl AsRef<str>>) {
        let live: HashSet<String> = live.into_iter().map(|id| id.as_ref().to_owned()).collect();
        self.selected.retain(|id| live.contains(id));
        if self.focus.as_ref().is_some_and(|id| !live.contains(id)) {
            self.focus = None;
        }
        if self.anchor.as_ref().is_some_and(|id| !live.contains(id)) {
            self.anchor = None;
        }
    }
}

#[cfg(test)]
mod tests;
