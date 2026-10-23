//! Tab domain: identity, lifecycle state, and the ordered registry.
//!
//! Owns no engine types — a `Tab` is pure state. The engine's webviews are
//! keyed by `TabId` in the app layer.

use std::fmt;
use std::time::Instant;

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
/// the address). What each state carries lives in `Lifecycle` — this is
/// the query type callers and the wire see.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabState {
    Active,
    Frozen,
    Discarded,
}

impl TabState {
    /// Stable lowercase name for the chrome wire format.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Frozen => "frozen",
            Self::Discarded => "discarded",
        }
    }
}

/// The state kept when a tab's surface is destroyed by freeze — enough to
/// rebuild it on restore. A value object: replaced wholesale, never
/// mutated in place. `frozen_at` comes from the caller, not a clock call
/// buried here, so budget policy and tests control time.
#[derive(Clone, Debug)]
pub struct TabSnapshot {
    /// The address the surface reloads at.
    pub url: String,
    /// When the view was dropped. Phase 2 budgets rank freeze age by it.
    pub frozen_at: Instant,
    /// Zoom factor the view had, to reapply on restore.
    pub zoom: f64,
    /// Mute state the view had, to reapply on restore.
    pub muted: bool,
}

/// A tab's place in the lifecycle, carrying what each phase needs.
/// `Frozen` owns its snapshot — a frozen tab without one cannot be built.
enum Lifecycle {
    Active,
    Frozen(TabSnapshot),
    Discarded,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TabError {
    #[error("unknown tab {0}")]
    UnknownTab(TabId),
    #[error("no active tab")]
    NoActiveTab,
    /// Freeze needs a live surface to snapshot.
    #[error("tab {0} is not live — nothing to freeze")]
    Freeze(TabId),
    /// Discarding a discarded tab is a no-op the caller should hear about.
    #[error("tab {0} is already discarded")]
    Discard(TabId),
    /// Restore needs a frozen or discarded tab.
    #[error("tab {0} is already live — nothing to restore")]
    Restore(TabId),
}

/// A browser tab. Mutated only through its methods — fields are private so
/// outside code can't create an invalid combination.
pub struct Tab {
    id: TabId,
    url: String,
    title: String,
    lifecycle: Lifecycle,
}

impl Tab {
    fn new(id: TabId, url: String) -> Self {
        Self {
            id,
            title: url.clone(),
            url,
            lifecycle: Lifecycle::Active,
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
        match self.lifecycle {
            Lifecycle::Active => TabState::Active,
            Lifecycle::Frozen(_) => TabState::Frozen,
            Lifecycle::Discarded => TabState::Discarded,
        }
    }

    /// The snapshot a frozen tab restores from — `None` while live or
    /// discarded. Read-only: callers look before respawning the surface.
    pub fn snapshot(&self) -> Option<&TabSnapshot> {
        match &self.lifecycle {
            Lifecycle::Frozen(s) => Some(s),
            _ => None,
        }
    }

    /// Freeze = snapshot the state; the caller drops the engine surface
    /// once this returns. Only a live tab can freeze — a frozen or
    /// discarded one has nothing left to snapshot.
    pub(crate) fn freeze(&mut self, snapshot: TabSnapshot) -> Result<(), TabError> {
        match self.lifecycle {
            Lifecycle::Active => {
                self.lifecycle = Lifecycle::Frozen(snapshot);
                Ok(())
            }
            _ => Err(TabError::Freeze(self.id)),
        }
    }

    /// Discard keeps only the address — the harsher, cheaper drop a budget
    /// enforcer uses under pressure. Legal from live and frozen (the
    /// snapshot dies with it); discarding twice is an error.
    pub(crate) fn discard(&mut self) -> Result<(), TabError> {
        match self.lifecycle {
            Lifecycle::Discarded => Err(TabError::Discard(self.id)),
            _ => {
                self.lifecycle = Lifecycle::Discarded;
                Ok(())
            }
        }
    }

    /// Back to live. The caller respawns the surface *before* calling
    /// this — if spawning fails the tab must stay frozen so the next
    /// activation can retry.
    pub(crate) fn restore(&mut self) -> Result<(), TabError> {
        match self.lifecycle {
            Lifecycle::Active => Err(TabError::Restore(self.id)),
            _ => {
                self.lifecycle = Lifecycle::Active;
                Ok(())
            }
        }
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

/// A closed tab kept for Ctrl+Shift+T: the domain object plus the slot it
/// came from. The engine surface is already gone — reopening respawns one
/// at the recorded address. Session history dies with the view (Phase 0).
struct ClosedTab {
    tab: Tab,
    index: usize,
}

/// How many closed tabs are remembered for reopen.
const MAX_CLOSED: usize = 32;

/// Ordered collection of tabs plus the active selection. The one place
/// tab state lives — no parallel maps keyed by `TabId` elsewhere.
#[derive(Default)]
pub struct TabRegistry {
    tabs: Vec<Tab>,
    closed: Vec<ClosedTab>,
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

    /// The tab that should take focus when `id` leaves the foreground —
    /// its right neighbour, else its left. Shared by close (which removes
    /// the tab) and freeze/discard (which keep it in the strip).
    pub fn focus_fallback(&self, id: TabId) -> Option<TabId> {
        let pos = self.position_of(id)?;
        self.tabs
            .get(pos + 1)
            .or_else(|| pos.checked_sub(1).and_then(|i| self.tabs.get(i)))
            .map(|t| t.id)
    }

    /// Removes the tab, stashes it for `reopen_closed`, and returns the id
    /// the caller should activate next (the neighbour), if any remain.
    /// The active selection is only *suggested* — the caller owns the
    /// transition and confirms it through `set_active`.
    pub fn close(&mut self, id: TabId) -> Result<Option<TabId>, TabError> {
        let pos = self
            .tabs
            .iter()
            .position(|t| t.id == id)
            .ok_or(TabError::UnknownTab(id))?;
        let fallback = self.focus_fallback(id);
        let tab = self.tabs.remove(pos);
        if self.closed.len() >= MAX_CLOSED {
            self.closed.remove(0);
        }
        self.closed.push(ClosedTab { tab, index: pos });
        if self.active == Some(id) {
            self.active = None;
            Ok(fallback)
        } else {
            Ok(self.active)
        }
    }

    /// Reinsert the most recently closed tab at its old position and
    /// return its id. The caller respawns the engine surface; the tab's
    /// old back/forward history is gone with the dead view.
    pub fn reopen_closed(&mut self) -> Option<TabId> {
        let closed = self.closed.pop()?;
        let at = closed.index.min(self.tabs.len());
        let id = closed.tab.id;
        self.tabs.insert(at, closed.tab);
        Some(id)
    }

    /// The id `offset` positions from the active tab, wrapping. A pure
    /// query — the caller activates the result.
    pub fn cycle_target(&self, offset: isize) -> Option<TabId> {
        if self.tabs.is_empty() {
            return None;
        }
        let current = self
            .active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0) as isize;
        let len = self.tabs.len() as isize;
        Some(self.tabs[((current + offset).rem_euclid(len)) as usize].id)
    }

    /// Position of `id` in strip order.
    pub fn position_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Move `id` to `index` (clamped). Used by Ctrl+Shift+PgUp/PgDn and
    /// Home/End variants. Returns the position it landed on.
    pub fn relocate(&mut self, id: TabId, index: usize) -> Result<usize, TabError> {
        let from = self.position_of(id).ok_or(TabError::UnknownTab(id))?;
        let to = index.min(self.tabs.len() - 1);
        if from != to {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
        }
        Ok(to)
    }

    /// Clone `id`'s address into a fresh tab inserted right after it.
    /// History is not copied — the new view starts clean.
    pub fn duplicate(&mut self, id: TabId) -> Result<TabId, TabError> {
        let pos = self.position_of(id).ok_or(TabError::UnknownTab(id))?;
        let url = self.tabs[pos].url.clone();
        let new_id = TabId(self.next_id);
        self.next_id += 1;
        self.tabs.insert(pos + 1, Tab::new(new_id, url));
        Ok(new_id)
    }

    /// Id of the tab at `index`, clamped to the last tab (Ctrl+9).
    pub fn id_at(&self, index: usize) -> Option<TabId> {
        self.tabs
            .get(index)
            .or_else(|| self.tabs.last())
            .map(|t| t.id)
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
    fn closing_active_suggests_neighbour_and_clears_selection() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        reg.set_active(b).unwrap();
        let next = reg.close(b).unwrap();
        assert_eq!(next, Some(c));
        // Suggestion only — the caller confirms via set_active.
        assert_eq!(reg.active_id(), None);
        reg.set_active(next.unwrap()).unwrap();
        assert_eq!(reg.active_id(), Some(c));

        let next = reg.close(c).unwrap();
        assert_eq!(next, Some(a));
    }

    #[test]
    fn closing_inactive_keeps_active() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        reg.set_active(a).unwrap();
        assert_eq!(reg.close(b).unwrap(), Some(a));
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
    fn reopen_restores_position_and_url() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        reg.close(b).unwrap();
        assert_eq!(reg.len(), 2);

        let reopened = reg.reopen_closed().unwrap();
        assert_eq!(reopened, b);
        assert_eq!(ids(&reg), vec![a.get(), b.get(), c.get()]);
        assert_eq!(reg.get(b).unwrap().url(), "b");

        // Only b was closed — the stack is now empty.
        assert_eq!(reg.reopen_closed(), None);
    }

    #[test]
    fn id_at_clamps_to_last() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        assert_eq!(reg.id_at(0), Some(a));
        assert_eq!(reg.id_at(1), Some(b));
        assert_eq!(reg.id_at(usize::MAX), Some(b));
        reg.close(b).unwrap();
        assert_eq!(reg.id_at(usize::MAX), Some(a));
    }

    #[test]
    fn cycle_target_wraps_without_selecting() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        reg.set_active(a).unwrap();
        assert_eq!(reg.cycle_target(1), Some(b));
        // Pure query — selection unchanged until the caller activates.
        assert_eq!(reg.active_id(), Some(a));
        reg.set_active(b).unwrap();
        assert_eq!(reg.cycle_target(1), Some(c));
        reg.set_active(c).unwrap();
        assert_eq!(reg.cycle_target(1), Some(a));
        assert_eq!(reg.cycle_target(-1), Some(b));
    }

    #[test]
    fn relocate_moves_and_clamps() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        assert_eq!(reg.relocate(a, 2).unwrap(), 2);
        assert_eq!(ids(&reg), vec![b.get(), c.get(), a.get()]);
        assert_eq!(reg.relocate(a, 99).unwrap(), 2);
        assert!(matches!(
            reg.relocate(TabId(99), 0),
            Err(TabError::UnknownTab(_))
        ));
    }

    #[test]
    fn duplicate_copies_url_right_after() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("https://example.com".into());
        let dup = reg.duplicate(a).unwrap();
        assert_eq!(ids(&reg), vec![a.get(), dup.get(), b.get()]);
        assert_eq!(reg.get(dup).unwrap().url(), "a");
        assert!(reg.get(dup).unwrap().id() != a);
        assert!(matches!(
            reg.duplicate(TabId(99)),
            Err(TabError::UnknownTab(_))
        ));
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

    fn snapshot(url: &str) -> TabSnapshot {
        TabSnapshot {
            url: url.to_string(),
            frozen_at: Instant::now(),
            zoom: 1.5,
            muted: true,
        }
    }

    #[test]
    fn freeze_keeps_the_snapshot_and_restore_flips_back() {
        let mut reg = TabRegistry::new();
        let id = reg.create("https://a".into());
        let tab = reg.get_mut(id).unwrap();

        tab.freeze(snapshot("https://a")).unwrap();
        assert_eq!(tab.state(), TabState::Frozen);
        assert_eq!(tab.snapshot().unwrap().url, "https://a");
        assert_eq!(tab.snapshot().unwrap().zoom, 1.5);

        tab.restore().unwrap();
        assert_eq!(tab.state(), TabState::Active);
        assert!(tab.snapshot().is_none());
    }

    #[test]
    fn freeze_and_restore_reject_the_wrong_state() {
        let mut reg = TabRegistry::new();
        let id = reg.create("https://a".into());
        let tab = reg.get_mut(id).unwrap();

        // Restoring a live tab is a bug, not a no-op.
        assert!(matches!(tab.restore(), Err(TabError::Restore(_))));

        tab.freeze(snapshot("https://a")).unwrap();
        // A frozen tab has nothing left to snapshot.
        assert!(matches!(
            tab.freeze(snapshot("https://a")),
            Err(TabError::Freeze(_))
        ));

        tab.restore().unwrap();
        tab.discard().unwrap();
        assert!(matches!(
            tab.freeze(snapshot("https://a")),
            Err(TabError::Freeze(_))
        ));
    }

    #[test]
    fn discard_drops_the_snapshot_but_keeps_the_address() {
        let mut reg = TabRegistry::new();
        let id = reg.create("https://a".into());
        let tab = reg.get_mut(id).unwrap();

        tab.freeze(snapshot("https://a")).unwrap();
        tab.discard().unwrap();
        assert_eq!(tab.state(), TabState::Discarded);
        assert!(tab.snapshot().is_none());
        assert_eq!(tab.url(), "https://a");

        // Discarding twice is reported; restoring still works.
        assert!(matches!(tab.discard(), Err(TabError::Discard(_))));
        tab.restore().unwrap();
        assert_eq!(tab.state(), TabState::Active);
    }

    #[test]
    fn focus_fallback_prefers_right_then_left_neighbour() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        let c = reg.create("c".into());
        assert_eq!(reg.focus_fallback(a), Some(b));
        assert_eq!(reg.focus_fallback(b), Some(c));
        assert_eq!(reg.focus_fallback(c), Some(b));
        assert_eq!(reg.focus_fallback(TabId(99)), None);

        reg.close(b).unwrap();
        reg.close(c).unwrap();
        assert_eq!(reg.focus_fallback(a), None);
    }

    #[test]
    fn a_frozen_tab_stays_frozen_through_close_and_reopen() {
        let mut reg = TabRegistry::new();
        let a = reg.create("a".into());
        let b = reg.create("b".into());
        reg.get_mut(b).unwrap().freeze(snapshot("b")).unwrap();
        reg.close(b).unwrap();
        assert_eq!(reg.reopen_closed(), Some(b));
        // The surface comes back through restore, not through reopen —
        // the domain still says Frozen so the caller spawns one first.
        assert_eq!(reg.get(b).unwrap().state(), TabState::Frozen);
        assert!(reg.get(a).unwrap().state() == TabState::Active);
    }
}
