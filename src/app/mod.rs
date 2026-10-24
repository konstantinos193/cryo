//! The app: single owner of event-loop state. Chrome commands, window
//! events and view callbacks all funnel through `handle_event` — nowhere
//! else does state change hands.
//!
//! `impl App` is split by responsibility, one file per slice: `keys` maps
//! input to commands, `surfaces` owns every view's birth and death (the
//! `views`/`registry` invariant lives there), `actions` holds the user
//! verbs and the chrome-command dispatch, `events` owns what the engine
//! surfaces report back, `chrome` pushes state into the chrome document,
//! `suggest` answers the omnibox, `update` runs the self-update flow,
//! `account` tracks the Google session, `library` owns bookmarks and
//! saved passwords, `transfer` runs the import/export flows. This file
//! keeps the state itself, the event entry points and the window-layout
//! plumbing.

mod account;
mod actions;
mod chrome;
mod downloads;
mod dragout;
mod events;
mod keys;
mod library;
mod suggest;
mod surfaces;
mod transfer;
mod update;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::ModifiersState;
use tao::window::Window;

use crate::bookmarks::Keeper;
use crate::engine::{Accelerator, ContentView, DownloadUpdate, Engine, LoadFailure, Rect};
use crate::persist::Recorder;
use crate::shell;
use crate::shell::chrome::{normalize_nav_input, ChromeCommand};
use crate::shell::pages::NEW_TAB_URL;
use crate::shell::search::SearchEngine;
use crate::tabs::{Clock, TabId, TabRegistry};
use crate::update::{Outcome, Update};
use crate::vault::Vault;

/// Domain URL recorded for a fresh tab. The view actually loads
/// `NEW_TAB_URL`; `about:blank` stays the tab's logical address so the
/// address bar starts empty and internal URLs never escape into it.
pub(crate) const DEFAULT_URL: &str = "about:blank";

/// How often the event loop wakes to give cold background tabs a
/// suspended renderer (`suspend_idle_views` does the deciding).
pub(crate) const IDLE_TICK: Duration = Duration::from_secs(15);

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
        crate::report!("event loop closed — event dropped");
    }
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
    /// A second process handed its launch address over the instance pipe
    /// (external link while already running) — a new tab here, and the
    /// window it landed in gets raised.
    ExternalOpen(String),
    /// A page element entered/left DOM fullscreen (video fullscreen).
    PageFullscreen(TabId, bool),
    /// A navigation ended with no document, or the renderer died under one.
    LoadFailed(TabId, LoadFailure),
    /// A view's download started, progressed, or settled — the shelf.
    Download(TabId, DownloadUpdate),
    DownloadFolder(TabId, bool),
    /// OS files landed on a view that reports drops (the chrome strip —
    /// a drag there opens each file in a tab).
    FilesDropped(Vec<std::path::PathBuf>),
    /// A worker finished reading an import file or profile — drafts
    /// in, or the reason there are none. Boxed: the error is the big
    /// side of this `Result` and every event pays for the larger one.
    Imported(Result<crate::transfer::Bundle, Box<crate::transfer::TransferError>>),
    /// A worker finished looking for importable browser profiles.
    ImportSources(Vec<crate::transfer::Profile>),
    /// A background update check finished — started at launch and by the
    /// menu's "Check for updates".
    UpdateChecked(Outcome),
    /// A worker answered who the profile's Google session belongs to —
    /// `None` reads as signed out.
    GoogleSession(Option<crate::account::Session>),
}

pub struct App {
    window: Rc<Window>,
    engine: Rc<dyn Engine>,
    /// The clock freeze times are stamped through (AGENTS §6) — the
    /// composition root injects it so budget policy and tests control
    /// what "now" means.
    clock: Rc<dyn Clock>,
    proxy: EventLoopProxy<AppEvent>,
    registry: TabRegistry,
    /// Engine surfaces keyed by tab — a map parallel to `registry` by
    /// necessity, not choice: a `Tab` can't hold a view without engine
    /// types leaking into `tabs` (§3). The two stay in sync through one
    /// rule, enforced only in `surfaces`: an entry exists exactly while
    /// its tab is `Active` — `spawn_view_at` inserts, `drop_view`
    /// removes for freeze, discard and close alike.
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
    /// Where non-URL omnibox input goes. The pick lives in the shell
    /// origin's localStorage (the settings page writes it); the chrome
    /// document reports it on its `/cmd` wire and `note_search_engine`
    /// keeps this copy — the storage area itself never reaches the app.
    search_engine: SearchEngine,
    /// Typed inputs and visited URLs — the omnibox's memory. `None` in
    /// incognito windows: forgetting is their whole point.
    recorder: Option<Recorder>,
    /// Kept pages: the ★, the bookmarks bar, the library panel. An
    /// incognito window gets a session-only one — the same verbs work,
    /// nothing outlives the window.
    bookmarks: Keeper,
    /// Saved sign-ins, sealed at rest. Session-only in incognito, for
    /// the same reason.
    vault: Vault,
    /// This window forgets everything when it closes. Imports refuse,
    /// and the library panel says why.
    incognito: bool,
    /// Which library pane is on screen, if any — what `push_library`
    /// repaints after a change. `None` means the panel is shut and
    /// there is nothing to push.
    library_pane: Option<String>,
    /// Browser profiles the last discovery found. The `import_profile`
    /// command indexes this list, so a path never travels through the
    /// chrome document.
    /// `None` until the first look finishes — "not searched yet" and
    /// "none installed" read the same in a list and must not.
    import_sources: Option<Vec<crate::transfer::Profile>>,
    /// The download shelf — see `downloads.rs`. Newest at the back.
    downloads: Vec<downloads::Entry>,
    /// Shelf key counter — chrome commands carry it back to us.
    download_seq: u64,
    /// Tabs spawned only to host a retried download — the view must live
    /// for the transfer's duration, so the tab closes itself when the
    /// download settles. A set, not a map: it carries no tab data, only
    /// which ids owe their existence to a retry.
    retry_tabs: HashSet<TabId>,
    /// Downloaded, checksum-verified installer waiting for a restart.
    /// Surfaced as the ⋮ badge and the "Restart to update" menu item.
    pending_update: Option<Update>,
    /// A manual check was requested — the next `UpdateChecked` outcome
    /// gets a dialog instead of staying silent.
    update_prompt: bool,
    /// The Google session the account button last painted — `None`
    /// shows the "Sign in" pill.
    google: Option<crate::account::Session>,
    /// The one in-flight session check and whether a second request
    /// folded into it (see `account::SessionCheck`).
    google_check: account::SessionCheck,
    /// The window has begun shutting down. WebView2 rejects every call
    /// after controller disposal starts, while tao may still deliver
    /// queued resize and view events.
    exiting: bool,
}

/// What a window remembers, as `lib.rs` wires it: visits, kept pages
/// and saved sign-ins. A normal window gets the stores under
/// `%LOCALAPPDATA%\cryo`; an incognito one gets their session-only
/// counterparts, so every verb still works and nothing outlives the
/// window. Passed down from the composition root — the app never picks
/// a path or a protector itself (AGENTS §5).
pub struct Profile {
    pub recorder: Option<Recorder>,
    pub bookmarks: Keeper,
    pub vault: Vault,
    pub incognito: bool,
}

impl Profile {
    /// Everything forgotten at close — incognito, and the test seam.
    pub fn session_only() -> Self {
        Self {
            recorder: None,
            bookmarks: Keeper::session_only(),
            vault: Vault::session_only(),
            incognito: true,
        }
    }
}

impl App {
    pub fn new(
        window: Rc<Window>,
        engine: Rc<dyn Engine>,
        clock: Rc<dyn Clock>,
        proxy: EventLoopProxy<AppEvent>,
        open_url: Option<String>,
        profile: Profile,
    ) -> anyhow::Result<Self> {
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        let logical_size = (logical.width, logical.height);
        let (chrome_rect, content_rect) = shell::layout(logical_size.0, logical_size.1);

        // Content spawns before chrome so the chrome bar sits above it in
        // the z-order of sibling container windows.
        let mut registry = TabRegistry::new();
        // A spawned window can be handed an address to open (Ctrl+Shift+
        // Enter, shell args). Blank or absent means the new-tab page.
        // The stored engine only arrives with the chrome document's
        // `ready`, after this address is already resolved — a launch-arg
        // search phrase is the one input that always takes the default.
        let (domain, load) = match open_url
            .as_deref()
            .map(|u| normalize_nav_input(u, &SearchEngine::DEFAULT))
        {
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
            clock,
            proxy,
            registry,
            views,
            chrome,
            logical_size,
            modifiers: ModifiersState::empty(),
            chrome_focused: false,
            page_fullscreen: false,
            overlay_open: false,
            search_engine: SearchEngine::DEFAULT,
            recorder: profile.recorder,
            bookmarks: profile.bookmarks,
            vault: profile.vault,
            incognito: profile.incognito,
            library_pane: None,
            import_sources: None,
            downloads: Vec::new(),
            download_seq: 0,
            retry_tabs: HashSet::new(),
            pending_update: None,
            update_prompt: false,
            google: None,
            google_check: account::SessionCheck::Idle,
            exiting: false,
        })
    }

    pub fn handle_event(&mut self, event: Event<'_, AppEvent>, control_flow: &mut ControlFlow) {
        if self.exiting {
            return;
        }
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => self.request_exit(control_flow),
                WindowEvent::Resized(size) => self.resize(size.width, size.height),
                WindowEvent::ModifiersChanged(m) => self.modifiers = m,
                WindowEvent::KeyboardInput { event, .. } => self.on_key(&event, control_flow),
                _ => {}
            },
            Event::UserEvent(e) => self.dispatch(e, control_flow),
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => self.suspend_idle_views(),
            _ => {}
        }
    }

    /// Mark the controller graph unavailable before asking tao to exit.
    /// Queued callbacks can then drain without touching disposed WebViews.
    pub(super) fn request_exit(&mut self, control_flow: &mut ControlFlow) {
        self.exiting = true;
        *control_flow = ControlFlow::Exit;
    }

    fn dispatch(&mut self, event: AppEvent, control_flow: &mut ControlFlow) {
        let result = match event {
            AppEvent::Accelerator(acc) => self.on_accelerator(acc, control_flow),
            AppEvent::Chrome(cmd) => self.on_chrome(cmd, control_flow),
            AppEvent::TitleChanged(id, title) => self.on_title_changed(id, title),
            AppEvent::Navigated(id, url) => self.on_navigated(id, url),
            AppEvent::FaviconChanged(id, png) => self.on_favicon_changed(id, png),
            AppEvent::LoadChanged(id, loading) => self.on_load_changed(id, loading),
            AppEvent::OpenInNewTab(url) => self.spawn_tab(&url, true),
            AppEvent::ExternalOpen(url) => {
                // The user clicked a link expecting to watch it land —
                // the window may be minimized or behind other windows.
                self.window.set_minimized(false);
                self.window.set_focus();
                self.spawn_tab(&normalize_nav_input(&url, &self.search_engine), true)
            }
            AppEvent::PageFullscreen(id, on) => self.page_fullscreen(id, on),
            AppEvent::LoadFailed(id, failure) => self.show_error(id, failure),
            AppEvent::Download(id, update) => self.download_update(id, update, control_flow),
            AppEvent::DownloadFolder(id, choose) => self.download_folder_setting(id, choose),
            AppEvent::FilesDropped(paths) => self.files_dropped(&paths),
            AppEvent::Imported(outcome) => self.on_imported(outcome),
            AppEvent::ImportSources(sources) => self.on_import_sources(sources),
            AppEvent::UpdateChecked(outcome) => self.on_update(outcome, control_flow),
            AppEvent::GoogleSession(session) => self.on_google_session(session),
        };
        #[cfg(debug_assertions)]
        self.assert_views_consistent();
        if let Err(e) = result {
            crate::report!("{e:?}");
        }
    }

    /// Remember a visit or a typed input — the one door into `persist`.
    /// No recorder (incognito), no memory.
    pub(super) fn remember(&mut self, text: &str) {
        if let Some(rec) = &mut self.recorder {
            if let Err(e) = rec.record(text, None, unix_now()) {
                crate::report!("{e:?}");
            }
        }
    }

    /// Attach a title to a remembered URL — `TitleChanged` events arrive
    /// after the visit was recorded.
    fn note_title(&mut self, url: &str, title: &str) {
        if let Some(rec) = &mut self.recorder {
            if let Err(e) = rec.note_title(url, title) {
                crate::report!("{e:?}");
            }
        }
    }

    /// Attach a site's icon to a remembered URL — `FaviconChanged` events
    /// arrive after the visit was recorded, like titles.
    fn note_favicon(&mut self, url: &str, data_url: &str) {
        if let Some(rec) = &mut self.recorder {
            if let Err(e) = rec.note_favicon(url, data_url) {
                crate::report!("{e:?}");
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
            crate::report!("{e:?}");
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
                crate::report!("{e:?}");
            }
        }
    }
}
