//! Ordered tab collection, selection, duplication and closed-tab recovery.
use super::*;

/// A closed tab kept for Ctrl+Shift+T: the domain object plus the slot it
/// came from. The engine surface is already gone — reopening respawns one
/// at the recorded address. Session history dies with the view (Phase 0).
struct ClosedTab<V, A> {
    tab: Tab<V, A>,
    index: usize,
}

/// What `TabRegistry::close` hands back: the tab the caller should
/// activate next, and the dead tab's surface for the caller to destroy
/// once the neighbour is showing.
pub struct Closed<V> {
    pub next: Option<TabId>,
    pub view: Option<V>,
}

/// How many closed tabs are remembered for reopen.
const MAX_CLOSED: usize = 32;

/// Ordered collection of tabs plus the active selection. The one place
/// tab state lives — surfaces included — so no map keyed by `TabId`
/// exists anywhere else.
pub struct TabRegistry<V, A = ()> {
    tabs: Vec<Tab<V, A>>,
    closed: Vec<ClosedTab<V, A>>,
    active: Option<TabId>,
    next_id: u64,
}

impl<V, A> Default for TabRegistry<V, A> {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            closed: Vec::new(),
            active: None,
            next_id: 0,
        }
    }
}

impl<V, A: SurfaceResources> TabRegistry<V, A> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a tab at `url` around a freshly spawned surface. `spawn` gets
    /// the key the surface's callbacks must carry; if it fails no tab is
    /// created — a live tab without a surface cannot exist.
    pub fn create_with<E>(
        &mut self,
        url: String,
        spawn: impl FnOnce(ViewKey) -> Result<V, E>,
    ) -> Result<TabId, E>
    where
        A: Default,
    {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let view = spawn(ViewKey {
            tab: id,
            generation: 0,
        })?;
        self.tabs.push(Tab::new(id, url, view));
        Ok(id)
    }

    pub fn get(&self, id: TabId) -> Option<&Tab<V, A>> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab<V, A>> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// The tab `key`'s surface belongs to — only while that surface is
    /// the tab's current one. A key from a dropped incarnation, or from a
    /// closed tab, answers `None`: its callbacks are history.
    pub fn live(&self, key: ViewKey) -> Option<&Tab<V, A>> {
        self.get(key.tab)
            .filter(|t| t.view().is_some() && t.generation == key.generation)
    }

    pub fn active(&self) -> Option<&Tab<V, A>> {
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

    /// Give a frozen or discarded tab a fresh surface under a new
    /// generation. `spawn` runs before anything changes; a failure leaves
    /// the tab as it was.
    pub fn restore_with<E: From<TabError>>(
        &mut self,
        id: TabId,
        spawn: impl FnOnce(ViewKey) -> Result<V, E>,
    ) -> Result<(), E> {
        self.get_mut(id)
            .ok_or(TabError::UnknownTab(id))?
            .restore_with(spawn)
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
    /// the caller should activate next (the neighbour), if any remain,
    /// plus the surface it had. The stashed tab keeps a frozen snapshot;
    /// a live one is stashed discarded — its surface is the caller's to
    /// destroy. The active selection is only *suggested* — the caller
    /// owns the transition and confirms it through `set_active`.
    pub fn close(&mut self, id: TabId) -> Result<Closed<V>, TabError> {
        let pos = self.position_of(id).ok_or(TabError::UnknownTab(id))?;
        let fallback = self.focus_fallback(id);
        let mut tab = self.tabs.remove(pos);
        let view = match tab.lifecycle {
            Lifecycle::Active(_) => tab.leave_live(Lifecycle::Discarded),
            _ => None,
        };
        if self.closed.len() >= MAX_CLOSED {
            self.closed.remove(0);
        }
        self.closed.push(ClosedTab { tab, index: pos });
        let next = if self.active == Some(id) {
            self.active = None;
            fallback
        } else {
            self.active
        };
        Ok(Closed { next, view })
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

    /// Clone `id`'s address into a fresh tab inserted right after it,
    /// around a surface `spawn` builds for that address. History is not
    /// copied — the new view starts clean.
    pub fn duplicate_with<E: From<TabError>>(
        &mut self,
        id: TabId,
        spawn: impl FnOnce(ViewKey, &str) -> Result<V, E>,
    ) -> Result<TabId, E>
    where
        A: Default,
    {
        let pos = self.position_of(id).ok_or(TabError::UnknownTab(id))?;
        let url = self.tabs[pos].url.clone();
        let new_id = TabId(self.next_id);
        self.next_id += 1;
        let view = spawn(
            ViewKey {
                tab: new_id,
                generation: 0,
            },
            &url,
        )?;
        self.tabs.insert(pos + 1, Tab::new(new_id, url, view));
        Ok(new_id)
    }

    /// Id of the tab at `index`, clamped to the last tab (Ctrl+9).
    pub fn id_at(&self, index: usize) -> Option<TabId> {
        self.tabs
            .get(index)
            .or_else(|| self.tabs.last())
            .map(|t| t.id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab<V, A>> {
        self.tabs.iter()
    }

    /// Tabs holding a live surface, with it.
    pub fn views(&self) -> impl Iterator<Item = (&Tab<V, A>, &V)> {
        self.tabs.iter().filter_map(|t| t.view().map(|v| (t, v)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab<V, A>> {
        self.tabs.iter_mut()
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

    #[test]
    fn failed_restore_callbacks_cannot_match_the_next_successful_restore() {
        let mut registry = TabRegistry::<u32>::new();
        let id = registry
            .create_with("https://a.test".into(), |_| Ok::<_, TabError>(0))
            .unwrap();
        registry.get_mut(id).unwrap().discard().unwrap();
        let mut failed_key = None;
        let failed = registry.restore_with(id, |key| {
            failed_key = Some(key);
            Err::<u32, _>(TabError::Restore(id))
        });
        assert!(failed.is_err());
        registry.restore_with(id, |_| Ok::<_, TabError>(1)).unwrap();
        assert!(registry.live(failed_key.unwrap()).is_none());
        assert!(registry
            .live(registry.get(id).unwrap().view_key().unwrap())
            .is_some());
    }

    #[derive(Default)]
    struct Attachments {
        prompt: bool,
        downloading: bool,
    }
    impl SurfaceResources for Attachments {
        fn drop_surface(&mut self) {
            self.prompt = false;
            self.downloading = false;
        }
    }

    #[test]
    fn all_surface_teardown_transitions_invalidate_owned_resources() {
        for transition in ["freeze", "discard", "close"] {
            let mut registry = TabRegistry::<u32, Attachments>::new();
            let id = registry
                .create_with("https://a.test".into(), |_| Ok::<_, TabError>(0))
                .unwrap();
            let tab = registry.get_mut(id).unwrap();
            tab.resources_mut().prompt = true;
            tab.resources_mut().downloading = true;
            match transition {
                "freeze" => {
                    tab.freeze(snapshot("https://a.test")).unwrap();
                }
                "discard" => {
                    tab.discard().unwrap();
                }
                _ => {
                    registry.close(id).unwrap();
                    registry.reopen_closed().unwrap();
                }
            }
            let resources = registry.get(id).unwrap().resources();
            assert!(!resources.prompt);
            assert!(!resources.downloading);
        }
    }

    /// Tests stand a `u32` in for the engine surface — a number to tell
    /// incarnations apart without a WebView.
    type Reg = TabRegistry<u32>;

    fn create(reg: &mut Reg, url: &str) -> TabId {
        reg.create_with(url.into(), |_| Ok::<_, TabError>(0))
            .unwrap()
    }

    fn ids(reg: &Reg) -> Vec<u64> {
        reg.iter().map(|t| t.id().get()).collect()
    }

    fn snapshot(url: &str) -> TabSnapshot {
        TabSnapshot {
            url: url.to_string(),
            frozen_at: Instant::now(),
            zoom: 1.5,
            muted: true,
        }
    }

    fn restore(reg: &mut Reg, id: TabId, view: u32) -> ViewKey {
        let mut minted = None;
        reg.restore_with(id, |key| {
            minted = Some(key);
            Ok::<_, TabError>(view)
        })
        .unwrap();
        minted.unwrap()
    }

    #[test]
    fn create_assigns_increasing_ids_and_keeps_order() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "about:blank");
        let b = create(&mut reg, "https://example.com");
        assert_eq!(ids(&reg), vec![a.get(), b.get()]);
        assert!(a.get() < b.get());
    }

    #[test]
    fn a_failed_spawn_creates_no_tab() {
        let mut reg = Reg::new();
        let made = reg.create_with("https://a".into(), |_| Err::<u32, _>("no view"));
        assert!(made.is_err());
        assert!(reg.is_empty());
        // Failed surfaces can queue callbacks: their ids must never be reused.
        assert_eq!(create(&mut reg, "https://b").get(), 1);
    }

    #[test]
    fn set_active_rejects_unknown_tab() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "about:blank");
        let ghost = TabId(id.get() + 99);
        assert!(matches!(
            reg.set_active(ghost),
            Err(TabError::UnknownTab(_))
        ));
    }

    #[test]
    fn closing_active_suggests_neighbour_and_clears_selection() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        let c = create(&mut reg, "c");
        reg.set_active(b).unwrap();
        let next = reg.close(b).unwrap().next;
        assert_eq!(next, Some(c));
        // Suggestion only — the caller confirms via set_active.
        assert_eq!(reg.active_id(), None);
        reg.set_active(next.unwrap()).unwrap();
        assert_eq!(reg.active_id(), Some(c));

        let next = reg.close(c).unwrap().next;
        assert_eq!(next, Some(a));
    }

    #[test]
    fn closing_inactive_keeps_active() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        reg.set_active(a).unwrap();
        assert_eq!(reg.close(b).unwrap().next, Some(a));
        assert_eq!(reg.active_id(), Some(a));
    }

    #[test]
    fn closing_last_tab_leaves_no_active() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        reg.set_active(a).unwrap();
        assert_eq!(reg.close(a).unwrap().next, None);
        assert!(reg.is_empty());
    }

    #[test]
    fn close_hands_back_the_live_surface() {
        let mut reg = Reg::new();
        let a = reg
            .create_with("a".into(), |_| Ok::<_, TabError>(7))
            .unwrap();
        assert_eq!(reg.close(a).unwrap().view, Some(7));
        // The stash holds no surface: the reopened tab must be restored.
        reg.reopen_closed();
        assert_eq!(reg.get(a).unwrap().state(), TabState::Discarded);
        assert!(reg.get(a).unwrap().view().is_none());
    }

    #[test]
    fn reopen_restores_position_and_url() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        let c = create(&mut reg, "c");
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
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        assert_eq!(reg.id_at(0), Some(a));
        assert_eq!(reg.id_at(1), Some(b));
        assert_eq!(reg.id_at(usize::MAX), Some(b));
        reg.close(b).unwrap();
        assert_eq!(reg.id_at(usize::MAX), Some(a));
    }

    #[test]
    fn cycle_target_wraps_without_selecting() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        let c = create(&mut reg, "c");
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
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        let c = create(&mut reg, "c");
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
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "https://example.com");
        let mut spawned_for = String::new();
        let dup = reg
            .duplicate_with(a, |_, url| {
                spawned_for = url.to_string();
                Ok::<_, TabError>(0)
            })
            .unwrap();
        assert_eq!(spawned_for, "a");
        assert_eq!(ids(&reg), vec![a.get(), dup.get(), b.get()]);
        assert_eq!(reg.get(dup).unwrap().url(), "a");
        assert!(reg.get(dup).unwrap().id() != a);
        assert!(matches!(
            reg.duplicate_with(TabId(99), |_, _| Ok::<_, TabError>(0)),
            Err(TabError::UnknownTab(_))
        ));
    }

    #[test]
    fn navigate_updates_url_but_empty_title_is_ignored() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "about:blank");
        let tab = reg.get_mut(id).unwrap();
        tab.navigate("https://example.com".into());
        tab.set_title(String::new());
        assert_eq!(tab.url(), "https://example.com");
        assert_eq!(tab.title(), "about:blank");
        tab.set_title("Example".into());
        assert_eq!(tab.title(), "Example");
    }

    #[test]
    fn freeze_keeps_the_snapshot_and_restore_flips_back() {
        let mut reg = Reg::new();
        let id = reg
            .create_with("https://a".into(), |_| Ok::<_, TabError>(1))
            .unwrap();
        let tab = reg.get_mut(id).unwrap();

        assert_eq!(tab.freeze(snapshot("https://a")).unwrap(), 1);
        assert_eq!(tab.state(), TabState::Frozen);
        assert!(tab.view().is_none());
        assert_eq!(tab.snapshot().unwrap().url, "https://a");
        assert_eq!(tab.snapshot().unwrap().zoom, 1.5);

        restore(&mut reg, id, 2);
        let tab = reg.get(id).unwrap();
        assert_eq!(tab.state(), TabState::Active);
        assert_eq!(tab.view(), Some(&2));
        assert!(tab.snapshot().is_none());
    }

    #[test]
    fn freeze_and_restore_reject_the_wrong_state() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "https://a");

        // Restoring a live tab is a bug, not a no-op — and the spawn
        // never runs, so no orphan surface is built.
        let mut spawned = false;
        let refused = reg.restore_with(id, |_| {
            spawned = true;
            Ok::<_, TabError>(0)
        });
        assert!(matches!(refused, Err(TabError::Restore(_))));
        assert!(!spawned);

        let tab = reg.get_mut(id).unwrap();
        tab.freeze(snapshot("https://a")).unwrap();
        // A frozen tab has nothing left to snapshot.
        assert!(matches!(
            tab.freeze(snapshot("https://a")),
            Err(TabError::Freeze(_))
        ));

        restore(&mut reg, id, 0);
        let tab = reg.get_mut(id).unwrap();
        tab.discard().unwrap();
        assert!(matches!(
            tab.freeze(snapshot("https://a")),
            Err(TabError::Freeze(_))
        ));
    }

    #[test]
    fn a_failed_restore_leaves_the_tab_frozen() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "https://a");
        reg.get_mut(id)
            .unwrap()
            .freeze(snapshot("https://a"))
            .unwrap();
        let failed = reg.restore_with(id, |_| Err::<u32, _>(TabError::NoActiveTab));
        assert!(failed.is_err());
        assert_eq!(reg.get(id).unwrap().state(), TabState::Frozen);
        assert!(reg.get(id).unwrap().snapshot().is_some());
    }

    #[test]
    fn discard_drops_the_snapshot_but_keeps_the_address() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "https://a");
        let tab = reg.get_mut(id).unwrap();

        tab.freeze(snapshot("https://a")).unwrap();
        assert_eq!(tab.discard().unwrap(), None, "a frozen tab has no view");
        assert_eq!(tab.state(), TabState::Discarded);
        assert!(tab.snapshot().is_none());
        assert_eq!(tab.url(), "https://a");

        // Discarding twice is reported; restoring still works.
        assert!(matches!(tab.discard(), Err(TabError::Discard(_))));
        restore(&mut reg, id, 0);
        assert_eq!(reg.get(id).unwrap().state(), TabState::Active);
    }

    #[test]
    fn discarding_a_live_tab_hands_back_its_surface() {
        let mut reg = Reg::new();
        let id = reg
            .create_with("https://a".into(), |_| Ok::<_, TabError>(5))
            .unwrap();
        assert_eq!(reg.get_mut(id).unwrap().discard().unwrap(), Some(5));
    }

    #[test]
    fn a_dropped_surface_key_never_reaches_the_restored_tab() {
        let mut reg = Reg::new();
        let mut first = None;
        let id = reg
            .create_with("https://a".into(), |key| {
                first = Some(key);
                Ok::<_, TabError>(1)
            })
            .unwrap();
        let first = first.unwrap();
        assert_eq!(reg.get(id).unwrap().view_key(), Some(first));
        assert!(reg.live(first).is_some());

        reg.get_mut(id)
            .unwrap()
            .freeze(snapshot("https://a"))
            .unwrap();
        // Frozen: no key is live — the tab has no surface to speak for.
        assert!(reg.live(first).is_none());

        let second = restore(&mut reg, id, 2);
        assert_ne!(first, second);
        assert_eq!(second.tab(), id);
        // The restored tab is Active again, which is exactly the state a
        // stale callback used to slip through on. The key says no.
        assert!(reg.live(first).is_none());
        assert_eq!(reg.live(second).map(|t| t.id()), Some(id));
    }

    #[test]
    fn a_closed_tabs_key_is_dead_even_after_reopen() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "https://a");
        let key = reg.get(id).unwrap().view_key().unwrap();
        reg.close(id).unwrap();
        assert!(reg.live(key).is_none());
        reg.reopen_closed();
        assert!(reg.live(key).is_none());
        let fresh = restore(&mut reg, id, 0);
        assert!(reg.live(key).is_none());
        assert!(reg.live(fresh).is_some());
    }

    #[test]
    fn the_retry_mark_dies_with_the_surface() {
        let mut reg = Reg::new();
        let id = create(&mut reg, "https://a/file.zip");
        let tab = reg.get_mut(id).unwrap();
        tab.host_retry();
        assert!(tab.is_retry_host());
        tab.freeze(snapshot("https://a/file.zip")).unwrap();
        assert!(!tab.is_retry_host());
        // A tab without a surface cannot host anything.
        tab.host_retry();
        assert!(!tab.is_retry_host());
        restore(&mut reg, id, 0);
        let tab = reg.get_mut(id).unwrap();
        tab.host_retry();
        assert!(tab.release_retry());
        assert!(!tab.release_retry());
    }

    #[test]
    fn focus_fallback_prefers_right_then_left_neighbour() {
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        let c = create(&mut reg, "c");
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
        let mut reg = Reg::new();
        let a = create(&mut reg, "a");
        let b = create(&mut reg, "b");
        reg.get_mut(b).unwrap().freeze(snapshot("b")).unwrap();
        assert_eq!(reg.close(b).unwrap().view, None);
        assert_eq!(reg.reopen_closed(), Some(b));
        // The surface comes back through restore, not through reopen —
        // the domain still says Frozen so the caller spawns one first.
        assert_eq!(reg.get(b).unwrap().state(), TabState::Frozen);
        assert!(reg.get(a).unwrap().state() == TabState::Active);
    }
}
