//! WebView2 backend: adapts `wry` to the `Engine`/`ContentView` traits.
//! The only module allowed to name `wry` types.

use std::borrow::Cow;
use std::rc::Rc;

use tao::window::Window;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size};
use wry::http::{header::CONTENT_TYPE, Response};
use wry::{Rect as WryRect, WebView, WebViewBuilder};

#[cfg(target_os = "windows")]
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2AcceleratorKeyPressedEventArgs, ICoreWebView2AcceleratorKeyPressedEventHandler,
    ICoreWebView2Profile2, ICoreWebView2_13, ICoreWebView2_25, ICoreWebView2_8,
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
};
#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use wry::{NewWindowResponse, WebViewExtWindows};

use crate::engine::{
    Accelerator, AcceleratorHandler, AcceleratorKey, ContentView, Engine, EngineError,
    PageResolver, Rect, ViewSpec,
};

/// Scheme name registered on every view. On Windows wry rewrites it to
/// `http://cryo.localhost/...`, which is also the form callers navigate to
/// directly (`load_url` does not apply the rewrite).
const INTERNAL_SCHEME: &str = "cryo";

/// Reserved path on the internal scheme: view → app command channel.
/// wry's own `with_ipc_handler` is not used — attaching it leaves the
/// document's scripts unexecuted on WebView2.
const CMD_PATH: &str = "/cmd";

pub struct WryEngine {
    window: Rc<Window>,
    resolve_page: PageResolver,
    /// Whole-process InPrivate mode: every view spawns on an incognito
    /// controller (WebView2 `IsInPrivateModeEnabled`).
    incognito: bool,
}

impl WryEngine {
    pub fn new(window: Rc<Window>, resolve_page: PageResolver, incognito: bool) -> Self {
        Self {
            window,
            resolve_page,
            incognito,
        }
    }
}

fn to_wry_rect(r: Rect) -> WryRect {
    WryRect {
        position: Position::Logical(LogicalPosition::new(r.x, r.y)),
        size: Size::Logical(LogicalSize::new(r.w, r.h)),
    }
}

impl Engine for WryEngine {
    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let resolve_page = self.resolve_page;
        let on_ipc = spec.on_ipc;
        let mut builder = WebViewBuilder::new()
            .with_bounds(to_wry_rect(spec.bounds))
            .with_visible(spec.visible)
            .with_devtools(cfg!(debug_assertions))
            // Internal pages must arrive over a real `http` URL: pages loaded
            // via NavigateToString land on a `data:` URI where WebView2 never
            // runs the document's own scripts, and wry's IPC bridge drops
            // messages whose Source URI fails `http::Uri` parsing — `data:`
            // does. Both directions of chrome IPC depend on this scheme.
            .with_custom_protocol(INTERNAL_SCHEME.into(), move |_id, request| {
                let path = request.uri().path();
                if path == CMD_PATH {
                    if let Some(cb) = &on_ipc {
                        cb(String::from_utf8_lossy(request.body()).into_owned());
                    }
                    return Response::builder()
                        .status(200)
                        .body(Cow::Borrowed(&b""[..]))
                        .expect("static response is valid");
                }
                match resolve_page(path) {
                    Some(html) => Response::builder()
                        .header(CONTENT_TYPE, "text/html; charset=utf-8")
                        .body(Cow::Borrowed(html.as_bytes()))
                        .expect("static headers are valid"),
                    None => Response::builder()
                        .status(404)
                        .body(Cow::Borrowed(&b"not found"[..]))
                        .expect("static response is valid"),
                }
            });

        if self.incognito {
            builder = builder.with_incognito(true);
        }
        if let Some(url) = spec.url {
            builder = builder.with_url(&url);
        }
        if let Some(on_title) = spec.on_title_change {
            builder = builder.with_document_title_changed_handler(on_title);
        }
        if let Some(on_nav) = spec.on_navigate {
            builder = builder.with_navigation_handler(on_nav);
        }
        if let Some(on_new_window) = spec.on_new_window {
            builder = builder.with_new_window_req_handler(move |url, _features| {
                on_new_window(url);
                NewWindowResponse::Deny
            });
        }

        let view = builder
            .build_as_child(&*self.window)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        WryView::new(view, spec.on_accelerator).map(|v| Box::new(v) as Box<dyn ContentView>)
    }
}

/// Attach WebView2's `AcceleratorKeyPressed` — the only key stream that
/// fires while the view holds focus. The controller raises it before the
/// engine processes the key, so `SetHandled` decides whether WebView2 or
/// the page ever sees it. Returns the handler so the caller can keep it
/// alive for the view's lifetime.
#[cfg(target_os = "windows")]
fn attach_accelerator(
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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

struct WryView {
    view: WebView,
    /// Keeps the COM callback alive; unregistration dies with the view.
    #[cfg(target_os = "windows")]
    _accel_handler: Option<ICoreWebView2AcceleratorKeyPressedEventHandler>,
}

impl WryView {
    fn new(view: WebView, on_accelerator: Option<AcceleratorHandler>) -> Result<Self, EngineError> {
        #[cfg(target_os = "windows")]
        {
            let _accel_handler = on_accelerator
                .map(|cb| attach_accelerator(&view, cb))
                .transpose()?;
            Ok(Self {
                view,
                _accel_handler,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = on_accelerator;
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
            // `Page.reload {ignoreCache:true}` over the DevTools protocol —
            // WebView2's own reload API has no cache-bypass variant.
            let done = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                |_hr, _json| Ok(()),
            ));
            unsafe {
                self.view.webview().CallDevToolsProtocolMethod(
                    &windows::core::HSTRING::from("Page.reload"),
                    &windows::core::HSTRING::from(r#"{"ignoreCache":true}"#),
                    &done,
                )
            }
            .map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("location.reload()")
        }
    }

    fn go_back(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv = self.view.webview();
                let mut can = windows::core::BOOL::default();
                wv.CanGoBack(&mut can)
                    .and_then(|_| if can.as_bool() { wv.GoBack() } else { Ok(()) })
            }
            .map_err(|e| EngineError::Navigate(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.back()")
        }
    }

    fn go_forward(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv = self.view.webview();
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
        #[cfg(not(target_os = "windows"))]
        {
            self.eval("history.forward()")
        }
    }

    fn stop(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe { self.view.webview().Stop() }.map_err(|e| EngineError::Navigate(e.to_string()))
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
            unsafe {
                let wv: ICoreWebView2_8 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let mut muted = windows::core::BOOL::default();
                wv.IsMuted(&mut muted)
                    .map_err(|e| EngineError::View(e.to_string()))?;
                Ok(muted.as_bool())
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    fn set_muted(&self, muted: bool) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_8 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                wv.SetIsMuted(muted)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
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
            let mut factor = 1.0f64;
            unsafe { self.view.controller().ZoomFactor(&mut factor) }
                .map_err(|e| EngineError::View(e.to_string()))?;
            Ok(factor)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(1.0)
        }
    }

    fn set_zoom_factor(&self, factor: f64) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe { self.view.controller().SetZoomFactor(factor) }
                .map_err(|e| EngineError::View(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.eval(&format!("document.body.style.zoom='{factor}'"))
        }
    }

    /// CapturePreview streams PNG bytes into an in-memory IStream; the
    /// completion handler signals a channel `wait_with_pump` blocks on —
    /// the event loop keeps spinning while WebView2 writes.
    #[cfg(target_os = "windows")]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        use windows::Win32::Foundation::HGLOBAL;
        use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
        use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};

        unsafe {
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)
                .map_err(|e| EngineError::View(e.to_string()))?;
            let (tx, rx) = std::sync::mpsc::channel();
            let done = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |hr| {
                let _ = tx.send(hr);
                Ok(())
            }));
            self.view
                .webview()
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

    #[cfg(not(target_os = "windows"))]
    fn capture_png(&self) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::View("capture unsupported".into()))
    }

    fn save_as(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_25 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let done =
                    webview2_com::ShowSaveAsUICompletedHandler::create(Box::new(|_hr, _result| {
                        Ok(())
                    }));
                wv.ShowSaveAsUI(&done)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(EngineError::View("save-as unsupported".into()))
        }
    }

    fn clear_browsing_data(&self) -> Result<(), EngineError> {
        #[cfg(target_os = "windows")]
        {
            unsafe {
                let wv: ICoreWebView2_13 = self
                    .view
                    .webview()
                    .cast()
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let profile: ICoreWebView2Profile2 = wv
                    .Profile()
                    .and_then(|p| p.cast())
                    .map_err(|e| EngineError::View(e.to_string()))?;
                let done =
                    webview2_com::ClearBrowsingDataCompletedHandler::create(Box::new(|_hr| Ok(())));
                profile
                    .ClearBrowsingDataAll(&done)
                    .map_err(|e| EngineError::View(e.to_string()))
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(())
        }
    }
}
