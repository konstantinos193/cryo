//! User actions: the chrome command dispatch and the tab/window
//! operations keys and menu items land on. Everything here is a verb
//! the user can reach — lifecycle plumbing lives in `surfaces`.

use tao::event_loop::ControlFlow;
use tao::window::Fullscreen;

use crate::shell;
use crate::shell::chrome::{
    canonize_nav_input, completion_for, normalize_nav_input, ChromeCommand, SuggestReply,
    Suggestion,
};
use crate::shell::dial;
use crate::shell::pages::{self, NEW_TAB_URL};
use crate::tabs::TabId;

use super::{content_url_for, downloads, App, DEFAULT_URL};

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
                // What the user typed is itself worth remembering — the
                // resolved URL gets recorded when the navigation commits.
                self.remember(&input);
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
            // The reload button becomes ✕ while a page loads.
            ChromeCommand::Stop => self.stop_active(),
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
            ChromeCommand::Suggest { q } => self.suggest(&q),
            ChromeCommand::DownloadCancel { key } => self.cancel_download(key),
            ChromeCommand::DownloadOpen { key } => {
                self.open_download(key);
                Ok(())
            }
            ChromeCommand::DownloadPause { key } => self.pause_download(key),
            ChromeCommand::DownloadResume { key } => self.resume_download(key),
            ChromeCommand::DownloadRetry { key } => self.retry_download(key),
            ChromeCommand::DownloadDrag { key } => {
                self.drag_download(key);
                Ok(())
            }
            ChromeCommand::DownloadShow { key } => {
                self.reveal_download(key);
                Ok(())
            }
            ChromeCommand::DownloadsClear => self.clear_finished_downloads(),
            ChromeCommand::BookmarkPage => self.bookmark_active(),
            ChromeCommand::BookmarkOpen { id, ctrl, shift } => self.open_bookmark(id, ctrl, shift),
            ChromeCommand::BookmarkRemove { id } => self.remove_bookmark(id),
            ChromeCommand::Library { pane } => self.show_library(&pane),
            ChromeCommand::PasswordReveal { origin, username } => {
                self.reveal_password(&origin, &username)
            }
            ChromeCommand::PasswordRemove { origin, username } => {
                self.remove_password(&origin, &username)
            }
            ChromeCommand::ImportFile {} => self.import_file(),
            ChromeCommand::ImportProfile { index } => self.import_profile(index),
            ChromeCommand::Export { what } => self.export(&what),
        }
    }

    /// The omnibox asks on every keystroke; the app owns what it can
    /// offer — a search/go row, then remembered inputs and visits, then
    /// dial matches — and answers with a `__cryo_suggest` eval. History
    /// is never fetchable: `/cmd` reaches us only from the chrome view
    /// (content views carry no `on_ipc`), and the page resolver would
    /// hand it to any origin.
    fn suggest(&self, q: &str) -> anyhow::Result<()> {
        let q = q.trim();
        let mut rows: Vec<Suggestion> = Vec::new();
        if !q.is_empty() {
            // The first row mirrors `normalize_nav_input`'s verdict: a
            // URL-looking input says "Go to", anything else is a search.
            let url = normalize_nav_input(q);
            let is_search = url.starts_with("https://duckduckgo.com/?q=");
            rows.push(Suggestion {
                label: if is_search {
                    format!("Search DuckDuckGo for \"{q}\"")
                } else {
                    format!("Go to {url}")
                },
                input: q.to_string(),
                // The typed-verdict row restates what was typed — there
                // is nothing to complete to.
                icon: None,
                complete: None,
                kind: if is_search { "search" } else { "go" },
            });
            let ql = q.to_lowercase();
            // Kept pages come before remembered ones: a bookmark is the
            // user saying this destination matters, which outranks the
            // fact that they once visited something similar.
            for mark in self.bookmarks.marks().matches(q, 4) {
                rows.push(Suggestion {
                    label: format!("{} — {}", mark.title(), mark.url()),
                    complete: completion_for(mark.url(), q),
                    input: mark.url().to_string(),
                    icon: None,
                    kind: "bookmark",
                });
            }
            if let Some(rec) = &self.recorder {
                for e in rec.matches(q, 8) {
                    let label = match e.title.as_deref().filter(|t| !t.is_empty()) {
                        Some(t) => format!("{t} — {}", e.text),
                        None => e.text.clone(),
                    };
                    rows.push(Suggestion {
                        label,
                        input: e.text.clone(),
                        icon: e.favicon.clone(),
                        complete: completion_for(&e.text, q),
                        kind: "page",
                    });
                }
            }
            for d in dial::entries() {
                if d.name.to_lowercase().contains(&ql) || d.url.to_lowercase().contains(&ql) {
                    rows.push(Suggestion {
                        label: format!("{} — {}", d.name, d.url),
                        complete: completion_for(&d.url, q),
                        input: d.url,
                        icon: None,
                        kind: "page",
                    });
                }
            }
            // Same destination from two sources — the first wins.
            let mut seen = std::collections::HashSet::new();
            rows.retain(|r| seen.insert(r.input.clone()));
            rows.truncate(8);
        }
        let reply = SuggestReply {
            q: q.to_string(),
            rows,
        };
        let json = serde_json::to_string(&reply)?;
        self.chrome
            .eval(&format!("window.__cryo_suggest({json})"))?;
        Ok(())
    }

    /// One window per process: a "new window" key spawns a sibling cryo
    /// process. `--incognito` puts every view of that process on an
    /// InPrivate profile — incognito is a property of the window, not a
    /// tab, same as Chrome. A bare arg hands the child an address to open.
    pub(super) fn spawn_window(incognito: bool, url: Option<&str>) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        // A spawned window keeps its address: the flag stops the child
        // from handing it to us over the single-instance pipe.
        cmd.arg("--new-window");
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
        self.spawn_view_at(id, url)?;
        if foreground {
            self.activate(id)
        } else {
            self.sync_chrome()
        }
    }

    /// Spawn the engine surface for `id`, register it, activate it.
    pub(super) fn spawn_view_for(&mut self, id: TabId, url: &str) -> anyhow::Result<()> {
        self.spawn_view_at(id, url)?;
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
                self.drop_view(id);
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
