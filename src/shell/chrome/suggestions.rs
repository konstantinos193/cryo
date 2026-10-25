//! Omnibox reply vocabulary and inline completion policy.
use serde::Serialize;

/// `__cryo_suggest` payload — `q` is the query echoed back so the
/// document can drop a reply that lands after the user typed on.
#[derive(Debug, Serialize)]
pub struct SuggestReply {
    pub q: String,
    pub rows: Vec<Suggestion>,
}

/// One omnibox row.
#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub label: String,
    /// What goes back through `navigate` — a URL for sites, the raw
    /// text for remembered input (the app normalizes it again).
    pub input: String,
    /// The row's icon as a `data:` URL — `None` falls back to a glyph
    /// (search/go rows) or a letter dot (sites with no remembered icon).
    pub icon: Option<String>,
    /// What ArrowRight/End fills in — the URL minus its scheme when it
    /// extends the typed text. `None` on rows that can't complete (the
    /// search row, substring-only matches).
    pub complete: Option<String>,
    /// "search" | "go" | "page" — picks the fallback glyph.
    pub kind: &'static str,
}

/// The inline completion a suggestion offers, in the address bar's own
/// vocabulary: what the user typed stays, the tail arrives selected. A
/// URL the query prefixes as-typed wins first (so "https://git" keeps
/// its scheme); otherwise the scheme and `www.` come off and the bare
/// form is tried — that's the text the box fills, so the row completes
/// the way the user expects to type it.
pub fn completion_for(url: &str, q: &str) -> Option<String> {
    let ql = q.to_lowercase();
    if url.len() > q.len() && url.to_lowercase().starts_with(&ql) {
        return Some(url.to_string());
    }
    let mut bare = url;
    for prefix in ["https://", "http://", "www."] {
        if let Some(rest) = bare.strip_prefix(prefix) {
            bare = rest;
        }
    }
    if bare.len() > q.len() && bare.to_lowercase().starts_with(&ql) {
        Some(bare.to_string())
    } else {
        None
    }
}
