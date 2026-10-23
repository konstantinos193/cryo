//! WebView2 backend: adapts `wry` to the `Engine`/`ContentView` traits.
//! The only module allowed to name `wry` types.

#[cfg(target_os = "windows")]
mod downloads;

use std::borrow::Cow;
use std::rc::Rc;
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::sync::Arc;

use tao::window::Window;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
use wry::http::{header::CONTENT_TYPE, Response};
use wry::{
    PermissionKind as WryPermissionKind, PermissionResponse, Rect as WryRect, WebView,
    WebViewBuilder,
};

#[cfg(target_os = "windows")]
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2AcceleratorKeyPressedEventArgs, ICoreWebView2AcceleratorKeyPressedEventHandler,
    ICoreWebView2ContainsFullScreenElementChangedEventHandler,
    ICoreWebView2DownloadStartingEventHandler, ICoreWebView2FaviconChangedEventHandler,
    ICoreWebView2NavigationCompletedEventArgs, ICoreWebView2NavigationCompletedEventArgs2,
    ICoreWebView2NavigationCompletedEventHandler, ICoreWebView2ProcessFailedEventHandler,
    ICoreWebView2Profile2, ICoreWebView2_13, ICoreWebView2_15, ICoreWebView2_25, ICoreWebView2_8, COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
    COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, COREWEBVIEW2_WEB_ERROR_STATUS,
    COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS,
    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
    COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE,
    COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
    COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
    COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED,
    COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE, COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT,
    COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN,
    COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED,
    COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED,
};
#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::IStream;
#[cfg(target_os = "windows")]
use wry::{NewWindowResponse, WebViewExtWindows};

use crate::engine::{
    Accelerator, AcceleratorHandler, AcceleratorKey, ContentView, DownloadHandler, DownloadId,
    Engine, EngineError, FaviconHandler, FullscreenHandler, HttpStatus, LoadFailure,
    LoadFailureHandler, PageResolver, PermissionDecision, PermissionKind, Rect, ViewSpec,
};

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
    /// Whole-process InPrivate mode: every view spawns on an incognito
    /// controller (WebView2 `IsInPrivateModeEnabled`).
    incognito: bool,
}

impl WryEngine {
    pub fn new(window: Rc<Window>, resolve_page: PageResolver, incognito: bool) -> Self {
        Self {
            window,
            resolve_page,
            incognito,
        }
    }
}

/// wry's permission kinds → our vocabulary. Everything wry doesn't name
/// (autoplay, sensors, fonts, ...) collapses to `Other`.
fn permission_kind(kind: WryPermissionKind) -> PermissionKind {
    match kind {
        WryPermissionKind::Microphone => PermissionKind::Microphone,
        WryPermissionKind::Camera => PermissionKind::Camera,
        WryPermissionKind::Geolocation => PermissionKind::Geolocation,
        WryPermissionKind::Notifications => PermissionKind::Notifications,
        WryPermissionKind::ClipboardRead => PermissionKind::ClipboardRead,
        _ => PermissionKind::Other,
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
            .with_transparent(spec.transparent)
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

        if self.incognito {
            builder = builder.with_incognito(true);
        }
        if let Some(background) = spec.background {
            builder = builder.with_background_color(background);
        }
        if let Some(url) = spec.url {
            builder = builder.with_url(&url);
        }
        if let Some(on_title) = spec.on_title_change {
            builder = builder.with_document_title_changed_handler(on_title);
        }
        // `Rc` rather than the spec's `Box`: the DownloadStarting flag also
        // reports through it (a navigation that became a download re-reports
        // the address the view still shows).
        let on_navigate: Option<Rc<dyn Fn(String) -> bool>> = spec.on_navigate.map(Rc::from);
        if let Some(on_nav) = &on_navigate {
            let on_nav = on_nav.clone();
            builder = builder.with_navigation_handler(move |uri| on_nav(uri));
        }
        if let Some(on_new_window) = spec.on_new_window {
            builder = builder.with_new_window_req_handler(move |url, _features| {
                on_new_window(url);
                NewWindowResponse::Deny
            });
        }
        if let Some(on_permission) = spec.on_permission {
            builder = builder.with_permission_handler(move |kind| {
                match on_permission(permission_kind(kind)) {
                    PermissionDecision::Allow => PermissionResponse::Allow,
                    PermissionDecision::Deny => PermissionResponse::Deny,
                }
            });
        }

        let view = builder
            .build_as_child(&*self.window)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        WryView::new(
            view,
            spec.on_accelerator,
            spec.on_load_failure,
            spec.on_fullscreen,
            spec.on_favicon,
            on_navigate,
            spec.on_download,
        )
        .map(|v| Box::new(v) as Box<dyn ContentView>)
    }
}

/// Attach WebView2's `AcceleratorKeyPressed` — the only key stream that
/// fires while the view holds focus. The controller raises it before the
/// engine processes the key, so `SetHandled` decides whether WebView2 or
/// the page ever sees it. Returns the handler so the caller can keep it
/// alive for the view's lifetime.
#[cfg(target_os = "windows")]
fn attach_accelerator(
    view: &WebView,
    on_accelerator: AcceleratorHandler,
) -> Result<ICoreWebView2AcceleratorKeyPressedEventHandler, EngineError> {
    let handler = webview2_com::AcceleratorKeyPressedEventHandler::create(Box::new(
        move |_controller, args| {
            let Some(args) = args else { return Ok(()) };
            match unsafe { read_accelerator(&args) } {
                Ok(Some(acc)) => unsafe { args.SetHandled(on_accelerator(acc)) },
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            }
        },
    ));
    let mut token = 0i64;
    unsafe {
        view.controller()
            .add_AcceleratorKeyPressed(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

/// Attach `ContainsFullScreenElementChanged` — DOM fullscreen (video)
/// lives inside the view bounds; the app decides to grow the view over
/// the chrome strip when a page element goes fullscreen.
#[cfg(target_os = "windows")]
fn attach_fullscreen_change(
    view: &WebView,
    on_fullscreen: FullscreenHandler,
) -> Result<ICoreWebView2ContainsFullScreenElementChangedEventHandler, EngineError> {
    let handler = webview2_com::ContainsFullScreenElementChangedEventHandler::create(Box::new(
        move |webview, _args| {
            let Some(wv) = webview else { return Ok(()) };
            let mut contains = windows::core::BOOL::default();
            unsafe { wv.ContainsFullScreenElement(&mut contains)? };
            on_fullscreen(contains.as_bool());
            Ok(())
        },
    ));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_ContainsFullScreenElementChanged(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

/// Translate a WebView2 accelerator event into our key type. Returns
/// `None` for key releases and keys the browser doesn't speak.
#[cfg(target_os = "windows")]
unsafe fn read_accelerator(
    args: &ICoreWebView2AcceleratorKeyPressedEventArgs,
) -> windows::core::Result<Option<Accelerator>> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};

    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
    args.KeyEventKind(&mut kind)?;
    // Alt chords arrive as "system" key-downs; releases are ignored.
    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
    {
        return Ok(None);
    }
    let mut vk = 0u32;
    args.VirtualKey(&mut vk)?;
    let Some(key) = accelerator_key(vk) else {
        return Ok(None);
    };
    let down =
        |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| GetKeyState(k.0 as i32) < 0;
    Ok(Some(Accelerator {
        key,
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
    }))
}

/// Win32 virtual-key code → `AcceleratorKey`. Kept here so the rest of
/// the crate never sees platform key numbers.
#[cfg(target_os = "windows")]
fn accelerator_key(vk: u32) -> Option<AcceleratorKey> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    Some(match vk as u16 {
        v @ 0x30..=0x39 => AcceleratorKey::Character(v as u8 as char),
        v @ 0x41..=0x5A => AcceleratorKey::Character((v + 32) as u8 as char),
        v if v == VK_TAB.0 => AcceleratorKey::Tab,
        v if v == VK_LEFT.0 => AcceleratorKey::Left,
        v if v == VK_RIGHT.0 => AcceleratorKey::Right,
        v if v == VK_PRIOR.0 => AcceleratorKey::PageUp,
        v if v == VK_NEXT.0 => AcceleratorKey::PageDown,
        v if v == VK_HOME.0 => AcceleratorKey::Home,
        v if v == VK_END.0 => AcceleratorKey::End,
        v if v == VK_DELETE.0 => AcceleratorKey::Delete,
        v if v == VK_ESCAPE.0 => AcceleratorKey::Escape,
        // VK codes are layout-agnostic: OEM_PLUS reports '=', the numpad
        // keys report their literal character.
        v if v == VK_OEM_PLUS.0 => AcceleratorKey::Character('='),
        v if v == VK_OEM_MINUS.0 || v == VK_SUBTRACT.0 => AcceleratorKey::Character('-'),
        v if v == VK_ADD.0 => AcceleratorKey::Character('+'),
        v if (VK_F1.0..=VK_F12.0).contains(&v) => AcceleratorKey::F((v - VK_F1.0 + 1) as u8),
        _ => return None,
    })
}

/// Asks the page whether the server sent anything worth rendering.
#[cfg(target_os = "windows")]
const EMPTY_BODY_PROBE: &str =
    "(function(){var b=document.body;return !b||(!b.children.length&&!b.textContent.trim());})()";

/// Attach `NavigationCompleted`: the event that says a navigation finished,
/// and whether it produced a document.
#[cfg(target_os = "windows")]
fn attach_navigation_completed(
    view: &WebView,
    report: Arc<dyn Fn(LoadFailure) + Send + Sync>,
    download_started: Arc<AtomicBool>,
) -> Result<ICoreWebView2NavigationCompletedEventHandler, EngineError> {
    let webview = view.webview();
    let handler =
        webview2_com::NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else { return Ok(()) };
            unsafe { navigation_completed(&webview, &args, &report, &download_started) }
        }));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_NavigationCompleted(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

#[cfg(target_os = "windows")]
unsafe fn navigation_completed(
    webview: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
    args: &ICoreWebView2NavigationCompletedEventArgs,
    report: &Arc<dyn Fn(LoadFailure) + Send + Sync>,
    download_started: &Arc<AtomicBool>,
) -> windows::core::Result<()> {
    let mut success = windows::core::BOOL::default();
    args.IsSuccess(&mut success)?;
    if !success.as_bool() {
        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
        args.WebErrorStatus(&mut status)?;
        // A navigation that became a download completes "aborted" by
        // design — but the two events race: DownloadStarting can land a
        // few ms *after* this abort. A flag already set is claimed now;
        // otherwise the report waits a beat on a worker so a download on
        // its way can still claim it. The abort arrives alone often
        // enough that delaying real failures by this much is fine.
        if status == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED {
            if !download_started.swap(false, Ordering::SeqCst) {
                let report = report.clone();
                let flag = download_started.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if !flag.swap(false, Ordering::SeqCst) {
                        (*report)(LoadFailure::ConnectionFailed);
                    }
                });
            }
            return Ok(());
        }
        if let Some(failure) = load_failure(status) {
            (**report)(failure);
        }
        return Ok(());
    }
    download_started.store(false, Ordering::SeqCst);

    // A 4xx or 5xx is still a successful navigation — the server answered.
    // Only step in when it answered with nothing to render, so a site's own
    // 404 page survives, the way Chrome leaves it alone.
    let Ok(args) = args.cast::<ICoreWebView2NavigationCompletedEventArgs2>() else {
        return Ok(());
    };
    let mut code = 0i32;
    args.HttpStatusCode(&mut code)?;
    let Ok(status) = u16::try_from(code) else {
        return Ok(());
    };
    if !HttpStatus::new(status).is_error() {
        return Ok(());
    }
    let report = report.clone();
    let probe = webview2_com::ExecuteScriptCompletedHandler::create(Box::new(move |_hr, json| {
        if json == "true" {
            (*report)(LoadFailure::Http(HttpStatus::new(status)));
        }
        Ok(())
    }));
    webview.ExecuteScript(&windows::core::HSTRING::from(EMPTY_BODY_PROBE), &probe)
}

/// Attach `ProcessFailed`: the renderer dying leaves a blank view behind,
/// and no navigation event ever fires for it.
#[cfg(target_os = "windows")]
fn attach_process_failed(
    view: &WebView,
    report: Arc<dyn Fn(LoadFailure) + Send + Sync>,
) -> Result<ICoreWebView2ProcessFailedEventHandler, EngineError> {
    let handler =
        webview2_com::ProcessFailedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            unsafe { args.ProcessFailedKind(&mut kind)? };
            // Only the process rendering this view blanks the page. A dead GPU
            // or utility process is the engine's problem, not a tab's.
            if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
            {
                (*report)(LoadFailure::RenderProcessGone);
            }
            Ok(())
        }));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_ProcessFailed(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

/// Attach `FaviconChanged` (ICoreWebView2_15): wry has no favicon API, so
/// the event re-fetches the PNG through `GetFavicon` ourselves and the
/// bytes cross the seam. `Ok(None)` on runtimes older than _15 — the view
/// then simply never reports an icon.
#[cfg(target_os = "windows")]
fn attach_favicon(
    view: &WebView,
    on_favicon: FaviconHandler,
) -> Result<Option<ICoreWebView2FaviconChangedEventHandler>, EngineError> {
    let Ok(webview) = view.webview().cast::<ICoreWebView2_15>() else {
        return Ok(None);
    };
    let report: Rc<dyn Fn(Option<Vec<u8>>)> = Rc::from(on_favicon);
    let wv = webview.clone();
    let cb = report.clone();
    let handler =
        webview2_com::FaviconChangedEventHandler::create(Box::new(move |_sender, _args| {
            fetch_favicon(&wv, &cb)
        }));
    let mut token = 0i64;
    unsafe {
        webview
            .add_FaviconChanged(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    // A url handed in the spec starts loading before this attaches —
    // pull whatever icon is already set instead of waiting for a change.
    if let Err(e) = fetch_favicon(&webview, &report) {
        eprintln!("cryo: favicon fetch failed: {e}");
    }
    Ok(Some(handler))
}

/// Ask WebView2 for the current icon; the completion reports the PNG
/// (`None` when the document has none) to `report`. A dispatch failure
/// surfaces to the event's caller.
#[cfg(target_os = "windows")]
fn fetch_favicon(
    webview: &ICoreWebView2_15,
    report: &Rc<dyn Fn(Option<Vec<u8>>)>,
) -> windows::core::Result<()> {
    let report = report.clone();
    let done = webview2_com::GetFaviconCompletedHandler::create(Box::new(move |hr, stream| {
        if hr.is_ok() {
            (*report)(read_favicon_stream(stream));
        }
        Ok(())
    }));
    unsafe { webview.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &done) }
}

/// Drain the stream `GetFavicon` hands back. Absent, empty, or unreadable
/// all collapse to `None` — "no icon" and "unreadable icon" paint the
/// same way.
#[cfg(target_os = "windows")]
fn read_favicon_stream(stream: Option<IStream>) -> Option<Vec<u8>> {
    use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

    let stream = stream?;
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME).ok()?;
        let mut buf = vec![0u8; stat.cbSize as usize];
        if buf.is_empty() {
            return None;
        }
        stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
        stream
            .Read(buf.as_mut_ptr().cast(), buf.len() as u32, None)
            .ok()
            .ok()?;
        Some(buf)
    }
}

/// WebView2 status → our vocabulary. `None` for the statuses the shell must
/// not paint over: a cancelled load is the user changing their mind, and an
/// authentication challenge is a dialog the engine raises itself.
#[cfg(target_os = "windows")]
fn load_failure(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> Option<LoadFailure> {
    Some(match status {
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => LoadFailure::Offline,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => LoadFailure::HostNotFound,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => LoadFailure::TimedOut,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET =>
        {
            LoadFailure::ConnectionFailed
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS =>
        {
            LoadFailure::CertificateRejected
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_REDIRECT_FAILED => LoadFailure::RedirectLoop,
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_ERROR_HTTP_INVALID_SERVER_RESPONSE => {
            LoadFailure::MalformedResponse
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED
            || s == COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED =>
        {
            return None
        }
        _ => LoadFailure::Unknown,
    })
}

struct WryView {
    view: WebView,
    /// Keep the COM callbacks alive; unregistration dies with the view.
    #[cfg(target_os = "windows")]
    _accel_handler: Option<ICoreWebView2AcceleratorKeyPressedEventHandler>,
    #[cfg(target_os = "windows")]
    _download_handler: Option<ICoreWebView2DownloadStartingEventHandler>,
    /// Live downloads on this view — `cancel_download` looks them up;
    /// each download's `StateChanged` removes its own entry at the end.
    #[cfg(target_os = "windows")]
    downloads: downloads::ActiveDownloads,
    #[cfg(target_os = "windows")]
    _navigation_handler: Option<ICoreWebView2NavigationCompletedEventHandler>,
    #[cfg(target_os = "windows")]
    _process_handler: Option<ICoreWebView2ProcessFailedEventHandler>,
    #[cfg(target_os = "windows")]
    _fullscreen_handler: Option<ICoreWebView2ContainsFullScreenElementChangedEventHandler>,
    #[cfg(target_os = "windows")]
    _favicon_handler: Option<ICoreWebView2FaviconChangedEventHandler>,
}

impl WryView {
    fn new(
        view: WebView,
        on_accelerator: Option<AcceleratorHandler>,
        on_load_failure: Option<LoadFailureHandler>,
        on_fullscreen: Option<FullscreenHandler>,
        on_favicon: Option<FaviconHandler>,
        on_navigate: Option<Rc<dyn Fn(String) -> bool>>,
        on_download: Option<DownloadHandler>,
    ) -> Result<Self, EngineError> {
        #[cfg(target_os = "windows")]
        {
            let _accel_handler = on_accelerator
                .map(|cb| attach_accelerator(&view, cb))
                .transpose()?;
            // Both events feed one callback; the download-abort check may
            // also report it from a worker, so it needs Arc + Send + Sync.
            let report: Option<Arc<dyn Fn(LoadFailure) + Send + Sync>> =
                on_load_failure.map(Arc::from);
            // DownloadStarting ⇄ NavigationCompleted: the flag marks the
            // abort that means "this navigation became a download" — in
            // either arrival order.
            let download_started = Arc::new(AtomicBool::new(false));
            let _navigation_handler = report
                .clone()
                .map(|cb| attach_navigation_completed(&view, cb, download_started.clone()))
                .transpose()?;
            let downloads = downloads::ActiveDownloads::default();
            let _download_handler = downloads::attach(
                &view,
                download_started,
                on_navigate,
                on_download.map(Rc::from),
                downloads.clone(),
            )?;
            let _process_handler = report
                .map(|cb| attach_process_failed(&view, cb))
                .transpose()?;
            let _fullscreen_handler = on_fullscreen
                .map(|cb| attach_fullscreen_change(&view, cb))
                .transpose()?;
            let _favicon_handler = on_favicon
                .map(|cb| attach_favicon(&view, cb))
                .transpose()?
                .flatten();
            Ok(Self {
                view,
                _accel_handler,
                _download_handler,
                downloads,
                _navigation_handler,
                _process_handler,
                _fullscreen_handler,
                _favicon_handler,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (
                on_accelerator,
                on_load_failure,
                on_fullscreen,
                on_favicon,
                on_navigate,
                on_download,
            );
            Ok(Self { view })
        }
    }
}

impl ContentView for WryView {
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        self.view
            .load_url(url)
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.view
            .reload()
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn hard_reload(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            // `Page.reload {ignoreCache:true}` over the DevTools protocol —
            // WebView2's own reload API has no cache-bypass variant.
            let done = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                |_hr, _json| Ok(()),
            ));
            unsafe {
                self.view.webview().CallDevToolsProtocolMethod(
                    &windows::core::HSTRING::from("Page.reload"),
                    &windows::core::HSTRING::from(r#"{"ignoreCache":true}"#),
                    &done,
                )
            }
            .map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("location.reload()")
        }
    }

    fn go_back(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv = self.view.webview();
                let mut can = windows::core::BOOL::default();
                wv.CanGoBack(&mut can)
                    .and_then(|_| if can.as_bool() { wv.GoBack() } else { Ok(()) })
            }
            .map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.back()")
        }
    }

    fn go_forward(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv = self.view.webview();
                let mut can = windows::core::BOOL::default();
                wv.CanGoForward(&mut can).and_then(|_| {
                    if can.as_bool() {
                        wv.GoForward()
                    } else {
                        Ok(())
                    }
                })
            }
            .map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.forward()")
        }
    }

    fn stop(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe { self.view.webview().Stop() }.map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("window.stop()")
        }
    }

    fn set_visible(&self, visible: bool) -> Result<(), EngineError> {
        self.view
            .set_visible(visible)
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError> {
        self.view
            .set_bounds(to_wry_rect(bounds))
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    /// Views spawned later sit above this one in the sibling z-order —
    /// wry sizes with `SWP_NOZORDER`, so a view that temporarily overlaps
    /// newer siblings (chrome grown for its menu) must re-raise itself.
    fn bring_to_front(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            unsafe {
                SetWindowPos(
                    self.view.hwnd(),
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            }
            .map_err(|e| EngineError::Layout(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn focus(&self) -> Result<(), EngineError> {
        self.view
            .focus()
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn eval(&self, js: &str) -> Result<(), EngineError> {
        self.view
            .evaluate_script(js)
            .map_err(|e| EngineError::Script(e.to_string()))
    }

    fn url(&self) -> Option<String> {
        self.view.url().ok()
    }

    fn open_devtools(&self) {
        #[cfg(debug_assertions)]
        self.view.open_devtools();
    }

    fn muted(&self) -> Result<bool, EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_8 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let mut muted = windows::core::BOOL::default();
                wv.IsMuted(&mut muted)
                    .map_err(|e| EngineError::View(e.to_string()))?;
                Ok(muted.as_bool())
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn set_muted(&self, muted: bool) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_8 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                wv.SetIsMuted(muted)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = muted;
            Ok(())
        }
    }

    fn zoom_factor(&self) -> Result<f64, EngineError> {
        #[cfg(target_os = "windows")]
        {
            let mut factor = 1.0f64;
            unsafe { self.view.controller().ZoomFactor(&mut factor) }
                .map_err(|e| EngineError::View(e.to_string()))?;
            Ok(factor)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(1.0)
        }
    }

    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe { self.view.controller().SetZoomFactor(factor) }
                .map_err(|e| EngineError::View(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval(&format!("document.body.style.zoom='{factor}'"))
        }
    }

    /// CapturePreview streams PNG bytes into an in-memory IStream; the
    /// completion handler signals a channel `wait_with_pump` blocks on —
    /// the event loop keeps spinning while WebView2 writes.
    #[cfg(target_os = "windows")]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        use windows::Win32::Foundation::HGLOBAL;
        use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
        use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

        unsafe {
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)
                .map_err(|e| EngineError::View(e.to_string()))?;
            let (tx, rx) = std::sync::mpsc::channel();
            let done = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |hr| {
                // A failed send means `wait_with_pump` already gave up —
                // nobody is left to signal, so the drop is deliberate.
                let _ = tx.send(hr);
                Ok(())
            }));
            self.view
                .webview()
                .CapturePreview(
                    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                    &stream,
                    &done,
                )
                .map_err(|e| EngineError::View(e.to_string()))?;
            // The completed-callback hands the HRESULT as a Result.
            webview2_com::wait_with_pump(rx)
                .map_err(|e| EngineError::View(e.to_string()))?
                .map_err(|e| EngineError::View(e.to_string()))?;

            let mut stat = STATSTG::default();
            stream
                .Stat(&mut stat, STATFLAG_NONAME)
                .map_err(|e| EngineError::View(e.to_string()))?;
            stream
                .Seek(0, STREAM_SEEK_SET, None)
                .map_err(|e| EngineError::View(e.to_string()))?;
            let mut buf = vec![0u8; stat.cbSize as usize];
            if !buf.is_empty() {
                stream
                    .Read(buf.as_mut_ptr().cast(), buf.len() as u32, None)
                    .ok()
                    .map_err(|e| EngineError::View(e.to_string()))?;
            }
            Ok(buf)
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::View("capture unsupported".into()))
    }

    fn save_as(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_25 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let done =
                    webview2_com::ShowSaveAsUICompletedHandler::create(Box::new(|_hr, _result| {
                        Ok(())
                    }));
                wv.ShowSaveAsUI(&done)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(EngineError::View("save-as unsupported".into()))
        }
    }

    fn clear_browsing_data(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_13 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let profile: ICoreWebView2Profile2 = wv
                    .Profile()
                    .and_then(|p| p.cast())
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let done =
                    webview2_com::ClearBrowsingDataCompletedHandler::create(Box::new(|_hr| Ok(())));
                profile
                    .ClearBrowsingDataAll(&done)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn cancel_download(&self, id: DownloadId) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            downloads::cancel(&self.downloads, id)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Ok(())
        }
    }
}
