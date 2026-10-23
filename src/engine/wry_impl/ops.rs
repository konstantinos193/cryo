//! WebView2 (`webview2_com`) and Win32 operations wry's API doesn't
//! reach: cache-bypass reload, history state, sibling z-order, mute,
//! zoom, page capture, save-as and profile-wide data clearing. `view`
//! dispatches each `ContentView` method that needs one here.

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Profile2, ICoreWebView2_13, ICoreWebView2_25, ICoreWebView2_8,
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
};
use windows::core::Interface;
use wry::{WebView, WebViewExtWindows};

use crate::engine::EngineError;

/// `Page.reload {ignoreCache:true}` over the DevTools protocol —
/// WebView2's own reload API has no cache-bypass variant.
pub(super) fn hard_reload(view: &WebView) -> Result<(), EngineError> {
    let done =
        webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_hr, _json| {
            Ok(())
        }));
    unsafe {
        view.webview().CallDevToolsProtocolMethod(
            &windows::core::HSTRING::from("Page.reload"),
            &windows::core::HSTRING::from(r#"{"ignoreCache":true}"#),
            &done,
        )
    }
    .map_err(|e| EngineError::Navigate(e.to_string()))
}

pub(super) fn go_back(view: &WebView) -> Result<(), EngineError> {
    unsafe {
        let wv = view.webview();
        let mut can = windows::core::BOOL::default();
        wv.CanGoBack(&mut can)
            .and_then(|_| if can.as_bool() { wv.GoBack() } else { Ok(()) })
    }
    .map_err(|e| EngineError::Navigate(e.to_string()))
}

pub(super) fn go_forward(view: &WebView) -> Result<(), EngineError> {
    unsafe {
        let wv = view.webview();
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

pub(super) fn can_go_back(view: &WebView) -> Result<bool, EngineError> {
    unsafe {
        let mut can = windows::core::BOOL::default();
        view.webview()
            .CanGoBack(&mut can)
            .map_err(|e| EngineError::Navigate(e.to_string()))?;
        Ok(can.as_bool())
    }
}

pub(super) fn can_go_forward(view: &WebView) -> Result<bool, EngineError> {
    unsafe {
        let mut can = windows::core::BOOL::default();
        view.webview()
            .CanGoForward(&mut can)
            .map_err(|e| EngineError::Navigate(e.to_string()))?;
        Ok(can.as_bool())
    }
}

pub(super) fn stop(view: &WebView) -> Result<(), EngineError> {
    unsafe { view.webview().Stop() }.map_err(|e| EngineError::Navigate(e.to_string()))
}

/// Views spawned later sit above this one in the sibling z-order —
/// wry sizes with `SWP_NOZORDER`, so a view that temporarily overlaps
/// newer siblings (chrome grown for its menu) must re-raise itself.
pub(super) fn bring_to_front(view: &WebView) -> Result<(), EngineError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    unsafe {
        SetWindowPos(
            view.hwnd(),
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

pub(super) fn muted(view: &WebView) -> Result<bool, EngineError> {
    unsafe {
        let wv: ICoreWebView2_8 = view
            .webview()
            .cast()
            .map_err(|e| EngineError::View(e.to_string()))?;
        let mut muted = windows::core::BOOL::default();
        wv.IsMuted(&mut muted)
            .map_err(|e| EngineError::View(e.to_string()))?;
        Ok(muted.as_bool())
    }
}

pub(super) fn set_muted(view: &WebView, muted: bool) -> Result<(), EngineError> {
    unsafe {
        let wv: ICoreWebView2_8 = view
            .webview()
            .cast()
            .map_err(|e| EngineError::View(e.to_string()))?;
        wv.SetIsMuted(muted)
            .map_err(|e| EngineError::View(e.to_string()))
    }
}

pub(super) fn zoom_factor(view: &WebView) -> Result<f64, EngineError> {
    let mut factor = 1.0f64;
    unsafe { view.controller().ZoomFactor(&mut factor) }
        .map_err(|e| EngineError::View(e.to_string()))?;
    Ok(factor)
}

pub(super) fn set_zoom_factor(view: &WebView, factor: f64) -> Result<(), EngineError> {
    unsafe { view.controller().SetZoomFactor(factor) }.map_err(|e| EngineError::View(e.to_string()))
}

/// CapturePreview streams PNG bytes into an in-memory IStream; the
/// completion handler signals a channel `wait_with_pump` blocks on —
/// the event loop keeps spinning while WebView2 writes.
pub(super) fn capture_png(view: &WebView) -> Result<Vec<u8>, EngineError> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

    unsafe {
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)
            .map_err(|e| EngineError::View(e.to_string()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        let done = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |hr| {
            // A failed send means `wait_with_pump` already gave up —
            // nobody is left to signal, so the drop is reported, not
            // silently `let _`'d.
            if tx.send(hr).is_err() {
                crate::report!("capture result arrived with no receiver");
            }
            Ok(())
        }));
        view.webview()
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

pub(super) fn save_as(view: &WebView) -> Result<(), EngineError> {
    unsafe {
        let wv: ICoreWebView2_25 = view
            .webview()
            .cast()
            .map_err(|e| EngineError::View(e.to_string()))?;
        let done =
            webview2_com::ShowSaveAsUICompletedHandler::create(Box::new(|_hr, _result| Ok(())));
        wv.ShowSaveAsUI(&done)
            .map_err(|e| EngineError::View(e.to_string()))
    }
}

pub(super) fn clear_browsing_data(view: &WebView) -> Result<(), EngineError> {
    unsafe {
        let wv: ICoreWebView2_13 = view
            .webview()
            .cast()
            .map_err(|e| EngineError::View(e.to_string()))?;
        let profile: ICoreWebView2Profile2 = wv
            .Profile()
            .and_then(|p| p.cast())
            .map_err(|e| EngineError::View(e.to_string()))?;
        let done = webview2_com::ClearBrowsingDataCompletedHandler::create(Box::new(|_hr| Ok(())));
        profile
            .ClearBrowsingDataAll(&done)
            .map_err(|e| EngineError::View(e.to_string()))
    }
}
