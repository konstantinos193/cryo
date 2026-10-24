//! Engine surfaces: spawning the chrome and content views, and the
//! freeze/discard/restore lifecycle that maps `Tab` states onto live
//! `ContentView`s in `self.views`.

use std::sync::Mutex;
use std::time::Duration;

use tao::event_loop::EventLoopProxy;
use tao::window::Fullscreen;

use crate::engine::{
    ContentView, DownloadState, DropEvent, Engine, LoadFailure, PermissionDecision, PermissionKind,
    Rect, ViewSpec,
};
use crate::shell::chrome::ChromeCommand;
use crate::shell::pages::{CHROME_URL, NEW_TAB_URL};
use crate::shell::{self, error};
use crate::tabs::{TabId, TabSnapshot, TabState};

use super::{content_url_for, post, App, AppEvent, DEFAULT_URL};

/// The view budget, counted in live surfaces: the Phase 2 byte budgets'
/// stand-in. A live WebView2 is the only heavyweight the app holds —
/// past this many, spawning a view freezes the coldest background tab
/// to pay for the new one. Uncapped views are the "eat until something
/// dies" behavior the shell exists to refuse.
const MAX_VIEWS: usize = 24;

/// Hard bound on strip entries, live or frozen — see `tab_limit_notice`.
const MAX_TABS: usize = 200;

/// A background tab hidden this long earns a suspended renderer — the
/// freeze machinery's lighter cousin applied by the clock rather than
/// the view count.
const SUSPEND_AFTER: Duration = Duration::from_secs(60);

impl App {
    /// The chrome strip view: transparent so the content shows through
    /// wherever the document paints nothing (the view grows over the
    /// whole window while an overlay is open).
    pub(super) fn spawn_chrome(
        engine: &dyn Engine,
        proxy: &EventLoopProxy<AppEvent>,
        bounds: Rect,
    ) -> anyhow::Result<Box<dyn ContentView>> {
        // The engine's command transport runs inside the protocol handler,
        // which is Sync-bound — the proxy goes behind a Mutex (uncontended;
        // WebView2 calls it on the UI thread anyway).
        let ipc_proxy = Mutex::new(proxy.clone());
        let drop_proxy = proxy.clone();
        let chrome = engine.spawn(ViewSpec {
            bounds,
            visible: true,
            transparent: true,
            background: Some(shell::CHROME_BACKGROUND),
            on_accelerator: Some(Self::accelerator_forwarder(proxy)),
            // File drops on the strip are ours, not the document's: a
            // file landing anywhere on it (a shelf row dragged back, or
            // one dragged in from Explorer) opens in a new tab.
            on_drop: Some(Box::new(move |event| {
                if let DropEvent::Dropped { paths, .. } = event {
                    post(&drop_proxy, AppEvent::FilesDropped(paths));
                }
            })),
            on_ipc: Some(Box::new(move |body| {
                if let Ok(cmd) = serde_json::from_str::<ChromeCommand>(&body) {
                    let proxy = ipc_proxy.lock().expect("ipc proxy lock");
                    post(&proxy, AppEvent::Chrome(cmd));
                }
            })),
            ..ViewSpec::url(CHROME_URL)
        })?;
        Ok(chrome)
    }

    pub(super) fn spawn_content_view(
        engine: &dyn Engine,
        proxy: &EventLoopProxy<AppEvent>,
        content_rect: Rect,
        id: TabId,
        url: &str,
        visible: bool,
    ) -> anyhow::Result<Box<dyn ContentView>> {
        let title_proxy = proxy.clone();
        let nav_proxy = proxy.clone();
        let fav_proxy = proxy.clone();
        let nw_proxy = proxy.clone();
        let fs_proxy = proxy.clone();
        let fail_proxy = proxy.clone();
        let dl_proxy = proxy.clone();
        let load_proxy = proxy.clone();
        let view = engine.spawn(ViewSpec {
            bounds: content_rect,
            visible,
            background: Some(shell::CONTENT_BACKGROUND),
            on_title_change: Some(Box::new(move |t| {
                post(&title_proxy, AppEvent::TitleChanged(id, t));
            })),
            on_navigate: Some(Box::new(move |uri| {
                if let Some(choose) = crate::shell::pages::download_folder_request(&uri) {
                    post(&nav_proxy, AppEvent::DownloadFolder(id, choose));
                    return false;
                }
                post(&nav_proxy, AppEvent::Navigated(id, uri.clone()));
                true
            })),
            on_favicon: Some(Box::new(move |png| {
                post(&fav_proxy, AppEvent::FaviconChanged(id, png));
            })),
            on_accelerator: Some(Self::accelerator_forwarder(proxy)),
            on_new_window: Some(Box::new(move |url| {
                post(&nw_proxy, AppEvent::OpenInNewTab(url));
            })),
            on_fullscreen: Some(Box::new(move |on| {
                post(&fs_proxy, AppEvent::PageFullscreen(id, on));
            })),
            on_load_failure: Some(Box::new(move |failure| {
                post(&fail_proxy, AppEvent::LoadFailed(id, failure));
            })),
            on_download: Some(Box::new(move |update| {
                post(&dl_proxy, AppEvent::Download(id, update));
            })),
            on_load_change: Some(Box::new(move |loading| {
                post(&load_proxy, AppEvent::LoadChanged(id, loading));
            })),
            // Pages may take the mic and camera — without a handler the
            // engine's default is a silent deny, which is why calls appear
            // to connect but transmit nothing. Everything else is denied.
            on_permission: Some(Box::new(|kind| match kind {
                PermissionKind::Microphone | PermissionKind::Camera => PermissionDecision::Allow,
                _ => PermissionDecision::Deny,
            })),
            ..ViewSpec::url(url)
        })?;
        Ok(view)
    }

    /// Spawn `id`'s engine surface at the content rect, keeping the
    /// registry entry and the view in step. Always spawned hidden —
    /// `activate` owns the switch. A view created visible paints the
    /// engine's white default for a frame before its background colour
    /// applies; one that is shown only once it exists never does.
    pub(super) fn spawn_view_at(&mut self, id: TabId, url: &str) -> anyhow::Result<()> {
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view =
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, id, url, false)?;
        self.views.insert(id, view);
        // A fresh surface is born hidden — cold until activated says
        // otherwise (the next line, for foreground spawns).
        if let Some(tab) = self.registry.get_mut(id) {
            tab.note_unfocused(self.clock.now());
        }
        self.enforce_view_budget(id)
    }

    /// Hold `views` to `MAX_VIEWS` by freezing the coldest background
    /// tab. `keep` is the view just paid for — the budget spends other
    /// tabs first; it is only frozen when it is the *only* live
    /// background surface, which cannot leave the strip unpaintable
    /// (the active tab's view is never a candidate).
    fn enforce_view_budget(&mut self, keep: TabId) -> anyhow::Result<()> {
        while self.views.len() > MAX_VIEWS {
            let Some(coldest) = self.coldest_live_tab(keep) else {
                return Ok(());
            };
            self.freeze_view(coldest)?;
        }
        Ok(())
    }

    /// The live-viewed tab that has been out of the foreground longest —
    /// never `keep`, never the active one. Frozen and discarded tabs
    /// aren't candidates: they hold no view to give back.
    fn coldest_live_tab(&self, keep: TabId) -> Option<TabId> {
        let active = self.registry.active_id();
        let now = self.clock.now();
        self.views
            .keys()
            .copied()
            .filter(|id| *id != keep && Some(*id) != active)
            .min_by_key(|id| {
                self.registry
                    .get(*id)
                    .and_then(|t| t.unfocused_since())
                    .unwrap_or(now)
            })
    }

    /// Destroy `id`'s surface and the shelf entries hanging off it.
    /// Every removal — freeze, discard, close — funnels here, so
    /// `views` has exactly one deleter keeping it in step with
    /// `registry`.
    pub(super) fn drop_view(&mut self, id: TabId) {
        self.drop_downloads_of(id);
        drop(self.views.remove(&id));
    }

    /// Give a view-less tab — frozen, discarded, or one coming back from
    /// the closed stack — a fresh surface, then flip it back to live.
    pub(super) fn restore_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(tab) = self.registry.get(id) else {
            return Ok(());
        };
        let snap = tab.snapshot();
        let url = snap.map(|s| s.url.as_str()).unwrap_or_else(|| tab.url());
        let load = content_url_for(url).to_string();
        let zoom = snap.map(|s| s.zoom).unwrap_or(1.0);
        let muted = snap.map(|s| s.muted).unwrap_or(false);
        self.spawn_view_at(id, &load)?;
        // Snapshot extras exist only for frozen tabs; discarded starts clean.
        if let Some(view) = self.views.get(&id) {
            if zoom != 1.0 {
                view.set_zoom_factor(zoom)?;
            }
            if muted {
                view.set_muted(true)?;
            }
        }
        // The surface exists — the domain may flip back to live. If spawn
        // had failed above we'd have bailed, leaving the tab frozen.
        if let Some(tab) = self.registry.get_mut(id) {
            if tab.state() != TabState::Active {
                tab.restore()?;
            }
        }
        Ok(())
    }

    /// Freeze `id`: snapshot its surface state, destroy the view, leave a
    /// dimmed marker in the strip. Freezing the active tab hands focus to
    /// the same neighbour a close would; a lone tab can't freeze because
    /// nothing would be left to paint, so the command just does nothing.
    pub(super) fn freeze_tab(&mut self, id: TabId) -> anyhow::Result<()> {
        if !self.leave_foreground(id)? {
            return Ok(());
        }
        self.freeze_view(id)?;
        self.sync_chrome()
    }

    /// Freeze `id`'s surface without touching focus or chrome — the core
    /// `freeze_tab` and the view budget both spend. Only a live surface
    /// has anything worth snapshotting — a view missing here means the
    /// tab is already frozen or discarded.
    fn freeze_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let zoom = view.zoom_factor().unwrap_or(1.0);
        let muted = view.muted().unwrap_or(false);
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        let frozen_at = self.clock.now();
        tab.freeze(TabSnapshot {
            url: tab.url().to_string(),
            frozen_at,
            zoom,
            muted,
        })?;
        self.drop_view(id);
        Ok(())
    }

    /// Discard `id`: drop the surface keeping nothing but the address —
    /// freeze without a snapshot. Under memory pressure this is what the
    /// (Phase 2) enforcer will pick; the trigger here is manual.
    pub(super) fn discard_tab(&mut self, id: TabId) -> anyhow::Result<()> {
        if !self.leave_foreground(id)? {
            return Ok(());
        }
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        tab.discard()?;
        self.drop_view(id);
        self.sync_chrome()
    }

    /// Move focus off `id` when it's the showing tab — freeze and discard
    /// leave the tab in the strip, so the neighbour a close would pick
    /// takes over first. Returns `false` when `id` is the only tab: with
    /// nothing left to paint, its view must stay alive.
    fn leave_foreground(&mut self, id: TabId) -> anyhow::Result<bool> {
        if self.registry.active_id() != Some(id) {
            return Ok(true);
        }
        let Some(next) = self.registry.focus_fallback(id) else {
            return Ok(false);
        };
        // A fullscreen page owns the whole window — leave fullscreen
        // before the switch or the strip stays hidden under it.
        if self.page_fullscreen {
            self.page_fullscreen(id, false)?;
        }
        self.activate(next)?;
        Ok(true)
    }

    /// Paint the error page over the tab whose load failed.
    ///
    /// The tab keeps its own address, so reload and the address bar still
    /// point at the destination rather than at the error page — and because
    /// internal URLs are never recorded, the error page cannot become a
    /// tab's address and send the next failure back to itself.
    pub(super) fn show_error(&mut self, id: TabId, failure: LoadFailure) -> anyhow::Result<()> {
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let attempted = self
            .registry
            .get(id)
            .map(|tab| tab.url().to_string())
            .unwrap_or_default();
        view.load_url(&error::error_url(failure, &attempted))?;
        Ok(())
    }

    /// DOM fullscreen (YouTube etc.): the page element fills the *view*,
    /// and the view only covers the area under the chrome strip — so we
    /// hide chrome and stretch the view over the whole window. Chrome
    /// restores on exit.
    pub(super) fn page_fullscreen(&mut self, id: TabId, on: bool) -> anyhow::Result<()> {
        if self.registry.active_id() != Some(id) {
            return Ok(());
        }
        if on {
            self.dismiss_overlays()?;
        }
        self.page_fullscreen = on;
        // Video fullscreen in a real browser is OS fullscreen — the window
        // goes borderless over the taskbar too, not just over the strip.
        self.window.set_fullscreen(if on {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        });
        self.chrome.set_visible(!on)?;
        if let Some(view) = self.views.get(&id) {
            let (w, h) = self.logical_size;
            let rect = if on {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                }
            } else {
                shell::layout(w, h).1
            };
            view.set_bounds(rect)?;
        }
        Ok(())
    }

    /// The event loop's heartbeat asks: which hidden tabs have earned a
    /// suspended renderer? Suspension is the gentle half of freezing —
    /// the surface lives on, but its script, timers and working set
    /// sleep. Skipped for anything still earning its keep: the active
    /// tab, a loading tab (settle events must land), a tab mid-download
    /// or serving a retry, and anything playing audio — suspending that
    /// would mute it.
    pub(super) fn suspend_idle_views(&mut self) {
        let now = self.clock.now();
        let active = self.registry.active_id();
        for (id, view) in &self.views {
            if Some(*id) == active || self.retry_tabs.contains(id) {
                continue;
            }
            let Some(tab) = self.registry.get(*id) else {
                continue;
            };
            let cold_enough = tab
                .unfocused_since()
                .is_some_and(|t| now.duration_since(t) >= SUSPEND_AFTER);
            if !cold_enough || tab.is_loading() {
                continue;
            }
            if self.downloads.iter().any(|e| {
                e.tab == *id && matches!(e.state, DownloadState::InProgress | DownloadState::Paused)
            }) {
                continue;
            }
            // Unknown counts as playing: a runtime that can't answer is
            // not evidence the tab is silent.
            if view.playing_audio().unwrap_or(true) {
                continue;
            }
            // Already asleep, or a runtime too old to say — either way
            // there is nothing to spend another suspend on.
            if view.is_suspended().unwrap_or(false) {
                continue;
            }
            if let Err(e) = view.suspend() {
                crate::report!("suspend tab {id}: {e:?}");
            }
        }
    }

    /// One window per process: a "new window" key spawns a sibling cryo
    /// process. `--incognito` puts every view of that process on an
    /// InPrivate profile — incognito is a property of the window, not a
    /// tab, same as Chrome. A bare arg hands the child an address to open.
    pub(super) fn spawn_window(incognito: bool, url: Option<&str>) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        // A spawned window keeps its address: the flag stops the child
        // from handing it to us over the single-instance pipe.
        cmd.arg("--new-window");
        if incognito {
            cmd.arg("--incognito");
        }
        if let Some(url) = url {
            cmd.arg(url);
        }
        cmd.spawn()?;
        Ok(())
    }

    pub(super) fn new_tab(&mut self) -> anyhow::Result<()> {
        if let Some(notice) = self.tab_limit_notice() {
            return notice;
        }
        let id = self.registry.create(DEFAULT_URL.to_string());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title("New tab".to_string());
        }
        self.spawn_view_for(id, NEW_TAB_URL)?;
        // A fresh tab means "type an address" — hand focus to the bar
        // like every browser does, not to the new-tab page.
        self.focus_address_bar()
    }

    /// Bring back the most recently closed tab — `activate` respawns the
    /// surface, so a tab that was closed while frozen comes back through
    /// the same restore path.
    pub(super) fn reopen_closed(&mut self) -> anyhow::Result<()> {
        match self.registry.reopen_closed() {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    /// Open `url` in a fresh tab — foreground for target=_blank /
    /// Alt+Enter, background for Alt+Shift+Enter.
    pub(super) fn spawn_tab(&mut self, url: &str, foreground: bool) -> anyhow::Result<()> {
        if let Some(notice) = self.tab_limit_notice() {
            return notice;
        }
        let id = self.registry.create(url.to_string());
        self.spawn_view_at(id, url)?;
        if foreground {
            self.activate(id)
        } else {
            self.sync_chrome()
        }
    }

    /// The strip is bounded too: frozen and discarded tabs are cheap,
    /// but unbounded strip entries are still unbounded state — a
    /// `window.open` loop or a folder's worth of dropped files must hit
    /// a wall. The notice doubles as the refusal.
    fn tab_limit_notice(&mut self) -> Option<anyhow::Result<()>> {
        if self.registry.len() < MAX_TABS {
            return None;
        }
        Some(self.notice(
            false,
            "Too many tabs",
            vec![format!(
                "cryo keeps at most {MAX_TABS} tabs — close or freeze some first."
            )],
        ))
    }

    /// Spawn the engine surface for `id`, register it, activate it.
    pub(super) fn spawn_view_for(&mut self, id: TabId, url: &str) -> anyhow::Result<()> {
        self.spawn_view_at(id, url)?;
        self.activate(id)
    }
    /// The `views`/`registry` invariant in types' place: a surface exists
    /// exactly while its tab is `Active`. `dispatch` runs this after every
    /// event in debug builds — a mutation path that forgets a side panics
    /// here, near the cause, instead of leaking the one resource this app
    /// exists to free.
    #[cfg(debug_assertions)]
    pub(super) fn assert_views_consistent(&self) {
        for id in self.views.keys() {
            debug_assert!(
                self.registry
                    .get(*id)
                    .is_some_and(|t| t.state() == TabState::Active),
                "view {id} exists but its tab is not active"
            );
        }
        for tab in self.registry.iter() {
            debug_assert!(
                tab.state() != TabState::Active || self.views.contains_key(&tab.id()),
                "active tab {} has no view",
                tab.id()
            );
        }
    }
}
