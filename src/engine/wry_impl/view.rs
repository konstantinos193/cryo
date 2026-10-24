//! `WryView`: `ContentView` over a `wry::WebView`. Construction wires
//! the COM event handlers; the trait impl calls wry where its API
//! reaches and dispatches to `ops` (raw WebView2/Win32) where it doesn't.

#[cfg(target_os = "windows")]
use std::cell::{Cell, RefCell};
#[cfg(target_os = "windows")]
use std::rc::Rc;
#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicBool;
#[cfg(target_os = "windows")]
use std::sync::Arc;

#[cfg(target_os = "windows")]
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2AcceleratorKeyPressedEventHandler,
    ICoreWebView2ContainsFullScreenElementChangedEventHandler,
    ICoreWebView2DownloadStartingEventHandler, ICoreWebView2FaviconChangedEventHandler,
    ICoreWebView2NavigationCompletedEventHandler, ICoreWebView2NavigationStartingEventHandler,
    ICoreWebView2ProcessFailedEventHandler,
};
use wry::WebView;

#[cfg(target_os = "windows")]
use crate::engine::LoadFailure;
#[cfg(target_os = "windows")]
use crate::engine::{
    AcceleratorHandler, DownloadHandler, FaviconHandler, FullscreenHandler, LoadChangeHandler,
    LoadFailureHandler,
};
use crate::engine::{ContentView, Cookie, DownloadId, EngineError, Rect};

#[cfg(target_os = "windows")]
use super::delay::Delayer;
use super::to_wry_rect;
#[cfg(target_os = "windows")]
use super::{downloads, events, ops};

/// Every callback a fresh view can wire — grouped so `new` doesn't take
/// one parameter per event kind. `delayer` is plumbing, not a callback:
/// the shared worker the download-abort grace re-check runs on.
pub(super) struct Handlers {
    #[cfg(target_os = "windows")]
    pub on_accelerator: Option<AcceleratorHandler>,
    #[cfg(target_os = "windows")]
    pub on_load_failure: Option<LoadFailureHandler>,
    #[cfg(target_os = "windows")]
    pub on_fullscreen: Option<FullscreenHandler>,
    #[cfg(target_os = "windows")]
    pub on_favicon: Option<FaviconHandler>,
    #[cfg(target_os = "windows")]
    pub on_navigate: Option<Rc<dyn Fn(String) -> bool>>,
    #[cfg(target_os = "windows")]
    pub on_download: Option<DownloadHandler>,
    #[cfg(target_os = "windows")]
    pub on_load_change: Option<LoadChangeHandler>,
    #[cfg(target_os = "windows")]
    pub delayer: Delayer,
}

pub(super) struct WryView {
    view: WebView,
    /// Keep the COM callbacks alive; unregistration dies with the view.
    #[cfg(target_os = "windows")]
    _accel_handler: Option<ICoreWebView2AcceleratorKeyPressedEventHandler>,
    #[cfg(target_os = "windows")]
    _download_handler: Option<ICoreWebView2DownloadStartingEventHandler>,
    /// Live downloads on this view — `cancel_download` looks them up;
    /// each download's `StateChanged` removes its own entry at the end.
    #[cfg(target_os = "windows")]
    downloads: downloads::ActiveDownloads,
    #[cfg(target_os = "windows")]
    _navigation_handler: Option<ICoreWebView2NavigationCompletedEventHandler>,
    #[cfg(target_os = "windows")]
    _nav_start_handler: Option<ICoreWebView2NavigationStartingEventHandler>,
    #[cfg(target_os = "windows")]
    _process_handler: Option<ICoreWebView2ProcessFailedEventHandler>,
    #[cfg(target_os = "windows")]
    _fullscreen_handler: Option<ICoreWebView2ContainsFullScreenElementChangedEventHandler>,
    #[cfg(target_os = "windows")]
    _favicon_handler: Option<ICoreWebView2FaviconChangedEventHandler>,
}

impl WryView {
    pub(super) fn new(view: WebView, handlers: Handlers) -> Result<Self, EngineError> {
        #[cfg(target_os = "windows")]
        {
            let Handlers {
                on_accelerator,
                on_load_failure,
                on_fullscreen,
                on_favicon,
                on_navigate,
                on_download,
                on_load_change,
                delayer,
            } = handlers;
            let _accel_handler = on_accelerator
                .map(|cb| events::attach_accelerator(&view, cb))
                .transpose()?;
            // Both events feed one callback; the download-abort check may
            // also report it from the delay worker, so it needs Arc +
            // Send + Sync.
            let report: Option<Arc<dyn Fn(LoadFailure) + Send + Sync>> =
                on_load_failure.map(Arc::from);
            // DownloadStarting ⇄ NavigationCompleted: the flag marks the
            // abort that means "this navigation became a download" — in
            // either arrival order.
            let download_started = Arc::new(AtomicBool::new(false));
            // NavigationStarting ⇄ NavigationCompleted: the id of the
            // newest navigation, so a stale completion can't clear the
            // loading signal a newer navigation is still using. The
            // attempted URI and retry budget feed the completion's one
            // silent retry for unclassified failures.
            let latest_nav = Rc::new(Cell::new(0u64));
            let attempted = Rc::new(RefCell::new(String::new()));
            let retry_budget = Rc::new(Cell::new(events::UNCLASSIFIED_RETRIES));
            // The starting handler records the URI even when the app
            // didn't ask for a loading signal, so it attaches whenever
            // the completion handler exists.
            let nav_start_needed = report.is_some() || on_load_change.is_some();
            let on_load_change: Option<Rc<dyn Fn(bool)>> = on_load_change.map(Rc::from);
            let _nav_start_handler = nav_start_needed
                .then(|| {
                    events::attach_navigation_starting(
                        &view,
                        on_load_change.clone().unwrap_or_else(|| Rc::new(|_| {})),
                        latest_nav.clone(),
                        attempted.clone(),
                    )
                })
                .transpose()?;
            let _navigation_handler = nav_start_needed
                .then(|| {
                    events::attach_navigation_completed(
                        &view,
                        report.clone(),
                        events::NavState {
                            download_started: download_started.clone(),
                            on_load_change: on_load_change.clone(),
                            latest_nav: latest_nav.clone(),
                            attempted: attempted.clone(),
                            retry_budget: retry_budget.clone(),
                        },
                        delayer,
                    )
                })
                .transpose()?;
            let downloads = downloads::ActiveDownloads::default();
            let _download_handler = downloads::attach(
                &view,
                download_started,
                on_navigate,
                on_download.map(Rc::from),
                downloads.clone(),
            )?;
            let _process_handler = report
                .map(|cb| events::attach_process_failed(&view, cb))
                .transpose()?;
            let _fullscreen_handler = on_fullscreen
                .map(|cb| events::attach_fullscreen_change(&view, cb))
                .transpose()?;
            let _favicon_handler = on_favicon
                .map(|cb| events::attach_favicon(&view, cb))
                .transpose()?
                .flatten();
            Ok(Self {
                view,
                _accel_handler,
                _download_handler,
                downloads,
                _navigation_handler,
                _nav_start_handler,
                _process_handler,
                _fullscreen_handler,
                _favicon_handler,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = handlers;
            Ok(Self { view })
        }
    }
}

impl ContentView for WryView {
    fn load_url(&self, url: &str) -> Result<(), EngineError> {
        self.view
            .load_url(url)
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn reload(&self) -> Result<(), EngineError> {
        self.view
            .reload()
            .map_err(|e| EngineError::Navigate(e.to_string()))
    }

    fn hard_reload(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::hard_reload(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("location.reload()")
        }
    }

    fn go_back(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::go_back(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.back()")
        }
    }

    fn go_forward(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::go_forward(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.forward()")
        }
    }

    fn can_go_back(&self) -> Result<bool, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::can_go_back(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn can_go_forward(&self) -> Result<bool, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::can_go_forward(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn stop(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::stop(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("window.stop()")
        }
    }

    fn set_visible(&self, visible: bool) -> Result<(), EngineError> {
        self.view
            .set_visible(visible)
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn set_bounds(&self, bounds: Rect) -> Result<(), EngineError> {
        self.view
            .set_bounds(to_wry_rect(bounds))
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn bring_to_front(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::bring_to_front(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn focus(&self) -> Result<(), EngineError> {
        self.view
            .focus()
            .map_err(|e| EngineError::Layout(e.to_string()))
    }

    fn eval(&self, js: &str) -> Result<(), EngineError> {
        self.view
            .evaluate_script(js)
            .map_err(|e| EngineError::Script(e.to_string()))
    }

    fn url(&self) -> Option<String> {
        self.view.url().ok()
    }

    fn open_devtools(&self) {
        #[cfg(debug_assertions)]
        self.view.open_devtools();
    }

    fn muted(&self) -> Result<bool, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::muted(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn set_muted(&self, muted: bool) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::set_muted(&self.view, muted)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = muted;
            Ok(())
        }
    }

    fn zoom_factor(&self) -> Result<f64, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::zoom_factor(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(1.0)
        }
    }

    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::set_zoom_factor(&self.view, factor)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval(&format!("document.body.style.zoom='{factor}'"))
        }
    }

    #[cfg(target_os = "windows")]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        ops::capture_png(&self.view)
    }

    #[cfg(not(target_os = "windows"))]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::View("capture unsupported".into()))
    }

    fn save_as(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::save_as(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(EngineError::View("save-as unsupported".into()))
        }
    }

    fn clear_browsing_data(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::clear_browsing_data(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn cookies(&self, uri: &str) -> Result<Vec<Cookie>, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::cookies(&self.view, uri)
        }
        #[cfg(not(target_os = "windows"))]
        {
            // wry reaches no cookie API off Windows — an empty jar reads
            // as signed-out, which is the honest answer.
            let _ = uri;
            Ok(Vec::new())
        }
    }

    fn cancel_download(&self, id: DownloadId) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            downloads::cancel(&self.downloads, id)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Ok(())
        }
    }

    fn pause_download(&self, id: DownloadId) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            downloads::pause(&self.downloads, id)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Ok(())
        }
    }

    fn resume_download(&self, id: DownloadId) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            downloads::resume(&self.downloads, id)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Ok(())
        }
    }

    fn suspend(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::suspend(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn resume(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::resume(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }

    fn is_suspended(&self) -> Result<bool, EngineError> {
        #[cfg(target_os = "windows")]
        {
            ops::is_suspended(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn playing_audio(&self) -> Option<bool> {
        #[cfg(target_os = "windows")]
        {
            ops::playing_audio(&self.view)
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }
}
