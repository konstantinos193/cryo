//! Internal pages served over the engine's `cryo` protocol.
//!
//! Shell surfaces are real `http://cryo.localhost/...` documents, not
//! `NavigateToString` HTML: a `data:`-document runs no scripts and its IPC
//! messages never reach the host (wry drops unparseable Source URIs).

use crate::shell::{chrome, error};

/// Origin that all internal pages share. `is_internal_url` keys off this —
/// keep the two in sync if the scheme registration ever changes.
pub(crate) const INTERNAL_ORIGIN: &str = "http://cryo.localhost";

pub const CHROME_URL: &str = "http://cryo.localhost/chrome";
pub const NEW_TAB_URL: &str = "http://cryo.localhost/newtab";

/// A URL that belongs to the shell, not to any web destination.
pub fn is_internal_url(url: &str) -> bool {
    url.starts_with(INTERNAL_ORIGIN)
}

/// Engine-facing resolver for the `cryo` scheme. wry hands us the request
/// path (`/chrome`), we hand back static HTML.
pub fn internal_page(path: &str) -> Option<&'static str> {
    match path {
        "/chrome" => Some(chrome::CHROME_HTML),
        "/newtab" => Some(NEW_TAB_HTML),
        // The error page reads its parameters from the query string, which
        // never reaches this resolver — one document covers every failure.
        error::ERROR_PATH => Some(error::ERROR_HTML),
        _ => None,
    }
}

/// New-tab page. The `<title>` matters: it is what the tab strip shows.
pub const NEW_TAB_HTML: &str = r##"<!doctype html>
<html>
<head><meta charset="utf-8"><title>New tab</title><style>
  body {
    height: 100vh; margin: 0; display: flex; align-items: center;
    justify-content: center; flex-direction: column; gap: 8px;
    background: #16181c; color: #d7dae0;
    font: 14px/1.5 system-ui, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  .brand { font-size: 28px; font-weight: 600; letter-spacing: 1px; }
  .tag { color: #8a8f98; }
</style></head>
<body>
  <div class="brand">cryo</div>
  <div class="tag">Tabs freeze. Budgets hold.</div>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_pages_only() {
        assert!(internal_page("/chrome").is_some());
        assert!(internal_page("/newtab").is_some());
        assert!(internal_page("/etc/passwd").is_none());
    }

    #[test]
    fn internal_urls_match_protocol_origin() {
        assert!(is_internal_url(NEW_TAB_URL));
        assert!(is_internal_url(CHROME_URL));
        assert!(!is_internal_url("https://example.com"));
        assert!(!is_internal_url("http://cryo.evil.com"));
    }
}
