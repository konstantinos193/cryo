//! WebView2 COM event wiring: one `attach_*` per event kind, translating
//! platform args into our types before the spec callbacks see them. The
//! returned handler tokens must outlive the view — `WryView` holds them
//! for exactly that.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2AcceleratorKeyPressedEventArgs, ICoreWebView2AcceleratorKeyPressedEventHandler,
    ICoreWebView2ContainsFullScreenElementChangedEventHandler,
    ICoreWebView2FaviconChangedEventHandler, ICoreWebView2NavigationCompletedEventArgs,
    ICoreWebView2NavigationCompletedEventArgs2, ICoreWebView2NavigationCompletedEventHandler,
    ICoreWebView2NavigationStartingEventHandler, ICoreWebView2ProcessFailedEventHandler,
    ICoreWebView2SourceChangedEventHandler,
    ICoreWebView2_15, COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
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
use windows::core::Interface;
use windows::Win32::System::Com::IStream;
use wry::{WebView, WebViewExtWindows};

use crate::engine::{
    Accelerator, AcceleratorHandler, AcceleratorKey, EngineError, FaviconHandler,
    FullscreenHandler, HttpStatus, LoadFailure,
};

use super::delay::Delayer;

/// Grace for the DownloadStarting ⇄ NavigationCompleted race: an abort
/// that means "this navigation became a download" gets this long for
/// the flag to land before it counts as a real connection failure.
/// The abort arrives alone often enough that delaying real failures by
/// this much is fine.
const DOWNLOAD_ABORT_GRACE: Duration = Duration::from_millis(400);

/// Automatic retries for failures WebView2 won't classify. Its `UNKNOWN`
/// and `UNEXPECTED_ERROR` statuses hide transient transport errors
/// (HTTP/2 protocol resets, QUIC falls) that a plain reload usually
/// clears — so the view gets one silent second chance before the app
/// is told anything went wrong. A persistent failure still reports on
/// the retry's completion, so the error page cannot be deferred forever.
pub(super) const UNCLASSIFIED_RETRIES: u8 = 1;

/// Attach WebView2's `AcceleratorKeyPressed` — the only key stream that
/// fires while the view holds focus. The controller raises it before the
/// engine processes the key, so `SetHandled` decides whether WebView2 or
/// the page ever sees it. Returns the handler so the caller can keep it
/// alive for the view's lifetime.
pub(super) fn attach_accelerator(
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
pub(super) fn attach_fullscreen_change(
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
const EMPTY_BODY_PROBE: &str =
    "(function(){var b=document.body;return !b||(!b.children.length&&!b.textContent.trim());})()";

/// Attach `SourceChanged`: the only event that reports same-document URL
/// changes — an SPA's `history.pushState`/`popstate` moves `Source` without
/// any NavigationStarting/Completed, so the address bar would otherwise
/// stay on the URL the real navigation carried. `attempted` holds the URI
/// that navigation reported; a SourceChanged merely echoing it is skipped,
/// anything else is a move the navigation handlers never saw and gets
/// reported through `on_navigate`. The event can't be vetoed — the URI has
/// already changed — so the callback's return is dropped.
pub(super) fn attach_source_changed(
    view: &WebView,
    on_navigate: Rc<dyn Fn(String) -> bool>,
    attempted: Rc<RefCell<String>>,
) -> Result<ICoreWebView2SourceChangedEventHandler, EngineError> {
    let handler =
        webview2_com::SourceChangedEventHandler::create(Box::new(move |webview, _args| {
            let Some(wv) = webview else { return Ok(()) };
            let mut uri = windows::core::PWSTR::null();
            unsafe { wv.Source(&mut uri)? };
            let uri = unsafe { uri.to_string() }.unwrap_or_default();
            let mut seen = attempted.borrow_mut();
            if !uri.is_empty() && uri != *seen {
                *seen = uri.clone();
                on_navigate(uri);
            }
            Ok(())
        }));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_SourceChanged(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

/// Attach `NavigationStarting`: remembers the newest navigation's id and
/// reports "loading". A navigation that supersedes one still in flight
/// replaces the id — the stale completion must not clear the spinner.
/// `attempted` keeps the URI for `navigation_completed`'s one retry:
/// a failed load leaves `Source` pointing at the old document, so the
/// retry needs the address the start event carried.
pub(super) fn attach_navigation_starting(
    view: &WebView,
    on_load_change: Rc<dyn Fn(bool)>,
    latest_nav: Rc<Cell<u64>>,
    attempted: Rc<RefCell<String>>,
) -> Result<ICoreWebView2NavigationStartingEventHandler, EngineError> {
    let handler =
        webview2_com::NavigationStartingEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else { return Ok(()) };
            let mut id = 0u64;
            unsafe { args.NavigationId(&mut id)? };
            latest_nav.set(id);
            let mut uri = windows::core::PWSTR::null();
            unsafe { args.Uri(&mut uri)? };
            *attempted.borrow_mut() = unsafe { uri.to_string() }.unwrap_or_default();
            on_load_change(true);
            Ok(())
        }));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_NavigationStarting(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

/// State the navigation start ⇄ completion pair shares, grouped so the
/// handlers don't grow a parameter per field. See `view::WryView::new`
/// for what each field guards.
pub(super) struct NavState {
    pub download_started: Arc<AtomicBool>,
    pub on_load_change: Option<Rc<dyn Fn(bool)>>,
    pub latest_nav: Rc<Cell<u64>>,
    pub attempted: Rc<RefCell<String>>,
    pub retry_budget: Rc<Cell<u8>>,
}

/// Attach `NavigationCompleted`: the event that says a navigation finished,
/// and whether it produced a document. `report` may be absent when only
/// the loading signal is wanted.
pub(super) fn attach_navigation_completed(
    view: &WebView,
    report: Option<Arc<dyn Fn(LoadFailure) + Send + Sync>>,
    state: NavState,
    delayer: Delayer,
) -> Result<ICoreWebView2NavigationCompletedEventHandler, EngineError> {
    let webview = view.webview();
    let handler =
        webview2_com::NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else { return Ok(()) };
            unsafe { navigation_completed(&webview, &args, report.as_ref(), &state, &delayer) }
        }));
    let mut token = 0i64;
    unsafe {
        view.webview()
            .add_NavigationCompleted(&handler, &mut token)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
    }
    Ok(handler)
}

unsafe fn navigation_completed(
    webview: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
    args: &ICoreWebView2NavigationCompletedEventArgs,
    report: Option<&Arc<dyn Fn(LoadFailure) + Send + Sync>>,
    state: &NavState,
    delayer: &Delayer,
) -> windows::core::Result<()> {
    let NavState {
        download_started,
        on_load_change,
        latest_nav,
        attempted,
        retry_budget,
    } = state;
    let on_load_change = on_load_change.as_ref();
    // Clear "loading" only for the navigation that set it: a superseded
    // navigation's completion arrives too, and must leave the newer
    // one's spinner running. `0` means no start event was seen — a spec
    // URL can complete without one, and clearing a never-set flag is
    // harmless.
    let mut nav_id = 0u64;
    args.NavigationId(&mut nav_id)?;
    if let Some(on_change) = on_load_change {
        if latest_nav.get() == 0 || nav_id == latest_nav.get() {
            on_change(false);
        }
    }
    let Some(report) = report else {
        return Ok(());
    };
    let mut success = windows::core::BOOL::default();
    args.IsSuccess(&mut success)?;
    if !success.as_bool() {
        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
        args.WebErrorStatus(&mut status)?;
        // A navigation that became a download completes "aborted" by
        // design — but the two events race: DownloadStarting can land a
        // few ms *after* this abort. A flag already set is claimed now;
        // otherwise the report is re-checked on the shared delay worker
        // once the grace expires, so a download on its way can still
        // claim it.
        if status == COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED {
            if !download_started.swap(false, Ordering::SeqCst) {
                let report = report.clone();
                let flag = download_started.clone();
                delayer.after(DOWNLOAD_ABORT_GRACE, move || {
                    if !flag.swap(false, Ordering::SeqCst) {
                        (*report)(LoadFailure::ConnectionFailed);
                    }
                });
            }
            return Ok(());
        }
        let failure = load_failure(status);
        if failure == Some(LoadFailure::Unknown) {
            // The raw status is the only evidence for what actually
            // broke — keep it in the log before retrying it away.
            crate::report!(
                "navigation to {} failed, status {status:?}",
                attempted.borrow()
            );
            if retry_budget.get() > 0 {
                retry_budget.set(retry_budget.get() - 1);
                let url = attempted.borrow().clone();
                if !url.is_empty()
                    && webview
                        .Navigate(&windows::core::HSTRING::from(url.as_str()))
                        .is_ok()
                {
                    return Ok(());
                }
            }
        }
        if let Some(failure) = failure {
            (**report)(failure);
        }
        return Ok(());
    }
    download_started.store(false, Ordering::SeqCst);
    retry_budget.set(UNCLASSIFIED_RETRIES);

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
pub(super) fn attach_process_failed(
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
pub(super) fn attach_favicon(
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
        crate::report!("favicon fetch failed: {e}");
    }
    Ok(Some(handler))
}

/// Ask WebView2 for the current icon; the completion reports the PNG
/// (`None` when the document has none) to `report`. A dispatch failure
/// surfaces to the event's caller.
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
