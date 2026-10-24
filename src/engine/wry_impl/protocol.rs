//! The `cryo` internal scheme: `/cmd` carries chrome IPC into the app,
//! every other path resolves through `PageResolver` to a shell document.

use std::borrow::Cow;

use wry::http::{header::CONTENT_TYPE, Request, Response};

use crate::engine::{IpcHandler, PageResolver};

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
) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    if path == CMD_PATH {
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
            .header(
                CONTENT_TYPE,
                if path.ends_with(".svg") {
                    "image/svg+xml"
                } else {
                    "text/html; charset=utf-8"
                },
            )
            .body(Cow::Borrowed(html.as_bytes()))
            .expect("static headers are valid"),
        None => Response::builder()
            .status(404)
            .body(Cow::Borrowed(&b"not found"[..]))
            .expect("static response is valid"),
    }
}
