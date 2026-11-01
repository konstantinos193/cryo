//! Speed dial: the pinned sites the shell keeps one click away.
//!
//! One list feeds two surfaces — the dial bar under the chrome's toolbar
//! and the tiles on the new-tab page. Both are shell documents that fetch
//! `/dials` over the internal protocol at load, so nothing here reaches
//! into the app. The protocol answers any origin, which means a website
//! can read the list too — keep it free of anything private.
//!
//! The entries are compile-time defaults until a `persist` context exists
//! (Phase 1+); a stored list then replaces [`ENTRIES`] and this module
//! becomes the seed + the migration home.

/// Path on the internal protocol serving [`json`]. Registered in
/// `pages::internal_page`; shell documents `fetch('/dials')`.
pub const PATH: &str = "/dials";

/// One pinned destination. The app matches these for omnibox
/// suggestions; the shell documents read the serialized list.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub name: &'static str,
    pub url: &'static str,
}

/// The shipped dial set, in display order. Destinations are `https` and
/// names are unique — the tests pin both down.
pub const ENTRIES: &[Entry] = &[
    Entry {
        name: "Google",
        url: "https://www.google.com",
    },
    Entry {
        name: "Reddit",
        url: "https://www.reddit.com",
    },
    Entry {
        name: "YouTube",
        url: "https://www.youtube.com",
    },
    Entry {
        name: "GitHub",
        url: "https://github.com",
    },
    Entry {
        name: "Wikipedia",
        url: "https://www.wikipedia.org",
    },
    Entry {
        name: "DuckDuckGo",
        url: "https://duckduckgo.com",
    },
];

/// The dial set as the shell documents consume it: `[{name, url}]`.
/// Built once from [`ENTRIES`] so the wire cannot drift from the list the
/// app matches against — the same shape `search::json` uses, and the
/// reason neither is a hand-written blob.
pub fn json() -> &'static str {
    static JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    JSON.get_or_init(|| {
        let rows: Vec<serde_json::Value> = ENTRIES
            .iter()
            .map(|e| serde_json::json!({ "name": e.name, "url": e.url }))
            .collect();
        serde_json::to_string(&rows).expect("dial rows are plain strings")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_nonempty_with_https_destinations_only() {
        assert!(!ENTRIES.is_empty());
        for e in ENTRIES {
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
        let mut names: Vec<&str> = ENTRIES.iter().map(|e| e.name).collect();
        let mut urls: Vec<&str> = ENTRIES.iter().map(|e| e.url).collect();
        names.sort_unstable();
        urls.sort_unstable();
        names.dedup();
        urls.dedup();
        assert_eq!(names.len(), ENTRIES.len(), "duplicate dial names");
        assert_eq!(urls.len(), ENTRIES.len(), "duplicate dial urls");
    }

    /// The documents read the wire, the omnibox reads the list. One
    /// derives from the other, and this is what says so.
    #[test]
    fn the_wire_carries_every_entry() {
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(json()).expect("the dial wire is JSON");
        assert_eq!(rows.len(), ENTRIES.len());
        for (row, entry) in rows.iter().zip(ENTRIES) {
            assert_eq!(row["name"], entry.name);
            assert_eq!(row["url"], entry.url);
        }
    }
}
