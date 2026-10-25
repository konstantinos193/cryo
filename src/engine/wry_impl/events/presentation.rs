//! Fullscreen, renderer failure and favicon event registration.
use std::rc::Rc;
use std::sync::Arc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2ContainsFullScreenElementChangedEventHandler,
    ICoreWebView2FaviconChangedEventHandler, ICoreWebView2ProcessFailedEventHandler,
    ICoreWebView2_15, COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, COREWEBVIEW2_PROCESS_FAILED_KIND,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
};
use windows::core::Interface;
use windows::Win32::System::Com::IStream;
use wry::{WebView, WebViewExtWindows};

use crate::engine::{EngineError, FaviconHandler, FullscreenHandler, LoadFailure};

/// Attach `ContainsFullScreenElementChanged` — DOM fullscreen (video)
/// lives inside the view bounds; the app decides to grow the view over
/// the chrome strip when a page element goes fullscreen.
pub(in crate::engine::wry_impl) fn attach_fullscreen_change(
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

/// Attach `ProcessFailed`: the renderer dying leaves a blank view behind,
/// and no navigation event ever fires for it.
pub(in crate::engine::wry_impl) fn attach_process_failed(
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
pub(in crate::engine::wry_impl) fn attach_favicon(
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
