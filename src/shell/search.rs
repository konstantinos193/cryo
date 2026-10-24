//! Search engines: where the shell sends input that is not a URL.
//!
//! `ALL` is the preset list the settings page offers; `DEFAULT` is
//! DuckDuckGo, which a fresh profile and an unreadable pick both fall
//! back to. The pick itself lives in the shell origin's localStorage —
//! the `cryo.searchEngine` key, which only shell documents can write
//! (storage is origin-scoped, so this needs no command channel through
//! the engine). It holds either a preset slug or a custom URL template
//! with `%s` where the query goes; `chrome.html` reports it verbatim on
//! its `/cmd` wire and `SearchEngine::from_pick` is the one door back
//! into a domain value. `pages` documents `fetch('/engines')` for the
//! preset list — the protocol answers any origin, so `json()` carries
//! only the presets; the pick never leaves localStorage.

/// Path on the internal protocol serving [`json`]. Registered in
/// `pages::internal_page`; shell documents `fetch('/engines')`.
pub const PATH: &str = "/engines";

/// The preset list as the shell documents consume it:
/// `[{slug, name, prefix}]` — `prefix` is the query URL up to the
/// encoded text. Built once from [`ALL`] so the wire can't drift from
/// the domain list — a `OnceLock`, not a hand-kept second copy (§5
/// bans state singletons; this is a derived constant). Custom
/// templates never appear here — they live only in the pick.
pub fn json() -> &'static str {
    static JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    JSON.get_or_init(|| {
        let rows: Vec<serde_json::Value> = ALL
            .iter()
            .map(|e| {
                serde_json::json!({
                    "slug": e.slug(),
                    "name": e.name(),
                    "prefix": e.url_prefix(),
                })
            })
            .collect();
        serde_json::to_string(&rows).expect("engine rows are plain strings")
    })
}

/// A search provider — a value object. The presets carry fixed
/// vocabulary; `Custom` is whatever valid template the settings page
/// stored (any `http(s)://` URL holding `%s`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEngine {
    DuckDuckGo,
    Google,
    Bing,
    Brave,
    /// A user-entered template — `%s` marks where the encoded query
    /// goes (`https://kagi.com/search?q=%s`).
    Custom(String),
}

/// Every preset the settings page lists, in display order — `DEFAULT`
/// is `ALL[0]`. `Custom` is not a preset: it comes from the pick.
pub const ALL: &[SearchEngine] = &[
    SearchEngine::DuckDuckGo,
    SearchEngine::Google,
    SearchEngine::Bing,
    SearchEngine::Brave,
];

impl SearchEngine {
    /// The out-of-box engine — and what any unreadable pick falls
    /// back to.
    pub const DEFAULT: SearchEngine = SearchEngine::DuckDuckGo;

    /// The hydration door (AGENTS §6): a stored or wire pick → an
    /// engine. A preset slug resolves to the preset; anything else is
    /// tried as a custom template — an `http(s)://` URL containing `%s`.
    /// `None` for anything the door can't validate — callers keep what
    /// they have or fall back to `DEFAULT`, never build one.
    pub fn from_pick(pick: &str) -> Option<Self> {
        let pick = pick.trim();
        if let Some(engine) = Self::by_slug(pick) {
            return Some(engine);
        }
        let rest = pick
            .strip_prefix("https://")
            .or_else(|| pick.strip_prefix("http://"))?;
        let host = rest.split('/').next().unwrap_or("");
        if host.is_empty() || !rest.contains("%s") || rest.contains(char::is_whitespace) {
            return None;
        }
        Some(Self::Custom(pick.to_string()))
    }

    /// Preset slugs only — `custom` is not one: a custom pick is the
    /// template itself, not a name for it.
    pub fn by_slug(slug: &str) -> Option<Self> {
        ALL.iter().find_map(|e| {
            if e.slug() == slug {
                Some(e.clone())
            } else {
                None
            }
        })
    }

    /// The preset's storage/wire name; "custom" for a template pick.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Brave => "brave",
            Self::Custom(_) => "custom",
        }
    }

    /// What the settings page and the omnibox's "Search …" row call it —
    /// the host for a custom engine ("kagi.com"), since a template has
    /// no name of its own.
    pub fn name(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::DuckDuckGo => "DuckDuckGo".into(),
            Self::Google => "Google".into(),
            Self::Bing => "Bing".into(),
            Self::Brave => "Brave".into(),
            Self::Custom(t) => t
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .filter(|host| !host.is_empty())
                .map(|host| host.to_string().into())
                .unwrap_or_else(|| "custom engine".into()),
        }
    }

    /// The search URL's fixed head — everything up to where the encoded
    /// query lands. Also the marker `suggest` uses to know
    /// `normalize_nav_input` returned a search, not an address.
    pub fn url_prefix(&self) -> &str {
        match self {
            Self::DuckDuckGo => "https://duckduckgo.com/?q=",
            Self::Google => "https://www.google.com/search?q=",
            Self::Bing => "https://www.bing.com/search?q=",
            Self::Brave => "https://search.brave.com/search?q=",
            Self::Custom(t) => t.split("%s").next().unwrap_or(t),
        }
    }

    /// Where `query` searches — the text is percent-encoded inside so no
    /// caller can build a half-escaped URL.
    pub fn search_url(&self, query: &str) -> String {
        let encoded = percent_encode(query);
        match self {
            Self::Custom(t) => t.replacen("%s", &encoded, 1),
            _ => format!("{}{}", self.url_prefix(), encoded),
        }
    }
}

/// Percent-encoding for the query tail — the RFC 3986 unreserved set
/// passes through, everything else is `%XX`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_duckduckgo() {
        assert_eq!(SearchEngine::DEFAULT.slug(), "duckduckgo");
        assert_eq!(
            SearchEngine::by_slug("duckduckgo"),
            Some(SearchEngine::DEFAULT)
        );
    }

    #[test]
    fn by_slug_finds_each_preset_and_rejects_unknowns() {
        for e in ALL {
            assert_eq!(SearchEngine::by_slug(e.slug()).as_ref(), Some(e));
        }
        assert_eq!(SearchEngine::by_slug("custom"), None);
        assert_eq!(SearchEngine::by_slug("altavista"), None);
        assert_eq!(SearchEngine::by_slug(""), None);
    }

    #[test]
    fn from_pick_accepts_slugs_and_valid_templates() {
        assert_eq!(
            SearchEngine::from_pick("google"),
            Some(SearchEngine::Google)
        );
        assert_eq!(
            SearchEngine::from_pick("https://kagi.com/search?q=%s"),
            Some(SearchEngine::Custom(
                "https://kagi.com/search?q=%s".to_string()
            ))
        );
        // Self-hosted engines are plain http — allowed.
        assert_eq!(
            SearchEngine::from_pick("http://localhost:8888/search?q=%s"),
            Some(SearchEngine::Custom(
                "http://localhost:8888/search?q=%s".to_string()
            ))
        );
    }

    #[test]
    fn from_pick_rejects_what_is_not_a_template() {
        for bad in [
            "",
            "  ",
            "altavista",
            // No query placeholder — the engine can't be expressed.
            "https://kagi.com/search",
            // No scheme at all, and a scheme we won't speak.
            "kagi.com/search?q=%s",
            "ftp://kagi.com/?q=%s",
            // No host, and whitespace in the URL.
            "https:///?q=%s",
            "https://kagi.com/?q= %s",
        ] {
            assert_eq!(SearchEngine::from_pick(bad), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn custom_names_its_host() {
        let e = SearchEngine::from_pick("https://kagi.com/search?q=%s").unwrap();
        assert_eq!(e.name(), "kagi.com");
        assert_eq!(e.slug(), "custom");
    }

    #[test]
    fn search_url_encodes_the_query() {
        assert_eq!(
            SearchEngine::DEFAULT.search_url("rust borrow checker"),
            "https://duckduckgo.com/?q=rust%20borrow%20checker"
        );
        assert_eq!(
            SearchEngine::Google.search_url("a&b"),
            "https://www.google.com/search?q=a%26b"
        );
        let kagi = SearchEngine::from_pick("https://kagi.com/search?q=%s&l=en").unwrap();
        assert_eq!(
            kagi.search_url("two words"),
            "https://kagi.com/search?q=two%20words&l=en"
        );
        assert_eq!(
            kagi.url_prefix(),
            "https://kagi.com/search?q=",
            "the prefix ends where the query lands"
        );
    }

    #[test]
    fn json_mirrors_all() {
        let rows: serde_json::Value = serde_json::from_str(json()).expect("engine JSON parses");
        let rows = rows.as_array().expect("engine JSON is an array");
        assert_eq!(rows.len(), ALL.len(), "wire list and ALL drifted");
        for (row, e) in rows.iter().zip(ALL) {
            assert_eq!(row["slug"].as_str().unwrap(), e.slug());
            assert_eq!(row["name"].as_str().unwrap(), e.name());
            assert_eq!(row["prefix"].as_str().unwrap(), e.url_prefix());
        }
    }

    #[test]
    fn every_preset_is_https() {
        for e in ALL {
            assert!(
                e.url_prefix().starts_with("https://"),
                "{} is not https",
                e.name()
            );
        }
    }
}
