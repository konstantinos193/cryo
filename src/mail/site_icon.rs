//! An organization's icon, from its own website.
//!
//! The website is named by a mail header, so anyone can pick it, and the
//! markup it answers with can name any further URL. Everything here is
//! fetched through [`Fetch`], which refuses what a stranger could use
//! against the reader: only `https` on the default port, only hosts that
//! resolve to public addresses, and each redirect hop judged the same
//! way, so a message can't aim a request at the reader's router.
//!
//! Which site gets asked is decided by [`registrable`], never by the full
//! address: a per-recipient `u123.tracker.test` collapses to one lookup
//! of `tracker.test`, so an icon can't double as a read receipt.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use url::Url;

/// A homepage's `<head>` is near the top. Reading stops here rather than
/// refusing the page, unlike an image, where a cut file is no image.
const MAX_PAGE_BYTES: u64 = 512 * 1024;

const MAX_REDIRECTS: usize = 3;

/// How many declared icons get tried before falling back.
const MAX_CANDIDATES: usize = 3;

/// The largest declared icon worth preferring — the disc is 64px at most,
/// and anything past this is usually over the image cap anyway.
const LARGEST_USEFUL: u32 = 256;

/// Second-level labels that are public suffixes under a country code, so
/// `acme.co.uk` is a registrable name and `co.uk` is not.
const SECOND_LEVEL: &[&str] = &["ac", "co", "com", "edu", "gov", "net", "org"];

/// The name an organization registered, from an email domain:
/// `mail.acme.gr` → `acme.gr`, `x.acme.co.uk` → `acme.co.uk`. A heuristic,
/// not the public-suffix list; a wrong guess costs one icon, and every
/// guess is still one lookup per registered name.
pub(super) fn registrable(domain: &str) -> &str {
    let labels: Vec<&str> = domain.split('.').collect();
    let keep = match labels.as_slice() {
        [.., second, tld] if tld.len() == 2 && SECOND_LEVEL.contains(second) => 3,
        _ => 2,
    };
    if labels.len() <= keep {
        return domain;
    }
    let skip: usize = labels[..labels.len() - keep]
        .iter()
        .map(|l| l.len() + 1)
        .sum();
    &domain[skip..]
}

/// A bounded round of requests: one deadline across all of them, so a
/// slow site costs the worker a known amount however many hops it asks
/// for.
pub(super) struct Fetch {
    deadline: Instant,
}

impl Fetch {
    pub(super) fn within(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
        }
    }

    /// The final response of `url`, its redirects walked by hand so each
    /// target is judged before anything is sent to it.
    fn walk(&self, mut url: Url) -> Option<(ureq::http::Response<ureq::Body>, Url)> {
        for _ in 0..=MAX_REDIRECTS {
            if !is_safe_target(&url) {
                return None;
            }
            let left = self.deadline.checked_duration_since(Instant::now())?;
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(left))
                .https_only(true)
                .max_redirects(0)
                .http_status_as_error(false)
                .user_agent(concat!("cryo/", env!("CARGO_PKG_VERSION")))
                .build()
                .new_agent();
            let response = agent.get(url.as_str()).call().ok()?;
            let status = response.status();
            if status.is_success() {
                return Some((response, url));
            }
            if !status.is_redirection() {
                return None;
            }
            let next = response.headers().get("location")?.to_str().ok()?;
            url = url.join(next).ok()?;
        }
        None
    }

    /// An image's bytes, whole or not at all.
    pub(super) fn image(&self, url: &str, limit: u64) -> Option<Vec<u8>> {
        self.walk(Url::parse(url).ok()?)?
            .0
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .ok()
    }

    /// The start of a page and where it really ended up — icons resolve
    /// against the final address, not the one asked for.
    fn page(&self, url: Url) -> Option<(String, Url)> {
        let (mut response, base) = self.walk(url)?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_PAGE_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        Some((String::from_utf8_lossy(&bytes).into_owned(), base))
    }
}

/// `https` on the default port, to a host that resolves only to public
/// addresses. A name resolving to a mix is refused: the connection could
/// pick either. DNS can still change between this check and the connect;
/// reaching anything that way takes a valid certificate for the name too.
fn is_safe_target(url: &Url) -> bool {
    if url.scheme() != "https" || url.port().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let Ok(addrs) = (host, 443).to_socket_addrs() else {
        return false;
    };
    let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
    !ips.is_empty() && ips.iter().all(is_public)
}

fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public(&IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// The URLs to try for `site`, best first: what its homepage declares,
/// then the conventional `/favicon.ico`.
pub(super) fn candidates(fetch: &Fetch, site: &str) -> Vec<String> {
    let Ok(home) = Url::parse(&format!("https://{site}/")) else {
        return Vec::new();
    };
    let mut out = match fetch.page(home.clone()) {
        Some((html, base)) => declared_icons(&html, &base),
        None => Vec::new(),
    };
    out.truncate(MAX_CANDIDATES);
    if let Ok(fallback) = home.join("/favicon.ico") {
        out.push(fallback.to_string());
    }
    out
}

/// The `<link rel="…icon…">` hrefs of a page, largest useful size first.
/// A bounded scan rather than a parser: nothing here is rendered, a bad
/// page only means no candidates, and every URL still has to pass
/// [`is_safe_target`] and be a real image.
fn declared_icons(html: &str, base: &Url) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found: Vec<(u32, String)> = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<link") {
        let start = at + start;
        let Some(len) = lower[start..].find('>') else {
            break;
        };
        let (tag, tag_lower) = (&html[start..start + len], &lower[start..start + len]);
        at = start + len;
        let Some(rel) = attribute(tag, tag_lower, "rel") else {
            continue;
        };
        let rel = rel.to_ascii_lowercase();
        if !rel.split_whitespace().any(is_icon_rel) {
            continue;
        }
        let Some(href) = attribute(tag, tag_lower, "href") else {
            continue;
        };
        if href.is_empty() {
            continue;
        }
        let Ok(url) = base.join(&href.replace("&amp;", "&")) else {
            continue;
        };
        let size = attribute(tag, tag_lower, "sizes")
            .as_deref()
            .and_then(icon_size)
            // Undeclared: a touch icon is ~180px by convention, a plain
            // favicon is small.
            .unwrap_or(if rel.contains("apple") { 180 } else { 32 });
        found.push((size, url.to_string()));
    }
    // Largest that isn't excessive, then the excessive ones smallest first.
    found.sort_by_key(|(size, _)| {
        if *size <= LARGEST_USEFUL {
            (0, LARGEST_USEFUL - size)
        } else {
            (1, *size)
        }
    });
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().map(|(_, url)| url).collect()
}

fn is_icon_rel(token: &str) -> bool {
    matches!(
        token,
        "icon" | "apple-touch-icon" | "apple-touch-icon-precomposed"
    )
}

/// `sizes="192x192"` → 192; the first size of a list; `any` is no number.
fn icon_size(sizes: &str) -> Option<u32> {
    let first = sizes.split_whitespace().next()?;
    first.split(['x', 'X']).next()?.parse().ok()
}

/// One attribute's value from a tag, quoted or bare. `lower` is `tag`
/// lower-cased, which keeps byte offsets because only ASCII changes.
fn attribute(tag: &str, lower: &str, name: &str) -> Option<String> {
    let mut from = 0;
    loop {
        let at = from + lower[from..].find(name)?;
        let end = at + name.len();
        let bounded = lower[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let rest = lower[end..].trim_start();
        if !bounded || !rest.starts_with('=') {
            from = end;
            continue;
        }
        let value_at = lower.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        return Some(match value.chars().next()? {
            q @ ('"' | '\'') => value[1..].split(q).next()?.to_string(),
            _ => value.split(char::is_whitespace).next()?.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_is_per_registered_name_never_per_address() {
        assert_eq!(registrable("hotelpoint.gr"), "hotelpoint.gr");
        assert_eq!(registrable("mail.hotelpoint.gr"), "hotelpoint.gr");
        // A per-recipient subdomain is the same lookup as any other.
        assert_eq!(registrable("u123.tracker.test"), "tracker.test");
        assert_eq!(registrable("u456.tracker.test"), "tracker.test");
        assert_eq!(registrable("acme.co.uk"), "acme.co.uk");
        assert_eq!(registrable("x.acme.co.uk"), "acme.co.uk");
        assert_eq!(registrable("a.b.acme.com.gr"), "acme.com.gr");
        assert_eq!(registrable("help.yahoo.com"), "yahoo.com");
    }

    #[test]
    fn nothing_on_the_readers_own_network_is_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!is_public(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn only_https_on_the_default_port_to_public_hosts_is_a_target() {
        for bad in [
            "http://1.1.1.1/x.png",
            "https://1.1.1.1:8443/x.png",
            "https://192.168.1.1/reboot",
            "https://127.0.0.1/x.png",
            "https://[::1]/x.png",
            "https://localhost/x.png",
            "file:///c:/windows/win.ini",
        ] {
            assert!(!is_safe_target(&Url::parse(bad).unwrap()), "{bad}");
        }
        assert!(is_safe_target(
            &Url::parse("https://1.1.1.1/x.png").unwrap()
        ));
    }

    #[test]
    fn declared_icons_come_largest_useful_first() {
        // The shape hotelpoint.gr's WordPress serves, plus the odd ones.
        let html = r#"<html><head>
            <LINK REL="stylesheet" href="/a.css">
            <link rel="icon" href="https://cdn.test/i-32.png" sizes="32x32" />
            <link rel="icon" href="/i-192.png?a=1&amp;b=2" sizes="192x192" />
            <link rel="apple-touch-icon" href="i-180.png" />
            <link rel='shortcut icon' href=/bare.ico>
            <link rel="mask-icon" href="/mask.svg">
            <link rel="icon" href="/huge.png" sizes="1024x1024">
        </head>"#;
        let base = Url::parse("https://a.test/dir/").unwrap();
        assert_eq!(
            declared_icons(html, &base),
            [
                "https://a.test/i-192.png?a=1&b=2",
                "https://a.test/dir/i-180.png",
                "https://cdn.test/i-32.png",
                "https://a.test/bare.ico",
                "https://a.test/huge.png",
            ]
        );
    }

    #[test]
    fn hostile_markup_yields_only_urls_the_fetch_will_judge() {
        let base = Url::parse("https://a.test/").unwrap();
        assert!(declared_icons("<link rel=\"icon\"", &base).is_empty());
        assert!(declared_icons("<link href=\"/x.png\">", &base).is_empty());
        assert!(declared_icons("<link rel=\"icon\" href=\"http://[\">", &base).is_empty());
        let aimed = declared_icons(
            "<link rel=\"icon\" href=\"https://192.168.1.1/reboot\">",
            &base,
        );
        assert!(!is_safe_target(&Url::parse(&aimed[0]).unwrap()));
    }

    /// The real path against a real site. Run on its own, needs network:
    /// `cargo test live_site_icon -- --ignored --nocapture`.
    #[test]
    #[ignore = "reaches a real website"]
    fn live_site_icon() {
        let fetch = Fetch::within(Duration::from_secs(20));
        let found = candidates(&fetch, "hotelpoint.gr");
        println!("{found:#?}");
        let bytes = found
            .iter()
            .find_map(|url| fetch.image(url, 128 * 1024))
            .expect("the site declares an icon");
        println!("{} bytes", bytes.len());
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn attributes_read_whole_names_only() {
        let tag = r#"<link data-href="/no.png" href="/yes.png""#;
        assert_eq!(
            attribute(tag, &tag.to_ascii_lowercase(), "href").unwrap(),
            "/yes.png"
        );
    }
}
