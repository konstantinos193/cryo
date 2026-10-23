//! The app: single owner of event-loop state. Chrome commands, window
//! events and view callbacks all funnel through `handle_event` — nowhere
//! else does state change hands.
//!
//! `impl App` is split by responsibility, one file per slice: `keys` maps
//! input to commands, `surfaces` owns every view's birth and death (the
//! `views`/`registry` invariant lives there), `actions` holds the user
//! verbs and the chrome-command dispatch, `update` runs the self-update
//! flow. This file keeps the state itself, the event entry points and
//! the window-layout plumbing.

mod actions;
mod downloads;
mod keys;
mod surfaces;
mod update;

use std::collections::HashMap;
use std::rc::Rc;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::ModifiersState;
use tao::window::Window;

use crate::engine::{Accelerator, ContentView, DownloadUpdate, Engine, LoadFailure, Rect};
use crate::persist::Recorder;
use crate::shell;
use crate::shell::chrome::{
    favicon_data_url, normalize_nav_input, ChromeCommand, ChromeState, ChromeTab,
};
use crate::shell::pages::{self, NEW_TAB_URL};
use crate::tabs::{TabId, TabRegistry, TabState};
use crate::update::{Outcome, Update};

/// Domain URL recorded for a fresh tab. The view actually loads
/// `NEW_TAB_URL`; `about:blank` stays the tab's logical address so the
/// address bar starts empty and internal URLs never escape into it.
pub(crate) const DEFAULT_URL: &str = "about:blank";

/// The address to hand the engine for a tab's domain URL. `about:blank`
/// is a never-painting document in WebView2 — the internal new-tab page
/// stands in for it everywhere a view (re)loads a tab's address.
pub(crate) fn content_url_for(domain_url: &str) -> &str {
    if domain_url == DEFAULT_URL {
        NEW_TAB_URL
    } else {
        domain_url
    }
}

/// Hand `event` to the event loop. A send only fails once the loop is
/// gone — the event has nowhere left to go, so the drop is reported,
/// not silently `let _ =`'d and not `?`'d into a teardown path.
pub(crate) fn post(proxy: &EventLoopProxy<AppEvent>, event: AppEvent) {
    if proxy.send_event(event).is_err() {
        eprintln!("cryo: event loop closed — event dropped");
    }
}

/// Where errors with no user-facing surface go: the one reporting seam
/// until a real logging facade lands (AGENTS.md §10).
pub(crate) fn report(err: impl std::fmt::Debug) {
    eprintln!("cryo: {err:?}");
}

/// Wall-clock stamp for history entries — the caller stamps so `persist`
/// keeps no ambient clock of its own.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Events that reach the loop from view callbacks (`AppEvent`) — chrome
/// IPC, title changes, navigations, keys the engine yielded to us.
pub enum AppEvent {
    Chrome(ChromeCommand),
    TitleChanged(TabId, String),
    /// The page's icon changed: PNG bytes, `None` when the document has
    /// none. `None` still clears — a page without an icon drops it.
    FaviconChanged(TabId, Option<Vec<u8>>),
    Navigated(TabId, String),
    /// A view's top-level navigation started or settled — the spinner's
    /// data (see `LoadChangeHandler` for what counts).
    LoadChanged(TabId, bool),
    Accelerator(Accelerator),
    /// Page asked for a new window (target=_blank, ctrl/shift/middle-click,
    /// window.open) — we open a tab instead.
    OpenInNewTab(String),
    /// A page element entered/left DOM fullscreen (video fullscreen).
    PageFullscreen(TabId, bool),
    /// A navigation ended with no document, or the renderer died under one.
    LoadFailed(TabId, LoadFailure),
    /// A view's download started, progressed, or settled — the shelf.
    Download(TabId, DownloadUpdate),
    /// A background update check finished — started at launch and by the
    /// menu's "Check for updates".
    UpdateChecked(Outcome),
}

pub struct App {
    window: Rc<Window>,
    engine: Rc<dyn Engine>,
    proxy: EventLoopProxy<AppEvent>,
    registry: TabRegistry,
    /// Engine surfaces keyed by tab — a map parallel to `registry` by
    /// necessity, not choice: a `Tab` can't hold a view without engine
    /// types leaking into `tabs` (§3). The two stay in sync through one
    /// rule, enforced only in `surfaces`: an entry exists exactly while
    /// its tab is `Active` — spawn/restore insert, freeze/discard/close
    /// remove.
    views: HashMap<TabId, Box<dyn ContentView>>,
    chrome: Box<dyn ContentView>,
    logical_size: (f64, f64),
    modifiers: ModifiersState,
    /// Where keystrokes go right now: chrome bar or the active page.
    /// Approximate — a page click isn't reported back — but F6's cycle
    /// only needs our own focus calls to be consistent.
    chrome_focused: bool,
    /// The active view is in DOM fullscreen — it covers the chrome strip.
    page_fullscreen: bool,
    /// A chrome overlay (⋮ menu, omnibox dropdown) is open — the chrome
    /// view is stretched over the whole window so the popup can paint
    /// above the content views.
    overlay_open: bool,
    /// Typed inputs and visited URLs — the omnibox's memory. `None` in
    /// incognito windows: forgetting is their whole point.
    recorder: Option<Recorder>,
    /// The download shelf — see `downloads.rs`. Newest at the back.
    downloads: Vec<downloads::Entry>,
    /// Shelf key counter — chrome commands carry it back to us.
    download_seq: u64,
    /// Downloaded, checksum-verified installer waiting for a restart.
    /// Surfaced as the ⋮ badge and the "Restart to update" menu item.
    pending_update: Option<Update>,
    /// A manual check was requested — the next `UpdateChecked` outcome
    /// gets a dialog instead of staying silent.
    update_prompt: bool,
}

impl App {
    pub fn new(
        window: Rc<Window>,
        engine: Rc<dyn Engine>,
        proxy: EventLoopProxy<AppEvent>,
        open_url: Option<String>,
        recorder: Option<Recorder>,
    ) -> anyhow::Result<Self> {
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        let logical_size = (logical.width, logical.height);
        let (chrome_rect, content_rect) = shell::layout(logical_size.0, logical_size.1);

        // Content spawns before chrome so the chrome bar sits above it in
        // the z-order of sibling container windows.
        let mut registry = TabRegistry::new();
        // A spawned window can be handed an address to open (Ctrl+Shift+
        // Enter, shell args). Blank or absent means the new-tab page.
        let (domain, load) = match open_url.as_deref().map(normalize_nav_input) {
            Some(u) if u != DEFAULT_URL => (u.clone(), u),
            _ => (DEFAULT_URL.to_string(), NEW_TAB_URL.to_string()),
        };
        let first = registry.create(domain);
        if let Some(tab) = registry.get_mut(first) {
            tab.set_title("New tab".to_string());
        }
        let first_view =
            Self::spawn_content_view(&*engine, &proxy, content_rect, first, &load, true)?;
        let mut views = HashMap::new();
        views.insert(first, first_view);
        registry.set_active(first)?;

        let chrome = Self::spawn_chrome(&*engine, &proxy, chrome_rect)?;

        Ok(Self {
            window,
            engine,
            proxy,
            registry,
            views,
            chrome,
            logical_size,
            modifiers: ModifiersState::empty(),
            chrome_focused: false,
            page_fullscreen: false,
            overlay_open: false,
            recorder,
            downloads: Vec::new(),
            download_seq: 0,
            pending_update: None,
            update_prompt: false,
        })
    }

    pub fn handle_event(&mut self, event: Event<'_, AppEvent>, control_flow: &mut ControlFlow) {
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                WindowEvent::Resized(size) => self.resize(size.width, size.height),
                WindowEvent::ModifiersChanged(m) => self.modifiers = m,
                WindowEvent::KeyboardInput { event, .. } => self.on_key(&event, control_flow),
                _ => {}
            },
            Event::UserEvent(e) => self.dispatch(e, control_flow),
            _ => {}
        }
    }

    fn dispatch(&mut self, event: AppEvent, control_flow: &mut ControlFlow) {
        let result = match event {
            AppEvent::Accelerator(acc) => self.on_accelerator(acc, control_flow),
            AppEvent::Chrome(cmd) => self.on_chrome(cmd, control_flow),
            AppEvent::TitleChanged(id, title) => {
                let url = self.registry.get(id).map(|t| t.url().to_string());
                if let Some(tab) = self.registry.get_mut(id) {
                    tab.set_title(title.clone());
                }
                // Titles arrive after the URL was already recorded — this
                // attaches it so suggestions can match on it. Internal
                // pages no-op: they were never recorded.
                if let Some(url) = url {
                    self.note_title(&url, &title);
                }
                self.sync_chrome()
            }
            AppEvent::Navigated(id, url) => {
                // Internal pages are shell surfaces, not tab destinations —
                // never record them as a tab's address.
                if pages::is_internal_url(&url) {
                    return;
                }
                // The visit counts whether the load works out or not —
                // real omniboxes remember typed destinations too.
                self.remember(&url);
                if let Some(tab) = self.registry.get_mut(id) {
                    // A view that already froze can still have events in
                    // flight from before the drop — they must not move the
                    // frozen tab's address away from its snapshot's.
                    if tab.state() == TabState::Active {
                        tab.navigate(url);
                    }
                }
                if self.registry.active_id() == Some(id) {
                    self.sync_chrome()
                } else {
                    Ok(())
                }
            }
            AppEvent::FaviconChanged(id, png) => {
                if let Some(tab) = self.registry.get_mut(id) {
                    // Same in-flight rule as Navigated: a view already
                    // frozen can still emit its last icon — don't write
                    // it over the frozen tab's.
                    if tab.state() == TabState::Active {
                        tab.set_favicon(png);
                    }
                }
                self.sync_chrome()
            }
            AppEvent::LoadChanged(id, loading) => {
                if let Some(tab) = self.registry.get_mut(id) {
                    // Same in-flight rule as Navigated — a dropped view's
                    // last events must not write into a frozen tab.
                    tab.set_loading(loading);
                }
                self.sync_chrome()
            }
            AppEvent::OpenInNewTab(url) => self.spawn_tab(&url, true),
            AppEvent::PageFullscreen(id, on) => self.page_fullscreen(id, on),
            AppEvent::LoadFailed(id, failure) => self.show_error(id, failure),
            AppEvent::Download(id, update) => self.download_update(id, update),
            AppEvent::UpdateChecked(outcome) => self.on_update(outcome, control_flow),
        };
        if let Err(e) = result {
            report(e);
        }
    }

    /// Chrome bounds for the current overlay state: the strip normally,
    /// the whole window while an overlay is open.
    fn chrome_rect(&self) -> Rect {
        let (w, h) = self.logical_size;
        if self.overlay_open {
            Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }
        } else {
            shell::layout(w, h).0
        }
    }

    /// Chrome overlays (the ⋮ menu, the omnibox dropdown) are part of the
    /// chrome document, but a view can't paint outside its own bounds —
    /// while one is open the chrome view is raised above the content
    /// views and stretched over the whole window. Its background is
    /// transparent, so only the strip and the popup paint; the page shows
    /// through and any click outside lands on the chrome document, which
    /// dismisses the overlay.
    pub(super) fn set_overlay_open(&mut self, open: bool) -> anyhow::Result<()> {
        if self.overlay_open == open {
            return Ok(());
        }
        self.overlay_open = open;
        self.chrome.set_bounds(self.chrome_rect())?;
        if open {
            self.chrome.bring_to_front()?;
        }
        Ok(())
    }

    /// App-side actions that replace what's on screen must close overlays —
    /// the document can't see them, so it's synced through `__cryo_overlay`
    /// before the bounds come back down.
    pub(super) fn dismiss_overlays(&mut self) -> anyhow::Result<()> {
        if !self.overlay_open {
            return Ok(());
        }
        self.chrome
            .eval("window.__cryo_overlay && window.__cryo_overlay(false)")?;
        self.set_overlay_open(false)
    }

    /// Remember a visit or a typed input — the one door into `persist`.
    /// No recorder (incognito), no memory.
    pub(super) fn remember(&mut self, text: &str) {
        if let Some(rec) = &mut self.recorder {
            if let Err(e) = rec.record(text, None, unix_now()) {
                report(e);
            }
        }
    }

    /// Attach a title to a remembered URL — `TitleChanged` events arrive
    /// after the visit was recorded.
    fn note_title(&mut self, url: &str, title: &str) {
        if let Some(rec) = &mut self.recorder {
            if let Err(e) = rec.note_title(url, title) {
                report(e);
            }
        }
    }

    /// The engine surface of the selected tab, if one is live.
    pub(super) fn active_view(&self) -> Option<&dyn ContentView> {
        self.registry
            .active_id()
            .and_then(|id| self.views.get(&id).map(|v| &**v))
    }

    fn resize(&mut self, width_px: u32, height_px: u32) {
        let scale = self.window.scale_factor();
        let w = width_px as f64 / scale;
        let h = height_px as f64 / scale;
        self.logical_size = (w, h);
        let (_, content_rect) = shell::layout(w, h);
        if let Err(e) = self.chrome.set_bounds(self.chrome_rect()) {
            report(e);
        }
        // A DOM-fullscreen view keeps covering the whole window on resize.
        let active = self.registry.active_id();
        for (id, view) in &self.views {
            let rect = if self.page_fullscreen && Some(*id) == active {
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                }
            } else {
                content_rect
            };
            if let Err(e) = view.set_bounds(rect) {
                report(e);
            }
        }
    }

    pub(super) fn sync_chrome(&self) -> anyhow::Result<()> {
        // History depth only exists while a view is live — a frozen or
        // discarded tab's arrows stay grey until it restores.
        let (can_back, can_fwd) = match self.active_view() {
            Some(v) => (v.can_go_back()?, v.can_go_forward()?),
            None => (false, false),
        };
        let state = ChromeState {
            tabs: self
                .registry
                .iter()
                .map(|t| ChromeTab {
                    id: t.id().get(),
                    title: t.title().to_string(),
                    active: self.registry.active_id() == Some(t.id()),
                    state: t.state().as_str(),
                    favicon: t.favicon().map(favicon_data_url),
                    loading: t.is_loading(),
                })
                .collect(),
            active_url: self
                .registry
                .active()
                .map(|t| t.url().to_string())
                .filter(|u| u != "about:blank")
                .unwrap_or_default(),
            can_back,
            can_fwd,
            update_version: self.pending_update.as_ref().map(|u| u.version.to_string()),
            downloads: self.download_rows(),
        };
        let json = serde_json::to_string(&state)?;
        self.chrome.eval(&format!("window.__cryo_update({json})"))?;
        Ok(())
    }
}
