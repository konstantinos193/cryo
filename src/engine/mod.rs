//! Engine seam: the traits the shell needs from a web engine, expressed in
//! our own types. `wry` types appear only in `wry_impl`; `fake` exists
//! for tests. A Servo backend slots in here later without touching policy.

pub mod accounting;
pub mod fake;
#[cfg(feature = "servo-engine")]
pub mod servo_impl;
pub mod wry_impl;

/// Position and size in logical pixels, relative to the parent window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Resolves a path on the internal `cryo` protocol (`/chrome`, `/newtab`)
/// to static HTML. Implemented by the shell, injected into the engine so
/// plumbing never names shell pages.
pub type PageResolver = fn(&str) -> Option<&'static str>;

/// A surface's immutable authority over internal documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewAccess {
    #[default]
    Web,
    Content,
    Chrome,
}

/// View → app messages. Transport is engine-defined (on wry: a POST to
/// `/cmd` on the internal protocol — see `wry_impl`), so Send + Sync.
pub type IpcHandler = Box<dyn Fn(String) + Send + Sync>;
pub type PageMessageHandler = Box<dyn Fn(PageMessage)>;
pub type TitleHandler = Box<dyn Fn(String)>;
/// The document's icon changed — PNG bytes of the new one, `None` when
/// the page has none (which clears whatever icon it had before).
pub type FaviconHandler = Box<dyn Fn(Option<Vec<u8>>)>;
/// Return `true` to allow the navigation, `false` to block it.
pub type NavigateHandler = Box<dyn Fn(String) -> bool>;
/// Called before the engine consumes a key. Return `true` to swallow it,
/// `false` to let the engine (or the page) have it.
pub type AcceleratorHandler = Box<dyn Fn(Accelerator) -> bool>;
/// Page asked for a new window (target=_blank, middle-click,
/// `window.open`). We deny the popup and open a tab ourselves — the URL
/// is the only thing the app needs. Registering this handler is also
/// what activates the middle-click shim: the engine can't tell the
/// gesture apart from a plain navigation, so the backend rewrites
/// middle-clicks on anchors into `window.open` to reach this callback.
pub type NewWindowHandler = Box<dyn Fn(String)>;
/// Called when a navigation ended without a document. See [`LoadFailure`] for
/// what counts — cancelled loads and credential prompts do not. `Send + Sync`
/// because the backend may report from a worker: a download's abort can
/// arrive *before* `DownloadStarting`, so the report is held briefly on a
/// thread while the engine decides which it is.
pub type LoadFailureHandler = Box<dyn Fn(LoadFailure) + Send + Sync>;
/// Page element entered or left DOM fullscreen (video fullscreen). The bool
/// is the engine's `ContainsFullScreenElement` state after the change.
pub type FullscreenHandler = Box<dyn Fn(bool)>;
/// A download started or made progress — first call announces the file,
/// later calls carry byte counts and the terminal state. Called on the
/// event loop thread.
pub type DownloadHandler = Box<dyn Fn(DownloadUpdate)>;
/// An OS file drag entered, moved over, or dropped onto the view. The
/// engine reports; the app decides what a drop means. Called on the
/// event loop thread.
pub type DropHandler = Box<dyn Fn(DropEvent)>;
/// The view's top-level navigation started (`true`) or the in-flight one
/// settled — finished, failed, or stopped (`false`). The loading
/// spinner's data; sub-frame navigations don't count. Called on the
/// event loop thread.
pub type LoadChangeHandler = Box<dyn Fn(bool)>;
/// A deferred request on the view thread. The app answers its id through
/// `ContentView::answer_permission`; views without a callback deny requests.
pub type PermissionHandler = Box<dyn Fn(PermissionRequest)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PermissionId(pub u64);

pub struct PermissionRequest {
    pub id: PermissionId,
    pub origin: String,
    pub kind: PermissionKind,
}

/// A document-to-host message with the engine-observed source URL.
pub struct PageMessage {
    pub source: String,
    pub body: String,
}

/// A capability a page requested through a browser API, in our own
/// vocabulary. Backends map their platform permission kinds onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PermissionKind {
    Microphone,
    Camera,
    Geolocation,
    Notifications,
    ClipboardRead,
    /// Anything we don't name — autoplay, sensors, local fonts, ...
    Other,
}

/// The app's explicit answer to a deferred permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// A key in our own vocabulary — the engine maps platform virtual-key
/// codes so policy never sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceleratorKey {
    /// Letter or digit, lowercased.
    Character(char),
    /// Function key: `F(5)` is F5.
    F(u8),
    Tab,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    Escape,
}

/// A key pressed while a view had focus, reported before the engine
/// processes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accelerator {
    pub key: AcceleratorKey,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Everything needed to create a view. Handlers are called on the event
/// loop thread.
#[derive(Default)]
pub struct ViewSpec {
    pub access: ViewAccess,
    pub url: Option<String>,
    pub bounds: Rect,
    pub visible: bool,
    /// Unpainted pixels let whatever is behind the view show through —
    /// required for a view that temporarily grows over its siblings (the
    /// chrome strip covering the window while its menu is open).
    pub transparent: bool,
    /// What the engine paints before the first document composites —
    /// the pre-paint flash guard (RGBA). `None` falls back to the
    /// engine's default, which is white on WebView2.
    pub background: Option<(u8, u8, u8, u8)>,
    pub on_ipc: Option<IpcHandler>,
    /// Script injected before a content document loads. Backends run it
    /// in the main frame only, keeping embedded third-party frames out.
    pub page_script: Option<&'static str>,
    /// Messages from `window.ipc.postMessage` in a content document.
    pub on_page_message: Option<PageMessageHandler>,
    pub on_title_change: Option<TitleHandler>,
    pub on_favicon: Option<FaviconHandler>,
    pub on_navigate: Option<NavigateHandler>,
    pub on_accelerator: Option<AcceleratorHandler>,
    pub on_new_window: Option<NewWindowHandler>,
    pub on_load_failure: Option<LoadFailureHandler>,
    pub on_fullscreen: Option<FullscreenHandler>,
    pub on_permission: Option<PermissionHandler>,
    /// When set the app owns the download UI — the engine suppresses its
    /// own dialog and reports every file's progress here. `None` leaves
    /// the engine's default download dialog running.
    pub on_download: Option<DownloadHandler>,
    /// When set the view reports OS file drags crossing it (enter, hover,
    /// drop, leave) instead of feeding them to the document. `None`
    /// leaves the engine's default: drops go to the page's own DOM
    /// handlers.
    pub on_drop: Option<DropHandler>,
    /// Navigation start/settle signal — drives the loading spinner.
    pub on_load_change: Option<LoadChangeHandler>,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        }
    }
}

impl ViewSpec {
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            ..Self::default()
        }
    }
}

/// A live web surface. All methods are called on the event loop thread.
pub trait ContentView {
    fn answer_permission(
        &self,
        _id: PermissionId,
        _decision: PermissionDecision,
    ) -> Result<(), EngineError> {
        Ok(())
    }
    fn load_url(&self, url: &str) -> Result<(), EngineError>;
    fn reload(&self) -> Result<(), EngineError>;
    /// Reload bypassing the HTTP cache (Ctrl+F5).
    fn hard_reload(&self) -> Result<(), EngineError>;
    /// Step session history back, no-op when there is none.
    fn go_back(&self) -> Result<(), EngineError>;
    fn go_forward(&self) -> Result<(), EngineError>;
    /// Whether session history has anywhere to step — the chrome greys
    /// out the arrows when it doesn't.
    fn can_go_back(&self) -> Result<bool, EngineError>;
    fn can_go_forward(&self) -> Result<bool, EngineError>;
    /// Abort an in-flight navigation or load.
    fn stop(&self) -> Result<(), EngineError>;
    fn set_visible(&self, visible: bool) -> Result<(), EngineError>;
    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError>;
    /// Raise the view above its siblings. Needed when a view overlaps
    /// others that were spawned later — spawn order is z-order.
    fn bring_to_front(&self) -> Result<(), EngineError>;
    fn focus(&self) -> Result<(), EngineError>;
    fn eval(&self, js: &str) -> Result<(), EngineError>;
    fn url(&self) -> Option<String>;
    /// Toggle the engine's inspector. No-op where unsupported.
    fn open_devtools(&self);
    /// Per-view audio mute state.
    fn muted(&self) -> Result<bool, EngineError>;
    fn set_muted(&self, muted: bool) -> Result<(), EngineError>;
    /// Page zoom factor, 1.0 = 100%. Persisted by the engine per view.
    fn zoom_factor(&self) -> Result<f64, EngineError>;
    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError>;
    /// Render the visible page to PNG bytes. The caller decides where the
    /// bytes go — the engine doesn't touch the filesystem.
    fn capture_png(&self) -> Result<Vec<u8>, EngineError>;
    /// Show the engine's native save-as UI for the current page.
    fn save_as(&self) -> Result<(), EngineError>;
    fn download_folder(&self) -> Result<std::path::PathBuf, EngineError> {
        Err(EngineError::View(
            "Download folder settings are unavailable on this platform".into(),
        ))
    }
    fn set_download_folder(&self, _path: &std::path::Path) -> Result<(), EngineError> {
        Err(EngineError::View(
            "Download folder settings are unavailable on this platform".into(),
        ))
    }
    /// Every cookie this view's profile would send for `uri` — HttpOnly
    /// included. Profile-scoped like `clear_browsing_data`: any view
    /// answers for the whole jar.
    fn cookies(&self, uri: &str) -> Result<Vec<Cookie>, EngineError>;
    /// Wipe every kind of browsing data for this view's profile —
    /// profile-scoped, so calling it on any view clears them all.
    fn clear_browsing_data(&self) -> Result<(), EngineError>;
    /// Cancel an in-flight download this view started. Finished or
    /// unknown ids are a no-op.
    fn cancel_download(&self, id: DownloadId) -> Result<(), EngineError>;
    /// Hold an in-flight download — it stays in the shelf, resumable.
    /// Unknown ids are a no-op; an engine that can't pause answers with
    /// an error and the row keeps moving.
    fn pause_download(&self, id: DownloadId) -> Result<(), EngineError>;
    /// Restart a paused download. Unknown ids are a no-op.
    fn resume_download(&self, id: DownloadId) -> Result<(), EngineError>;
    /// Ask the engine to sleep the view's renderer without dropping it —
    /// the freeze machinery's lighter cousin for tabs merely hidden, not
    /// frozen. A no-op where the engine can't suspend.
    fn suspend(&self) -> Result<(), EngineError>;
    /// Wake a suspended view. A no-op on a view that never slept or on
    /// an engine that can't suspend.
    fn resume(&self) -> Result<(), EngineError>;
    /// Whether the view's renderer is currently suspended. `false` where
    /// the engine can't say — the safe answer, since callers only ever
    /// suspend.
    fn is_suspended(&self) -> Result<bool, EngineError>;
    /// Whether the page is playing audio. `None` when the engine can't
    /// say — callers must then assume it might be, before suspending.
    fn playing_audio(&self) -> Option<bool>;
}

/// One cookie from the profile's jar, in our own vocabulary — name and
/// value only, which is all a `Cookie:` header needs. Backends hand
/// them back; what they get sent to is the caller's business.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
}

/// A download's identity within its view — a per-view sequence, so the
/// app pairs it with the tab id before putting it on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DownloadId(pub u64);

/// Where a download stands, in our own vocabulary. `Cancelled` is the
/// user stopping it — an interrupt reason on the wire, but a distinct
/// state here because the UI paints them differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DownloadState {
    InProgress,
    /// The user held it mid-flight — an interrupt reason on the wire,
    /// but the transfer is still alive and resumable.
    Paused,
    Done,
    /// The engine stopped it mid-flight — network drop, policy block,
    /// disk full, shutdown.
    Interrupted,
    /// The user asked to stop it.
    Cancelled,
}

/// One report about a file the engine is saving. `total` is `None`
/// until the server declares a length (some never do).
#[derive(Clone, Debug)]
pub struct DownloadUpdate {
    pub id: DownloadId,
    /// Suggested filename, deduplicated against the download folder.
    pub name: String,
    /// Where the bytes are going.
    pub path: String,
    /// The address the bytes come from — a retry re-navigates to it.
    /// Empty when the engine won't say.
    pub uri: String,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

/// An OS file drag crossing a view, in our own vocabulary. `paths` are
/// the files being dragged; `position` is view-relative in the engine's
/// pixel space.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DropEvent {
    /// The drag entered the view carrying `paths`.
    Entered {
        paths: Vec<std::path::PathBuf>,
        position: (i32, i32),
    },
    /// The drag is hovering — `position` tracks the cursor.
    Over { position: (i32, i32) },
    /// `paths` landed on the view.
    Dropped {
        paths: Vec<std::path::PathBuf>,
        position: (i32, i32),
    },
    /// The drag left the view or was cancelled.
    Left,
}

/// An HTTP response status. A newtype so a status cannot be mistaken for any
/// other number crossing the seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpStatus(u16);

impl HttpStatus {
    pub const fn new(code: u16) -> Self {
        Self(code)
    }

    pub const fn code(self) -> u16 {
        self.0
    }

    /// 4xx and 5xx — the range where the server may have sent no document
    /// worth showing.
    pub const fn is_error(self) -> bool {
        self.0 >= 400
    }
}

/// Why a navigation ended without a document, in our own vocabulary. Backends
/// map their platform status codes onto these, so nothing outside `engine`
/// ever sees a WebView2 or Chromium error constant.
///
/// Cancelled loads and credential prompts are deliberately absent: the first
/// is the user changing their mind, the second is a dialog, and neither is a
/// failure the shell should paint a page for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadFailure {
    /// No route to the network at all.
    Offline,
    /// DNS had no answer for the host.
    HostNotFound,
    /// The host took the connection but never finished answering.
    TimedOut,
    /// Refused outright, or aborted or reset mid-flight.
    ConnectionFailed,
    /// TLS could not be trusted: wrong name, expired, revoked or malformed.
    CertificateRejected,
    /// The redirect chain never terminated.
    RedirectLoop,
    /// The server answered with something that is not a document.
    MalformedResponse,
    /// The server answered with an error status and no body of its own.
    Http(HttpStatus),
    /// The renderer process died under the page.
    RenderProcessGone,
    /// The backend could not classify the failure.
    Unknown,
}

/// Factory for views. One engine per window.
pub trait Engine {
    /// Give the engine's own loop a turn. Backends that drive their own
    /// loop through the platform's (WebView2 inside wry) leave this a
    /// no-op; Servo calls `Servo::spin_event_loop` from here whenever the
    /// app dispatches an event or a waker-posted `EngineWake` arrives.
    fn spin_event_loop(&self) {}

    fn measure_memory(&self, _complete: accounting::MemoryCallback) -> Result<(), EngineError> {
        Err(EngineError::ReadStats(
            "Memory accounting is unavailable on this backend".into(),
        ))
    }

    /// What the engine says it has loaded, keyed by extension id. The
    /// catalogue knows what is on disk; only the engine knows what is
    /// actually running and whether it is switched on, so the pane asks for
    /// both and shows where they disagree.
    ///
    /// Backends that load no extensions answer with an empty list rather than
    /// an error: nothing loaded is a true answer, not a failure.
    fn loaded_extensions(&self) -> Result<Vec<LoadedExtension>, EngineError> {
        Ok(Vec::new())
    }

    /// Switch one on or off without uninstalling it.
    fn set_extension_enabled(&self, _id: &str, _enabled: bool) -> Result<(), EngineError> {
        Err(EngineError::Unsupported(
            "this backend cannot enable or disable extensions",
        ))
    }

    /// Uninstall through the engine, which is what actually unregisters an
    /// extension from the profile. Deleting the folder makes the engine drop
    /// it as a side effect; this asks.
    fn remove_extension(&self, _id: &str) -> Result<(), EngineError> {
        Err(EngineError::Unsupported(
            "this backend cannot remove extensions",
        ))
    }

    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError>;
}

/// One extension as the *engine* sees it, which is not the same question as
/// what the catalogue has on disk: a folder can be present while the engine
/// never loaded it, and an extension can be loaded but switched off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedExtension {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum EngineError {
    #[error("memory accounting failed: {0}")]
    ReadStats(String),
    #[error("view creation failed: {0}")]
    Spawn(String),
    #[error("navigation failed: {0}")]
    Navigate(String),
    #[error("layout operation failed: {0}")]
    Layout(String),
    #[error("view operation failed: {0}")]
    View(String),
    #[error("script evaluation failed: {0}")]
    Script(String),
    #[error("{0}")]
    Unsupported(&'static str),
}
