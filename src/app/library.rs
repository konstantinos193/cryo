//! The library: keeping pages, and the panel that shows what is kept.
//!
//! Bookmarks and saved passwords are reachable from exactly one surface
//! — the chrome document — because that is the only view with a command
//! channel. The internal protocol answers any origin, so a bookmarks
//! page living in a content tab would hand the list (and the ★ button's
//! power) to any website; the panel is part of the chrome view instead,
//! raised over the window like the ⋮ menu (see `set_overlay_open`).
//!
//! Secrets travel one at a time, on the user's explicit click, through
//! `__cryo_secret`. The pushed panel state carries usernames and
//! lengths only.
//!
//! Import and export live next door in `transfer`.

use crate::bookmarks::{BookmarkId, Draft};
use crate::shell::chrome::{
    favicon_data_url, ChromeBookmark, ChromeHistoryEntry, ChromeLibrary, ChromePassword,
    ChromeSource, Notice, SecretReply,
};
use crate::shell::pages;

use super::{unix_now, App, DEFAULT_URL};

/// Most rows one panel push carries. The panel filters what it has in
/// the document; past this it says how many more are kept, which is
/// honest and keeps one `eval` from carrying a megabyte.
pub(super) const MAX_PANEL_ROWS: usize = 1_000;

/// Most bar rows one `ChromeState` carries. The bar is a strip, not the
/// library — it overflows long before this, and the rows ride inside
/// every state push, favicon `data:` URLs included. Deeper kept pages
/// are still reachable from the library panel.
const BAR_ROWS: usize = 64;

impl App {
    /// The ★ button and Ctrl+D: keep the active page, or drop it again.
    pub(super) fn bookmark_active(&mut self) -> anyhow::Result<()> {
        let Some(tab) = self.registry.active() else {
            return Ok(());
        };
        let (url, title) = (tab.url().to_string(), tab.title().to_string());
        // The icon the tab already wears, if one has arrived — a late
        // `FaviconChanged` fills it in through `set_favicon` instead.
        let favicon = tab.favicon().map(favicon_data_url);
        // The new-tab page and the shell's own surfaces are not
        // destinations — there is nothing to come back to.
        if url == DEFAULT_URL || pages::is_internal_url(&url) {
            return self.notice(false, "Nothing to keep", vec!["Open a page first.".into()]);
        }
        match self.bookmarks.marks().starred(&url) {
            Some(id) => {
                self.bookmarks.remove(id)?;
            }
            None => {
                let draft = Draft {
                    title,
                    url,
                    // The ★ keeps to the bar, where the user can see it.
                    folder: String::new(),
                    added_at: unix_now(),
                    favicon,
                };
                if let Err(e) = self.bookmarks.add(&draft) {
                    return self.notice(false, "Could not keep that page", vec![e.to_string()]);
                }
            }
        }
        self.sync_chrome()?;
        self.push_library()
    }

    /// Open a kept page: in place, or in a tab when Ctrl is held, or in
    /// a background tab with Ctrl+Shift — the bar behaves like the dial.
    pub(super) fn open_bookmark(&mut self, id: u64, ctrl: bool, shift: bool) -> anyhow::Result<()> {
        let Some(url) = self
            .bookmarks
            .marks()
            .get(BookmarkId::from_raw(id))
            .map(|b| b.url().to_string())
        else {
            return Ok(());
        };
        match (ctrl, shift) {
            // Background-open is the library's multi-open path: keep the
            // panel in place so another saved page is one click away.
            (true, true) => self.spawn_tab(&url, false),
            (true, false) => {
                self.dismiss_overlays()?;
                self.spawn_tab(&url, true)
            }
            _ => {
                self.dismiss_overlays()?;
                self.navigate_active(&url)
            }
        }
    }

    pub(super) fn remove_bookmark(&mut self, id: u64) -> anyhow::Result<()> {
        if !self.bookmarks.remove(BookmarkId::from_raw(id))? {
            return Ok(());
        }
        self.sync_chrome()?;
        self.push_library()
    }

    /// The bar's rows, as `ChromeState` carries them — capped for the
    /// strip, not the panel: every `sync_chrome` resends these.
    pub(super) fn bar_rows(&self) -> Vec<ChromeBookmark> {
        self.bookmarks
            .marks()
            .bar()
            .take(BAR_ROWS)
            .map(wire_bookmark)
            .collect()
    }

    /// Is the active tab's address kept? Fills the ★ in.
    pub(super) fn active_starred(&self) -> bool {
        self.registry
            .active()
            .is_some_and(|t| self.bookmarks.marks().starred(t.url()).is_some())
    }

    /// A pane was opened: remember which one and push its data.
    pub(super) fn show_library(&mut self, pane: &str) -> anyhow::Result<()> {
        self.library_pane = Some(pane.to_string());
        // The import pane is the only one whose data is not already in
        // memory — looking for installed browsers touches the disk, so
        // it happens on a worker thread (AGENTS §9).
        if pane == "import" {
            self.find_import_sources();
        }
        self.push_library()
    }

    /// Ctrl+Shift+O: open the panel from the keyboard. The document owns
    /// its own open/shut state, so this asks it to show the pane and the
    /// overlay bounds follow from its `overlay` report.
    pub(super) fn toggle_library(&mut self) -> anyhow::Result<()> {
        self.chrome
            .eval("window.__cryo_library_toggle && window.__cryo_library_toggle('bookmarks')")?;
        Ok(())
    }

    /// Ctrl+H: same door as the library, straight onto the history pane.
    /// History never becomes a fetchable page — `persist`'s doc comment
    /// explains why — so the "history page" is this overlay.
    pub(super) fn toggle_history(&mut self) -> anyhow::Result<()> {
        self.chrome
            .eval("window.__cryo_library_toggle && window.__cryo_library_toggle('history')")?;
        Ok(())
    }

    /// A history row clicked: where the address lands follows the
    /// modifiers, same rule as `open_bookmark` — Ctrl+Shift a background
    /// tab that keeps the panel up, Ctrl a foreground tab, plain click
    /// replaces the active page.
    pub(super) fn open_history(
        &mut self,
        url: &str,
        ctrl: bool,
        shift: bool,
    ) -> anyhow::Result<()> {
        match (ctrl, shift) {
            (true, true) => self.spawn_tab(url, false),
            (true, false) => {
                self.dismiss_overlays()?;
                self.spawn_tab(url, true)
            }
            _ => {
                self.dismiss_overlays()?;
                self.navigate_active(url)
            }
        }
    }

    /// Forget one destination — the pane's per-row ✕.
    pub(super) fn remove_history(&mut self, text: &str) -> anyhow::Result<()> {
        if let Some(rec) = &mut self.recorder {
            rec.remove(text)?;
        }
        self.push_library()
    }

    /// "Clear history": an irreversible wipe, so the OS dialog asks
    /// first — the same gate a revealed password goes through.
    pub(super) fn clear_history(&mut self) -> anyhow::Result<()> {
        if !confirm(
            "Clear history",
            "Forget every recorded visit and typed address? Kept pages and saved passwords stay.",
        ) {
            return Ok(());
        }
        if let Some(rec) = &mut self.recorder {
            rec.clear()?;
        }
        self.push_library()
    }

    /// The panel closed — stop pushing to it.
    pub(super) fn forget_library(&mut self) {
        self.library_pane = None;
    }

    /// Push the open pane's data. A shut panel is skipped: the push is
    /// only worth its JSON while something is looking at it.
    pub(super) fn push_library(&mut self) -> anyhow::Result<()> {
        let Some(pane) = self.library_pane.clone() else {
            return Ok(());
        };
        let marks = self.bookmarks.marks();
        let creds = self.vault.credentials();
        let mut history: Vec<ChromeHistoryEntry> = self
            .recorder
            .iter()
            .flat_map(|rec| rec.entries())
            .map(|e| ChromeHistoryEntry {
                text: e.text.clone(),
                title: e.title.clone(),
                uses: e.uses,
                last_seen: e.last_seen,
                favicon: e.favicon.clone(),
            })
            .collect();
        // The pane reads newest-first; the store keeps insertion order.
        history.sort_by_key(|e| std::cmp::Reverse(e.last_seen));
        let more_history = history.len().saturating_sub(MAX_PANEL_ROWS);
        history.truncate(MAX_PANEL_ROWS);
        let state = ChromeLibrary {
            pane,
            bookmarks: marks
                .rows()
                .iter()
                .take(MAX_PANEL_ROWS)
                .map(wire_bookmark)
                .collect(),
            more_bookmarks: marks.len().saturating_sub(MAX_PANEL_ROWS),
            history,
            more_history,
            passwords: creds
                .rows()
                .iter()
                .take(MAX_PANEL_ROWS)
                .map(|c| ChromePassword {
                    origin: c.origin().to_string(),
                    username: c.username().to_string(),
                    length: c.secret().len(),
                    note: c.note().to_string(),
                })
                .collect(),
            more_passwords: creds.len().saturating_sub(MAX_PANEL_ROWS),
            sources: self
                .import_sources
                .iter()
                .flatten()
                .enumerate()
                .map(|(index, p)| ChromeSource {
                    index,
                    browser: p.browser.clone(),
                    profile: p.name.clone(),
                    count: p.count,
                })
                .collect(),
            sources_ready: self.import_sources.is_some(),
            locked: self.vault.locked_out().map(str::to_string),
            incognito: self.incognito,
        };
        let json = serde_json::to_string(&state)?;
        self.chrome
            .eval(&format!("window.__cryo_library({json})"))?;
        Ok(())
    }

    /// Show one saved password in the clear.
    ///
    /// A click in the document is not enough on its own: the OS dialog
    /// is the point where a person standing at the machine agrees. The
    /// secret then goes to the chrome view only, which no website can
    /// reach.
    pub(super) fn reveal_password(&mut self, origin: &str, username: &str) -> anyhow::Result<()> {
        let Some(secret) = self
            .vault
            .credentials()
            .find(origin, username)
            .map(|c| c.secret().expose().to_string())
        else {
            return Ok(());
        };
        let who = if username.is_empty() {
            origin.to_string()
        } else {
            format!("{username} at {origin}")
        };
        if !confirm(
            "Show password",
            &format!("Show the saved password for {who} on screen?"),
        ) {
            return Ok(());
        }
        let reply = SecretReply {
            origin: origin.to_string(),
            username: username.to_string(),
            secret,
        };
        let json = serde_json::to_string(&reply)?;
        self.chrome.eval(&format!("window.__cryo_secret({json})"))?;
        Ok(())
    }

    pub(super) fn remove_password(&mut self, origin: &str, username: &str) -> anyhow::Result<()> {
        match self.vault.remove(origin, username) {
            Ok(false) => Ok(()),
            Ok(true) => self.push_library(),
            Err(e) => self.notice(false, "Could not forget that password", vec![e.to_string()]),
        }
    }

    /// The one way anything in here talks back to the user. Phrasing
    /// stays on this side: the document renders the lines it is given.
    pub(super) fn notice(&self, ok: bool, title: &str, lines: Vec<String>) -> anyhow::Result<()> {
        let notice = Notice {
            ok,
            title: title.to_string(),
            lines,
        };
        let json = serde_json::to_string(&notice)?;
        self.chrome.eval(&format!("window.__cryo_notice({json})"))?;
        Ok(())
    }
}

fn wire_bookmark(mark: &crate::bookmarks::Bookmark) -> ChromeBookmark {
    ChromeBookmark {
        id: mark.id().get(),
        title: mark.title().to_string(),
        url: mark.url().to_string(),
        folder: mark.folder().to_string(),
        added_at: mark.added_at(),
        favicon: mark.favicon().map(str::to_string),
    }
}

/// A yes/no the user has to answer before something irreversible or
/// revealing happens. Blocks the event loop while it is up, same as
/// every other modal system dialog the shell opens (Ctrl+O, Save as).
pub(super) fn confirm(title: &str, question: &str) -> bool {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(question)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}
