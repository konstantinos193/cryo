//! Tab domain: identity, lifecycle state, and the ordered registry.
//!
//! Names no engine types. A live tab *owns* its surface, but as an opaque
//! `V` the caller picks (the app's `Box<dyn ContentView>`, tests' `()`):
//! the surface lives inside `Lifecycle::Active`, so "a view exists exactly
//! while the tab is active" is a fact of the types, not a convention two
//! collections keep in step.
//!
//! Every surface incarnation gets a `ViewKey` — the tab plus a generation
//! that grows on each restore. Engine callbacks carry the key they were
//! born with; `TabRegistry::live` answers only for the current one, so a
//! callback still in flight from a dropped surface cannot reach the tab's
//! next incarnation.

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

/// One incarnation of a tab's engine surface: the tab plus the generation
/// its view was spawned at. Minted only here, when a surface is attached —
/// a key from a surface that has since been dropped never matches again.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ViewKey {
    tab: TabId,
    generation: u64,
}

impl ViewKey {
    pub fn tab(self) -> TabId {
        self.tab
    }
}

impl fmt::Display for ViewKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.tab, self.generation)
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

/// A monotonic clock — the port freeze times are stamped through
/// (AGENTS §6). `Instant::now()` is an ambient call, so the caller
/// injects this and budget policy and tests control what "now" means.
/// The composition root wires `SystemClock`; a test wires one it can
/// advance.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// The real clock — `Instant::now()` behind the `Clock` port.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A tab's place in the lifecycle, carrying what each phase needs.
/// `Active` owns the live surface and `Frozen` its snapshot — neither can
/// be built without the thing it stands for.
enum Lifecycle<V> {
    Active(V),
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

/// Largest page icon kept, in PNG bytes. `GetFavicon` answers whatever
/// the site declared — a megabyte "favicon" is a payload, not an icon,
/// and the strip can paint a few kilobytes either way.
const MAX_FAVICON_PNG: usize = 96_000;

/// Lifecycle hook for opaque per-tab application resources. The tab invokes
/// it for every transition that relinquishes a surface, including close.
pub trait SurfaceResources {
    fn drop_surface(&mut self);
}

impl SurfaceResources for () {
    fn drop_surface(&mut self) {}
}

/// A browser tab. Mutated only through its methods — fields are private so
/// outside code can't create an invalid combination.
pub struct Tab<V, A = ()> {
    resources: A,
    id: TabId,
    /// Generation of the newest surface ever attached. Only grows — the
    /// current surface's `ViewKey` is `(id, generation)`.
    generation: u64,
    url: String,
    title: String,
    /// The page's icon as PNG bytes, last thing the engine reported.
    /// Kept across freeze/discard — the strip badges the site, not the
    /// surface's liveness.
    favicon: Option<Vec<u8>>,
    /// A top-level navigation is in flight on this tab's surface — the
    /// strip's spinner and the reload↔stop swap read it. Cleared by the
    /// engine's settle event and by any transition that drops the view.
    loading: bool,
    /// The tab exists only to host a retried download — its surface must
    /// live for the transfer, and the tab closes itself once it settles.
    /// Dropped with the surface: a frozen tab hosts nothing.
    retry_host: bool,
    /// When the tab last left the foreground — `None` while it is the
    /// active tab. Set by the caller (surfaces), which owns the clock;
    /// the budget enforcer reads it to pick the coldest background tab.
    unfocused_since: Option<Instant>,
    lifecycle: Lifecycle<V>,
}

impl<V, A: SurfaceResources> Tab<V, A> {
    pub fn resources(&self) -> &A {
        &self.resources
    }
    pub fn resources_mut(&mut self) -> &mut A {
        &mut self.resources
    }
    fn new(id: TabId, url: String, view: V) -> Self
    where
        A: Default,
    {
        Self {
            id,
            resources: A::default(),
            generation: 0,
            title: url.clone(),
            url,
            favicon: None,
            loading: false,
            retry_host: false,
            unfocused_since: None,
            lifecycle: Lifecycle::Active(view),
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

    /// PNG bytes of the page's icon, if the engine reported one.
    pub fn favicon(&self) -> Option<&[u8]> {
        self.favicon.as_deref()
    }

    pub fn state(&self) -> TabState {
        match self.lifecycle {
            Lifecycle::Active(_) => TabState::Active,
            Lifecycle::Frozen(_) => TabState::Frozen,
            Lifecycle::Discarded => TabState::Discarded,
        }
    }

    /// The live surface — `None` exactly when the tab is frozen or
    /// discarded.
    pub fn view(&self) -> Option<&V> {
        match &self.lifecycle {
            Lifecycle::Active(view) => Some(view),
            _ => None,
        }
    }

    /// The key the live surface's callbacks carry — `None` without one.
    pub fn view_key(&self) -> Option<ViewKey> {
        self.view().map(|_| self.current_key())
    }

    fn current_key(&self) -> ViewKey {
        ViewKey {
            tab: self.id,
            generation: self.generation,
        }
    }

    /// Whether this tab's surface has a navigation in flight.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The engine's start/settle signal. A non-live tab has no surface —
    /// nothing can be loading, so the flag drops with the view.
    pub(crate) fn set_loading(&mut self, loading: bool) {
        self.loading = loading && self.view().is_some();
    }

    /// Whether the tab exists only to host a retried download.
    pub fn is_retry_host(&self) -> bool {
        self.retry_host
    }

    /// Mark a live tab as a retry's host. A tab without a surface can't
    /// fetch anything, so the mark only lands on a live one.
    pub(crate) fn host_retry(&mut self) {
        self.retry_host = self.view().is_some();
    }

    /// The tab stops being disposable — its download settled or the user
    /// took it over. Returns whether it was a retry host.
    pub(crate) fn release_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry_host)
    }

    /// The snapshot a frozen tab restores from — `None` while live or
    /// discarded. Read-only: callers look before respawning the surface.
    pub fn snapshot(&self) -> Option<&TabSnapshot> {
        match &self.lifecycle {
            Lifecycle::Frozen(s) => Some(s),
            _ => None,
        }
    }

    /// Freeze = keep the snapshot, hand back the surface for the caller
    /// to destroy. Only a live tab can freeze — a frozen or discarded one
    /// has nothing left to snapshot.
    pub(crate) fn freeze(&mut self, snapshot: TabSnapshot) -> Result<V, TabError> {
        if self.view().is_none() {
            return Err(TabError::Freeze(self.id));
        }
        self.leave_live(Lifecycle::Frozen(snapshot))
            .ok_or(TabError::Freeze(self.id))
    }

    /// Discard keeps only the address — the harsher, cheaper drop a budget
    /// enforcer uses under pressure. Legal from live (the surface comes
    /// back for the caller to destroy) and frozen (the snapshot dies);
    /// discarding twice is an error.
    pub(crate) fn discard(&mut self) -> Result<Option<V>, TabError> {
        match self.lifecycle {
            Lifecycle::Discarded => Err(TabError::Discard(self.id)),
            _ => Ok(self.leave_live(Lifecycle::Discarded)),
        }
    }

    /// Move to a surface-less phase. Everything that only means something
    /// while a surface exists goes with it; the old surface, if any, is
    /// returned.
    fn leave_live(&mut self, next: Lifecycle<V>) -> Option<V> {
        if self.view().is_some() {
            self.resources.drop_surface();
        }
        self.loading = false;
        self.retry_host = false;
        match std::mem::replace(&mut self.lifecycle, next) {
            Lifecycle::Active(view) => Some(view),
            _ => None,
        }
    }

    /// Back to live with a freshly spawned surface under a new generation.
    /// `spawn` runs first and gets the key its callbacks must carry — if
    /// it fails the tab stays frozen so the next activation can retry.
    fn restore_with<E: From<TabError>>(
        &mut self,
        spawn: impl FnOnce(ViewKey) -> Result<V, E>,
    ) -> Result<(), E> {
        if self.view().is_some() {
            return Err(TabError::Restore(self.id).into());
        }
        let key = ViewKey {
            tab: self.id,
            generation: self.generation + 1,
        };
        self.generation = key.generation;
        // Failed spawns can queue callbacks; their keys must never be reused.
        let view = spawn(key)?;
        self.lifecycle = Lifecycle::Active(view);
        Ok(())
    }

    /// Point the tab at an address. A frozen tab's snapshot follows: the
    /// page it froze on is not where it is going any more, and a restore
    /// that reloaded the old address would silently discard the
    /// navigation.
    pub(crate) fn navigate(&mut self, url: String) {
        if let Lifecycle::Frozen(snap) = &mut self.lifecycle {
            snap.url.clone_from(&url);
        }
        self.url = url;
    }

    pub(crate) fn set_title(&mut self, title: String) {
        if !title.is_empty() {
            self.title = title;
        }
    }

    /// The engine reports the icon it just fetched; `None` clears — a
    /// document without an icon drops the previous page's. An oversized
    /// payload is dropped, not kept: it is not an icon anymore.
    pub(crate) fn set_favicon(&mut self, png: Option<Vec<u8>>) {
        self.favicon = png.filter(|b| b.len() <= MAX_FAVICON_PNG);
    }

    /// The moment this tab last left the foreground — `None` while it is
    /// active. Read by the view-budget enforcement (freeze the coldest)
    /// and the idle suspender.
    pub(crate) fn unfocused_since(&self) -> Option<Instant> {
        self.unfocused_since
    }

    /// The tab left the foreground — a surface spawn counts: a view is
    /// always born hidden, so a tab never yet activated is cold from
    /// birth. `now` is stamped by the caller — no ambient clock here.
    pub(crate) fn note_unfocused(&mut self, now: Instant) {
        self.unfocused_since = Some(now);
    }

    /// The tab took the foreground — it is the warm tab now.
    pub(crate) fn note_focused(&mut self) {
        self.unfocused_since = None;
    }
}

mod registry;
pub use registry::{Closed, TabRegistry};
