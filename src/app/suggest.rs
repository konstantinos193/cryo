//! The omnibox's suggestion list: what a keystroke gets offered — a
//! search/go verdict row, then kept pages, remembered inputs and
//! visits, then dial matches — answered with a `__cryo_suggest` eval.
//!
//! History is never fetchable: `/cmd` reaches us only from the chrome
//! view (content views carry no `on_ipc`), and the page resolver would
//! hand it to any origin.

use crate::shell::chrome::{completion_for, normalize_nav_input, SuggestReply, Suggestion};
use crate::shell::dial;

use super::App;

impl App {
    /// Build the rows for `q` and push them to the chrome document.
    /// The first row mirrors `normalize_nav_input`'s verdict: a
    /// URL-looking input says "Go to", anything else is a search.
    pub(super) fn suggest(&self, q: &str) -> anyhow::Result<()> {
        let q = q.trim();
        let mut rows: Vec<Suggestion> = Vec::new();
        if !q.is_empty() {
            let engine = &self.search_engine;
            let url = normalize_nav_input(q, engine);
            let is_search = url.starts_with(engine.url_prefix());
            rows.push(Suggestion {
                label: if is_search {
                    format!("Search {} for \"{q}\"", engine.name())
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
                    icon: mark.favicon().map(str::to_string),
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
}
