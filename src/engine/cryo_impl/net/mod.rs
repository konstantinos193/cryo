//! Fetching: a URL goes in, a resource comes out.
//!
//! [`Fetch`] is a **port**, in the sense AGENTS §4 means. The engine
//! declares what it needs — give me these bytes and tell me what they
//! are — and an adapter satisfies it. [`http`] is the real one;
//! [`memory`] is an in-memory map that lets every stage above here be
//! tested without a network, a server, or a clock.
//!
//! Two things are deliberately **not** written here, and the reason is
//! the same for both: being almost right is a vulnerability rather than
//! a bug.
//!
//! - **TLS.** A hand-rolled implementation is how timing attacks and
//!   certificate-validation holes happen. The adapter uses the same
//!   vetted stack the rest of cryo does.
//! - **URL parsing.** Two parsers that disagree about what the origin
//!   of a URL is produce origin-confusion bugs, which is most of what
//!   the WHATWG URL spec exists to prevent. Servo takes the same view
//!   and uses the same crate.
//!
//! Everything above that line is ours: what a content type means, which
//! encoding a document is in, what counts as a redirect loop, and what
//! the engine does with a response it did not expect.

pub mod http;
pub mod memory;
pub mod mime;
pub mod url;

pub use http::HttpFetch;
pub use memory::MemoryFetch;
pub use mime::{MimeType, ResourceKind};

use super::encoding::{detect, Encoding, EncodingSource};

/// How many redirects to follow before giving up.
///
/// Ten, as every browser uses. A loop has to terminate somehow, and a
/// chain longer than this is a misconfiguration rather than a journey.
pub const MAX_REDIRECTS: u8 = 10;

/// What came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The URL the bytes actually came from — after redirects, not the
    /// one that was asked for. Relative URLs in the document resolve
    /// against *this*, which is why it is carried rather than assumed.
    pub url: String,
    pub status: u16,
    pub mime: MimeType,
    pub body: Vec<u8>,
}

impl Resource {
    /// Decode the body as text, deciding the encoding from the charset
    /// this response declared and from the bytes themselves.
    pub fn text(&self) -> DecodedText {
        let (text, encoding, source) = detect(&self.body, self.mime.charset());
        DecodedText {
            text,
            encoding,
            source,
        }
    }

    pub fn kind(&self) -> ResourceKind {
        self.mime.resource_kind()
    }

    /// Whether the status says the request succeeded.
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

/// A decoded body, with how its encoding was decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    pub encoding: Encoding,
    pub source: EncodingSource,
}

/// What can go wrong fetching.
///
/// Named for what the engine must *do* about each, not for where in the
/// stack it arose: the shell shows a different page for "no network"
/// than for "the certificate is wrong", and the same page for every
/// flavour of connection failure underneath.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum FetchError {
    #[error("the address could not be understood: {0}")]
    BadUrl(String),
    #[error("the scheme {0} is not supported")]
    UnsupportedScheme(String),
    #[error("there is no route to the network")]
    Offline,
    #[error("no address was found for {0}")]
    HostNotFound(String),
    #[error("the server took too long to answer")]
    TimedOut,
    #[error("the connection failed: {0}")]
    ConnectionFailed(String),
    #[error("the certificate could not be trusted: {0}")]
    CertificateRejected(String),
    #[error("the redirect chain never ended")]
    RedirectLoop,
    #[error("the server answered with {0}")]
    Status(u16),
    #[error("the response could not be read: {0}")]
    Malformed(String),
}

/// Somewhere bytes can be fetched from.
pub trait Fetch {
    /// Retrieve `url`, following redirects.
    fn fetch(&self, url: &str) -> Result<Resource, FetchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(body: &[u8], content_type: &str) -> Resource {
        Resource {
            url: "https://example.test/".to_owned(),
            status: 200,
            mime: MimeType::parse(content_type).expect("a valid type"),
            body: body.to_vec(),
        }
    }

    #[test]
    fn a_response_decodes_using_its_declared_charset() {
        // 0xe9 is `é` in windows-1252 and invalid UTF-8.
        let fetched = resource(&[b'c', b'a', b'f', 0xe9], "text/html; charset=windows-1252");
        let decoded = fetched.text();
        assert_eq!(decoded.text, "café");
        assert_eq!(decoded.encoding, Encoding::Windows1252);
        assert_eq!(decoded.source, EncodingSource::Transport);
    }

    #[test]
    fn a_response_with_no_charset_falls_back_to_the_document() {
        let html = "<meta charset=\"utf-8\"><p>café".as_bytes();
        let decoded = resource(html, "text/html").text();
        assert_eq!(decoded.encoding, Encoding::Utf8);
        assert_eq!(decoded.source, EncodingSource::Document);
        assert!(decoded.text.ends_with("café"));
    }

    #[test]
    fn a_byte_order_mark_overrides_the_declared_charset() {
        let mut body = vec![0xef, 0xbb, 0xbf];
        body.extend_from_slice("café".as_bytes());
        let decoded = resource(&body, "text/html; charset=windows-1252").text();
        assert_eq!(decoded.text, "café");
        assert_eq!(decoded.source, EncodingSource::ByteOrderMark);
    }

    #[test]
    fn a_resource_reports_what_it_is() {
        assert_eq!(resource(b"", "text/html").kind(), ResourceKind::Html);
        assert_eq!(resource(b"", "text/css").kind(), ResourceKind::Css);
        assert_eq!(
            resource(b"", "application/zip").kind(),
            ResourceKind::Download
        );
    }

    #[test]
    fn success_is_the_two_hundreds_and_nothing_else() {
        let mut fetched = resource(b"", "text/html");
        for status in [200, 201, 204, 299] {
            fetched.status = status;
            assert!(fetched.is_success(), "{status}");
        }
        for status in [100, 301, 304, 404, 500] {
            fetched.status = status;
            assert!(!fetched.is_success(), "{status}");
        }
    }

    #[test]
    fn errors_say_what_to_do_about_them() {
        // Each variant has to be distinguishable, because the shell
        // shows a different page for several of them.
        let messages = [
            FetchError::Offline.to_string(),
            FetchError::HostNotFound("example.test".into()).to_string(),
            FetchError::TimedOut.to_string(),
            FetchError::CertificateRejected("expired".into()).to_string(),
            FetchError::RedirectLoop.to_string(),
            FetchError::Status(404).to_string(),
        ];
        let mut unique = messages.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), messages.len(), "two errors read the same");
        assert!(messages[1].contains("example.test"), "the host is named");
        assert!(messages[5].contains("404"), "the status is named");
    }

    #[test]
    fn the_redirect_limit_is_the_usual_ten() {
        assert_eq!(MAX_REDIRECTS, 10);
    }
}
