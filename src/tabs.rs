//! Tab domain: identity, lifecycle state, and the ordered registry.
//!
//! Owns no engine types — a `Tab` is pure state. The engine's webviews are
//! keyed by `TabId` in the app layer.

use std::fmt;

/// Identity of a tab. Opaque outside this module.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TabId(u64);

impl TabId {
    /// Rebuild an id from the wire (chrome IPC). Ids originate here — wire
    /// values are just round-tripped back.
    pub(crate) fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Lifecycle states per the project vocabulary: a tab is active, frozen
/// (snapshot kept, engine surface dropped), or discarded (gone except for
/// the restore record). Phase 0 creates only `Active` tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabState {
    Active,
    Frozen,
    Discarded,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TabError {
    #[error("unknown tab {0}")]
    UnknownTab(TabId),
    #[error("no active tab")]
    NoActiveTab,
}

/// A browser tab. Mutated only through its methods — fields are private so
/// outside code can't create an invalid combination.
pub struct Tab {
    id: TabId,
    url: String,
    title: String,
    state: TabState,
}

impl Tab {
    fn new(id: TabId, url: String) -> Self {
        Self {
            id,
            title: url.clone(),
            url,
            state: TabState::Active,
        }
    }

    pub fn id(&self) -> TabId {
        self.id
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn state(&self) -> TabState {
        self.state
    }

    pub(crate) fn navigate(&mut self, url: String) {
        self.url = url;
    }

    pub(crate) fn set_title(&mut self, title: String) {
        if !title.is_empty() {
            self.title = title;
        }
    }
}

/// Ordered collection of tabs plus the active selection. The one place
/// tab state lives — no parallel maps keyed by `TabId` elsewhere.
#[derive(Default)]
pub struct TabRegistry {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
}

impl TabRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, url: String) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab::new(id, url));
        id
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn set_active(&mut self, id: TabId) -> Result<(), TabError> {
        match self.get(id) {
            Some(_) => {
                self.active = Some(id);
                Ok(())
            }
            None => Err(TabError::UnknownTab(id)),
        }
    }

    /// Removes the tab and returns the id the caller should activate next
    /// (the neighbour), if any remain.
    pub fn close(&mut self, id: TabId) -> Result<Option<TabId>, TabError> {
        let pos = self
            .tabs
            .iter()
            .position(|t| t.id == id)
            .ok_or(TabError::UnknownTab(id))?;
        self.tabs.remove(pos);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(pos)
                .or_else(|| self.tabs.last())
                .map(|t| t.id);
        }
        Ok(self.active)
    }

    /// Activate the tab `offset` positions from the current one, wrapping.
    pub fn cycle_active(&mut self, offset: isize) -> Option<TabId> {
        if self.tabs.is_empty() {
            return None;
        }
        let current = self
            .active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0) as isize;
        let len = self.tabs.len() as isize;
        let next = self.tabs[((current + offset).rem_euclid(len)) as usize].id;
        self.active = Some(next);
        Some(next)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(reg: &TabRegistry) -> Vec<u64> {
        reg.iter().map(|t| t.id().get()).collect()
    }

    #[test]
    fn create_assigns_increasing_ids_and_keeps_order() {
        let mut reg = TabRegistry::new();
        let a = reg.create("about:blank".into());
        let b = reg.create("https://example.com".into());
        assert_eq!(ids(&reg), vec![a.get(), b.get()]);
        assert!(a.get() < b.get());
    }

    #[test]
    fn set_active_rejects_unknown_tab() {
        let mut reg = TabRegistry::new();
        let id = reg.create("about:blank".into());
        let ghost = TabId(id.get() + 99);
        assert!(matches!(
            reg.set_active(ghost),
            Err(TabError::UnknownTab(_))
        ));
    }

    #[test]
    fn closing_active_moves_selection_to_neighbour() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        reg.set_active(b).unwrap();
        let next = reg.close(b).unwrap();
        assert_eq!(next, Some(c));
        assert_eq!(reg.active_id(), Some(c));

        reg.close(c).unwrap();
        assert_eq!(reg.active_id(), Some(a));
    }

    #[test]
    fn closing_last_tab_leaves_no_active() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        reg.set_active(a).unwrap();
        assert_eq!(reg.close(a).unwrap(), None);
        assert!(reg.is_empty());
    }

    #[test]
    fn cycle_wraps_both_directions() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        reg.set_active(a).unwrap();
        assert_eq!(reg.cycle_active(1), Some(b));
        assert_eq!(reg.cycle_active(1), Some(c));
        assert_eq!(reg.cycle_active(1), Some(a));
        assert_eq!(reg.cycle_active(-1), Some(c));
    }

    #[test]
    fn navigate_updates_url_but_empty_title_is_ignored() {
        let mut reg = TabRegistry::new();
        let id = reg.create("about:blank".into());
        let tab = reg.get_mut(id).unwrap();
        tab.navigate("https://example.com".into());
        tab.set_title(String::new());
        assert_eq!(tab.url(), "https://example.com");
        assert_eq!(tab.title(), "about:blank");
        tab.set_title("Example".into());
        assert_eq!(tab.title(), "Example");
    }
}
