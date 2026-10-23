//! Speed dial: the pinned sites the shell keeps one click away.
//!
//! One list feeds two surfaces — the dial bar under the chrome's toolbar
//! and the tiles on the new-tab page. Both are shell documents that fetch
//! `/dials` over the internal protocol at load, so nothing here reaches
//! into the app. The protocol answers any origin, which means a website
//! can read the list too — keep it free of anything private.
//!
//! The entries are compile-time defaults until a `persist` context exists
//! (Phase 1+); a stored list then replaces `JSON` and this module becomes
//! the seed + the migration home.

/// Path on the internal protocol serving [`JSON`]. Registered in
/// `pages::internal_page`; shell documents `fetch('/dials')`.
pub const PATH: &str = "/dials";

/// The dial set as the shell documents consume it: `[{name, url}]`.
/// Served verbatim — it must stay valid JSON and only carry `https`
/// destinations, which the tests pin down.
pub const JSON: &str = r#"[
  {"name":"Google","url":"https://www.google.com"},
  {"name":"Reddit","url":"https://www.reddit.com"},
  {"name":"YouTube","url":"https://www.youtube.com"},
  {"name":"GitHub","url":"https://github.com"},
  {"name":"Wikipedia","url":"https://www.wikipedia.org"},
  {"name":"DuckDuckGo","url":"https://duckduckgo.com"}
]"#;

/// One pinned destination, parsed from [`JSON`]. The app matches these
/// for omnibox suggestions; the shell documents read the JSON directly.
#[derive(Debug, serde::Deserialize)]
pub struct Entry {
    pub name: String,
    pub url: String,
}

/// The dial set parsed. Called per keystroke from `suggest` — cheap at
/// this list's size; if it ever grows, cache it.
pub fn entries() -> Vec<Entry> {
    serde_json::from_str(JSON).expect("dial JSON parses — the tests pin it")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_nonempty_with_https_destinations_only() {
        let entries = entries();
        assert!(!entries.is_empty());
        for e in &entries {
            assert!(!e.name.trim().is_empty(), "dial entry needs a name");
            assert!(
                e.url.starts_with("https://"),
                "dial entry '{}' is not https: {}",
                e.name,
                e.url
            );
        }
    }

    #[test]
    fn names_and_urls_are_unique() {
        let entries = entries();
        let mut names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        let mut urls: Vec<_> = entries.iter().map(|e| e.url.as_str()).collect();
        names.sort_unstable();
        urls.sort_unstable();
        names.dedup();
        urls.dedup();
        assert_eq!(names.len(), entries.len(), "duplicate dial names");
        assert_eq!(urls.len(), entries.len(), "duplicate dial urls");
    }
}
