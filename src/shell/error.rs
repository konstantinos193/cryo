//! The page cryo shows when a navigation produced no document.
//!
//! The engine reports *why* a load failed as a [`LoadFailure`]; this module
//! turns that into an internal URL, and the page at that URL renders the copy.
//! The copy lives in the page's script rather than in Rust because the page
//! is static: `PageResolver` hands back `&'static str`, so the parameters
//! travel in the query string and the document fills itself in.
//!
//! Document, stylesheet and script live in `error/`, one file each.

use crate::engine::LoadFailure;
use crate::shell::pages::INTERNAL_ORIGIN;

/// Path the error page is served on, relative to [`INTERNAL_ORIGIN`].
pub const ERROR_PATH: &str = "/error";

/// The error document. Reads its parameters from `location.search` and
/// picks its own copy; `<title>` is set from script because the tab strip
/// reads it.
pub(crate) const HTML: &str = include_str!("error/error.html");

/// Its stylesheet, served at `/error/error.css`.
pub(crate) const STYLE: &str = include_str!("error/error.css");

/// Its script, served at `/error/error.js`. This is where the copy for
/// every failure lives.
pub(crate) const SCRIPT: &str = include_str!("error/error.js");

/// URL that renders `failure` for the address the user was trying to reach.
pub fn error_url(failure: LoadFailure, attempted: &str) -> String {
    let mut url = format!(
        "{INTERNAL_ORIGIN}{ERROR_PATH}?kind={}&url={}",
        slug(failure),
        encode(attempted)
    );
    if let LoadFailure::Http(status) = failure {
        url.push_str("&code=");
        url.push_str(&status.code().to_string());
    }
    url
}

/// URL for a Chrome Web Store install that could not happen. The caller
/// decides a failed address was one of those — `extensions::web_store` owns
/// that judgement — because the engine only ever reports "host not found",
/// which is true of the placeholder address and no use to anyone reading it.
pub fn web_store_url(attempted: &str) -> String {
    format!(
        "{INTERNAL_ORIGIN}{ERROR_PATH}?kind={WEB_STORE_KIND}&url={}",
        encode(attempted)
    )
}

/// Not a `LoadFailure`: the engine cannot tell this apart from any other
/// name that does not resolve, so the kind is chosen above it.
const WEB_STORE_KIND: &str = "web-store";

/// Stable name for a failure, shared by the URL and the page's copy table.
/// Renaming one without the other silently falls back to the generic page.
fn slug(failure: LoadFailure) -> &'static str {
    match failure {
        LoadFailure::Offline => "offline",
        LoadFailure::HostNotFound => "host-not-found",
        LoadFailure::TimedOut => "timed-out",
        LoadFailure::ConnectionFailed => "connection-failed",
        LoadFailure::CertificateRejected => "certificate-rejected",
        LoadFailure::RedirectLoop => "redirect-loop",
        LoadFailure::MalformedResponse => "malformed-response",
        LoadFailure::Http(_) => "http",
        LoadFailure::RenderProcessGone => "render-process-gone",
        LoadFailure::Unknown => "unknown",
    }
}

/// Percent-encode a query value. Everything outside the unreserved set goes,
/// so an address carrying `&`, `#` or `=` cannot restructure the query it
/// rides in.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(hex(byte >> 4));
                out.push(hex(byte & 0x0f));
            }
        }
    }
    out
}

fn hex(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16)
        .expect("nibble is below 16")
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::HttpStatus;

    const ALL: [LoadFailure; 10] = [
        LoadFailure::Offline,
        LoadFailure::HostNotFound,
        LoadFailure::TimedOut,
        LoadFailure::ConnectionFailed,
        LoadFailure::CertificateRejected,
        LoadFailure::RedirectLoop,
        LoadFailure::MalformedResponse,
        LoadFailure::Http(HttpStatus::new(500)),
        LoadFailure::RenderProcessGone,
        LoadFailure::Unknown,
    ];

    #[test]
    fn url_carries_kind_and_target() {
        let url = error_url(LoadFailure::HostNotFound, "https://example.com/a b");
        assert!(url.starts_with("http://cryo.localhost/error?"));
        assert!(url.contains("kind=host-not-found"));
        assert!(url.contains("url=https%3A%2F%2Fexample.com%2Fa%20b"));
    }

    #[test]
    fn http_failures_carry_the_status_code() {
        let url = error_url(
            LoadFailure::Http(HttpStatus::new(503)),
            "https://example.com",
        );
        assert!(url.contains("kind=http"));
        assert!(url.contains("&code=503"));
    }

    #[test]
    fn other_failures_carry_no_status_code() {
        assert!(!error_url(LoadFailure::Offline, "https://example.com").contains("code="));
    }

    #[test]
    fn the_web_store_page_has_copy_and_offers_no_retry() {
        let url = web_store_url(
            "https://permanently-removed.invalid/?prod=chromiumcrx&x=id%3Dnkbihfbeogaeaoehlefnkodbefgpgknn",
        );
        assert!(url.contains("kind=web-store"));
        // No code, and the attempted address is still carried for display.
        assert!(!url.contains("code="));
        assert!(url.contains("url=https%3A%2F%2Fpermanently-removed.invalid"));
        // The copy table must hold the kind, or the page silently falls back
        // to the generic one.
        assert!(SCRIPT.contains("\"web-store\": ["));
        // Retrying a placeholder address can only fail again.
        assert!(SCRIPT.contains("kind !== \"web-store\""));
    }

    #[test]
    fn encoding_cannot_restructure_the_query() {
        let url = error_url(
            LoadFailure::Unknown,
            "https://evil.test/?a=1&kind=offline#x",
        );
        assert_eq!(url.matches("kind=").count(), 1);
        assert!(!url.contains('#'));
    }

    #[test]
    fn every_failure_has_a_distinct_slug() {
        let mut slugs: Vec<&str> = ALL.iter().map(|f| slug(*f)).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), before, "two failures share a slug");
    }

    /// The script is the only place the copy lives, so a slug missing from
    /// its table renders the generic page by accident.
    #[test]
    fn script_has_copy_for_every_slug() {
        for failure in ALL {
            if matches!(failure, LoadFailure::Http(_)) {
                continue; // keyed by status code, not by slug
            }
            let key = format!("\"{}\":", slug(failure));
            assert!(SCRIPT.contains(&key), "script has no copy for {key}");
        }
        assert!(
            SCRIPT.contains("kind === \"http\""),
            "script never branches on http status codes"
        );
    }

    /// The copy table is script, not markup: the document holds only the
    /// shape the script fills in.
    #[test]
    fn the_document_carries_no_copy() {
        assert!(!HTML.contains("KINDS"));
        assert!(HTML.contains("id=\"title\""));
        assert!(HTML.contains("id=\"retry\""));
    }
}
