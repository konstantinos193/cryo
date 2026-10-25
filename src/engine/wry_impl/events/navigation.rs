//! Navigation start, completion, SPA changes and download abort races.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2NavigationCompletedEventArgs, ICoreWebView2NavigationCompletedEventArgs2,
    ICoreWebView2NavigationCompletedEventHandler, ICoreWebView2NavigationStartingEventHandler,
    ICoreWebView2SourceChangedEventHandler, COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
    COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN,
};
use windows::core::Interface;
use wry::{WebView, WebViewExtWindows};

use crate::engine::{EngineError, HttpStatus, LoadFailure};

use super::super::delay::Delayer;

use super::failure::load_failure;
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
pub(in crate::engine::wry_impl) const UNCLASSIFIED_RETRIES: u8 = 1;

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
pub(in crate::engine::wry_impl) fn attach_source_changed(
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
pub(in crate::engine::wry_impl) fn attach_navigation_starting(
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
pub(in crate::engine::wry_impl) struct NavState {
    pub download_started: Arc<AtomicBool>,
    pub on_load_change: Option<Rc<dyn Fn(bool)>>,
    pub latest_nav: Rc<Cell<u64>>,
    pub attempted: Rc<RefCell<String>>,
    pub retry_budget: Rc<Cell<u8>>,
}

/// Attach `NavigationCompleted`: the event that says a navigation finished,
/// and whether it produced a document. `report` may be absent when only
/// the loading signal is wanted.
pub(in crate::engine::wry_impl) fn attach_navigation_completed(
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
