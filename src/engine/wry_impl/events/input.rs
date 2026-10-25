//! Keyboard accelerator translation and registration.

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2AcceleratorKeyPressedEventArgs, ICoreWebView2AcceleratorKeyPressedEventHandler,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
};
use wry::{WebView, WebViewExtWindows};

use crate::engine::{Accelerator, AcceleratorHandler, AcceleratorKey, EngineError};

/// Attach WebView2's `AcceleratorKeyPressed` — the only key stream that
/// fires while the view holds focus. The controller raises it before the
/// engine processes the key, so `SetHandled` decides whether WebView2 or
/// the page ever sees it. Returns the handler so the caller can keep it
/// alive for the view's lifetime.
pub(in crate::engine::wry_impl) fn attach_accelerator(
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
