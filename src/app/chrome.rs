//! Painting the app's state into the chrome view: `sync_chrome` pushes
//! the whole `ChromeState` in one eval, and the overlay plumbing grows
//! the chrome document over the window while a popup is open.

use crate::engine::Rect;
use crate::shell;
use crate::shell::chrome::{favicon_data_url, ChromeState, ChromeTab};
use crate::shell::search::SearchEngine;

use super::App;

impl App {
    /// Chrome bounds for the current overlay state: the strip normally,
    /// the whole window while an overlay is open.
    pub(super) fn chrome_rect(&self) -> Rect {
        let (w, h) = self.logical_size;
        if self.overlay_open {
            Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }
        } else {
            shell::layout(w, h).0
        }
    }

    /// Chrome overlays (the ⋮ menu, the omnibox dropdown) are part of the
    /// chrome document, but a view can't paint outside its own bounds —
    /// while one is open the chrome view is raised above the content
    /// views and stretched over the whole window. Its background is
    /// transparent, so only the strip and the popup paint; the page shows
    /// through and any click outside lands on the chrome document, which
    /// dismisses the overlay.
    pub(super) fn set_overlay_open(&mut self, open: bool) -> anyhow::Result<()> {
        if self.overlay_open == open {
            return Ok(());
        }
        self.overlay_open = open;
        self.chrome.set_bounds(self.chrome_rect())?;
        if open {
            self.chrome.bring_to_front()?;
        }
        Ok(())
    }

    /// App-side actions that replace what's on screen must close overlays —
    /// the document can't see them, so it's synced through `__cryo_overlay`
    /// before the bounds come back down.
    pub(super) fn dismiss_overlays(&mut self) -> anyhow::Result<()> {
        self.cancel_permission_prompt()?;
        if !self.overlay_open {
            return Ok(());
        }
        self.chrome
            .eval("window.__cryo_overlay && window.__cryo_overlay(false)")?;
        self.take_password_prompt();
        // The library panel is one of those overlays — once it is shut
        // there is nothing to push to it.
        self.forget_library();
        self.set_overlay_open(false)
    }

    /// The stored search engine rides on the chrome document's commands
    /// (it lives in the shell origin's localStorage, which the app can't
    /// read — the settings page owns the write). The pick is a preset
    /// slug or a custom `%s` template; what `from_pick` can't validate
    /// is ignored, not reset to the default.
    pub(super) fn note_search_engine(&mut self, slug: Option<String>) {
        if let Some(engine) = slug.as_deref().and_then(SearchEngine::from_pick) {
            self.search_engine = engine;
        }
    }

    /// Push the whole window state to the chrome document in one eval —
    /// the strip, the downloads shelf, the bookmarks bar, the account
    /// button. Anything that changes state ends here.
    pub(super) fn sync_chrome(&self) -> anyhow::Result<()> {
        // History depth only exists while a view is live — a frozen or
        // discarded tab's arrows stay grey until it restores.
        let (can_back, can_fwd) = match self.active_view() {
            Some(v) => (v.can_go_back()?, v.can_go_forward()?),
            None => (false, false),
        };
        let state = ChromeState {
            tabs: self
                .registry
                .iter()
                .map(|t| ChromeTab {
                    id: t.id().get(),
                    title: t.title().to_string(),
                    active: self.registry.active_id() == Some(t.id()),
                    state: t.state().as_str(),
                    favicon: t.favicon().map(favicon_data_url),
                    loading: t.is_loading(),
                })
                .collect(),
            active_url: self
                .registry
                .active()
                .map(|t| t.url().to_string())
                .filter(|u| u != "about:blank")
                .unwrap_or_default(),
            can_back,
            can_fwd,
            update_version: self.pending_update.as_ref().map(|u| u.version.to_string()),
            downloads: self.download_rows(),
            bookmarks: self.bar_rows(),
            starred: self.active_starred(),
            accounts: self.chrome_accounts(),
            active_account: self.active_account as u64,
        };
        let json = serde_json::to_string(&state)?;
        self.chrome.eval(&format!("window.__cryo_update({json})"))?;
        Ok(())
    }
}
