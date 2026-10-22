//! Engine seam: the traits the shell needs from a web engine, expressed in
//! our own types. `wry`/`tao` types appear only in `wry_impl`; `fake` exists
//! for tests. A Servo backend slots in here later without touching policy.

pub mod fake;
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

/// View → app messages. Transport is engine-defined (on wry: a POST to
/// `/cmd` on the internal protocol — see `wry_impl`), so Send + Sync.
pub type IpcHandler = Box<dyn Fn(String) + Send + Sync>;
pub type TitleHandler = Box<dyn Fn(String)>;
/// Return `true` to allow the navigation, `false` to block it.
pub type NavigateHandler = Box<dyn Fn(String) -> bool>;
/// Called before the engine consumes a key. Return `true` to swallow it,
/// `false` to let the engine (or the page) have it.
pub type AcceleratorHandler = Box<dyn Fn(Accelerator) -> bool>;
/// Page asked for a new window (target=_blank, ctrl/middle/shift+click,
/// `window.open`). We deny the popup and open a tab ourselves — the URL is
/// the only thing the app needs.
pub type NewWindowHandler = Box<dyn Fn(String)>;
/// Called when a navigation ended without a document. See [`LoadFailure`] for
/// what counts — cancelled loads and credential prompts do not.
pub type LoadFailureHandler = Box<dyn Fn(LoadFailure)>;
/// Page element entered or left DOM fullscreen (video fullscreen). The bool
/// is the engine's `ContainsFullScreenElement` state after the change.
pub type FullscreenHandler = Box<dyn Fn(bool)>;

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
    pub url: Option<String>,
    pub bounds: Rect,
    pub visible: bool,
    pub on_ipc: Option<IpcHandler>,
    pub on_title_change: Option<TitleHandler>,
    pub on_navigate: Option<NavigateHandler>,
    pub on_accelerator: Option<AcceleratorHandler>,
    pub on_new_window: Option<NewWindowHandler>,
    pub on_load_failure: Option<LoadFailureHandler>,
    pub on_fullscreen: Option<FullscreenHandler>,
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
    fn load_url(&self, url: &str) -> Result<(), EngineError>;
    fn reload(&self) -> Result<(), EngineError>;
    /// Reload bypassing the HTTP cache (Ctrl+F5).
    fn hard_reload(&self) -> Result<(), EngineError>;
    /// Step session history back, no-op when there is none.
    fn go_back(&self) -> Result<(), EngineError>;
    fn go_forward(&self) -> Result<(), EngineError>;
    /// Abort an in-flight navigation or load.
    fn stop(&self) -> Result<(), EngineError>;
    fn set_visible(&self, visible: bool) -> Result<(), EngineError>;
    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError>;
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
    /// Wipe every kind of browsing data for this view's profile —
    /// profile-scoped, so calling it on any view clears them all.
    fn clear_browsing_data(&self) -> Result<(), EngineError>;
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
    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError>;
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum EngineError {
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
}
