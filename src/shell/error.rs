//! The page cryo shows when a navigation produced no document.
//!
//! The engine reports *why* a load failed as a [`LoadFailure`]; this module
//! turns that into an internal URL, and the page at that URL renders the copy.
//! The copy lives in the template rather than in Rust because the page is
//! static: `PageResolver` hands back `&'static str`, so the parameters travel
//! in the query string and the document fills itself in.

use crate::engine::LoadFailure;
use crate::shell::pages::INTERNAL_ORIGIN;

/// Path the error page is served on, relative to [`INTERNAL_ORIGIN`].
pub const ERROR_PATH: &str = "/error";

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

/// Error page. Reads its parameters from `location.search` and picks its own
/// copy; `<title>` is set from script because the tab strip reads it.
pub const ERROR_HTML: &str = r##"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Problem loading page</title><style>
  :root { color-scheme: dark; }
  body {
    min-height: 100vh; margin: 0; display: flex; align-items: center;
    justify-content: center; background: #16181c; color: #d7dae0;
    font: 15px/1.6 system-ui, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  main { width: min(460px, calc(100vw - 64px)); padding: 32px 0; }
  svg { width: 56px; height: 56px; display: block; opacity: .85; }
  h1 { font-size: 21px; font-weight: 600; margin: 22px 0 8px; letter-spacing: -.01em; }
  p { margin: 0; color: #9aa0aa; }
  .target {
    margin-top: 18px; padding: 10px 12px; border-radius: 6px;
    background: #1d2026; color: #7e858f; font-size: 13px;
    font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
    word-break: break-all; -webkit-user-select: text; user-select: text;
  }
  button {
    margin-top: 22px; padding: 9px 18px; border: 0; border-radius: 6px;
    background: #2a7089; color: #eaf9fd; font: inherit; font-size: 14px;
    font-weight: 500; cursor: pointer;
  }
  button:hover { background: #3b8ea8; }
  button:focus-visible { outline: 2px solid #78c7dc; outline-offset: 2px; }
</style></head>
<body>
  <main>
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" aria-hidden="true"><defs><clipPath id="cryoMark"><path d="M76.18 69.55 L52.25 85.44 L19.82 66.72 L19.82 29.28 L52.25 10.56 L76.18 26.45 L69.46 32.5 L52.25 28.65 L37.54 39.51 L37.54 56.49 L52.25 67.35 L69.46 63.5Z"/></clipPath></defs><path d="M76.18 69.55 L52.25 85.44 L19.82 66.72 L19.82 29.28 L52.25 10.56 L76.18 26.45 L69.46 32.5 L52.25 28.65 L37.54 39.51 L37.54 56.49 L52.25 67.35 L69.46 63.5Z" fill="#17455A"/><path d="M76.34 69.5 L52.15 85.58 L52.11 67.24Z" fill="#3B8EA8"/><path d="M76.35 69.59 L52.07 67.36 L69.58 63.38Z" fill="#3B8EA8"/><path d="M52.37 85.56 L19.65 66.69 L37.56 56.32Z" fill="#3B8EA8"/><path d="M52.3 85.6 L37.44 56.35 L52.4 67.28Z" fill="#A2DEEE"/><path d="M19.78 66.89 L19.76 29.12 L37.7 39.44Z" fill="#EAF9FD"/><path d="M19.7 66.85 L37.61 39.35 L37.71 56.55Z" fill="#A2DEEE"/><path d="M19.66 29.33 L52.36 10.43 L52.4 28.73Z" fill="#EAF9FD"/><path d="M19.65 29.25 L52.41 28.6 L37.57 39.68Z" fill="#A2DEEE"/><path d="M52.12 10.44 L76.34 26.5 L69.52 32.66Z" fill="#EAF9FD"/><path d="M52.18 10.4 L69.6 32.61 L52.11 28.76Z" fill="#3B8EA8"/><path d="M19.82 29.28 L52.25 10.56 L76.18 26.45" fill="none" stroke="#EAF9FD" stroke-width="2.44" clip-path="url(#cryoMark)"/></svg>
    <h1 id="title">Something went wrong</h1>
    <p id="message"></p>
    <p class="target" id="target" hidden></p>
    <button id="retry" hidden>Try again</button>
  </main>
<script>
  var KINDS = {
    "offline": ["You are offline",
      "cryo cannot reach the network at all. Check your Wi-Fi or cable, then try again."],
    "host-not-found": ["This site cannot be found",
      "Nothing answers to that address. Check the domain for a typo."],
    "timed-out": ["The site took too long",
      "The server accepted the connection but never finished answering."],
    "connection-failed": ["The connection failed",
      "The server refused the connection, or dropped it before the page arrived."],
    "certificate-rejected": ["This connection is not private",
      "The site security certificate does not check out, so cryo stopped before sending anything."],
    "redirect-loop": ["Too many redirects",
      "The site kept forwarding cryo somewhere else and never settled on a page."],
    "malformed-response": ["cryo could not read the answer",
      "The server replied with something that is not a web page."],
    "render-process-gone": ["This tab crashed",
      "The page renderer stopped and cryo shut it down. Reloading usually brings it back."],
    "unknown": ["Something went wrong",
      "The page did not load, and the engine did not say why."]
  };
  var HTTP = {
    "400": "The server rejected the request as malformed.",
    "401": "This page needs credentials cryo does not have.",
    "403": "The server understood the request and refused it.",
    "404": "The server has no page at this address.",
    "405": "The server does not allow that method on this address.",
    "408": "The server gave up waiting for the request.",
    "410": "This page was removed on purpose and is not coming back.",
    "418": "The server is a teapot and declines to brew coffee.",
    "429": "Too many requests, too fast. Wait a moment, then retry.",
    "500": "The server hit an error of its own while building the page.",
    "501": "The server does not support what was asked of it.",
    "502": "A server upstream returned something the gateway could not use.",
    "503": "The server is overloaded or down for maintenance.",
    "504": "A server upstream did not answer in time."
  };

  var q = new URLSearchParams(location.search);
  var kind = q.get("kind") || "unknown";
  var code = q.get("code") || "";

  // Only ever hand the retry button a web URL. This page sits on cryo's own
  // origin, so a page that navigated here with `url=javascript:...` would
  // otherwise get script execution on it.
  var target = "";
  try {
    var parsed = new URL(q.get("url") || "");
    if (parsed.protocol === "http:" || parsed.protocol === "https:") target = parsed.href;
  } catch (e) { /* unparseable: show no address and offer no retry */ }

  var copy = KINDS[kind] || KINDS["unknown"];
  var title = copy[0];
  var message = copy[1];
  if (kind === "http") {
    title = "HTTP " + code;
    message = HTTP[code] || "The server answered with an error and no page.";
  }

  document.title = title;
  document.getElementById("title").textContent = title;
  document.getElementById("message").textContent = message;
  if (target) {
    var targetEl = document.getElementById("target");
    targetEl.textContent = target;
    targetEl.hidden = false;
    var retry = document.getElementById("retry");
    retry.hidden = false;
    retry.addEventListener("click", function () { location.replace(target); });
  }
</script>
</body>
</html>
"##;

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

    /// The template is the only place the copy lives, so a slug missing from
    /// its table renders the generic page by accident.
    #[test]
    fn template_has_copy_for_every_slug() {
        for failure in ALL {
            if matches!(failure, LoadFailure::Http(_)) {
                continue; // keyed by status code, not by slug
            }
            let key = format!("\"{}\":", slug(failure));
            assert!(ERROR_HTML.contains(&key), "template has no copy for {key}");
        }
        assert!(
            ERROR_HTML.contains("kind === \"http\""),
            "template never branches on http status codes"
        );
    }
}
