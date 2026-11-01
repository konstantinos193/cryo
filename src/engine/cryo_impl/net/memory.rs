//! An in-memory [`Fetch`], for tests and for the engine's own pages.
//!
//! The reason the port exists. Every stage above networking — loading a
//! document, following its stylesheets, resolving its images — can be
//! driven from a map of canned responses, with no server, no socket and
//! no timing. A test can state "this URL returns that HTML and this one
//! 404s" and then assert on what the engine did about it.
//!
//! It also serves the engine's own pages later: `cryo://newtab` is a
//! resource like any other, and routing it through the same port means
//! nothing above has a special case for internal documents.

use std::collections::BTreeMap;

use super::{Fetch, FetchError, MimeType, Resource};

/// A canned response.
#[derive(Clone, Debug)]
struct Canned {
    status: u16,
    mime: MimeType,
    body: Vec<u8>,
    /// Where this URL redirects to, if it does.
    redirect: Option<String>,
}

/// A map of URLs to responses.
#[derive(Clone, Debug, Default)]
pub struct MemoryFetch {
    entries: BTreeMap<String, Canned>,
}

impl MemoryFetch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve `body` as HTML at `url`.
    pub fn html(mut self, url: &str, body: &str) -> Self {
        self.insert(url, 200, super::mime::html(), body.as_bytes().to_vec());
        self
    }

    /// Serve `body` at `url` with a given content type.
    pub fn serve(mut self, url: &str, content_type: &str, body: &[u8]) -> Self {
        let mime = MimeType::parse(content_type).unwrap_or_else(super::mime::html);
        self.insert(url, 200, mime, body.to_vec());
        self
    }

    /// Answer `url` with a status and no useful body.
    pub fn status(mut self, url: &str, status: u16) -> Self {
        self.insert(url, status, super::mime::html(), Vec::new());
        self
    }

    /// Redirect `url` to `target`.
    pub fn redirect(mut self, url: &str, target: &str) -> Self {
        self.entries.insert(
            url.to_owned(),
            Canned {
                status: 302,
                mime: super::mime::html(),
                body: Vec::new(),
                redirect: Some(target.to_owned()),
            },
        );
        self
    }

    fn insert(&mut self, url: &str, status: u16, mime: MimeType, body: Vec<u8>) {
        self.entries.insert(
            url.to_owned(),
            Canned {
                status,
                mime,
                body,
                redirect: None,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Fetch for MemoryFetch {
    fn fetch(&self, url: &str) -> Result<Resource, FetchError> {
        let mut current = url.to_owned();
        // Follow redirects here too, so the fake behaves like the real
        // adapter: a test for redirect handling must exercise the same
        // bound the network path does.
        for _ in 0..=super::MAX_REDIRECTS {
            let Some(entry) = self.entries.get(&current) else {
                return Err(FetchError::HostNotFound(current));
            };
            if let Some(target) = &entry.redirect {
                current = target.clone();
                continue;
            }
            return Ok(Resource {
                url: current,
                status: entry.status,
                mime: entry.mime.clone(),
                body: entry.body.clone(),
            });
        }
        Err(FetchError::RedirectLoop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::cryo_impl::net::ResourceKind;

    #[test]
    fn a_served_page_comes_back() {
        let net = MemoryFetch::new().html("https://a.test/", "<p>hi</p>");
        let fetched = net.fetch("https://a.test/").expect("it is served");
        assert_eq!(fetched.status, 200);
        assert!(fetched.is_success());
        assert_eq!(fetched.text().text, "<p>hi</p>");
        assert_eq!(fetched.kind(), ResourceKind::Html);
        assert_eq!(fetched.url, "https://a.test/");
    }

    #[test]
    fn an_unknown_url_is_not_found() {
        let net = MemoryFetch::new();
        assert!(net.is_empty());
        assert!(matches!(
            net.fetch("https://nowhere.test/"),
            Err(FetchError::HostNotFound(_))
        ));
    }

    #[test]
    fn a_content_type_is_carried_through() {
        let net = MemoryFetch::new().serve("https://a.test/s.css", "text/css", b"p{color:red}");
        let fetched = net.fetch("https://a.test/s.css").expect("served");
        assert_eq!(fetched.kind(), ResourceKind::Css);
        assert_eq!(fetched.text().text, "p{color:red}");
    }

    #[test]
    fn an_error_status_is_reported_rather_than_hidden() {
        let net = MemoryFetch::new().status("https://a.test/missing", 404);
        let fetched = net.fetch("https://a.test/missing").expect("it answered");
        assert_eq!(fetched.status, 404);
        assert!(!fetched.is_success());
    }

    #[test]
    fn a_redirect_is_followed_and_reports_where_it_landed() {
        // The landing URL matters: relative links resolve against it.
        let net = MemoryFetch::new()
            .redirect("https://a.test/old", "https://a.test/new")
            .html("https://a.test/new", "<p>moved</p>");
        let fetched = net.fetch("https://a.test/old").expect("followed");
        assert_eq!(fetched.url, "https://a.test/new");
        assert_eq!(fetched.text().text, "<p>moved</p>");
    }

    #[test]
    fn a_chain_of_redirects_is_followed() {
        let net = MemoryFetch::new()
            .redirect("https://a.test/1", "https://a.test/2")
            .redirect("https://a.test/2", "https://a.test/3")
            .html("https://a.test/3", "<p>end</p>");
        assert_eq!(
            net.fetch("https://a.test/1").expect("followed").url,
            "https://a.test/3"
        );
    }

    #[test]
    fn a_redirect_loop_terminates() {
        let net = MemoryFetch::new()
            .redirect("https://a.test/a", "https://a.test/b")
            .redirect("https://a.test/b", "https://a.test/a");
        assert!(matches!(
            net.fetch("https://a.test/a"),
            Err(FetchError::RedirectLoop)
        ));
    }

    #[test]
    fn a_redirect_to_nowhere_is_not_found() {
        let net = MemoryFetch::new().redirect("https://a.test/x", "https://a.test/gone");
        assert!(matches!(
            net.fetch("https://a.test/x"),
            Err(FetchError::HostNotFound(_))
        ));
    }

    #[test]
    fn several_pages_can_be_served_at_once() {
        let net = MemoryFetch::new()
            .html("https://a.test/", "<p>one</p>")
            .html("https://a.test/two", "<p>two</p>")
            .serve("https://a.test/s.css", "text/css", b"");
        assert_eq!(net.len(), 3);
        assert_eq!(
            net.fetch("https://a.test/two").expect("served").text().text,
            "<p>two</p>"
        );
    }

    #[test]
    fn bytes_are_served_as_given_not_as_text() {
        // An image is not text, and must survive the round trip whole.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let net = MemoryFetch::new().serve("https://a.test/i.png", "image/png", &png);
        let fetched = net.fetch("https://a.test/i.png").expect("served");
        assert_eq!(fetched.body, png);
        assert_eq!(fetched.kind(), ResourceKind::Image);
    }
}
