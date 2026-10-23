//! Engine surfaces: spawning the chrome and content views, and the
//! freeze/discard/restore lifecycle that maps `Tab` states onto live
//! `ContentView`s in `self.views`.

use std::sync::Mutex;

use tao::event_loop::EventLoopProxy;
use tao::window::Fullscreen;

use crate::engine::{
    ContentView, Engine, LoadFailure, PermissionDecision, PermissionKind, Rect, ViewSpec,
};
use crate::shell::chrome::ChromeCommand;
use crate::shell::pages::CHROME_URL;
use crate::shell::{self, error};
use crate::tabs::{TabId, TabSnapshot, TabState};

use super::{content_url_for, post, App, AppEvent};

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
        let chrome = engine.spawn(ViewSpec {
            bounds,
            visible: true,
            transparent: true,
            background: Some(shell::CHROME_BACKGROUND),
            on_accelerator: Some(Self::accelerator_forwarder(proxy)),
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
        let view = engine.spawn(ViewSpec {
            bounds: content_rect,
            visible,
            background: Some(shell::CONTENT_BACKGROUND),
            on_title_change: Some(Box::new(move |t| {
                post(&title_proxy, AppEvent::TitleChanged(id, t));
            })),
            on_navigate: Some(Box::new(move |uri| {
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
    /// registry entry and the view in step. `visible` only decides the
    /// initial visibility — `activate` owns the switch.
    pub(super) fn spawn_view_at(
        &mut self,
        id: TabId,
        url: &str,
        visible: bool,
    ) -> anyhow::Result<()> {
        let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
        let view =
            Self::spawn_content_view(&*self.engine, &self.proxy, content_rect, id, url, visible)?;
        self.views.insert(id, view);
        Ok(())
    }

    /// Give a view-less tab — frozen, discarded, or one coming back from
    /// the closed stack — a fresh surface, then flip it back to live.
    /// Spawned invisible: `activate` owns the show/hide ordering.
    pub(super) fn restore_view(&mut self, id: TabId) -> anyhow::Result<()> {
        let Some(tab) = self.registry.get(id) else {
            return Ok(());
        };
        let snap = tab.snapshot();
        let url = snap.map(|s| s.url.as_str()).unwrap_or_else(|| tab.url());
        let load = content_url_for(url).to_string();
        let zoom = snap.map(|s| s.zoom).unwrap_or(1.0);
        let muted = snap.map(|s| s.muted).unwrap_or(false);
        self.spawn_view_at(id, &load, false)?;
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
        // Only a live surface has anything worth snapshotting — a view
        // missing here means the tab is already frozen or discarded.
        let Some(view) = self.views.get(&id) else {
            return Ok(());
        };
        let zoom = view.zoom_factor().unwrap_or(1.0);
        let muted = view.muted().unwrap_or(false);
        let Some(tab) = self.registry.get_mut(id) else {
            return Ok(());
        };
        tab.freeze(TabSnapshot {
            url: tab.url().to_string(),
            frozen_at: std::time::Instant::now(),
            zoom,
            muted,
        })?;
        drop(self.views.remove(&id));
        self.sync_chrome()
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
        drop(self.views.remove(&id));
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
}
