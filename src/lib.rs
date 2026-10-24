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

pub mod account;
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

/// Read a whole file — `Ok(None)` when it isn't there, since a missing
/// file is a fresh profile, not an error. Every store adapter reads
/// through this so the rule lives once (AGENTS §2).
pub(crate) fn read_file(path: &std::path::Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write `bytes` to `path` atomically: the parent dir is created, the
/// payload lands on a sibling `.tmp` name, and a rename puts it in
/// place — a crash mid-write leaves the old file whole. Every store
/// adapter writes through this so the dance lives once (AGENTS §2).
pub(crate) fn write_file_atomic(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Standard base64 with padding — one small encoder beats a dependency
/// for the places a `data:` URL is built (the favicon strip, the account
/// picture). Lives here for the same reason `read_file` does: the rule
/// exists once, at the crate root every module can reach.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHA[(n >> 18) as usize & 63] as char);
        out.push(ALPHA[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHA[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The inverse of `base64`, forgiving the way foreign wires are:
/// whitespace and stray bytes outside the alphabet are skipped rather
/// than failing the decode — a truncated or padded reply still yields
/// what it carried.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for &b in s.as_bytes() {
        if b == b'=' {
            break;
        }
        let Some(v) = val(b) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // Bits short of a full byte at the end are padding's remainder —
    // a reply cut mid-quad still decodes its whole bytes.
    Some(out)
}

/// A temp dir no other test on this machine shares — the store
/// adapters' file tests all scratch in one.
#[cfg(test)]
pub(crate) fn test_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("cryo-{tag}-{}", std::process::id()))
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
    let mut app = App::new(
        window,
        engine,
        Rc::new(tabs::SystemClock),
        proxy,
        open_url,
        profile,
    )?;

    event_loop.run(move |event, _target, control_flow| {
        // A heartbeat, not a poll: the loop wakes at least this often so
        // background tabs that have gone cold can be suspended — the
        // timer is the event loop's own, no second thread (AGENTS §9).
        *control_flow = ControlFlow::WaitUntil(std::time::Instant::now() + app::IDLE_TICK);
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

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_to_quads() {
        // Vectors from RFC 4648.
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foob"), "Zm9vYg==");
        assert_eq!(super::base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
