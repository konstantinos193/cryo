//! User actions: the chrome command dispatch and the tab/window
//! operations keys and menu items land on. Everything here is a verb
//! the user can reach — lifecycle plumbing lives in `surfaces`.

use tao::event_loop::ControlFlow;
use tao::window::Fullscreen;

use crate::shell;
use crate::shell::chrome::{canonize_nav_input, normalize_nav_input, ChromeCommand};
use crate::shell::pages;
use crate::tabs::TabId;

use super::{content_url_for, downloads, App};

impl App {
    pub(super) fn on_chrome(
        &mut self,
        cmd: ChromeCommand,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        match cmd {
            ChromeCommand::PermissionAnswer { token, allow } => {
                self.answer_permission_prompt(token, allow)
            }
            ChromeCommand::Ready { engine } => {
                self.note_search_engine(engine);
                // First paint of a window — the one moment the account
                // button's truth is unknown.
                self.refresh_google_session();
                self.sync_chrome()
            }
            ChromeCommand::NewTab => self.new_tab(),
            ChromeCommand::Activate { id } => self.activate(TabId::from_raw(id)),
            ChromeCommand::Close { id } => self.close_tab(TabId::from_raw(id), control_flow),
            ChromeCommand::Navigate {
                input,
                ctrl,
                alt,
                shift,
                engine,
            } => self.navigate_command(&input, ctrl, alt, shift, engine),
            ChromeCommand::Reload => self.reload_active(),
            // The reload button becomes ✕ while a page loads.
            ChromeCommand::Stop => self.stop_active(),
            ChromeCommand::Back => self.back_active(),
            ChromeCommand::Forward => self.forward_active(),
            // The WebView2 profile cookie jar is shared across views, so a
            // Google sign-in tab authenticates every tab — and persists.
            ChromeCommand::SignIn => self.spawn_tab("https://accounts.google.com", true),
            // The account's position in the session list is its
            // `authuser` index on Google properties — picking the row
            // makes it the window's account, not just an open tab.
            ChromeCommand::AccountOpen { index } => self.account_open(index),
            ChromeCommand::NewIncognito => Self::spawn_window(true, None),
            ChromeCommand::DevTools => self.devtools_active(),
            ChromeCommand::ClearData => self.clear_data(),
            ChromeCommand::Settings => self.spawn_tab(pages::SETTINGS_URL, true),
            ChromeCommand::CheckUpdates => {
                self.request_update_check(true);
                Ok(())
            }
            ChromeCommand::UpdateApply => self.apply_update(control_flow),
            ChromeCommand::Freeze { id } => match self.tab_or_active(id) {
                Some(id) => self.freeze_tab(id),
                None => Ok(()),
            },
            ChromeCommand::Discard { id } => match self.tab_or_active(id) {
                Some(id) => self.discard_tab(id),
                None => Ok(()),
            },
            ChromeCommand::Overlay { open } => self.set_overlay_open(open),
            ChromeCommand::Suggest { q, engine } => {
                self.note_search_engine(engine);
                self.suggest(&q)
            }
            // The shelf's commands — the keys are app-issued row numbers,
            // so they go straight to `downloads`.
            ChromeCommand::DownloadCancel { .. }
            | ChromeCommand::DownloadOpen { .. }
            | ChromeCommand::DownloadPause { .. }
            | ChromeCommand::DownloadResume { .. }
            | ChromeCommand::DownloadRetry { .. }
            | ChromeCommand::DownloadDrag { .. }
            | ChromeCommand::DownloadShow { .. }
            | ChromeCommand::DownloadsClear => self.download_command(cmd),
            ChromeCommand::BookmarkPage => self.bookmark_active(),
            ChromeCommand::BookmarkOpen { id, ctrl, shift } => self.open_bookmark(id, ctrl, shift),
            ChromeCommand::BookmarkRemove { id } => self.remove_bookmark(id),
            ChromeCommand::Library { pane } => self.show_library(&pane),
            ChromeCommand::ExtensionAdd => self.add_extension(),
            ChromeCommand::ExtensionRemove { slug } => self.remove_extension(&slug),
            ChromeCommand::ExtensionInstall { input } => self.install_from_store(&input),
            ChromeCommand::ExtensionEnable { slug, enabled } => {
                self.set_extension_enabled(&slug, enabled)
            }
            ChromeCommand::HistoryOpen { url, ctrl, shift } => self.open_history(&url, ctrl, shift),
            ChromeCommand::HistoryRemove { text } => self.remove_history(&text),
            ChromeCommand::HistoryClear {} => self.clear_history(),
            ChromeCommand::PasswordReveal { origin, username } => {
                self.reveal_password(&origin, &username)
            }
            ChromeCommand::PasswordCopy { origin, username } => {
                self.copy_password(&origin, &username)
            }
            ChromeCommand::PasswordRemove { origin, username } => {
                self.remove_password(&origin, &username)
            }
            ChromeCommand::PasswordFill { username } => self.fill_password(&username),
            ChromeCommand::PasswordSave {} => self.save_password(),
            ChromeCommand::PasswordPromptClose {} => self.close_password_prompt(),
            ChromeCommand::ImportFile {} => self.import_file(),
            ChromeCommand::ImportProfile { index } => self.import_profile(index),
            ChromeCommand::Export { what } => self.export(&what),
        }
    }

    /// Where an omnibox commit lands — modifier keys pick the surface:
    /// plain Enter the active tab, Ctrl+Shift+Enter a new window,
    /// Alt+Enter a foreground tab, Alt+Shift+Enter a background one.
    /// What the user typed is itself worth remembering — the resolved
    /// URL gets recorded when the navigation commits.
    fn navigate_command(
        &mut self,
        input: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        engine: Option<String>,
    ) -> anyhow::Result<()> {
        self.note_search_engine(engine);
        self.remember(input);
        let url = if ctrl && !alt && !shift {
            canonize_nav_input(input, &self.search_engine)
        } else {
            normalize_nav_input(input, &self.search_engine)
        };
        match (ctrl, alt, shift) {
            (true, false, true) => Self::spawn_window(false, Some(&url)),
            (false, true, false) => self.spawn_tab(&url, true),
            (false, true, true) => self.spawn_tab(&url, false),
            _ => self.navigate_active(&url),
        }
    }

    /// The tab a menu command points at — the ⋮ menu carries no id, so
    /// it means the active one; the strip's per-tab buttons name theirs.
    fn tab_or_active(&self, id: Option<u64>) -> Option<TabId> {
        id.map(TabId::from_raw)
            .or_else(|| self.registry.active_id())
    }

    pub(super) fn activate(&mut self, id: TabId) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        let prev = self.registry.active_id();
        if prev == Some(id) {
            return Ok(());
        }
        // Frozen, discarded, and reopened tabs have no surface — clicking
        // them is what "restore" means.
        if self.view_of(id).is_none() {
            self.restore_view(id)?;
        }
        self.registry.set_active(id)?;
        // The clock the idle suspender reads: the outgoing tab starts
        // counting cold time now, the incoming one stops.
        if let Some(tab) = prev.and_then(|p| self.registry.get_mut(p)) {
            tab.note_unfocused(self.clock.now());
        }
        if let Some(tab) = self.registry.get_mut(id) {
            tab.note_focused();
        }
        if let Some(view) = self.view_of(id) {
            let (_, content_rect) = shell::layout(self.logical_size.0, self.logical_size.1);
            // Show before hiding: the incoming view becomes visible while
            // the outgoing one is still painted, so no frame ever shows
            // the bare window behind them. Both changes settle in this
            // same event turn, so z-order between the two never reaches
            // the screen.
            // A suspended view needs waking before it paints — a failed
            // resume is reported but must not block the switch.
            if let Err(e) = view.resume() {
                crate::report!("resume tab {id}: {e:?}");
            }
            view.set_bounds(content_rect)?;
            view.set_visible(true)?;
            view.focus()?;
            self.chrome_focused = false;
        }
        if let Some(p) = prev.and_then(|p| self.view_of(p)) {
            p.set_visible(false)?;
        }
        self.sync_chrome()
    }

    pub(super) fn close_tab(
        &mut self,
        id: TabId,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        let key = self.registry.get(id).and_then(|t| t.view_key());
        self.archive_tab_downloads(id);
        let closed = self.registry.close(id)?;
        let Some(next) = closed.next else {
            // The last tab: the window goes with it, surface and all.
            self.request_exit(control_flow);
            return Ok(());
        };
        // Reveal the neighbour first, then destroy the dead view —
        // dropping it while it's still the only painted surface leaves a
        // one-frame hole where the window shows through.
        let shown = self.activate(next);
        if let (Some(key), Some(view)) = (key, closed.view) {
            self.drop_view(key, view);
        }
        shown?;
        self.sync_chrome()
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
        if let Some(view) = self.view_of(id) {
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
        let dup = self.duplicate_tab(id)?;
        self.activate(dup)
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

    /// Ctrl+J toggles the download shelf — the popover is a chrome-side
    /// overlay, so the key just evals its toggle. Stealing the chord in
    /// `key_command` is what keeps the engine's own downloads UI shut.
    pub(super) fn toggle_downloads(&mut self) -> anyhow::Result<()> {
        self.chrome
            .eval("window.__cryo_downloads && window.__cryo_downloads()")?;
        Ok(())
    }

    /// Ctrl+O: open a local file — the file dialog blocks the loop while
    /// it's up, same as any modal system dialog.
    pub(super) fn open_file(&mut self) -> anyhow::Result<()> {
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return Ok(());
        };
        self.navigate_active(&downloads::file_url(&path))
    }

    /// OS files dropped on the strip — each opens in its own tab, the
    /// first in front. Where they land is decided by the view's drop
    /// region (chrome covers the strip, or the whole window while an
    /// overlay is up), so no hit-testing happens here.
    pub(super) fn files_dropped(&mut self, paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
        self.dismiss_overlays()?;
        for (i, path) in paths.iter().enumerate() {
            self.spawn_tab(&downloads::file_url(path), i == 0)?;
        }
        Ok(())
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
        crate::report!("screenshot saved to {}", path.display());
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
