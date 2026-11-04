//! Pumping the thread queue while a tao handler is on the stack. WebView2
//! completions are awaited with `wait_with_pump`, which dispatches queued
//! messages — but tao buffers every mid-handler event except
//! `RedrawRequested`, which it calls re-entrantly: a `WM_PAINT` delivered
//! inside the wait panics "event handler is re-entrant". The paint can't be
//! deferred — dispatching it *is* the re-entry — so pumps eat it:
//! `ValidateRect` clears the update region or the message just regenerates,
//! and tao repaints on its next cycle.

use std::sync::mpsc::Receiver;

use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, SendMessageW, TranslateMessage, MSG, PM_REMOVE,
    WM_PAINT, WM_SETREDRAW,
};

/// Toggle `WM_SETREDRAW` on the host window around a WebView2 spawn.
/// `clear_pending_paints` drains paints already queued; it cannot stop a
/// NEW one being generated while wry's `build_as_child` waits inside its
/// own pump — an occlusion change, a resize, a sibling painting. That
/// paint dispatches into tao's runner mid-handler and panics it (#23).
/// `WM_SETREDRAW false` means none is generated for the wait's duration;
/// re-enabling plus `InvalidateRect` repaints what was suppressed.
pub(crate) fn suppress_redraw(handle: &raw_window_handle::RawWindowHandle, suppress: bool) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    let raw_window_handle::RawWindowHandle::Win32(win32) = handle else {
        return;
    };
    let hwnd = HWND(win32.hwnd.get() as _);
    unsafe {
        SendMessageW(
            Some(hwnd),
            WM_SETREDRAW,
            Some(WPARAM(suppress as usize)),
            Some(LPARAM(0)),
        );
        if !suppress {
            InvalidateRect(Some(hwnd), None, true);
        }
    }
}

/// Drop queued paints and clear their windows' update regions. Run before
/// handing the thread to a pump we don't own — wry's `build_as_child` waits
/// inside `webview2_com::wait_with_pump` — so no paint is left to dispatch.
/// PeekMessage *generates* WM_PAINT for a still-invalid window, so the
/// region must be cleared inside the loop or removal never converges.
pub(crate) fn clear_pending_paints() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, WM_PAINT, WM_PAINT, PM_REMOVE).as_bool() {
            let _ = ValidateRect(Some(msg.hwnd), None);
        }
    }
}

/// `webview2_com::wait_with_pump` with `WM_PAINT` eaten instead of
/// dispatched — same contract otherwise: the loop ends on a channel result,
/// a `GetMessage` error, or `WM_QUIT`.
pub(super) fn wait_with_pump<T>(rx: Receiver<T>) -> webview2_com::Result<T> {
    clear_pending_paints();
    let mut msg = MSG::default();
    loop {
        if let Ok(result) = rx.try_recv() {
            return Ok(result);
        }
        unsafe {
            match GetMessageW(&mut msg, None, 0, 0).0 {
                -1 => return Err(windows::core::Error::from_thread().into()),
                0 => return Err(webview2_com::Error::TaskCanceled),
                _ => {
                    if msg.message == WM_PAINT {
                        let _ = ValidateRect(Some(msg.hwnd), None);
                    } else {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint-eating must not swallow the wake: the pump unblocks on a
    /// channel result exactly like `webview2_com::wait_with_pump`. The
    /// sender posts a thread message to kick the blocked `GetMessage`,
    /// the same kick WebView2's own callbacks provide.
    #[test]
    fn pump_still_completes_on_channel_result() {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, PM_NOREMOVE, WM_APP};

        // Force the thread's message queue into existence first, or the
        // PostThreadMessage kick below has nowhere to land.
        let mut priming = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut priming, None, 0, 0, PM_NOREMOVE);
        }
        let thread = unsafe { GetCurrentThreadId() };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(42).unwrap();
            unsafe {
                let _ = PostThreadMessageW(thread, WM_APP, WPARAM(0), LPARAM(0));
            }
        });
        assert_eq!(wait_with_pump(rx).unwrap(), 42);
    }

    /// The scrub must terminate on a message queue with no invalid
    /// windows — the ValidateRect-in-loop is what makes that hold.
    #[test]
    fn scrub_terminates_with_nothing_pending() {
        clear_pending_paints();
    }
}
