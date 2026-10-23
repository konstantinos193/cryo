//! cryo — a memory-obsessed browser shell.
//!
//! `run()` builds the window, engine and app, then hands the thread to the
//! tao event loop and never returns.

/// The one reporting seam: a message with no user-facing surface goes
/// through `crate::report!`, so the `cryo: ` prefix and the sink exist
/// in exactly one place — any module can reach the crate root, so the
/// facade lives here, not in `app` (AGENTS §10).
#[macro_export]
macro_rules! report {
    ($($arg:tt)*) => {
        eprintln!("cryo: {}", format_args!($($arg)*))
    };
}

pub mod app;
pub mod bookmarks;
pub mod engine;
pub mod instance;
pub mod persist;
pub mod shell;
pub mod tabs;
pub mod transfer;
pub mod update;
pub mod vault;

use std::rc::Rc;

use raw_window_handle::HasWindowHandle;
use tao::dpi::LogicalSize;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;

use crate::app::{App, AppEvent};
use crate::engine::wry_impl::WryEngine;
use crate::engine::Engine;

/// The per-user data root every store hangs off (the composition root
/// names paths, AGENTS §5): `%LOCALAPPDATA%\cryo` on Windows,
/// `$XDG_DATA_HOME/cryo` or `~/.local/share/cryo` on Linux,
/// `~/Library/Application Support/cryo` on macOS. The temp dir is the
/// last-resort fallback — a box without HOME has no profile anyway.
pub(crate) fn data_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h)
            .join("Library")
            .join("Application Support")
    });
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
        });
    base.unwrap_or_else(std::env::temp_dir).join("cryo")
}

pub fn run(incognito: bool, new_window: bool, open_url: Option<String>) -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Who are we, before a window is paid for? The first normal window
    // claims the single-instance mutex and serves the link pipe; a
    // later process carrying an address hands it over and exits — the
    // link becomes a tab in the running window, not a new window.
    // Incognito stays out of the handoff entirely: its links belong to
    // no profile. `--new-window` is how a spawned window says it was
    // asked for on purpose, so it never forwards.
    let primary = if incognito {
        None
    } else {
        let ipc_proxy = proxy.clone();
        instance::claim(move |url| app::post(&ipc_proxy, AppEvent::ExternalOpen(url)))
    };
    if !incognito
        && !new_window
        && primary.is_none()
        && open_url.as_deref().is_some_and(instance::forwarded)
    {
        return Ok(());
    }
    let _primary = primary;

    let title = if incognito {
        "cryo — incognito"
    } else {
        "cryo"
    };
    // The window itself paints the content colour: whatever shows through
    // between one view hiding and the next painting is then the same dark
    // the pages use, not the system's white.
    let builder = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_background_color(shell::CONTENT_BACKGROUND);
    #[cfg(windows)]
    let builder = with_icons(builder)?;
    let window = Rc::new(builder.build(&event_loop)?);

    let engine: Rc<dyn Engine> = Rc::new(WryEngine::new(
        window.window_handle()?.as_raw(),
        shell::pages::internal_page,
        incognito,
    ));
    // Update checks only make sense for installed release builds — a dev
    // binary would just offer to replace itself with a tagged release.
    #[cfg(all(windows, not(debug_assertions)))]
    {
        let update_proxy = proxy.clone();
        update::spawn_check(move |outcome| {
            app::post(&update_proxy, AppEvent::UpdateChecked(outcome));
        });
    }
    // Incognito keeps no history — not even in memory — and its kept
    // pages and passwords live only as long as the window. Everything
    // else is stored per user under %LOCALAPPDATA%\cryo: history and
    // bookmarks as JSON, the vault sealed by DPAPI against this Windows
    // account. The composition root is the only place that names a path
    // or picks a protector (AGENTS §5).
    let profile = if incognito {
        app::Profile::session_only()
    } else {
        app::Profile {
            recorder: Some(persist::Recorder::open(persist::JsonFile::new(
                persist::JsonFile::default_path(),
            ))),
            bookmarks: bookmarks::Keeper::open(bookmarks::JsonFile::new(
                bookmarks::JsonFile::default_path(),
            )),
            vault: vault::Vault::open(vault::SealedFile::new(
                vault::SealedFile::default_path(),
                vault_protector(),
            )),
            incognito: false,
        }
    };
    let mut app = App::new(window, engine, proxy, open_url, profile)?;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        app.handle_event(event, control_flow);
    });
}

/// The vault's protector, chosen here at the composition root (AGENTS
/// §6): DPAPI ties the key to the Windows account; off Windows a random
/// key lives in the OS keychain (`vault::Keyring`).
#[cfg(windows)]
fn vault_protector() -> impl vault::Protector {
    vault::Dpapi
}

#[cfg(not(windows))]
fn vault_protector() -> impl vault::Protector {
    vault::Keyring
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
