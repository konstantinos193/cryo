//! The app: single owner of event-loop state. Chrome commands, window
//! events and view callbacks all funnel through `handle_event` — nowhere
//! else does state change hands.
//!
//! `impl App` is split by responsibility, one file per slice: `keys` maps
//! input to commands, `surfaces` owns every view's birth and death,
//! `actions` holds the user verbs and the chrome-command dispatch,
//! `events` owns what the engine surfaces report back, `chrome` pushes
//! state into the chrome document, `suggest` answers the omnibox,
//! `update` runs the self-update flow, `account` tracks the Google
//! session, `library` owns bookmarks and saved passwords, `transfer`
//! runs the import/export flows, and `budget` executes decisions from
//! the memory context. This file keeps the state itself, the event
//! entry points and the window-layout plumbing.

mod account;
mod actions;
mod autofill;
mod budget;
mod chrome;
mod downloads;
mod dragout;
mod events;
#[cfg(all(test, target_os = "windows"))]
mod freeze_probe;
mod keys;
mod library;
mod permissions;
mod resources;
mod suggest;
mod surfaces;
mod transfer;
mod update;
mod window;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopProxy};
use tao::keyboard::ModifiersState;
use tao::window::Window;

use crate::bookmarks::Keeper;
use crate::engine::{
    Accelerator, ContentView, DownloadUpdate, Engine, LoadFailure, PageMessage, Rect,
};
use crate::persist::Recorder;
use crate::shell;
use crate::shell::chrome::{normalize_nav_input, ChromeCommand};
use crate::shell::pages::{self, NEW_TAB_URL};
use crate::shell::search::SearchEngine;
use crate::tabs::{Clock, TabId, TabRegistry, ViewKey};
use crate::update::{Outcome, Update};
use crate::vault::Vault;
use resources::TabResources;

/// Domain URL recorded for a fresh tab. The view actually loads
/// `NEW_TAB_URL`; `about:blank` stays the tab's logical address so the
/// address bar starts empty and internal URLs never escape into it.
pub(crate) const DEFAULT_URL: &str = "about:blank";

/// How often the event loop wakes to give cold background tabs a
/// suspended renderer (`suspend_idle_views` does the deciding).
pub(crate) const IDLE_TICK: Duration = Duration::from_secs(5);

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

/// What one content surface reports about itself. Always travels inside
/// `AppEvent::View` with the `ViewKey` the surface was spawned under —
/// `dispatch` drops the report unless that key is still the tab's live
/// one, so no handler below ever sees a dead surface's callback.
pub enum ViewEvent {
    OpenInNewTab(String),
    Accelerator(Accelerator),
    Permission(crate::engine::PermissionRequest),
    TitleChanged(String),
    /// The page's icon changed: PNG bytes, `None` when the document has
    /// none. `None` still clears — a page without an icon drops it.
    FaviconChanged(Option<Vec<u8>>),
    Navigated(String),
    /// A top-level navigation started or settled — the spinner's data
    /// (see `LoadChangeHandler` for what counts).
    LoadChanged(bool),
    /// A page element entered/left DOM fullscreen (video fullscreen).
    PageFullscreen(bool),
    /// A navigation ended with no document, or the renderer died under one.
    LoadFailed(LoadFailure),
    /// A download started, progressed, or settled — the shelf.
    Download(DownloadUpdate),
    DownloadFolder(bool),
    /// A trusted message from the main document.
    PageMessage(PageMessage),
}

/// Events that reach the loop from view callbacks (`AppEvent`) — chrome
/// IPC, title changes, navigations, keys the engine yielded to us.
pub enum AppEvent {
    Chrome(ChromeCommand),
    /// A content surface reported something — see `ViewEvent`.
    View(ViewKey, ViewEvent),
    Accelerator(Accelerator),
    /// A second process handed its launch address over the instance pipe
    /// (external link while already running) — a new tab here, and the
    /// window it landed in gets raised.
    ExternalOpen(String),
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
    /// A store's background write failed — what was kept in memory did
    /// not reach the disk. Carries the store's own account of why.
    StoreFailed(String),
    MemoryMeasured(u64, crate::engine::accounting::MemorySnapshot),
    /// A backend that drives its own loop inside ours asked for a turn —
    /// Servo's `EventLoopWaker` posts these; wry never does.
    EngineWake,
}

/// The registry as the app holds it: every tab owns its engine surface.
type Tabs = TabRegistry<Box<dyn ContentView>, TabResources>;

pub struct App {
    window: Rc<Window>,
    engine: Rc<dyn Engine>,
    memory_budget: crate::memory::BudgetEnforcer,
    /// The clock freeze times are stamped through (AGENTS §6) — the
    /// composition root injects it so budget policy and tests control
    /// what "now" means.
    clock: Rc<dyn Clock>,
    proxy: EventLoopProxy<AppEvent>,
    /// Every tab, each owning its engine surface while it is live — one
    /// collection, so no map beside it can drift out of step.
    registry: Tabs,
    chrome: Box<dyn ContentView>,
    logical_size: (f64, f64),
    /// Inner size while not maximized — a maximized close reopens at
    /// this size, not at the screen's.
    normal_size: (f64, f64),
    /// File the window's placement is written to at close — `None` in
    /// incognito, which leaves nothing behind.
    window_state: Option<PathBuf>,
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
    /// Explicit consent decisions, scoped to this window and web origin.
    permission_grants: permissions::Grants,
    permission_seq: u64,
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
    /// Settled download records whose tabs have closed. Live transfers
    /// belong to their tab resources; this archive cannot control a view.
    download_archive: Vec<downloads::Entry>,
    /// Shelf key counter — chrome commands carry it back to us.
    download_seq: u64,
    /// Downloaded, checksum-verified installer waiting for a restart.
    /// Surfaced as the ⋮ badge and the "Restart to update" menu item.
    pending_update: Option<Update>,
    /// A manual check was requested — the next `UpdateChecked` outcome
    /// gets a dialog instead of staying silent.
    update_prompt: bool,
    /// The Google session the account button last painted — `None`
    /// shows the "Sign in" pill.
    google: Option<crate::account::Session>,
    /// Which account in `google.accounts` the window is browsing as —
    /// the index the dropdown's pick set. `accounts` is jar order, so
    /// `0` is Google's own default; a nonzero pick makes navigations to
    /// Google properties carry `authuser=N` (`account::with_authuser`)
    /// and the button wear that account's face.
    active_account: usize,
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
        window_state: Option<PathBuf>,
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
        let first = registry.create_with(domain, |key| {
            Self::spawn_content_view(&*engine, &proxy, content_rect, key, &load, true)
        })?;
        if let Some(tab) = registry.get_mut(first) {
            tab.set_title("New tab".to_string());
        }
        registry.set_active(first)?;

        let chrome = Self::spawn_chrome(&*engine, &proxy, chrome_rect)?;

        let memory_budget = crate::memory::BudgetEnforcer::new(Default::default(), clock.now());
        Ok(Self {
            memory_budget,
            window,
            engine,
            clock,
            proxy,
            registry,
            chrome,
            logical_size,
            normal_size: logical_size,
            window_state,
            modifiers: ModifiersState::empty(),
            chrome_focused: false,
            page_fullscreen: false,
            overlay_open: false,
            search_engine: SearchEngine::DEFAULT,
            recorder: profile.recorder,
            bookmarks: profile.bookmarks,
            vault: profile.vault,
            permission_grants: permissions::Grants::default(),
            permission_seq: 0,
            incognito: profile.incognito,
            library_pane: None,
            import_sources: None,
            download_archive: Vec::new(),
            download_seq: 0,
            pending_update: None,
            update_prompt: false,
            google: None,
            active_account: 0,
            google_check: account::SessionCheck::Idle,
            exiting: false,
        })
    }

    pub fn handle_event(&mut self, event: Event<'_, AppEvent>, control_flow: &mut ControlFlow) {
        if self.exiting {
            return;
        }
        // Every dispatch is also a turn for a backend with its own loop —
        // cheap for wry (no-op), what keeps Servo's pages moving.
        self.engine.spin_event_loop();
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => self.request_exit(control_flow),
                WindowEvent::Resized(size) => self.resize(size.width, size.height),
                WindowEvent::ModifiersChanged(m) => self.modifiers = m,
                WindowEvent::KeyboardInput { event, .. } => self.on_key(&event, control_flow),
                _ => {}
            },
            Event::UserEvent(e) => self.dispatch(e, control_flow),
            _ => {}
        }
    }

    /// Mark the controller graph unavailable before asking tao to exit.
    /// Queued callbacks can then drain without touching disposed WebViews.
    pub(super) fn request_exit(&mut self, control_flow: &mut ControlFlow) {
        self.exiting = true;
        *control_flow = ControlFlow::Exit;
        self.note_placement();
    }

    /// Write where the window ended up so the next launch reopens there.
    /// The remembered size is the un-maximized one — storing a maximized
    /// window's screen-sized bounds would reopen huge on a smaller
    /// display.
    fn note_placement(&mut self) {
        let Some(path) = &self.window_state else {
            return;
        };
        let scale = self.window.scale_factor();
        let (x, y) = match self.window.outer_position() {
            Ok(p) => (Some(p.x as f64 / scale), Some(p.y as f64 / scale)),
            Err(_) => (None, None),
        };
        let maximized = self.window.is_maximized();
        let (width, height) = if maximized {
            self.normal_size
        } else {
            self.logical_size
        };
        let placement = shell::geometry::Placement {
            width,
            height,
            x,
            y,
            maximized,
        };
        if let Err(e) = shell::geometry::save(path, &placement) {
            crate::report!("window placement not saved: {e}");
        }
    }

    fn dispatch(&mut self, event: AppEvent, control_flow: &mut ControlFlow) {
        let result = match event {
            AppEvent::Accelerator(acc) => self.on_accelerator(acc, control_flow),
            AppEvent::Chrome(cmd) => self.on_chrome(cmd, control_flow),
            AppEvent::View(key, event) => self.on_view_event(key, event, control_flow),
            AppEvent::ExternalOpen(url) => {
                // The user clicked a link expecting to watch it land —
                // the window may be minimized or behind other windows.
                self.window.set_minimized(false);
                self.window.set_focus();
                match normalize_nav_input(&url, &self.search_engine) {
                    url if pages::is_internal_url(&url) => Ok(()),
                    url => self.spawn_tab(&url, true),
                }
            }
            AppEvent::MemoryMeasured(sequence, snapshot) => {
                self.on_memory_measured(sequence, snapshot)
            }
            AppEvent::EngineWake => {
                self.engine.spin_event_loop();
                Ok(())
            }
            AppEvent::FilesDropped(paths) => self.files_dropped(&paths),
            AppEvent::Imported(outcome) => self.on_imported(outcome),
            AppEvent::ImportSources(sources) => self.on_import_sources(sources),
            AppEvent::UpdateChecked(outcome) => self.on_update(outcome, control_flow),
            AppEvent::GoogleSession(session) => self.on_google_session(session),
            AppEvent::StoreFailed(failure) => {
                crate::report!("store write failed: {failure}");
                self.notice(
                    false,
                    "Changes have not been saved",
                    vec![
                        failure,
                        "Your changes remain in memory. cryo will retry the disk write.".into(),
                    ],
                )
            }
        };
        if let Err(e) = result {
            crate::report!("{e:?}");
        }
    }

    /// The one gate every surface report passes: a key that is no longer
    /// its tab's live surface — frozen, discarded, closed, or restored
    /// since under a newer generation — is a dead view talking, and its
    /// report is dropped before any handler can write it into the tab.
    fn on_view_event(
        &mut self,
        key: ViewKey,
        event: ViewEvent,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        if self.registry.live(key).is_none() {
            return Ok(());
        }
        let id = key.tab();
        match event {
            ViewEvent::Permission(request) => self.on_permission(key, request),
            ViewEvent::OpenInNewTab(url) if pages::is_internal_url(&url) => Ok(()),
            ViewEvent::OpenInNewTab(url) => self.spawn_tab(&url, true),
            ViewEvent::Accelerator(acc) => self.on_accelerator(acc, control_flow),
            ViewEvent::TitleChanged(title) => self.on_title_changed(id, title),
            ViewEvent::Navigated(url) => self.on_navigated(id, url),
            ViewEvent::FaviconChanged(png) => self.on_favicon_changed(id, png),
            ViewEvent::LoadChanged(loading) => self.on_load_changed(id, loading),
            ViewEvent::PageFullscreen(on) => self.page_fullscreen(id, on),
            ViewEvent::LoadFailed(failure) => self.show_error(id, failure),
            ViewEvent::Download(update) => self.download_update(key, update, control_flow),
            ViewEvent::DownloadFolder(choose) => self.download_folder_setting(id, choose),
            ViewEvent::PageMessage(message) => self.on_page_message(id, message),
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
        self.registry.active_id().and_then(|id| self.view_of(id))
    }

    /// `id`'s live engine surface — `None` while frozen or discarded.
    pub(super) fn view_of(&self, id: TabId) -> Option<&dyn ContentView> {
        self.registry.get(id).and_then(|t| t.view()).map(|v| &**v)
    }

    fn resize(&mut self, width_px: u32, height_px: u32) {
        let scale = self.window.scale_factor();
        let w = width_px as f64 / scale;
        let h = height_px as f64 / scale;
        self.logical_size = (w, h);
        if !self.window.is_maximized() {
            self.normal_size = (w, h);
        }
        let (_, content_rect) = shell::layout(w, h);
        if let Err(e) = self.chrome.set_bounds(self.chrome_rect()) {
            crate::report!("{e:?}");
        }
        // A DOM-fullscreen view keeps covering the whole window on resize.
        let active = self.registry.active_id();
        for (tab, view) in self.registry.views() {
            let rect = if self.page_fullscreen && Some(tab.id()) == active {
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
