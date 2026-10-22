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
    fn set_visible(&self, visible: bool) -> Result<(), EngineError>;
    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError>;
    fn focus(&self) -> Result<(), EngineError>;
    fn eval(&self, js: &str) -> Result<(), EngineError>;
    fn url(&self) -> Option<String>;
    /// Toggle the engine's inspector. No-op where unsupported.
    fn open_devtools(&self);
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
    #[error("script evaluation failed: {0}")]
    Script(String),
}
