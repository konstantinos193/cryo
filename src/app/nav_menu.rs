//! The back/forward arrows' right-click menus, app side: read the active
//! view's session history when the document asks, push the rows as
//! `__cryo_nav_menu`, and jump to the row picked. The document owns the
//! popup; a page's trail lives in the engine and is never copied here.

use crate::engine::{EngineError, SessionEntry, SessionEntryId, SessionHistory};
use crate::shell::chrome::{ChromeNavMenu, ChromeNavRow};
use crate::tabs::TabId;

use super::App;

/// Rows in one menu. Past this a long trail is a scroll nobody wants; the
/// menu's "Show full history" row is the way further back.
const MENU_ROWS: usize = 12;

/// The blank document a view starts on. It can sit on the trail, but it is
/// not a page anyone visited.
const BLANK: &str = "about:blank";

impl App {
    /// Answer a right-click on an arrow. An engine that can't list its
    /// history leaves the click with nothing to show, which is not a fault.
    pub(super) fn push_nav_menu(&mut self, forward: bool) -> anyhow::Result<()> {
        let (Some(id), Some(view)) = (self.registry.active_id(), self.active_view()) else {
            return Ok(());
        };
        let history = match view.session_history() {
            Ok(history) => history,
            Err(EngineError::Unsupported(_)) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let rows = menu_rows(&history, forward, |url| self.known_favicon(url));
        let menu = ChromeNavMenu {
            tab: id.get(),
            forward,
            rows,
        };
        let json = serde_json::to_string(&menu)?;
        // The menu takes the arrow keys, and DOM focus alone leaves them
        // with whichever view last held the keyboard.
        self.chrome.focus()?;
        self.chrome_focused = true;
        self.chrome
            .eval(&format!("window.__cryo_nav_menu({json})"))?;
        Ok(())
    }

    /// A menu row picked. A tab switch between listing and picking makes
    /// the id meaningless to the view now on screen, so the jump is dropped
    /// rather than aimed at whatever that view calls the same number.
    pub(super) fn go_to_session_entry(&mut self, tab: u64, entry: i64) -> anyhow::Result<()> {
        if self.registry.active_id() != Some(TabId::from_raw(tab)) {
            return Ok(());
        }
        if let Some(view) = self.active_view() {
            view.go_to_session_entry(SessionEntryId::from_raw(entry))?;
        }
        Ok(())
    }

    /// The icon the history store holds for `url`, if it has one.
    fn known_favicon(&self, url: &str) -> Option<String> {
        self.recorder
            .as_ref()?
            .entries()
            .iter()
            .find(|e| e.text == url)?
            .favicon
            .clone()
    }
}

fn menu_rows(
    history: &SessionHistory,
    forward: bool,
    favicon: impl Fn(&str) -> Option<String>,
) -> Vec<ChromeNavRow> {
    let entries: Box<dyn Iterator<Item = &SessionEntry>> = if forward {
        Box::new(history.ahead())
    } else {
        Box::new(history.behind())
    };
    entries
        .filter(|e| e.url != BLANK)
        .take(MENU_ROWS)
        .map(|e| ChromeNavRow {
            entry: e.id.get(),
            title: e.title.clone(),
            url: e.url.clone(),
            favicon: favicon(&e.url),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(urls: &[&str], current: usize) -> SessionHistory {
        let entries = urls
            .iter()
            .enumerate()
            .map(|(i, url)| SessionEntry {
                id: SessionEntryId::from_raw(100 + i as i64),
                url: (*url).to_string(),
                title: format!("Page {i}"),
            })
            .collect();
        SessionHistory::new(entries, current).unwrap()
    }

    fn urls(rows: &[ChromeNavRow]) -> Vec<&str> {
        rows.iter().map(|r| r.url.as_str()).collect()
    }

    #[test]
    fn each_arrow_lists_its_own_direction_nearest_first() {
        let trail = history(&["a", "b", "c", "d"], 2);
        assert_eq!(urls(&menu_rows(&trail, false, |_| None)), ["b", "a"]);
        assert_eq!(urls(&menu_rows(&trail, true, |_| None)), ["d"]);
    }

    #[test]
    fn a_row_carries_what_the_jump_needs() {
        let trail = history(&["a", "b"], 1);
        let rows = menu_rows(&trail, false, |url| Some(format!("icon:{url}")));
        assert_eq!(rows[0].entry, 100);
        assert_eq!(rows[0].title, "Page 0");
        assert_eq!(rows[0].favicon.as_deref(), Some("icon:a"));
    }

    #[test]
    fn the_blank_starting_document_is_not_a_page_you_visited() {
        let trail = history(&[BLANK, "a", "b"], 2);
        assert_eq!(urls(&menu_rows(&trail, false, |_| None)), ["a"]);
    }

    #[test]
    fn a_long_trail_stops_at_the_nearest_rows() {
        let many: Vec<String> = (0..30).map(|i| format!("p{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let rows = menu_rows(&history(&many, 29), false, |_| None);
        assert_eq!(rows.len(), MENU_ROWS);
        assert_eq!(rows[0].url, "p28");
    }
}
