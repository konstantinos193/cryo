//! View callbacks: what the engine surfaces report back — titles,
//! icons, navigations and the loading signal. `dispatch` stays a flat
//! table of one-liners; the policy those events need lives here.
//!
//! A view already dropped can still have events in flight from before the
//! drop. None reach these handlers: `App::on_view_event` lets a report
//! through only while the `ViewKey` it carries is its tab's live surface,
//! so every handler here may treat `id` as live.

use crate::shell::chrome::favicon_data_url;
use crate::shell::pages;
use crate::tabs::TabId;

use super::{App, DEFAULT_URL};

impl App {
    /// A view's document title changed — record it on the tab and attach
    /// it to the remembered visit so suggestions can match on it.
    /// Internal pages no-op: they were never recorded.
    pub(super) fn on_title_changed(&mut self, id: TabId, title: String) -> anyhow::Result<()> {
        let url = self.registry.get(id).map(|t| t.url().to_string());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title(title.clone());
        }
        // Titles arrive after the URL was already recorded — this
        // attaches it so suggestions can match on it.
        if let Some(url) = url {
            self.note_title(&url, &title);
        }
        self.sync_chrome()
    }

    /// A view committed a top-level navigation: record the visit, move
    /// the tab's address, and let it count as a session-change hint.
    pub(super) fn on_navigated(&mut self, id: TabId, url: String) -> anyhow::Result<()> {
        if self.registry.active_id() == Some(id) {
            self.cancel_permission_prompt()?;
            self.close_password_prompt()?;
        }
        // Internal pages are shell surfaces, not tab destinations —
        // never record them as a tab's address.
        if pages::is_internal_url(&url) {
            return Ok(());
        }
        // Sign-ins and sign-outs all route through Google — a
        // navigation onto one is the cheapest signal the session
        // may have changed.
        if crate::account::is_google_url(&url) {
            self.refresh_google_session();
            // A non-default account is browsing this window: point the
            // navigation at its `authuser` instead of Google's default.
            // The rewritten load reports back here carrying the param —
            // `with_authuser` then leaves it alone, so this runs once.
            if let Some(rewritten) = crate::account::with_authuser(&url, self.active_account) {
                if let Some(view) = self.view_of(id) {
                    return view.load_url(&rewritten).map_err(anyhow::Error::from);
                }
            }
        }
        // The visit counts whether the load works out or not —
        // real omniboxes remember typed destinations too.
        // about:blank is no destination at all — downloads that
        // re-report their host's blank start would litter it in.
        if url != DEFAULT_URL {
            self.remember(&url);
        }
        // A retry tab reporting somewhere other than its own
        // download URI (or the blank it never left) means the
        // user took the tab over — it stops being disposable.
        if let Some(tab) = self.registry.get_mut(id) {
            if tab.is_retry_host() && url != DEFAULT_URL && tab.url() != url {
                tab.release_retry();
            }
            tab.navigate(url);
        }
        if self.registry.active_id() == Some(id) {
            self.sync_chrome()
        } else {
            Ok(())
        }
    }

    /// A view's icon arrived (or cleared). The tab wears it, history
    /// remembers it beside the visit, and a kept page gets it too — the
    /// star may have been pressed before the icon arrived, and an
    /// imported bookmark only gets one on this first visit.
    pub(super) fn on_favicon_changed(
        &mut self,
        id: TabId,
        png: Option<Vec<u8>>,
    ) -> anyhow::Result<()> {
        let url = self.registry.get(id).map(|t| t.url().to_string());
        let icon = png.as_deref().map(favicon_data_url);
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_favicon(png);
        }
        // The icon outlives the tab — the omnibox paints it beside the
        // remembered visit. Internal pages don't reach history, so
        // nothing records them.
        let mut lib_dirty = false;
        if let (Some(url), Some(icon)) = (url, icon) {
            if !pages::is_internal_url(&url) {
                self.note_favicon(&url, &icon);
                match self.bookmarks.set_favicon(&url, &icon) {
                    Ok(changed) => lib_dirty = changed,
                    Err(e) => crate::report!("{e:?}"),
                }
            }
        }
        let synced = self.sync_chrome();
        if synced.is_ok() && lib_dirty {
            self.push_library()
        } else {
            synced
        }
    }

    /// A view's loading signal flipped — the spinner's data.
    pub(super) fn on_load_changed(&mut self, id: TabId, loading: bool) -> anyhow::Result<()> {
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_loading(loading);
        }
        self.sync_chrome()
    }
}
