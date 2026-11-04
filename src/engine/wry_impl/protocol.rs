//! The `cryo` internal scheme: `/cmd` carries chrome IPC into the app,
//! every other path resolves through `PageResolver` to a shell document.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use wry::http::{header::CONTENT_TYPE, Request, Response};

use crate::engine::{IpcHandler, PageResolver, ViewAccess};

pub(super) fn internal_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.origin().ascii_serialization() == "http://cryo.localhost")
}

pub(super) fn allowed(access: ViewAccess, internal: bool, path: &str) -> bool {
    // Furniture every shell document wears: the palette, the script that
    // applies the stored theme, and that theme's favicon and mark. A
    // surface that may load a shell page may load its theme — a document
    // served without one renders unstyled.
    let theme = path.starts_with("/theme/");
    match access {
        ViewAccess::Chrome => {
            path == "/chrome"
                || path.starts_with("/chrome/")
                || theme
                || matches!(path, "/cmd" | "/dials" | "/engines")
        }
        // A shell page is a document plus the folder next to it, so the
        // stylesheet and script it links have to pass as well. The
        // resolver still answers nothing outside the paths it knows.
        ViewAccess::Content if internal => {
            path.starts_with("/newtab/")
                || path.starts_with("/settings/")
                || path.starts_with("/mail/")
                || path.starts_with("/error/")
                || theme
                || matches!(
                    path,
                    "/newtab"
                        | "/settings"
                        | "/mail"
                        | "/error"
                        | "/dials"
                        | "/engines"
                        | "/themes"
                )
        }
        _ => false,
    }
}

/// Scheme name registered on every view. On Windows wry rewrites it to
/// `http://cryo.localhost/...`, which is also the form callers navigate
/// to directly (`load_url` does not apply the rewrite).
pub(super) const INTERNAL_SCHEME: &str = "cryo";

/// Reserved path on the internal scheme: view → app command channel.
/// wry's own `with_ipc_handler` is not used — attaching it leaves the
/// document's scripts unexecuted on WebView2.
const CMD_PATH: &str = "/cmd";

/// Answer one `cryo:` request. Internal pages must arrive over a real
/// `http` URL: pages loaded via NavigateToString land on a `data:` URI
/// where WebView2 never runs the document's own scripts, and wry's IPC
/// bridge drops messages whose Source URI fails `http::Uri` parsing —
/// `data:` does. Both directions of chrome IPC depend on this scheme.
pub(super) fn respond(
    request: &Request<Vec<u8>>,
    on_ipc: &Option<IpcHandler>,
    resolve_page: PageResolver,
    access: ViewAccess,
    internal: &AtomicBool,
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    if !allowed(access, internal.load(Ordering::Acquire), path) {
        return Response::builder()
            .status(403)
            .body(Cow::Borrowed(&b"forbidden"[..]))
            .expect("static response is valid");
    }
    if path == CMD_PATH {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|h| h.to_str().ok());
        if request.method() != "POST" || origin != Some("http://cryo.localhost") {
            return Response::builder()
                .status(403)
                .body(Cow::Borrowed(&b"forbidden"[..]))
                .expect("static response is valid");
        }
        if let Some(cb) = on_ipc {
            cb(String::from_utf8_lossy(request.body()).into_owned());
        }
        return Response::builder()
            .status(200)
            .body(Cow::Borrowed(&b""[..]))
            .expect("static response is valid");
    }
    match resolve_page(path) {
        Some(html) => Response::builder()
            .header("X-Frame-Options", "DENY")
            .header("Content-Security-Policy", "frame-ancestors 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; script-src 'self' 'unsafe-inline'; connect-src 'self'")
            .header("X-Content-Type-Options", "nosniff")
            .header(CONTENT_TYPE, crate::engine::content_type(path))
            .body(Cow::Borrowed(html.as_bytes()))
            .expect("static headers are valid"),
        None => Response::builder()
            .status(404)
            .body(Cow::Borrowed(&b"not found"[..]))
            .expect("static response is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    fn page(_: &str) -> Option<&'static str> {
        Some("internal document")
    }

    #[test]
    fn untrusted_surfaces_cannot_fetch_internal_pages_or_send_commands() {
        for access in [ViewAccess::Web, ViewAccess::Content] {
            for path in ["/chrome", "/chrome/chrome.js", "/cmd"] {
                let request = Request::builder()
                    .uri(format!("http://cryo.localhost{path}"))
                    .method("POST")
                    .header("origin", "http://cryo.localhost")
                    .body(vec![])
                    .unwrap();
                assert_eq!(
                    respond(&request, &None, page, access, &AtomicBool::new(true)).status(),
                    403
                );
            }
        }
        let request = Request::builder()
            .uri("http://cryo.localhost/settings")
            .body(vec![])
            .unwrap();
        assert_eq!(
            respond(
                &request,
                &None,
                page,
                ViewAccess::Content,
                &AtomicBool::new(false)
            )
            .status(),
            403
        );
        let response = respond(
            &request,
            &None,
            page,
            ViewAccess::Content,
            &AtomicBool::new(true),
        );
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-frame-options"], "DENY");
    }

    #[test]
    fn chrome_commands_require_post_and_exact_origin_even_with_callback() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let callback: Option<IpcHandler> = Some(Box::new(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
        }));
        for (method, origin, status) in [
            ("GET", "http://cryo.localhost", 403),
            ("POST", "https://evil.test", 403),
            ("POST", "http://cryo.localhost.evil.test", 403),
            ("POST", "http://cryo.localhost", 200),
        ] {
            let request = Request::builder()
                .uri("http://cryo.localhost/cmd")
                .method(method)
                .header("origin", origin)
                .body(vec![])
                .unwrap();
            assert_eq!(
                respond(
                    &request,
                    &callback,
                    page,
                    ViewAccess::Chrome,
                    &AtomicBool::new(true)
                )
                .status(),
                status
            );
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// Each shell surface is a document plus the files it links. Leave
    /// one of those out of the gate and the page renders unstyled and
    /// dead, which no test of the resolver would catch — so the list
    /// comes off the documents themselves rather than being kept here
    /// by hand. (Shipping code in `engine` still names no shell page;
    /// this reads them the way a view would.)
    #[test]
    fn a_shell_page_may_fetch_the_files_its_document_links() {
        let links = |page: &str| {
            let html = crate::shell::pages::internal_page(page).expect("a shell document");
            html.match_indices("=\"/")
                .map(|(at, _)| {
                    let rest = &html[at + 2..];
                    rest[..rest.find('"').expect("an attribute is quoted")].to_string()
                })
                .collect::<Vec<_>>()
        };
        for page in [
            "/newtab",
            "/settings",
            "/mail",
            crate::shell::error::ERROR_PATH,
        ] {
            for asset in links(page) {
                assert!(
                    allowed(ViewAccess::Content, true, &asset),
                    "{page} links {asset}, which the gate denies"
                );
            }
        }
        for asset in links("/chrome") {
            assert!(
                allowed(ViewAccess::Chrome, true, &asset),
                "the chrome document links {asset}, which the gate denies"
            );
        }
        // What the documents fetch rather than link.
        for path in ["/dials", "/engines", "/themes"] {
            assert!(allowed(ViewAccess::Content, true, path), "{path} is denied");
        }
        for path in ["/dials", "/engines"] {
            assert!(allowed(ViewAccess::Chrome, true, path), "{path} is denied");
        }
        // A website, or a view that has navigated off the internal
        // origin, still gets none of it.
        for path in ["/newtab/newtab.js", "/theme/tokens.css", "/themes"] {
            assert!(!allowed(ViewAccess::Content, false, path));
            assert!(!allowed(ViewAccess::Web, true, path));
        }
    }

    #[test]
    fn internal_origin_is_not_a_string_prefix() {
        assert!(internal_url("http://cryo.localhost/settings"));
        for url in [
            "http://cryo.localhost.evil.test/",
            "http://cryo.localhost@evil.test/",
            "https://cryo.localhost/",
            "http://cryo.localhost:81/",
        ] {
            assert!(!internal_url(url));
        }
    }
}
