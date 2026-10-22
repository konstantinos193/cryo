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

pub fn run(incognito: bool, open_url: Option<String>) -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let title = if incognito {
        "cryo — incognito"
    } else {
        "cryo"
    };
    let mut builder = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1280.0, 800.0));
    #[cfg(windows)]
    {
        builder = with_icons(builder)?;
    }
    let window = Rc::new(builder.build(&event_loop)?);

    let engine: Rc<dyn Engine> = Rc::new(WryEngine::new(
        window.clone(),
        shell::pages::internal_page,
        incognito,
    ));
    let proxy = event_loop.create_proxy();
    let mut app = App::new(window, engine, proxy, open_url)?;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        app.handle_event(event, control_flow);
    });
}

/// Resource id the icon is compiled under — must match `assets/icon.rc`.
#[cfg(windows)]
const ICON_RESOURCE_ID: u16 = 1;

/// Attach the embedded icon to both slots Windows reads.
///
/// `with_window_icon` only fills `ICON_SMALL`, which the title bar uses. The
/// taskbar button reads `ICON_BIG` and needs a handle of its own, otherwise it
/// falls back to the generic application icon.
#[cfg(windows)]
fn with_icons(builder: WindowBuilder) -> anyhow::Result<WindowBuilder> {
    use tao::dpi::PhysicalSize;
    use tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows};
    use tao::window::Icon;

    let title_bar = Icon::from_resource(ICON_RESOURCE_ID, Some(PhysicalSize::new(16, 16)))?;
    let taskbar = Icon::from_resource(ICON_RESOURCE_ID, Some(PhysicalSize::new(32, 32)))?;
    Ok(builder
        .with_window_icon(Some(title_bar))
        .with_taskbar_icon(Some(taskbar)))
}
