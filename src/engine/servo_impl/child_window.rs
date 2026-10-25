//! One HWND child per view — the model WebView2 uses, so the OS composites
//! z-order and bounds instead of the backend growing a GL blitter. The
//! child is a bare `DefWindowProc` window: Servo paints it through its own
//! surfman surface; nothing else needs its messages.
//!
//! `Rect`s arriving here are logical pixels relative to the parent client
//! area — the same convention `spawn` hands every engine.

use std::sync::Once;

use raw_window_handle::RawWindowHandle;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, NULL_BRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, SWP_NOACTIVATE, SWP_NOREDRAW, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN,
};

use crate::engine::{EngineError, Rect};

const CLASS_NAME: &windows::core::HSTRING = &windows::core::w!("cryo servo view");

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(
                unsafe { GetModuleHandleW(None) }
                    .map(|m| m.0)
                    .unwrap_or_default(),
            ),
            // No background erase — the surface is always fully painted by
            // surfman; an erase would just be a flash of nothing.
            hbrBackground: unsafe { HBRUSH(GetStockObject(NULL_BRUSH).0) },
            lpszClassName: windows::core::PCWSTR(CLASS_NAME.as_ptr()),
            ..Default::default()
        };
        unsafe { RegisterClassW(&class) };
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A view's own piece of screen. `Drop` destroys the HWND — dropping the
/// `servo::WebView` first frees the surfman surface that targeted it.
pub(super) struct ChildWindow {
    hwnd: HWND,
    /// Device-pixel size as created — surfman's `PhysicalSize`.
    physical: dpi::PhysicalSize<u32>,
}

impl ChildWindow {
    pub(super) fn create(
        parent: RawWindowHandle,
        bounds: Rect,
        visible: bool,
    ) -> Result<Self, EngineError> {
        register_class();
        let RawWindowHandle::Win32(parent_handle) = parent else {
            return Err(EngineError::Spawn(
                "the servo backend only knows Win32 parents".into(),
            ));
        };
        let parent = HWND(parent_handle.hwnd.get() as _);
        let scale = unsafe { GetDpiForWindow(parent) } as f64 / 96.0;
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let to_dev = |v: f64| (v * scale).round() as i32;
        let style = if visible {
            WS_CHILD | WS_CLIPCHILDREN | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE
        } else {
            WS_CHILD | WS_CLIPCHILDREN
        };
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                windows::core::PCWSTR::null(),
                style,
                to_dev(bounds.x),
                to_dev(bounds.y),
                to_dev(bounds.w).max(1),
                to_dev(bounds.h).max(1),
                Some(parent),
                None,
                Some(HINSTANCE(
                    GetModuleHandleW(None).map(|m| m.0).unwrap_or_default(),
                )),
                None,
            )
        }
        .map_err(|e| EngineError::Spawn(e.to_string()))?;
        Ok(Self {
            hwnd,
            physical: dpi::PhysicalSize::new(
                to_dev(bounds.w).max(1) as u32,
                to_dev(bounds.h).max(1) as u32,
            ),
        })
    }

    pub(super) fn raw_handle(&self) -> RawWindowHandle {
        let mut handle = raw_window_handle::Win32WindowHandle::new(
            std::num::NonZeroIsize::new(self.hwnd.0 as isize).expect("a created HWND is non-null"),
        );
        handle.hinstance = std::num::NonZeroIsize::new(unsafe {
            GetModuleHandleW(None)
                .map(|m| m.0 as isize)
                .unwrap_or_default()
        });
        RawWindowHandle::Win32(handle)
    }

    pub(super) fn physical_size(&self) -> dpi::PhysicalSize<u32> {
        self.physical
    }

    /// Bounds in logical pixels relative to the parent — matches `spawn`.
    pub(super) fn set_bounds(&self, bounds: Rect) -> dpi::PhysicalSize<u32> {
        let parent = unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(self.hwnd) };
        let scale = unsafe { GetDpiForWindow(parent) } as f64 / 96.0;
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let to_dev = |v: f64| (v * scale).round() as i32;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                to_dev(bounds.x),
                to_dev(bounds.y),
                to_dev(bounds.w).max(1),
                to_dev(bounds.h).max(1),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
            );
        }
        dpi::PhysicalSize::new(
            to_dev(bounds.w).max(1) as u32,
            to_dev(bounds.h).max(1) as u32,
        )
    }

    pub(super) fn set_visible(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    pub(super) fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                    | SWP_NOREDRAW,
            );
        }
    }

    pub(super) fn focus(&self) {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
        }
    }
}

impl Drop for ChildWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
