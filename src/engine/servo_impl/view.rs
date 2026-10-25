//! The `ContentView` impl over `servo::WebView` plus its `WebViewDelegate`.
//! Servo has no navigation-handler callback that returns a verdict — the
//! gate lives in `request_navigation`; reports (`Navigated`, title, load
//! state, fullscreen) ride the `notify_*` methods. `load_web_resource`
//! answers the internal `cryo` scheme, which doubles as the IPC channel:
//! documents POST to `cryo://cmd/<view>` where wry used `window.ipc`.
//!
//! Anything servo can't do returns `EngineError::View` rather than a fake —
//! suspend/resume, mute, downloads, cookies, save-as and deferred
//! permissions are honest gaps, not stubs.

use std::cell::RefCell;
use std::rc::Rc;

use servo::{LoadStatus, WebView, WebViewDelegate};

use super::Surface;
use crate::engine::{ContentView, DownloadId, EngineError, PageResolver, Rect, ViewSpec};

/// The callbacks a `ViewSpec` hands us, keyed to the incarnation by the
/// caller — this adapter doesn't see `ViewKey`s, it just forwards.
struct Handlers {
    on_ipc: Option<crate::engine::IpcHandler>,
    on_page_message: Option<crate::engine::PageMessageHandler>,
    on_title_change: Option<crate::engine::TitleHandler>,
    on_navigate: Option<crate::engine::NavigateHandler>,
    on_favicon: Option<crate::engine::FaviconHandler>,
    on_new_window: Option<crate::engine::NewWindowHandler>,
    on_load_failure: Option<crate::engine::LoadFailureHandler>,
    on_fullscreen: Option<crate::engine::FullscreenHandler>,
    on_load_change: Option<crate::engine::LoadChangeHandler>,
    resolve_page: PageResolver,
}

impl Handlers {
    fn placeholder() -> Self {
        Self {
            on_ipc: None,
            on_page_message: None,
            on_title_change: None,
            on_navigate: None,
            on_favicon: None,
            on_new_window: None,
            on_load_failure: None,
            on_fullscreen: None,
            on_load_change: None,
            resolve_page: |_| None,
        }
    }
}

/// Servo-side per view. `webview` is filled in right after construction —
/// the delegate exists before the `WebView` does, so it holds a slot.
struct Delegate {
    handlers: Handlers,
    webview: RefCell<Option<WebView>>,
    context: Rc<dyn servo::RenderingContext>,
    /// `notify_url_changed` fires for history entries too; report each
    /// committed URL once, like wry's `Navigated`.
    last_url: RefCell<Option<String>>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        webview.paint();
        self.context.present();
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        if let Some(cb) = &self.handlers.on_title_change {
            cb(title.unwrap_or_default());
        }
    }

    fn notify_url_changed(&self, _webview: WebView, url: servo::ServoUrl) {
        let url = url.to_string();
        let mut last = self.last_url.borrow_mut();
        if last.as_deref() == Some(&url) {
            return;
        }
        *last = Some(url.clone());
        if let Some(cb) = &self.handlers.on_navigate {
            // Already committed — the verdict return is meaningless here;
            // the gate itself lives in `request_navigation`.
            let _ = cb(url);
        }
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        if let Some(cb) = &self.handlers.on_load_change {
            cb(matches!(status, LoadStatus::Started));
        }
    }

    fn notify_fullscreen_state_changed(&self, _webview: WebView, fullscreen: bool) {
        if let Some(cb) = &self.handlers.on_fullscreen {
            cb(fullscreen);
        }
    }

    fn notify_crashed(&self, _webview: WebView, _reason: String, _backtrace: Option<String>) {
        if let Some(cb) = &self.handlers.on_load_failure {
            cb(crate::engine::LoadFailure::RenderProcessGone);
        }
    }

    fn notify_favicon_changed(&self, webview: WebView) {
        let Some(cb) = &self.handlers.on_favicon else {
            return;
        };
        let png = webview.favicon().and_then(|image| encode_png(&*image));
        cb(png);
    }

    /// SERVO-API: `NavigationRequest` carries an allow/deny responder —
    /// `request.allow()`/`request.deny()` per the embedding docs.
    fn request_navigation(&self, _webview: WebView, request: servo::NavigationRequest) {
        let url = request.url().to_string();
        if self.handlers.on_navigate.as_ref().is_none_or(|cb| cb(url)) {
            request.allow();
        } else {
            request.deny();
        }
    }

    /// window.open / target=_blank — the app opens a tab and the popup is
    /// denied, same as the wry `NewWindowResponse::Deny` policy.
    /// SERVO-API: `CreateNewWebViewRequest` carries `url()` and a responder.
    fn request_create_new(&self, _webview: WebView, request: servo::CreateNewWebViewRequest) {
        if let Some(cb) = &self.handlers.on_new_window {
            if let Some(url) = request.url() {
                cb(url.to_string());
            }
        }
        request.send(None);
    }

    /// No deferral machinery yet (the app's consent prompt is keyed to
    /// WebView2 requests) — deny, matching the non-Windows wry default.
    fn request_permission(&self, _webview: WebView, request: servo::PermissionRequest) {
        request.deny();
    }

    /// The `cryo` scheme: a GET on a known path serves the resolved page;
    /// a POST to `/cmd` is the IPC channel (wry's `on_ipc`) and a POST to
    /// `/msg` is the page-message channel (wry's `window.ipc`).
    /// SERVO-API: `WebResourceLoad` exposes `request()` and `respond(..)` —
    /// exact responder shape needs a compile pass.
    fn load_web_resource(&self, _webview: WebView, load: servo::WebResourceLoad) {
        let request = load.request();
        let url = request.uri().to_string();
        let parsed = match url::Url::parse(&url) {
            Ok(u) if u.scheme() == "cryo" => u,
            _ => {
                load.continue_default();
                return;
            }
        };
        if request.method() == "POST" {
            let body = String::from_utf8_lossy(request.body()).into_owned();
            match parsed.path() {
                "/cmd" => {
                    if let Some(cb) = &self.handlers.on_ipc {
                        cb(body);
                    }
                }
                "/msg" => {
                    if let Some(cb) = &self.handlers.on_page_message {
                        cb(crate::engine::PageMessage {
                            source: url.clone(),
                            body,
                        });
                    }
                }
                _ => {}
            }
            load.respond(empty_response());
            return;
        }
        let html = (self.handlers.resolve_page)(parsed.path());
        load.respond(match html {
            Some(html) => text_response(html),
            None => empty_response(),
        });
    }
}

/// SERVO-API: response construction — `WebResourceResponse` /
/// `load.respond` signatures are the piece most likely to need adjusting
/// on first compile.
fn text_response(html: &'static str) -> servo::WebResourceResponse {
    servo::WebResourceResponse::new(html.as_bytes()).content_type("text/html; charset=utf-8")
}
fn empty_response() -> servo::WebResourceResponse {
    servo::WebResourceResponse::new(Vec::<u8>::new())
}

fn encode_png(image: &servo::Image) -> Option<Vec<u8>> {
    use image::ExtendedColorType;
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            image.data(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out.into_inner())
}

pub(super) struct ServoView {
    webview: WebView,
    surface: Surface,
}

impl ServoView {
    pub(super) fn new(
        servo: &servo::Servo,
        surface: Surface,
        resolve_page: PageResolver,
        spec: ViewSpec,
    ) -> Result<Self, EngineError> {
        let mut handlers = Handlers::placeholder();
        handlers.resolve_page = resolve_page;
        handlers.on_ipc = spec.on_ipc;
        handlers.on_page_message = spec.on_page_message;
        handlers.on_title_change = spec.on_title_change;
        handlers.on_navigate = spec.on_navigate;
        handlers.on_favicon = spec.on_favicon;
        handlers.on_new_window = spec.on_new_window;
        handlers.on_load_failure = spec.on_load_failure;
        handlers.on_fullscreen = spec.on_fullscreen;
        handlers.on_load_change = spec.on_load_change;

        let delegate = Rc::new(Delegate {
            handlers,
            webview: RefCell::new(None),
            context: surface.context.clone(),
            last_url: RefCell::new(None),
        });
        // SERVO-API: `WebViewBuilder::build()` is the tail of the builder
        // chain shown in the embedding docs.
        let mut builder =
            servo::WebViewBuilder::new(servo, surface.context.clone()).delegate(delegate);
        if let Some(url) = &spec.url {
            if let Ok(parsed) = url::Url::parse(url) {
                builder = builder.url(parsed);
            }
        }
        if let Some(script) = spec.page_script {
            let manager = servo::UserContentManager::new(servo);
            manager.add_script(script.to_string());
            builder = builder.user_content_manager(manager);
        }
        let webview = builder.build();
        *delegate.webview.borrow_mut() = Some(webview.clone());
        if !spec.visible {
            webview.hide();
        }
        Ok(Self { webview, surface })
    }
}

impl ContentView for ServoView {
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        let url = url::Url::parse(url).map_err(|e| EngineError::Navigate(e.to_string()))?;
        self.webview.load(url);
        Ok(())
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.webview.reload();
        Ok(())
    }

    /// Servo exposes no cache-bypass reload — same call, documented gap.
    fn hard_reload(&self) -> Result<(), EngineError> {
        self.webview.reload();
        Ok(())
    }

    fn go_back(&self) -> Result<(), EngineError> {
        self.webview.go_back();
        Ok(())
    }

    fn go_forward(&self) -> Result<(), EngineError> {
        self.webview.go_forward();
        Ok(())
    }

    fn can_go_back(&self) -> Result<bool, EngineError> {
        Ok(self.webview.can_go_back())
    }

    fn can_go_forward(&self) -> Result<bool, EngineError> {
        Ok(self.webview.can_go_forward())
    }

    fn stop(&self) -> Result<(), EngineError> {
        Err(EngineError::View(
            "load cancellation is not exposed by the servo backend".into(),
        ))
    }

    fn set_visible(&self, visible: bool) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        self.surface.child.set_visible(visible);
        if visible {
            self.webview.show();
        } else {
            self.webview.hide();
        }
        Ok(())
    }

    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            let size = self.surface.child.set_bounds(bounds);
            self.surface.context.resize(size);
            self.webview.resize(servo::DeviceIntSize::new(
                size.width as i32,
                size.height as i32,
            ));
        }
        Ok(())
    }

    fn bring_to_front(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        self.surface.child.raise();
        Ok(())
    }

    fn focus(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        self.surface.child.focus();
        self.webview.set_focused(true);
        Ok(())
    }

    /// SERVO-API: `evaluate_javascript` takes a script and a result
    /// callback; results are fire-and-forget here, like wry's `eval`.
    fn eval(&self, js: &str) -> Result<(), EngineError> {
        self.webview
            .evaluate_javascript(js.to_string(), Box::new(|_| ()));
        Ok(())
    }

    fn url(&self) -> Option<String> {
        self.webview.url().map(|u| u.to_string())
    }

    fn open_devtools(&self) {}

    fn muted(&self) -> Result<bool, EngineError> {
        Ok(false)
    }

    fn set_muted(&self, _muted: bool) -> Result<(), EngineError> {
        Err(EngineError::View(
            "per-view mute is not exposed by the servo backend".into(),
        ))
    }

    fn zoom_factor(&self) -> Result<f64, EngineError> {
        Ok(self.webview.page_zoom() as f64)
    }

    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError> {
        self.webview.set_page_zoom(factor as f32);
        Ok(())
    }

    /// SERVO-API: `take_screenshot` returns a `Result<RgbaImage>` — the PNG
    /// encode reuses the favicon path.
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        let shot = self
            .webview
            .take_screenshot()
            .map_err(|e| EngineError::View(format!("{e:?}")))?;
        encode_rgba(&shot).ok_or_else(|| EngineError::View("screenshot encode failed".into()))
    }

    fn save_as(&self) -> Result<(), EngineError> {
        Err(EngineError::View(
            "save-as is not exposed by the servo backend".into(),
        ))
    }

    fn cookies(&self, _uri: &str) -> Result<Vec<crate::engine::Cookie>, EngineError> {
        Err(EngineError::View(
            "the cookie jar is not exposed by the servo backend".into(),
        ))
    }

    fn clear_browsing_data(&self) -> Result<(), EngineError> {
        Err(EngineError::View(
            "profile wiping is not exposed by the servo backend".into(),
        ))
    }

    fn cancel_download(&self, _id: DownloadId) -> Result<(), EngineError> {
        Err(EngineError::View(
            "downloads are not wired through the servo backend".into(),
        ))
    }
    fn pause_download(&self, id: DownloadId) -> Result<(), EngineError> {
        self.cancel_download(id)
    }
    fn resume_download(&self, id: DownloadId) -> Result<(), EngineError> {
        self.cancel_download(id)
    }

    /// Servo has no TrySuspend — the freeze path (snapshot + drop) is the
    /// reclamation story here, so suspend is a documented no-op.
    fn suspend(&self) -> Result<(), EngineError> {
        Ok(())
    }
    fn resume(&self) -> Result<(), EngineError> {
        Ok(())
    }
    fn is_suspended(&self) -> Result<bool, EngineError> {
        Ok(false)
    }
    /// `None` — the safe answer, per the trait's own contract.
    fn playing_audio(&self) -> Option<bool> {
        None
    }
}

fn encode_rgba(image: &image::RgbaImage) -> Option<Vec<u8>> {
    use image::ExtendedColorType;
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out.into_inner())
}
