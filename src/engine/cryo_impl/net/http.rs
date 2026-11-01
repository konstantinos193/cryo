//! The real [`Fetch`]: HTTP and HTTPS over the network.
//!
//! The only module in the engine that touches a socket, and the only
//! one that names `ureq`. Everything above it sees [`Resource`] and
//! [`FetchError`] and cannot tell a live server from
//! [`MemoryFetch`](super::MemoryFetch).
//!
//! Its real work is **translation**. A transport reports failures in
//! its own vocabulary — I/O kinds, TLS alerts, status codes — and the
//! shell needs them in the engine's: "there is no network" and "that
//! certificate is wrong" are different pages, while every flavour of
//! refused connection underneath is one. Doing that mapping here means
//! no other module ever matches on a dependency's error type, which
//! AGENTS §7 requires and which is also what makes the adapter
//! replaceable.
//!
//! What is deliberately absent: no caching, no cookie jar, no
//! conditional requests, no connection reuse beyond what the transport
//! does itself. Each belongs in its own place once there is something
//! to cache against.

use std::io::Read;

use ureq::ResponseExt;

use super::{Fetch, FetchError, MimeType, Resource, MAX_REDIRECTS};

/// How long to wait before giving up on a server.
const TIMEOUT_SECONDS: u64 = 30;

/// How much of a response to read.
///
/// A bound rather than trust: a server can stream forever, and a
/// browser that believes it will exhaust memory. 32 MiB is far past any
/// real document and far short of a problem.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// The limit has to stay past any real document and short of a denial
/// of service. Checked at compile time, since a runtime assertion on a
/// constant proves nothing.
const _: () = assert!(MAX_BODY_BYTES > 8 * 1024 * 1024);
const _: () = assert!(MAX_BODY_BYTES < 256 * 1024 * 1024);

/// Fetches over the network.
#[derive(Debug, Default, Clone)]
pub struct HttpFetch {
    /// Sent as `User-Agent`. Servers vary their output by it, so it is
    /// configurable rather than baked in.
    user_agent: String,
}

impl HttpFetch {
    pub fn new() -> Self {
        Self {
            user_agent: default_user_agent(),
        }
    }

    pub fn with_user_agent(user_agent: impl Into<String>) -> Self {
        Self {
            user_agent: user_agent.into(),
        }
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

/// The engine's own identifier.
fn default_user_agent() -> String {
    format!(
        "Mozilla/5.0 (compatible; cryo/{})",
        env!("CARGO_PKG_VERSION")
    )
}

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str) -> Result<Resource, FetchError> {
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase())
            .ok_or_else(|| FetchError::BadUrl(url.to_owned()))?;
        if scheme != "http" && scheme != "https" {
            return Err(FetchError::UnsupportedScheme(scheme));
        }

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(TIMEOUT_SECONDS)))
            .max_redirects(MAX_REDIRECTS.into())
            .user_agent(&self.user_agent)
            .build()
            .new_agent();

        let response = agent.get(url).call().map_err(translate)?;
        let status = response.status().as_u16();
        // The type the server claimed. Absent means we do not guess —
        // see the note on sniffing in `mime`.
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        // Where the bytes actually came from, after any redirects. The
        // document's relative URLs resolve against this, not against
        // what was asked for.
        let landed = response.get_uri().to_string();

        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_BODY_BYTES as u64)
            .read_to_end(&mut body)
            .map_err(|error| FetchError::Malformed(error.to_string()))?;

        Ok(Resource {
            url: landed,
            status,
            mime: content_type
                .as_deref()
                .and_then(MimeType::parse)
                .unwrap_or_else(unknown_type),
            body,
        })
    }
}

/// What a response with no usable `Content-Type` is treated as.
///
/// `application/octet-stream`, which [`MimeType::resource_kind`] turns
/// into a download. A server that says nothing is not the same as one
/// saying HTML, and guessing is how a text file becomes a script.
fn unknown_type() -> MimeType {
    MimeType {
        kind: "application".to_owned(),
        subtype: "octet-stream".to_owned(),
        parameters: Vec::new(),
    }
}

/// Turn a transport failure into one the engine knows how to show.
///
/// The categories are the ones the shell paints differently. A DNS
/// miss, an expired certificate and a refused socket all look alike to
/// a stack trace and entirely different to a reader.
fn translate(error: ureq::Error) -> FetchError {
    match error {
        ureq::Error::StatusCode(status) => FetchError::Status(status),
        ureq::Error::Timeout(_) => FetchError::TimedOut,
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => FetchError::RedirectLoop,
        ureq::Error::HostNotFound => FetchError::HostNotFound(String::new()),
        ureq::Error::BadUri(uri) => FetchError::BadUrl(uri),
        ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => {
            FetchError::ConnectionFailed(String::from("the connection could not be established"))
        }
        // Every way the transport can report a certificate problem.
        // Distinct variants rather than sniffing the message text: the
        // reader must be told "this site's identity is wrong" apart
        // from "this site is unreachable", and a prose match would
        // break the first time a dependency rewords itself.
        ureq::Error::Tls(reason) => FetchError::CertificateRejected(reason.to_owned()),
        // Certificate validation failures arrive here: rustls reports
        // an expired or untrusted chain as its own error type, which
        // ureq wraps rather than flattening.
        ureq::Error::Rustls(inner) => FetchError::CertificateRejected(inner.to_string()),
        ureq::Error::Pem(inner) => FetchError::CertificateRejected(inner.to_string()),
        ureq::Error::TlsRequired => {
            FetchError::CertificateRejected(String::from("a secure connection was required"))
        }
        ureq::Error::Io(io) => translate_io(&io),
        other => FetchError::ConnectionFailed(other.to_string()),
    }
}

fn translate_io(error: &std::io::Error) -> FetchError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut => FetchError::TimedOut,
        ErrorKind::NotFound | ErrorKind::AddrNotAvailable => {
            FetchError::HostNotFound(error.to_string())
        }
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionAborted
        | ErrorKind::ConnectionReset
        | ErrorKind::BrokenPipe => FetchError::ConnectionFailed(error.to_string()),
        ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable => FetchError::Offline,
        _ => FetchError::ConnectionFailed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_user_agent_names_the_engine() {
        let agent = HttpFetch::new();
        assert!(agent.user_agent().contains("cryo"));
        // Servers branch on `Mozilla/5.0`; omitting it gets a different
        // page from a surprising number of them.
        assert!(agent.user_agent().starts_with("Mozilla/5.0"));
    }

    #[test]
    fn the_user_agent_can_be_set() {
        let agent = HttpFetch::with_user_agent("custom/1.0");
        assert_eq!(agent.user_agent(), "custom/1.0");
    }

    #[test]
    fn a_url_with_no_scheme_is_rejected_before_any_socket() {
        let agent = HttpFetch::new();
        assert!(matches!(
            agent.fetch("example.test/path"),
            Err(FetchError::BadUrl(_))
        ));
    }

    #[test]
    fn schemes_the_network_cannot_serve_are_refused() {
        let agent = HttpFetch::new();
        for url in ["file:///etc/passwd", "ftp://a.test/", "cryo://newtab"] {
            match agent.fetch(url) {
                Err(FetchError::UnsupportedScheme(_)) => {}
                // The error detail is not printed: a certificate
                // rejection carries TLS detail that must not reach
                // stderr.
                _ => panic!("{url} should be refused"),
            }
        }
    }

    #[test]
    fn the_scheme_check_ignores_case() {
        let agent = HttpFetch::new();
        // Not an unsupported-scheme error: it got past the check and
        // failed on the network instead, which is the point.
        assert!(!matches!(
            agent.fetch("HTTPS://nonexistent.invalid/"),
            Err(FetchError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn io_failures_map_to_the_categories_the_shell_shows() {
        use std::io::{Error, ErrorKind};
        let cases = [
            (ErrorKind::TimedOut, "TimedOut"),
            (ErrorKind::ConnectionRefused, "ConnectionFailed"),
            (ErrorKind::ConnectionReset, "ConnectionFailed"),
            (ErrorKind::NetworkUnreachable, "Offline"),
            (ErrorKind::HostUnreachable, "Offline"),
        ];
        for (kind, expected) in cases {
            let translated = translate_io(&Error::new(kind, "test"));
            let name = match translated {
                FetchError::TimedOut => "TimedOut",
                FetchError::Offline => "Offline",
                FetchError::ConnectionFailed(_) => "ConnectionFailed",
                FetchError::HostNotFound(_) => "HostNotFound",
                _ => "unexpected",
            };
            assert_eq!(name, expected, "for {kind:?}");
        }
    }

    #[test]
    fn a_certificate_failure_is_distinguished_from_a_refused_connection() {
        // The reader needs these apart: one is "this site is broken",
        // the other is "something is wrong with this site's identity".
        let tls = translate(ureq::Error::Tls("invalid peer certificate"));
        assert!(matches!(tls, FetchError::CertificateRejected(_)));

        let refused = translate(ureq::Error::ConnectionFailed);
        assert!(matches!(refused, FetchError::ConnectionFailed(_)));
    }

    #[test]
    fn transport_failures_map_to_the_categories_the_shell_shows() {
        let cases = [
            (ureq::Error::StatusCode(404), "Status"),
            (ureq::Error::TooManyRedirects, "RedirectLoop"),
            (ureq::Error::RedirectFailed, "RedirectLoop"),
            (ureq::Error::HostNotFound, "HostNotFound"),
            (ureq::Error::BadUri("::".into()), "BadUrl"),
            (ureq::Error::ConnectionFailed, "ConnectionFailed"),
            (ureq::Error::Tls("bad cert"), "CertificateRejected"),
            (ureq::Error::TlsRequired, "CertificateRejected"),
        ];
        for (error, expected) in cases {
            let name = match translate(error) {
                FetchError::Status(_) => "Status",
                FetchError::RedirectLoop => "RedirectLoop",
                FetchError::HostNotFound(_) => "HostNotFound",
                FetchError::BadUrl(_) => "BadUrl",
                FetchError::ConnectionFailed(_) => "ConnectionFailed",
                FetchError::CertificateRejected(_) => "CertificateRejected",
                FetchError::TimedOut => "TimedOut",
                // Naming the variant is fine; printing the error is
                // not, since `CertificateRejected` carries TLS detail.
                _ => "unexpected",
            };
            assert_eq!(name, expected);
        }
    }

    #[test]
    fn a_status_code_survives_translation() {
        match translate(ureq::Error::StatusCode(503)) {
            FetchError::Status(503) => {}
            _ => panic!("expected Status(503)"),
        }
    }

    #[test]
    fn a_response_with_no_content_type_is_a_download_not_html() {
        // The security decision, as a type rather than a comment.
        assert_eq!(
            unknown_type().resource_kind(),
            super::super::ResourceKind::Download
        );
    }

    /// Fetches a real page. Run with:
    ///
    /// ```text
    /// cargo test live_http_fetch -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access"]
    fn live_http_fetch() {
        let agent = HttpFetch::new();
        let fetched = agent.fetch("https://example.com/").expect("example.com");
        assert!(fetched.is_success(), "status {}", fetched.status);
        assert_eq!(fetched.kind(), super::super::ResourceKind::Html);
        let decoded = fetched.text();
        assert!(
            decoded.text.to_ascii_lowercase().contains("<html"),
            "got {} bytes",
            fetched.body.len()
        );
        println!(
            "fetched {} bytes of {} as {}",
            fetched.body.len(),
            fetched.mime.essence(),
            decoded.encoding.label()
        );
    }
}
