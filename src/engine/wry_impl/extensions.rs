//! Asking WebView2 about the extensions it has loaded, and switching them.
//!
//! Installing is a folder on disk plus a load at view creation; everything
//! here is about what the engine actually did with that. The profile is the
//! authority: it knows which extensions are registered, which are enabled,
//! and it is what properly uninstalls one — deleting the folder only makes
//! WebView2 drop it as a side effect.
//!
//! Every call is asynchronous with a completion handler, and all of them are
//! made while a tao handler is on the stack. They go through `pump`, which
//! eats `WM_PAINT` instead of dispatching it — a paint delivered inside one
//! of these waits would re-enter tao's handler and panic (see `pump`).

use std::sync::mpsc::channel;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2BrowserExtension, ICoreWebView2Profile7,
};
use webview2_com::{
    BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
    ProfileGetBrowserExtensionsCompletedHandler,
};
use windows::core::Error as WinError;
use windows::Win32::Foundation::E_POINTER;

use super::pump;
use crate::engine::{EngineError, LoadedExtension};

/// Everything the profile has loaded.
pub(super) fn loaded(profile: &ICoreWebView2Profile7) -> Result<Vec<LoadedExtension>, EngineError> {
    let list = get_list(profile)?;
    let mut count = 0u32;
    unsafe { list.Count(&mut count) }.map_err(read_failed)?;
    let mut found = Vec::with_capacity(count as usize);
    for index in 0..count {
        let extension = unsafe { list.GetValueAtIndex(index) }.map_err(read_failed)?;
        found.push(describe(&extension)?);
    }
    Ok(found)
}

/// Switch one on or off. An id that names nothing loaded is an error rather
/// than a silent no-op: the pane offered a control for it, so a toggle that
/// did nothing would be a lie.
pub(super) fn set_enabled(
    profile: &ICoreWebView2Profile7,
    id: &str,
    enabled: bool,
) -> Result<(), EngineError> {
    let extension = find(profile, id)?;
    let (tx, rx) = channel();
    let handler = BrowserExtensionEnableCompletedHandler::create(Box::new(move |code| {
        tx.send(code).map_err(|_| WinError::from(E_POINTER))
    }));
    unsafe { extension.Enable(enabled, &handler) }.map_err(op_failed)?;
    pump::wait_with_pump(rx)
        .map_err(op_failed)?
        .map_err(op_failed)?;
    Ok(())
}

/// Uninstall through the profile. The caller still deletes the folder
/// afterwards — this is what unregisters it.
pub(super) fn remove(profile: &ICoreWebView2Profile7, id: &str) -> Result<(), EngineError> {
    let extension = find(profile, id)?;
    let (tx, rx) = channel();
    let handler = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |code| {
        tx.send(code).map_err(|_| WinError::from(E_POINTER))
    }));
    unsafe { extension.Remove(&handler) }.map_err(op_failed)?;
    pump::wait_with_pump(rx)
        .map_err(op_failed)?
        .map_err(op_failed)?;
    Ok(())
}

fn find(
    profile: &ICoreWebView2Profile7,
    id: &str,
) -> Result<ICoreWebView2BrowserExtension, EngineError> {
    let list = get_list(profile)?;
    let mut count = 0u32;
    unsafe { list.Count(&mut count) }.map_err(read_failed)?;
    for index in 0..count {
        let extension = unsafe { list.GetValueAtIndex(index) }.map_err(read_failed)?;
        if text(|out| unsafe { extension.Id(out) })? == id {
            return Ok(extension);
        }
    }
    Err(EngineError::View(format!("{id} is not loaded")))
}

fn get_list(
    profile: &ICoreWebView2Profile7,
) -> Result<
    webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2BrowserExtensionList,
    EngineError,
> {
    let (tx, rx) = channel();
    let handler =
        ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |code, list| {
            let result = (|| {
                code?;
                list.ok_or_else(|| WinError::from(E_POINTER))
            })();
            tx.send(result).map_err(|_| WinError::from(E_POINTER))
        }));
    unsafe { profile.GetBrowserExtensions(&handler) }.map_err(read_failed)?;
    pump::wait_with_pump(rx)
        .map_err(read_failed)?
        .map_err(read_failed)
}

fn describe(extension: &ICoreWebView2BrowserExtension) -> Result<LoadedExtension, EngineError> {
    let mut enabled = windows::core::BOOL::default();
    unsafe { extension.IsEnabled(&mut enabled) }.map_err(read_failed)?;
    Ok(LoadedExtension {
        id: text(|out| unsafe { extension.Id(out) })?,
        name: text(|out| unsafe { extension.Name(out) })?,
        enabled: enabled.as_bool(),
    })
}

/// Read one of the `PWSTR`-out-parameter getters and free what it allocated.
fn text(
    read: impl FnOnce(*mut windows::core::PWSTR) -> windows::core::Result<()>,
) -> Result<String, EngineError> {
    let mut raw = windows::core::PWSTR::null();
    read(&mut raw).map_err(read_failed)?;
    if raw.is_null() {
        return Ok(String::new());
    }
    // SAFETY: the getter either filled `raw` with a CoTaskMem string or
    // failed above; `to_string` stops at the terminator the API wrote.
    let value = unsafe { raw.to_string() }.unwrap_or_default();
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(raw.as_ptr() as *const _)) };
    Ok(value)
}

fn read_failed(e: impl std::fmt::Display) -> EngineError {
    EngineError::View(format!("the engine's extension list was not readable: {e}"))
}

fn op_failed(e: impl std::fmt::Display) -> EngineError {
    EngineError::View(format!("the engine refused the extension change: {e}"))
}
