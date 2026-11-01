//! Active-document search orchestration; queries stay in the find field only.

use crate::engine::FindMatches;
use crate::tabs::TabId;

use super::{post, App, AppEvent, ViewEvent};

/// The current window's transient search identity, never persisted.
#[derive(Default)]
pub(super) struct FindSession {
    open: bool,
    revision: u64,
}

impl FindSession {
    fn accepts(&self, revision: u64) -> bool {
        self.open && self.revision == revision
    }
}

impl App {
    pub(super) fn open_find(&mut self) -> anyhow::Result<()> {
        let supported = self.active_view().is_some_and(|view| view.supports_find());
        self.dismiss_overlays()?;
        self.find.open = true;
        self.chrome.focus()?;
        self.chrome_focused = true;
        self.chrome
            .eval(&format!("window.__cryo_find_open({supported})"))?;
        Ok(())
    }

    pub(super) fn search_page(&mut self, query: String, match_case: bool) -> anyhow::Result<()> {
        if !self.find.open {
            return Ok(());
        }
        self.find.revision = self.find.revision.wrapping_add(1);
        let revision = self.find.revision;
        let Some(id) = self.registry.active_id() else {
            return Ok(());
        };
        let Some(key) = self.registry.get(id).and_then(|tab| tab.view_key()) else {
            return Ok(());
        };
        let Some(view) = self.active_view() else {
            return Ok(());
        };
        if query.is_empty() {
            view.find_clear()?;
            return self.find_result(id, revision, Ok(FindMatches::default()));
        }
        if query.chars().count() > 2048 {
            view.find_clear()?;
            return self.find_result(
                id,
                revision,
                Err(crate::engine::EngineError::View(
                    "Search is too long".into(),
                )),
            );
        }
        let proxy = self.proxy.clone();
        let result = view.find(
            &query,
            match_case,
            Box::new(move |result| {
                post(
                    &proxy,
                    AppEvent::View(key, ViewEvent::FindResult(revision, result)),
                );
            }),
        );
        if let Err(error) = result {
            return self.find_result(id, revision, Err(error));
        }
        Ok(())
    }

    pub(super) fn next_match(&mut self, backwards: bool) -> anyhow::Result<()> {
        if !self.find.open {
            return self.open_find();
        }
        if let Some(view) = self.active_view() {
            if let Err(error) = view.find_next(backwards) {
                self.chrome.eval(&format!(
                    "window.__cryo_find_result({})",
                    serde_json::json!({"error":error.to_string()})
                ))?;
            }
        }
        Ok(())
    }

    pub(super) fn close_find(&mut self) -> anyhow::Result<()> {
        if !self.find.open {
            return Ok(());
        }
        self.find.open = false;
        self.find.revision = self.find.revision.wrapping_add(1);
        if let Some(view) = self.active_view() {
            view.find_clear()?;
        }
        self.chrome.eval("window.__cryo_find_close()")?;
        Ok(())
    }

    pub(super) fn dismiss_find(&mut self) -> anyhow::Result<()> {
        self.close_find()?;
        self.focus_page()
    }

    pub(super) fn stop_or_close_find(&mut self) -> anyhow::Result<()> {
        if self.find.open {
            self.dismiss_find()
        } else {
            self.stop_active()
        }
    }

    pub(super) fn find_result(
        &self,
        id: TabId,
        revision: u64,
        result: Result<FindMatches, crate::engine::EngineError>,
    ) -> anyhow::Result<()> {
        if !self.find.accepts(revision) || self.registry.active_id() != Some(id) {
            return Ok(());
        }
        let payload = match result {
            Ok(found) => serde_json::json!({"active":found.active,"total":found.total}),
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        self.chrome
            .eval(&format!("window.__cryo_find_result({payload})"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_from_closed_or_superseded_searches_are_ignored() {
        let session = FindSession {
            open: true,
            revision: 9,
        };
        assert!(session.accepts(9));
        assert!(!session.accepts(8));
        let closed = FindSession {
            open: false,
            revision: 9,
        };
        assert!(!closed.accepts(9));
    }
}
