//! Servo backend (Phase 5): adapts `servo` to the `Engine`/`ContentView`
//! traits. `servo` types may appear only in this module (AGENTS §3) — like
//! wry, the only other place they exist is nowhere.
//!
//! Each view gets a real OS child window with its own `WindowRenderingContext`
//! — the same model as WebView2's per-view HWNDs — so the platform composites
//! z-order and bounds; no custom blitter. Windows-only surface plumbing so
//! far: other platforms refuse `spawn` until a native child exists there.
//!
//! Wiring: Servo asks for a turn through `EventLoopWaker`, which posts
//! `AppEvent::EngineWake`; `App::handle_event` calls `Engine::spin_event_loop`
//! on every dispatch. `Servo` is `Rc`-backed — everything runs on the event
//! loop thread, like `WryEngine`.
//!
//! Known gaps vs the wry backend:
//! - `measure_memory` declines — Servo is single-process, so content memory
//!   is the host process's; a budget adapter would measure the host and is
//!   a deliberate follow-up, not a port of the WebView2/tree walkers.
//! - suspend/resume, mute, downloads, save-as, cookies, data clearing and
//!   deferred permissions have no Servo API yet — they deny or no-op.
//! - `window.ipc.postMessage` doesn't exist; pages reach the app by POSTing
//!   to the internal scheme, which `WebViewDelegate::load_web_resource`
//!   intercepts.
//!
//! Built with `--features servo-engine`. The module is written against the
//! servo 0.7 embedding docs; signatures marked `SERVO-API` were checked
//! against doc.servo.org but not yet compiled (the servo build needs
//! clang/cmake/mozjs locally).

#[cfg(target_os = "windows")]
mod child_window;
mod view;

use std::rc::Rc;
use std::sync::Arc;

use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};

use crate::engine::{ContentView, Engine, EngineError, PageResolver, ViewSpec};

/// Bridges Servo's wake requests into the app event loop. `wake` may arrive
/// on any thread — the closure posts `AppEvent::EngineWake` back to ours.
struct ProxyWaker(Arc<dyn Fn() + Send + Sync>);

impl servo::EventLoopWaker for ProxyWaker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(Self(self.0.clone()))
    }

    fn wake(&self) {
        (self.0)()
    }
}

/// One engine per window. `parent` is the window every view's child HWND
/// hangs off; `display` is the raw display handle surfman contexts need.
/// Both are plain data — the backend never names tao (AGENTS §3).
pub struct ServoEngine {
    servo: servo::Servo,
    parent: RawWindowHandle,
    display: RawDisplayHandle,
    resolve_page: PageResolver,
    /// Servo profile separation is a follow-up — incognito windows already
    /// get session-only app stores (AGENTS §4); engine-level forgetting is
    /// still to wire.
    #[expect(dead_code)]
    incognito: bool,
}

impl ServoEngine {
    /// `wake` is called (possibly off-thread) whenever Servo wants a spin —
    /// the composition root wires it to `AppEvent::EngineWake`.
    pub fn new(
        parent: RawWindowHandle,
        display: RawDisplayHandle,
        resolve_page: PageResolver,
        wake: impl Fn() + Send + Sync + 'static,
        incognito: bool,
    ) -> Self {
        let servo = servo::ServoBuilder::default()
            .event_loop_waker(Box::new(ProxyWaker(Arc::new(wake))))
            .build();
        Self {
            servo,
            parent,
            display,
            resolve_page,
            incognito,
            _pin: (),
        }
    }

    #[cfg(target_os = "windows")]
    fn spawn_surface(&self, spec: &ViewSpec) -> Result<Surface, EngineError> {
        let child = child_window::ChildWindow::create(self.parent, spec.bounds, spec.visible)?;
        let display = unsafe { DisplayHandle::borrow_raw(self.display) };
        let window = unsafe { WindowHandle::borrow_raw(child.raw_handle()) };
        let size = child.physical_size();
        let rc = servo::WindowRenderingContext::new(display, window, size)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        Ok(Surface {
            child,
            context: Rc::new(rc),
        })
    }

    #[cfg(not(target_os = "windows"))]
    fn spawn_surface(&self, _spec: &ViewSpec) -> Result<Surface, EngineError> {
        Err(EngineError::Spawn(
            "the servo backend has no surface plumbing off Windows yet".into(),
        ))
    }
}

/// The window-side half of a view: its child HWND and the surfman context
/// Servo paints into. `view::ServoView` owns one per spawned surface.
struct Surface {
    #[cfg(target_os = "windows")]
    child: child_window::ChildWindow,
    context: Rc<dyn servo::RenderingContext>,
}

impl Engine for ServoEngine {
    fn spin_event_loop(&self) {
        self.servo.spin_event_loop();
    }

    fn spawn(&self, spec: ViewSpec) -> Result<Box<dyn ContentView>, EngineError> {
        let surface = self.spawn_surface(&spec)?;
        let view = view::ServoView::new(&self.servo, surface, self.resolve_page, spec)?;
        Ok(Box::new(view))
    }
}
