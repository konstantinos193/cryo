//! WebView2 backend: adapts `wry` to the `Engine`/`ContentView` traits.
//! The only module allowed to name `wry` types.
//!
//! `protocol` answers the `cryo` scheme, `view` is the `ContentView`
//! impl. Windows adds `events` (COM event wiring), `ops` (the COM
//! operations wry doesn't reach), `downloads` (the file shelf's engine
//! side) and `delay` (the shared deferred worker).

#[cfg(target_os = "windows")]
mod delay;
#[cfg(target_os = "windows")]
mod downloads;
#[cfg(target_os = "windows")]
mod events;
#[cfg(target_os = "windows")]
mod ops;
mod protocol;
mod view;

use std::rc::Rc;

use raw_window_handle::{RawWindowHandle, WindowHandle};
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
#[cfg(target_os = "windows")]
use wry::NewWindowResponse;
#[cfg(target_os = "windows")]
use wry::WebViewBuilderExtWindows;
use wry::{
    PermissionKind as WryPermissionKind, PermissionResponse, Rect as WryRect, WebViewBuilder,
};

#[cfg(target_os = "windows")]
use delay::Delayer;
use protocol::INTERNAL_SCHEME;
use view::{Handlers, WryView};

use crate::engine::{
    ContentView, DropEvent, Engine, EngineError, PageResolver, PermissionDecision, PermissionKind,
    Rect, ViewSpec,
};

pub struct WryEngine {
    /// Handle of the window views are spawned into — plain data rather
    /// than the toolkit's `Window` type, so the backend never names tao
    /// (AGENTS §3). The app owns the window and outlives every spawn.
    parent: RawWindowHandle,
    resolve_page: PageResolver,
    /// Whole-process InPrivate mode: every view spawns on an incognito
    /// controller (WebView2 `IsInPrivateModeEnabled`).
    incognito: bool,
    /// The one deferred-work worker — shared by every view, so an
    /// aborted navigation doesn't spawn a sleeper of its own (the
    /// DownloadStarting race; see `events::navigation_completed`).
    #[cfg(target_os = "windows")]
    delayer: Delayer,
}

impl WryEngine {
    pub fn new(parent: RawWindowHandle, resolve_page: PageResolver, incognito: bool) -> Self {
        Self {
            parent,
            resolve_page,
            incognito,
            #[cfg(target_os = "windows")]
            delayer: Delayer::new(),
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

/// Browser flags passed to every WebView2 view.
/// `with_additional_browser_args` *replaces* wry's defaults, so the
/// wry trio (`msWebOOUI,msPdfOOUI,msSmartScreenProtection`) is
/// re-declared here. On top of that, `HardwareSecureDecoding` (and its
/// experiment/fallback companions) is off: hardware-DRM video — F1TV's
/// Widevine path — presents through a protected overlay plane that
/// corrupts into static once the host window goes borderless
/// fullscreen. Software-secure decode composites like any other frame,
/// so fullscreen stays clean. Extra flags can still be merged in via
/// the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` environment variable.
#[cfg(target_os = "windows")]
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,\
     msSmartScreenProtection,HardwareSecureDecoding,\
     HardwareSecureDecodingExperiment,HardwareSecureDecodingFallback";

#[cfg(target_os = "windows")]
fn browser_args() -> String {
    let mut args = String::from(BROWSER_ARGS);
    if let Ok(extra) = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        args.push(' ');
        args.push_str(extra.trim());
    }
    args
}

impl Engine for WryEngine {
    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let resolve_page = self.resolve_page;
        let on_ipc = spec.on_ipc;
        let mut builder = WebViewBuilder::new()
            .with_bounds(to_wry_rect(spec.bounds))
            .with_visible(spec.visible)
            .with_transparent(spec.transparent)
            .with_devtools(cfg!(debug_assertions));
        #[cfg(target_os = "windows")]
        {
            builder = builder.with_additional_browser_args(browser_args());
        }
        let mut builder = builder
            // The scheme exists for chrome IPC as much as for pages —
            // see `protocol::respond` for why it must be a real URL.
            .with_custom_protocol(INTERNAL_SCHEME.into(), move |_id, request| {
                protocol::respond(&request, &on_ipc, resolve_page)
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
        if let Some(on_drop) = spec.on_drop {
            builder = builder.with_drag_drop_handler(move |event| {
                let mapped = match event {
                    wry::DragDropEvent::Enter { paths, position } => {
                        DropEvent::Entered { paths, position }
                    }
                    wry::DragDropEvent::Over { position } => DropEvent::Over { position },
                    wry::DragDropEvent::Drop { paths, position } => {
                        DropEvent::Dropped { paths, position }
                    }
                    _ => DropEvent::Left,
                };
                // wry ignores the return on Windows — the event is the
                // payload, claiming is automatic there.
                on_drop(mapped);
                true
            });
        }

        // SAFETY: `parent` is the handle of the window `App` owns for the
        // whole event loop — no spawn can outlive it.
        let parent = unsafe { WindowHandle::borrow_raw(self.parent) };
        let view = builder
            .build_as_child(&parent)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        WryView::new(
            view,
            Handlers {
                on_accelerator: spec.on_accelerator,
                on_load_failure: spec.on_load_failure,
                on_fullscreen: spec.on_fullscreen,
                on_favicon: spec.on_favicon,
                on_navigate,
                on_download: spec.on_download,
                on_load_change: spec.on_load_change,
                #[cfg(target_os = "windows")]
                delayer: self.delayer.clone(),
            },
        )
        .map(|v| Box::new(v) as Box<dyn ContentView>)
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::BROWSER_ARGS;

    #[test]
    fn browser_args_keep_wry_defaults() {
        // `with_additional_browser_args` replaces wry's defaults — the
        // string must still carry the trio wry disables itself.
        for feature in ["msWebOOUI", "msPdfOOUI", "msSmartScreenProtection"] {
            assert!(
                BROWSER_ARGS.contains(feature),
                "browser args dropped wry default {feature}"
            );
        }
    }

    #[test]
    fn browser_args_disable_hardware_secure_decoding() {
        // The fix for DRM fullscreen snow lives or dies on this feature
        // being off — a refactor that drops it must fail loudly.
        for feature in [
            "HardwareSecureDecoding",
            "HardwareSecureDecodingExperiment",
            "HardwareSecureDecodingFallback",
        ] {
            assert!(
                BROWSER_ARGS.contains(feature),
                "browser args dropped {feature}"
            );
        }
    }

    #[test]
    fn browser_args_are_a_single_disable_features_flag() {
        // Chromium merges repeated `--disable-features`, but one flag
        // keeps the intent obvious.
        assert_eq!(BROWSER_ARGS.matches("--disable-features").count(), 1);
    }
}
