//! User actions: the chrome command dispatch and the tab/window
//! operations keys and menu items land on. Everything here is a verb
//! the user can reach — lifecycle plumbing lives in `surfaces`.

use tao::event_loop::ControlFlow;
use tao::window::Fullscreen;

use crate::shell;
use crate::shell::chrome::{canonize_nav_input, normalize_nav_input, ChromeCommand};
use crate::shell::pages::{self, NEW_TAB_URL};
use crate::tabs::TabId;

use super::{content_url_for, App, DEFAULT_URL};

impl App {
    pub(super) fn on_chrome(
        &mut self,
        cmd: ChromeCommand,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        match cmd {
            ChromeCommand::Ready => self.sync_chrome(),
            ChromeCommand::NewTab => self.new_tab(),
            ChromeCommand::Activate { id } => self.activate(TabId::from_raw(id)),
            ChromeCommand::Close { id } => self.close_tab(TabId::from_raw(id), control_flow),
            ChromeCommand::Navigate {
                input,
                ctrl,
                alt,
                shift,
            } => {
                let url = if ctrl && !alt && !shift {
                    canonize_nav_input(&input)
                } else {
                    normalize_nav_input(&input)
                };
                match (ctrl, alt, shift) {
                    // Ctrl+Shift+Enter: open the address in a new window.
                    (true, false, true) => Self::spawn_window(false, Some(&url)),
                    // Alt+Enter: new foreground tab; Alt+Shift+Enter: background.
                    (false, true, false) => self.spawn_tab(&url, true),
                    (false, true, true) => self.spawn_tab(&url, false),
                    _ => self.navigate_active(&url),
                }
            }
            ChromeCommand::Reload => self.reload_active(),
            ChromeCommand::Back => self.back_active(),
            ChromeCommand::Forward => self.forward_active(),
            // The WebView2 profile cookie jar is shared across views, so a
            // Google sign-in tab authenticates every tab — and persists.
            ChromeCommand::SignIn => self.spawn_tab("https://accounts.google.com", true),
            ChromeCommand::NewIncognito => Self::spawn_window(true, None),
            ChromeCommand::DevTools => self.devtools_active(),
            ChromeCommand::ClearData => self
                .chrome
                .clear_browsing_data()
                .map_err(|e| anyhow::anyhow!(e)),
            ChromeCommand::Settings => self.spawn_tab(pages::SETTINGS_URL, true),
            ChromeCommand::CheckUpdates => {
                self.request_update_check(true);
                Ok(())
            }
            ChromeCommand::UpdateApply => self.apply_update(control_flow),
            ChromeCommand::Freeze { id } => {
                match id
                    .map(TabId::from_raw)
                    .or_else(|| self.registry.active_id())
                {
                    Some(id) => self.freeze_tab(id),
                    None => Ok(()),
                }
            }
            ChromeCommand::Discard { id } => {
                match id
                    .map(TabId::from_raw)
                    .or_else(|| self.registry.active_id())
                {
                    Some(id) => self.discard_tab(id),
                    None => Ok(()),
                }
            }
            ChromeCommand::Overlay { open } => self.set_overlay_open(open),
        }
    }

    /// One window per process: a "new window" key spawns a sibling cryo
    /// process. `--incognito` puts every view of that process on an
    /// InPrivate profile — incognito is a property of the window, not a
    /// tab, same as Chrome. A bare arg hands the child an address to open.
    pub(super) fn spawn_window(incognito: bool, url: Option<&str>) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        if incognito {
            cmd.arg("--incognito");
        }
        if let Some(url) = url {
            cmd.arg(url);
        }
        cmd.spawn()?;
        Ok(())
    }

    pub(super) fn new_tab(&mut self) -> anyhow::Result<()> {
        let id = self.registry.create(DEFAULT_URL.to_string());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title("New tab".to_string());
        }
        self.spawn_view_for(id, NEW_TAB_URL)?;
        // A fresh tab means "type an address" — hand focus to the bar
        // like every browser does, not to the new-tab page.
        self.focus_address_bar()
    }

    /// Bring back the most recently closed tab — `activate` respawns the
    /// surface, so a tab that was closed while frozen comes back through
    /// the same restore path.
    pub(super) fn reopen_closed(&mut self) -> anyhow::Result<()> {
        match self.registry.reopen_closed() {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    /// Open `url` in a fresh tab — foreground for target=_blank /
    /// Alt+Enter, background for Alt+Shift+Enter.
    pub(super) fn spawn_tab(&mut self, url: &str, foreground: bool) -> anyhow::Result<()> {
        let id = self.registry.create(url.to_string());
        self.spawn_view_at(id, url, foreground)?;
        if foreground {
            self.activate(id)
        } else {
            self.sync_chrome()
        }
    }

    /// Spawn the engine surface for `id`, register it, activate it.
    pub(super) fn spawn_view_for(&mut self, id: TabId, url: &str) -> anyhow::Result<()> {
        self.spawn_view_at(id, url, true)?;
        self.activate(id)
    }

    pub(super) fn activate(&mut self, id: TabId) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        let prev = self.registry.active_id();
        if prev == Some(id) {
            return Ok(());
        }
        // Frozen, discarded, and reopened tabs have no surface — clicking
        // them is what "restore" means.
        if !self.views.contains_key(&id) {
            self.restore_view(id)?;
        }
        self.registry.set_active(id)?;
        if let Some(view) = self.views.get(&id) {
            let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
            // Show before hiding: the incoming view becomes visible while
            // the outgoing one is still painted, so no frame ever shows
            // the bare window behind them. Both changes settle in this
            // same event turn, so z-order between the two never reaches
            // the screen.
            view.set_bounds(content_rect)?;
            view.set_visible(true)?;
            view.focus()?;
            self.chrome_focused = false;
        }
        if let Some(p) = prev.and_then(|p| self.views.get(&p)) {
            p.set_visible(false)?;
        }
        self.sync_chrome()
    }

    pub(super) fn close_tab(
        &mut self,
        id: TabId,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        match self.registry.close(id)? {
            // Reveal the neighbour first, then destroy the dead view —
            // dropping it while it's still the only painted surface
            // leaves a one-frame hole where the window shows through.
            Some(next) => {
                self.activate(next)?;
                drop(self.views.remove(&id));
                self.sync_chrome()
            }
            None => {
                *control_flow = ControlFlow::Exit;
                Ok(())
            }
        }
    }

    pub(super) fn cycle(&mut self, offset: isize) -> anyhow::Result<()> {
        match self.registry.cycle_target(offset) {
            Some(id) => self.activate(id),
            None => Ok(()),
        }
    }

    pub(super) fn navigate_active(&mut self, url: &str) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        if let Some(view) = self.views.get(&id) {
            view.load_url(content_url_for(url))?;
            // Hand keyboard focus to the page — a committed navigation in a
            // real browser moves focus out of the address bar.
            view.focus()?;
            self.chrome_focused = false;
        }
        if let Some(tab) = self.registry.get_mut(id) {
            tab.navigate(url.to_string());
        }
        self.sync_chrome()
    }

    pub(super) fn reload_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.reload()?;
        }
        Ok(())
    }

    pub(super) fn hard_reload_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.hard_reload()?;
        }
        Ok(())
    }

    pub(super) fn back_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.go_back()?;
        }
        Ok(())
    }

    pub(super) fn forward_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.go_forward()?;
        }
        Ok(())
    }

    pub(super) fn stop_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.stop()?;
        }
        Ok(())
    }

    pub(super) fn move_active_tab(&mut self, delta: isize) -> anyhow::Result<()> {
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        let Some(pos) = self.registry.position_of(id) else {
            return Ok(());
        };
        self.registry
            .relocate(id, (pos as isize + delta).max(0) as usize)?;
        self.sync_chrome()
    }

    pub(super) fn move_active_tab_to(&mut self, index: usize) -> anyhow::Result<()> {
        match self.registry.active_id() {
            Some(id) => {
                self.registry.relocate(id, index)?;
                self.sync_chrome()
            }
            None => Ok(()),
        }
    }

    pub(super) fn duplicate_active(&mut self) -> anyhow::Result<()> {
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        let dup = self.registry.duplicate(id)?;
        let url = self.registry.get(dup).map(|t| t.url().to_string());
        let Some(url) = url else { return Ok(()) };
        let load = content_url_for(&url).to_string();
        self.spawn_view_for(dup, &load)
    }

    pub(super) fn mute_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.set_muted(!view.muted()?)?;
        }
        Ok(())
    }

    /// F6 toggles between the address bar and the page; Ctrl+F6 jumps
    /// straight to the page.
    pub(super) fn cycle_focus(&mut self) -> anyhow::Result<()> {
        if self.chrome_focused {
            self.focus_page()
        } else {
            self.focus_address_bar()
        }
    }

    pub(super) fn focus_page(&mut self) -> anyhow::Result<()> {
        self.chrome_focused = false;
        if let Some(view) = self.active_view() {
            view.focus()?;
        }
        Ok(())
    }

    pub(super) fn print_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.eval("window.print()")?;
        }
        Ok(())
    }

    pub(super) fn save_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.save_as()?;
        }
        Ok(())
    }

    /// Ctrl+O: open a local file — the file dialog blocks the loop while
    /// it's up, same as any modal system dialog.
    pub(super) fn open_file(&mut self) -> anyhow::Result<()> {
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return Ok(());
        };
        let url = format!("file:///{}", path.display().to_string().replace('\\', "/"));
        self.navigate_active(&url)
    }

    /// Ctrl+U: view-source opens in a new tab, same as Chrome.
    pub(super) fn view_source(&mut self) -> anyhow::Result<()> {
        let url = self.registry.active().map(|t| t.url().to_string());
        match url {
            Some(u) if u.starts_with("http") => self.spawn_tab(&format!("view-source:{u}"), true),
            _ => Ok(()),
        }
    }

    /// Steps: in ×1.25, out ÷1.25, reset to 1.0 — clamped like Chrome's
    /// 25%–500% range.
    pub(super) fn zoom_active(&mut self, step: i32) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            let current = view.zoom_factor()?;
            let next = match step {
                1 => current * 1.25,
                -1 => current / 1.25,
                _ => 1.0,
            }
            .clamp(0.25, 5.0);
            view.set_zoom_factor(next)?;
        }
        Ok(())
    }

    /// Ctrl+Shift+S: PNG of the visible page under Pictures\cryo.
    pub(super) fn screenshot_active(&mut self) -> anyhow::Result<()> {
        let Some(view) = self.active_view() else {
            return Ok(());
        };
        let png = view.capture_png()?;
        if png.is_empty() {
            return Ok(());
        }
        let dir = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Pictures"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir)
            .join("cryo");
        std::fs::create_dir_all(&dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{ts}.png"));
        std::fs::write(&path, png)?;
        eprintln!("cryo: screenshot saved to {}", path.display());
        Ok(())
    }

    pub(super) fn toggle_fullscreen(&mut self) -> anyhow::Result<()> {
        let next = if self.window.fullscreen().is_some() {
            None
        } else {
            Some(Fullscreen::Borderless(None))
        };
        self.window.set_fullscreen(next);
        Ok(())
    }

    pub(super) fn devtools_active(&mut self) -> anyhow::Result<()> {
        if let Some(view) = self.active_view() {
            view.open_devtools();
        }
        Ok(())
    }

    pub(super) fn focus_address_bar(&mut self) -> anyhow::Result<()> {
        // Move controller focus to the chrome view first — `u.focus()` in
        // JS sets DOM focus only, so without this the keystrokes keep going
        // to whichever webview last had it.
        self.chrome.focus()?;
        self.chrome
            .eval("const u=document.getElementById('url');u.focus();u.select();")?;
        self.chrome_focused = true;
        Ok(())
    }
}
