//! cryo — a memory-obsessed browser shell.
//!
//! `run()` builds the window, engine and app, then hands the thread to the
//! tao event loop and never returns.

pub mod app;
pub mod engine;
pub mod shell;
pub mod tabs;

use std::rc::Rc;

use tao::dpi::LogicalSize;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;

use crate::app::{App, AppEvent};
use crate::engine::wry_impl::WryEngine;
use crate::engine::Engine;

pub fn run() -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let mut builder = WindowBuilder::new()
        .with_title("cryo")
        .with_inner_size(LogicalSize::new(1280.0, 800.0));
    #[cfg(windows)]
    {
        use tao::platform::windows::IconExtWindows;
        if let Ok(icon) = tao::window::Icon::from_resource(1, None) {
            builder = builder.with_window_icon(Some(icon));
        }
    }
    let window = Rc::new(builder.build(&event_loop)?);

    let engine: Rc<dyn Engine> =
        Rc::new(WryEngine::new(window.clone(), shell::pages::internal_page));
    let proxy = event_loop.create_proxy();
    let mut app = App::new(window, engine, proxy)?;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        app.handle_event(event, control_flow);
    });
}
