//! Resolving one URL against another.
//!
//! A document's links are mostly relative — `/style.css`, `../img/a.png`,
//! `?page=2` — and every one has to be made absolute before it can be
//! fetched. The base is the URL the bytes **actually came from**, after
//! redirects, not the one that was requested: a page moved from
//! `/old/` to `/new/` has its relative links resolve against `/new/`,
//! and getting that wrong fetches a working site's assets from the
//! wrong directory.
//!
//! A `<base href>` element overrides that, which is the one case where
//! the document gets to say where its own links point.
//!
//! The parsing itself is the `url` crate's. Two URL parsers that
//! disagree about what a URL means is a security bug, not a style
//! difference — it is how origin confusion happens — so this wraps a
//! spec implementation rather than adding a second opinion.

use url::Url;

/// Make `reference` absolute against `base`.
///
/// `None` when the result is not a usable URL: an unparseable base, or
/// a reference that resolves to nothing fetchable.
pub fn resolve(base: &str, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let base = Url::parse(base).ok()?;
    base.join(reference).ok().map(|resolved| {
        // The fragment never reaches the network; keeping it would
        // make two requests for the same resource look different.
        let mut resolved = resolved;
        resolved.set_fragment(None);
        resolved.to_string()
    })
}

/// Whether a URL is one this engine will fetch over the network.
///
/// `data:` and `javascript:` are deliberately excluded: the first needs
/// its own decoder and the second must never be fetched at all.
pub fn is_fetchable(url: &str) -> bool {
    Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

/// The origin of a URL — scheme, host and port — as a comparable string.
///
/// What "same site" means for every security decision above here.
pub fn origin(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    Some(parsed.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.test/dir/page.html";

    #[test]
    fn an_absolute_reference_is_returned_as_it_is() {
        assert_eq!(
            resolve(BASE, "https://other.test/x.css").as_deref(),
            Some("https://other.test/x.css")
        );
    }

    #[test]
    fn a_root_relative_reference_replaces_the_path() {
        assert_eq!(
            resolve(BASE, "/style.css").as_deref(),
            Some("https://example.test/style.css")
        );
    }

    #[test]
    fn a_relative_reference_resolves_against_the_directory() {
        // Not against the page itself: `page.html` is a file, and its
        // siblings live beside it.
        assert_eq!(
            resolve(BASE, "style.css").as_deref(),
            Some("https://example.test/dir/style.css")
        );
    }

    #[test]
    fn a_parent_reference_walks_up() {
        assert_eq!(
            resolve(BASE, "../assets/a.png").as_deref(),
            Some("https://example.test/assets/a.png")
        );
    }

    #[test]
    fn a_protocol_relative_reference_keeps_the_scheme() {
        assert_eq!(
            resolve(BASE, "//cdn.test/lib.css").as_deref(),
            Some("https://cdn.test/lib.css")
        );
    }

    #[test]
    fn a_query_only_reference_keeps_the_path() {
        assert_eq!(
            resolve(BASE, "?page=2").as_deref(),
            Some("https://example.test/dir/page.html?page=2")
        );
    }

    #[test]
    fn a_fragment_is_dropped_because_it_never_reaches_the_network() {
        assert_eq!(
            resolve(BASE, "/a.css#section").as_deref(),
            Some("https://example.test/a.css")
        );
    }

    #[test]
    fn whitespace_around_a_reference_is_ignored() {
        // Markup is written by hand, and attributes get padded.
        assert_eq!(
            resolve(BASE, "  /style.css  ").as_deref(),
            Some("https://example.test/style.css")
        );
    }

    #[test]
    fn an_empty_reference_resolves_to_nothing() {
        assert_eq!(resolve(BASE, ""), None);
        assert_eq!(resolve(BASE, "   "), None);
    }

    #[test]
    fn an_unparseable_base_resolves_to_nothing() {
        assert_eq!(resolve("not a url", "/a.css"), None);
        assert_eq!(resolve("", "/a.css"), None);
    }

    #[test]
    fn resolution_uses_the_base_it_is_given_not_the_one_requested() {
        // The redirect case: a page served from /new/ must pull its
        // assets from /new/, whatever URL was asked for.
        let landed = "https://example.test/new/index.html";
        assert_eq!(
            resolve(landed, "style.css").as_deref(),
            Some("https://example.test/new/style.css")
        );
    }

    #[test]
    fn only_http_and_https_are_fetchable() {
        assert!(is_fetchable("https://a.test/"));
        assert!(is_fetchable("http://a.test/"));
        for url in [
            "data:text/css,body{}",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "cryo://newtab",
            "not a url",
        ] {
            assert!(!is_fetchable(url), "{url} should not be fetched");
        }
    }

    #[test]
    fn an_origin_is_scheme_host_and_port() {
        assert_eq!(
            origin("https://example.test/a/b?c").as_deref(),
            Some("https://example.test")
        );
        assert_eq!(
            origin("http://example.test:8080/").as_deref(),
            Some("http://example.test:8080")
        );
    }

    #[test]
    fn different_schemes_are_different_origins() {
        assert_ne!(origin("https://a.test/"), origin("http://a.test/"));
        assert_ne!(origin("https://a.test/"), origin("https://b.test/"));
    }

    #[test]
    fn a_path_does_not_change_the_origin() {
        assert_eq!(origin("https://a.test/one"), origin("https://a.test/two"));
    }
}
