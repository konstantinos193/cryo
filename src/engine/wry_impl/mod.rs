//! WebView2 backend: adapts `wry` to the `Engine`/`ContentView` traits.
//! The only module allowed to name `wry` types.
//!
//! `protocol` answers the `cryo` scheme, `view` is the `ContentView`
//! impl. Windows adds `events` (COM event wiring), `ops` (the COM
//! operations wry doesn't reach), `downloads` (the file shelf's engine
//! side) and `delay` (the shared deferred worker).

mod accounting;
#[cfg(target_os = "windows")]
mod delay;
#[cfg(target_os = "windows")]
mod downloads;
#[cfg(target_os = "windows")]
mod events;
#[cfg(target_os = "windows")]
mod extensions;
#[cfg(target_os = "windows")]
mod ops;
#[cfg(target_os = "windows")]
mod permission_profile;
#[cfg(target_os = "windows")]
mod permissions;
mod protocol;
#[cfg(target_os = "windows")]
pub(crate) mod pump;
mod view;

#[cfg(target_os = "windows")]
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use raw_window_handle::{RawWindowHandle, WindowHandle};
#[cfg(target_os = "windows")]
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
#[cfg(target_os = "windows")]
use wry::WebViewBuilderExtWindows;
use wry::{NewWindowResponse, Rect as WryRect, WebViewBuilder};

#[cfg(target_os = "windows")]
use delay::Delayer;
use protocol::INTERNAL_SCHEME;
use view::{Handlers, WryView};

use crate::engine::{
    ContentView, DropEvent, Engine, EngineError, PageMessage, PageResolver, Rect, ViewSpec,
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
    /// One WebView2 environment for every view. Without it wry creates
    /// one per spawn — each paying the browser-process spin-up, which is
    /// the bulk of the startup delay. Lazy: created on the first spawn so
    /// engine construction stays infallible. Incognito is a controller
    /// option, not an environment one, so all views can share it.
    #[cfg(target_os = "windows")]
    environment: RefCell<Option<ICoreWebView2Environment>>,
    /// Directory of unpacked extensions to load, or `None` for a window
    /// that runs without them. Incognito passes `None`: extensions see
    /// every page they are granted, and a private window is not the place
    /// to decide that silently.
    #[cfg(target_os = "windows")]
    extensions_dir: Option<std::path::PathBuf>,
    /// Extensions are installed into the profile, not into a view, and
    /// WebView2 keeps them there. Loading on every spawn would re-add the
    /// same set per tab — each one a COM wait that pumps the message queue,
    /// which is the hazard `pump` exists for — so the first content view
    /// of the process does it and the rest inherit the result.
    #[cfg(target_os = "windows")]
    extensions_loaded: std::cell::Cell<bool>,
    /// The profile the loaded extensions belong to, taken from the first
    /// content view. Extensions are registered against the profile rather
    /// than a view, so any view's profile answers for all of them — but
    /// there is no profile at all until one view exists.
    #[cfg(target_os = "windows")]
    extension_profile:
        RefCell<Option<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Profile7>>,
}

impl WryEngine {
    pub fn new(
        parent: RawWindowHandle,
        resolve_page: PageResolver,
        incognito: bool,
        #[cfg_attr(not(target_os = "windows"), allow(unused_variables))] extensions_dir: Option<
            std::path::PathBuf,
        >,
    ) -> Self {
        Self {
            parent,
            resolve_page,
            incognito,
            #[cfg(target_os = "windows")]
            delayer: Delayer::new(),
            #[cfg(target_os = "windows")]
            environment: RefCell::new(None),
            #[cfg(target_os = "windows")]
            extensions_dir,
            #[cfg(target_os = "windows")]
            extensions_loaded: std::cell::Cell::new(false),
            #[cfg(target_os = "windows")]
            extension_profile: RefCell::new(None),
        }
    }

    /// The profile to ask about extensions, if a view has given us one yet.
    #[cfg(target_os = "windows")]
    fn extension_profile(
        &self,
    ) -> Option<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Profile7> {
        self.extension_profile.borrow().clone()
    }

    /// The shared environment, created once — the same options wry
    /// would apply per-view (browser args, OS UI language), so passing
    /// it to `with_environment` changes nothing but who pays the cost.
    #[cfg(target_os = "windows")]
    fn environment(&self) -> Result<ICoreWebView2Environment, EngineError> {
        if let Some(env) = &*self.environment.borrow() {
            return Ok(env.clone());
        }
        let env = create_environment(self.extensions_dir.is_some())?;
        *self.environment.borrow_mut() = Some(env.clone());
        Ok(env)
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

/// Middle-click is a browser-chrome gesture WebView2 never routes to
/// `NewWindowRequested` — the navigation just lands in the same view.
/// When the app asks for new-window requests at all, this script gives
/// it the complete set: a captured auxclick on an anchor is turned into
/// a `window.open`, which does raise the event (and which we then deny
/// in favour of our own tab). Middle-click off a link keeps its engine
/// default (autoscroll); javascript: and non-http(s) hrefs stay in the
/// page's hands.
const AUX_CLICK_JS: &str = r#"addEventListener('auxclick',e=>{
if(e.button!==1)return;
const a=e.target&&e.target.closest?e.target.closest('a[href]'):null;
if(!a||!/^https?:/.test(a.href))return;
e.preventDefault();
window.open(a.href,'_blank');
},true);"#;

#[cfg(target_os = "windows")]
fn browser_args() -> String {
    let mut args = String::from(BROWSER_ARGS);
    if let Ok(extra) = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") {
        args.push(' ');
        args.push_str(extra.trim());
    }
    args
}

/// Build the one `ICoreWebView2Environment` every view shares — the same
/// call wry makes inside `build_as_child`, hoisted here so it runs once.
/// Null executable folder and user-data folder mean WebView2's defaults,
/// which is what a wry view with no custom context already gets.
#[cfg(target_os = "windows")]
/// `extensions` turns on `AreBrowserExtensionsEnabled`. It has to be decided
/// here rather than on the builder: wry sets the same option during *its*
/// environment creation, which `with_environment` replaces, so a builder
/// flag alone would leave the environment refusing installs. WebView2
/// requires runtime 120.0.2210.55 or newer for this; older runtimes ignore
/// the option and extensions simply never appear.
fn create_environment(extensions: bool) -> Result<ICoreWebView2Environment, EngineError> {
    use std::sync::mpsc::channel;
    use webview2_com::{
        CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2EnvironmentOptions,
        },
    };
    use windows::core::{Error as WinError, PCWSTR};
    use windows::Win32::Foundation::{E_POINTER, E_UNEXPECTED};
    use windows::Win32::Globalization::{
        GetUserDefaultUILanguage, LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES, MAX_LOCALE_NAME,
    };

    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        options.set_additional_browser_arguments(browser_args());
        options.set_are_browser_extensions_enabled(extensions);
        let lcid = GetUserDefaultUILanguage();
        let mut lang = [0u16; MAX_LOCALE_NAME as usize];
        LCIDToLocaleName(lcid as u32, Some(&mut lang), LOCALE_ALLOW_NEUTRAL_NAMES);
        options.set_language(String::from_utf16_lossy(&lang));
    }

    let (tx, rx) = channel();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |error_code, environment| {
            let result = (|| {
                error_code?;
                environment.ok_or_else(|| WinError::from(E_POINTER))
            })();
            tx.send(result).map_err(|_| WinError::from(E_UNEXPECTED))
        },
    ));
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            PCWSTR::null(),
            &ICoreWebView2EnvironmentOptions::from(options),
            &handler,
        )
    }
    .map_err(|e| EngineError::Spawn(e.to_string()))?;
    pump::wait_with_pump(rx)
        .map_err(|e| EngineError::Spawn(e.to_string()))?
        .map_err(|e| EngineError::Spawn(e.to_string()))
}

impl Engine for WryEngine {
    /// Nothing is loaded before the first content view exists, and an empty
    /// list is the true answer then — not a failure.
    #[cfg(target_os = "windows")]
    fn loaded_extensions(&self) -> Result<Vec<crate::engine::LoadedExtension>, EngineError> {
        match self.extension_profile() {
            Some(profile) => extensions::loaded(&profile),
            None => Ok(Vec::new()),
        }
    }

    #[cfg(target_os = "windows")]
    fn set_extension_enabled(&self, id: &str, enabled: bool) -> Result<(), EngineError> {
        let profile = self.extension_profile().ok_or(EngineError::Unsupported(
            "no view has loaded extensions yet",
        ))?;
        extensions::set_enabled(&profile, id, enabled)
    }

    #[cfg(target_os = "windows")]
    fn remove_extension(&self, id: &str) -> Result<(), EngineError> {
        let profile = self.extension_profile().ok_or(EngineError::Unsupported(
            "no view has loaded extensions yet",
        ))?;
        extensions::remove(&profile, id)
    }

    #[cfg(target_os = "windows")]
    fn measure_memory(
        &self,
        complete: crate::engine::accounting::MemoryCallback,
    ) -> Result<(), EngineError> {
        accounting::measure(self.environment.borrow().as_ref(), complete)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn measure_memory(
        &self,
        complete: crate::engine::accounting::MemoryCallback,
    ) -> Result<(), EngineError> {
        accounting::measure(complete)
    }

    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let access = spec.access;
        let internal = Arc::new(AtomicBool::new(
            spec.url.as_deref().is_some_and(protocol::internal_url),
        ));
        let protocol_internal = internal.clone();
        let profile_ready = Arc::new(AtomicBool::new(!cfg!(target_os = "windows")));
        let resolve_page = self.resolve_page;
        let on_ipc = spec.on_ipc;
        let builder = WebViewBuilder::new()
            .with_bounds(to_wry_rect(spec.bounds))
            .with_visible(spec.visible)
            .with_transparent(spec.transparent)
            .with_devtools(cfg!(debug_assertions));
        // The shared environment already carries the browser args —
        // `with_additional_browser_args` only feeds wry's per-view
        // environment creation, which sharing replaces.
        #[cfg(target_os = "windows")]
        let builder = builder.with_environment(self.environment()?);
        // Extensions belong to content views: the chrome document is ours
        // and does not host anyone else's scripts. wry loads them during a
        // build and only when this flag is set on the builder too — the
        // environment option alone unlocks installing, not loading.
        #[cfg(target_os = "windows")]
        let load_extensions = self
            .extensions_dir
            .as_ref()
            .filter(|_| access != crate::engine::ViewAccess::Chrome)
            .filter(|_| !self.extensions_loaded.get());
        #[cfg(target_os = "windows")]
        let builder = match load_extensions {
            Some(dir) => builder
                .with_browser_extensions_enabled(true)
                .with_extensions_path(dir.clone()),
            None => builder,
        };
        let mut builder = builder
            // The scheme exists for chrome IPC as much as for pages —
            // see `protocol::respond` for why it must be a real URL.
            .with_custom_protocol(INTERNAL_SCHEME.into(), move |_id, request| {
                protocol::respond(&request, &on_ipc, resolve_page, access, &protocol_internal)
            });

        if self.incognito {
            builder = builder.with_incognito(true);
        }
        if let Some(background) = spec.background {
            builder = builder.with_background_color(background);
        }
        if let Some(script) = spec.page_script {
            builder = builder.with_initialization_script_for_main_only(script, true);
        }
        if let Some(on_message) = spec.on_page_message {
            builder = builder.with_ipc_handler(move |request| {
                on_message(PageMessage {
                    source: request.uri().to_string(),
                    body: request.into_body(),
                });
            });
        }
        let initial_url = spec.url;
        #[cfg(not(target_os = "windows"))]
        if let Some(url) = &initial_url {
            builder = builder.with_url(url);
        }
        if let Some(on_title) = spec.on_title_change {
            builder = builder.with_document_title_changed_handler(on_title);
        }
        // `Rc` rather than the spec's `Box`: the DownloadStarting flag also
        // reports through it (a navigation that became a download re-reports
        // the address the view still shows).
        let on_navigate: Option<Rc<dyn Fn(String) -> bool>> = spec.on_navigate.map(Rc::from);
        let on_nav = on_navigate.clone();
        let navigation_internal = internal.clone();
        let navigation_ready = profile_ready.clone();
        builder = builder.with_navigation_handler(move |uri| {
            let is_internal = protocol::internal_url(&uri);
            if !is_internal && uri != "about:blank" && !navigation_ready.load(Ordering::Acquire) {
                return false;
            }
            let path = url::Url::parse(&uri).ok().map(|u| u.path().to_string());
            if access == crate::engine::ViewAccess::Chrome
                && (!is_internal || path.as_deref() != Some("/chrome"))
            {
                return false;
            }
            if is_internal
                && !path.as_deref().is_some_and(|p| {
                    protocol::allowed(access, navigation_internal.load(Ordering::Acquire), p)
                })
            {
                return false;
            }
            let accepted = on_nav.as_ref().is_none_or(|cb| cb(uri));
            if accepted && !is_internal {
                navigation_internal.store(false, Ordering::Release);
            }
            accepted
        });
        if let Some(on_new_window) = spec.on_new_window {
            builder = builder.with_new_window_req_handler(move |url, _features| {
                on_new_window(url);
                NewWindowResponse::Deny
            });
            builder = builder.with_initialization_script(AUX_CLICK_JS);
        }
        #[cfg(not(target_os = "windows"))]
        {
            builder = builder.with_permission_handler(|_| wry::PermissionResponse::Deny);
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

        // `build_as_child` waits on its own pump; clear queued paints or a
        // mid-spawn WM_PAINT re-enters tao's handler (see `pump`).
        #[cfg(target_os = "windows")]
        pump::clear_pending_paints();

        // SAFETY: `parent` is the handle of the window `App` owns for the
        // whole event loop — no spawn can outlive it.
        let parent = unsafe { WindowHandle::borrow_raw(self.parent) };
        let view = builder
            .build_as_child(&parent)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        // Only now: a spawn that failed loaded nothing, and the next
        // content view should still get the chance to.
        #[cfg(target_os = "windows")]
        if load_extensions.is_some() {
            self.extensions_loaded.set(true);
        }
        // Keep the first content view's profile: it is how the pane asks what
        // the engine actually loaded, rather than reporting folder contents
        // as though they were running.
        #[cfg(target_os = "windows")]
        if access != crate::engine::ViewAccess::Chrome && self.extension_profile.borrow().is_none()
        {
            use webview2_com::Microsoft::Web::WebView2::Win32::{
                ICoreWebView2Profile7, ICoreWebView2_13,
            };
            use windows::core::Interface;
            use wry::WebViewExtWindows;
            let profile = (|| -> windows::core::Result<ICoreWebView2Profile7> {
                let webview = unsafe { view.controller().CoreWebView2()? };
                unsafe { webview.cast::<ICoreWebView2_13>()?.Profile()? }.cast()
            })();
            match profile {
                Ok(profile) => *self.extension_profile.borrow_mut() = Some(profile),
                // An older runtime has no profile interface for this. The
                // pane then shows the catalogue alone, which is the honest
                // fallback rather than a failed spawn.
                Err(e) => crate::report!("no extension profile on this runtime: {e}"),
            }
        }
        let view = WryView::new(
            view,
            internal,
            profile_ready,
            Handlers {
                #[cfg(target_os = "windows")]
                on_permission: spec.on_permission,
                #[cfg(target_os = "windows")]
                on_accelerator: spec.on_accelerator,
                #[cfg(target_os = "windows")]
                on_load_failure: spec.on_load_failure,
                #[cfg(target_os = "windows")]
                on_fullscreen: spec.on_fullscreen,
                #[cfg(target_os = "windows")]
                on_favicon: spec.on_favicon,
                #[cfg(target_os = "windows")]
                on_navigate,
                #[cfg(target_os = "windows")]
                on_download: spec.on_download,
                #[cfg(target_os = "windows")]
                on_load_change: spec.on_load_change,
                #[cfg(target_os = "windows")]
                delayer: self.delayer.clone(),
            },
        )?;
        #[cfg(target_os = "windows")]
        if let Some(url) = initial_url {
            view.load_url(&url)?;
        }
        Ok(Box::new(view) as Box<dyn ContentView>)
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
