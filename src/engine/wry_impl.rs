//! WebView2 backend: adapts `wry` to the `Engine`/`ContentView` traits.
//! The only module allowed to name `wry` types.

use std::borrow::Cow;
use std::rc::Rc;

use tao::window::Window;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
use wry::http::{header::CONTENT_TYPE, Response};
use wry::{Rect as WryRect, WebView, WebViewBuilder};

use crate::engine::{ContentView, Engine, EngineError, PageResolver, Rect, ViewSpec};

/// Scheme name registered on every view. On Windows wry rewrites it to
/// `http://cryo.localhost/...`, which is also the form callers navigate to
/// directly (`load_url` does not apply the rewrite).
const INTERNAL_SCHEME: &str = "cryo";

/// Reserved path on the internal scheme: view → app command channel.
/// wry's own `with_ipc_handler` is not used — attaching it leaves the
/// document's scripts unexecuted on WebView2.
const CMD_PATH: &str = "/cmd";

pub struct WryEngine {
    window: Rc<Window>,
    resolve_page: PageResolver,
}

impl WryEngine {
    pub fn new(window: Rc<Window>, resolve_page: PageResolver) -> Self {
        Self {
            window,
            resolve_page,
        }
    }
}

fn to_wry_rect(r: Rect) -> WryRect {
    WryRect {
        position: Position::Logical(LogicalPosition::new(r.x, r.y)),
        size: Size::Logical(LogicalSize::new(r.w, r.h)),
    }
}

impl Engine for WryEngine {
    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let resolve_page = self.resolve_page;
        let on_ipc = spec.on_ipc;
        let mut builder = WebViewBuilder::new()
            .with_bounds(to_wry_rect(spec.bounds))
            .with_visible(spec.visible)
            .with_devtools(cfg!(debug_assertions))
            // Internal pages must arrive over a real `http` URL: pages loaded
            // via NavigateToString land on a `data:` URI where WebView2 never
            // runs the document's own scripts, and wry's IPC bridge drops
            // messages whose Source URI fails `http::Uri` parsing — `data:`
            // does. Both directions of chrome IPC depend on this scheme.
            .with_custom_protocol(INTERNAL_SCHEME.into(), move |_id, request| {
                let path = request.uri().path();
                if path == CMD_PATH {
                    if let Some(cb) = &on_ipc {
                        cb(String::from_utf8_lossy(request.body()).into_owned());
                    }
                    return Response::builder()
                        .status(200)
                        .body(Cow::Borrowed(&b""[..]))
                        .expect("static response is valid");
                }
                match resolve_page(path) {
                    Some(html) => Response::builder()
                        .header(CONTENT_TYPE, "text/html; charset=utf-8")
                        .body(Cow::Borrowed(html.as_bytes()))
                        .expect("static headers are valid"),
                    None => Response::builder()
                        .status(404)
                        .body(Cow::Borrowed(&b"not found"[..]))
                        .expect("static response is valid"),
                }
            });

        if let Some(url) = spec.url {
            builder = builder.with_url(&url);
        }
        if let Some(on_title) = spec.on_title_change {
            builder = builder.with_document_title_changed_handler(on_title);
        }
        if let Some(on_nav) = spec.on_navigate {
            builder = builder.with_navigation_handler(on_nav);
        }

        let view = builder
            .build_as_child(&*self.window)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        Ok(Box::new(WryView(view)))
    }
}

struct WryView(WebView);

impl ContentView for WryView {
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        self.0
            .load_url(url)
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.0
            .reload()
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn set_visible(&self, visible: bool) -> Result<(), EngineError> {
        self.0
            .set_visible(visible)
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError> {
        self.0
            .set_bounds(to_wry_rect(bounds))
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn focus(&self) -> Result<(), EngineError> {
        self.0
            .focus()
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn eval(&self, js: &str) -> Result<(), EngineError> {
        self.0
            .evaluate_script(js)
            .map_err(|e| EngineError::Script(e.to_string()))
    }

    fn url(&self) -> Option<String> {
        self.0.url().ok()
    }

    fn open_devtools(&self) {
        #[cfg(debug_assertions)]
        self.0.open_devtools();
    }
}
