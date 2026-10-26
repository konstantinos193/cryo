//! Engine surfaces: spawning the chrome and content views, and the
//! freeze/discard/restore lifecycle. A live tab owns its `ContentView`
//! inside the registry; every surface is spawned under the `ViewKey` its
//! callbacks report with, so a dropped surface's reports die at the gate
//! in `App::on_view_event`.

use std::sync::Mutex;

use tao::event_loop::EventLoopProxy;

use crate::engine::{ContentView, DropEvent, Engine, LoadFailure, Rect, ViewSpec};
use crate::shell::chrome::ChromeCommand;
use crate::shell::pages::{self, CHROME_URL, NEW_TAB_URL};
use crate::shell::{self, error};
use crate::tabs::{TabId, TabSnapshot, ViewKey};

use super::{content_url_for, post, App, AppEvent, ViewEvent, DEFAULT_URL};

/// Hard bound on strip entries, live or frozen — see `tab_limit_notice`.
const MAX_TABS: usize = 200;

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
            access: crate::engine::ViewAccess::Chrome,
            bounds,
            visible: true,
            transparent: true,
            background: Some(shell::CHROME_BACKGROUND),
            on_accelerator: Some(Self::accelerator_forwarder(proxy, None)),
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
            // The strip is the one surface that may load the chrome
            // document — and the only one with a command channel.
            ..ViewSpec::url(CHROME_URL)
        })?;
        Ok(chrome)
    }

    /// Spawn a content surface whose every report carries `key` — the
    /// incarnation it belongs to, so the app can tell a dead surface's
    /// late callbacks from the live one's.
    pub(super) fn spawn_content_view(
        engine: &dyn Engine,
        proxy: &EventLoopProxy<AppEvent>,
        content_rect: Rect,
        key: ViewKey,
        url: &str,
        visible: bool,
    ) -> anyhow::Result<Box<dyn ContentView>> {
        let report = |proxy: &EventLoopProxy<AppEvent>| {
            let proxy = proxy.clone();
            move |event: ViewEvent| post(&proxy, AppEvent::View(key, event))
        };
        let page = report(proxy);
        let title = report(proxy);
        let nav = report(proxy);
        let favicon = report(proxy);
        let fullscreen = report(proxy);
        let download = report(proxy);
        let load = report(proxy);
        let fail = report(proxy);
        let permission = report(proxy);
        let nw_proxy = proxy.clone();
        let view = engine.spawn(ViewSpec {
            access: crate::engine::ViewAccess::Content,
            bounds: content_rect,
            visible,
            background: Some(shell::CONTENT_BACKGROUND),
            page_script: Some(super::autofill::PAGE_SCRIPT),
            // New-tab, settings and error pages — never the chrome
            // document, and no `/cmd`: content views carry no `on_ipc`.
            on_page_message: Some(Box::new(move |m| page(ViewEvent::PageMessage(m)))),
            on_title_change: Some(Box::new(move |t| title(ViewEvent::TitleChanged(t)))),
            on_navigate: Some(Box::new(move |uri| {
                if let Some(choose) = pages::download_folder_request(&uri) {
                    nav(ViewEvent::DownloadFolder(choose));
                    return false;
                }
                nav(ViewEvent::Navigated(uri));
                true
            })),
            on_favicon: Some(Box::new(move |png| favicon(ViewEvent::FaviconChanged(png)))),
            on_accelerator: Some(Self::accelerator_forwarder(proxy, Some(key))),
            on_new_window: Some(Box::new(move |url| {
                post(&nw_proxy, AppEvent::View(key, ViewEvent::OpenInNewTab(url)));
            })),
            on_fullscreen: Some(Box::new(move |on| {
                fullscreen(ViewEvent::PageFullscreen(on))
            })),
            on_load_failure: Some(Box::new(move |failure| {
                fail(ViewEvent::LoadFailed(failure))
            })),
            on_download: Some(Box::new(move |update| {
                download(ViewEvent::Download(update))
            })),
            on_load_change: Some(Box::new(move |loading| {
                load(ViewEvent::LoadChanged(loading))
            })),
            // The engine defers the request until trusted chrome collects
            // consent for this origin and this surface incarnation.
            on_permission: Some(Box::new(move |request| {
                permission(ViewEvent::Permission(request))
            })),
            ..ViewSpec::url(url)
        })?;
        Ok(view)
    }

    /// Open a tab at `domain` around a fresh surface loading `load`.
    /// Always spawned hidden — `activate` owns the switch. A view created
    /// visible paints the engine's white default for a frame before its
    /// background colour applies; one shown only once it exists never
    /// does.
    pub(super) fn open_tab(&mut self, domain: String, load: &str) -> anyhow::Result<TabId> {
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let id = self.registry.create_with(domain, |key| {
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, key, load, false)
        })?;
        self.adopt_view(id)?;
        Ok(id)
    }

    /// Clone `id`'s address into a fresh tab right after it, around its
    /// own surface. The new tab comes up hidden, like `open_tab`'s.
    pub(super) fn duplicate_tab(&mut self, id: TabId) -> anyhow::Result<TabId> {
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let dup = self.registry.duplicate_with(id, |key, url| {
            let load = content_url_for(url);
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, key, load, false)
        })?;
        self.adopt_view(dup)?;
        Ok(dup)
    }

    /// Destroy a surface the registry just handed back, and everything
    /// hanging off that incarnation: its shelf entries and any capability
    /// request it still had open. Freeze, discard and close all funnel
    /// here.
    pub(super) fn drop_view(&mut self, key: ViewKey, view: Box<dyn ContentView>) {
        debug_assert!(
            self.registry.live(key).is_none(),
            "dropped surface is no longer owned by a live tab"
        );
        drop(view);
    }

    /// Give a view-less tab — frozen, discarded, or one coming back from
    /// the closed stack — a fresh surface under a new generation. If the
    /// spawn fails the tab stays as it was, so the next activation can
    /// retry.
    pub(super) fn restore_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(tab) = self.registry.get(id) else {
            return Ok(());
        };
        let snap = tab.snapshot();
        let url = snap.map(|s| s.url.as_str()).unwrap_or_else(|| tab.url());
        let load = content_url_for(url).to_string();
        // Snapshot extras exist only for frozen tabs; discarded starts clean.
        let zoom = snap.map(|s| s.zoom).unwrap_or(1.0);
        let muted = snap.map(|s| s.muted).unwrap_or(false);
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        self.registry.restore_with(id, |key| {
            let view = Self::spawn_content_view(
                &*self.engine,
                &self.proxy,
                content_rect,
                key,
                &load,
                false,
            )?;
            if zoom != 1.0 {
                view.set_zoom_factor(zoom)?;
            }
            if muted {
                view.set_muted(true)?;
            }
            anyhow::Ok(view)
        })?;
        self.adopt_view(id)
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
    /// has anything worth snapshotting — a tab without one is already
    /// frozen or discarded.
    pub(super) fn freeze_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        let (Some(view), Some(key)) = (tab.view(), tab.view_key()) else {
            return Ok(());
        };
        let snapshot = TabSnapshot {
            url: tab.url().to_string(),
            frozen_at: self.clock.now(),
            zoom: view.zoom_factor().unwrap_or(1.0),
            muted: view.muted().unwrap_or(false),
        };
        let view = tab.freeze(snapshot)?;
        self.drop_view(key, view);
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
        let key = tab.view_key();
        if let (Some(key), Some(view)) = (key, tab.discard()?) {
            self.drop_view(key, view);
        }
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
        let Some(tab) = self.registry.get(id) else {
            return Ok(());
        };
        if let Some(view) = tab.view() {
            // A Web Store install fails as an unresolvable host, which is
            // true and tells the reader nothing. Name what actually happened.
            let url = match crate::extensions::web_store::install_request(tab.url()) {
                Some(_) => error::web_store_url(tab.url()),
                None => error::error_url(failure, tab.url()),
            };
            view.load_url(&url)?;
        }
        Ok(())
    }

    pub(super) fn new_tab(&mut self) -> anyhow::Result<()> {
        if let Some(notice) = self.tab_limit_notice() {
            return notice;
        }
        let id = self.open_tab(DEFAULT_URL.to_string(), NEW_TAB_URL)?;
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title("New tab".to_string());
        }
        self.activate(id)?;
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
        let id = self.open_tab(url.to_string(), url)?;
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
}
