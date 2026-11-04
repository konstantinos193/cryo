//! The site-info popup behind the lock in the address bar: what the
//! active page is, whether its connection is secure, how many cookies
//! it gets, and which permission decisions are remembered for it. The
//! document asks with `site_info` and paints the `__cryo_site` reply;
//! a reset repaints through the same push.

use crate::shell::chrome::ChromeSiteInfo;
use crate::shell::pages;

use super::App;

impl App {
    /// Describe the active page to the popup. A tab with no live surface
    /// (frozen, discarded) still answers — the cookie count is the one
    /// thing that needs the surface, and it reads as zero then.
    pub(super) fn push_site_info(&self) -> anyhow::Result<()> {
        let Some(tab) = self.registry.active() else {
            return Ok(());
        };
        let mut info = describe(tab.url(), |url| {
            self.active_view()
                .and_then(|view| view.cookies(url).ok())
                .map_or(0, |jar| jar.len())
        });
        info.grants = self.permission_grants.for_origin(&info.origin);
        let json = serde_json::to_string(&info)?;
        self.chrome.eval(&format!("window.__cryo_site({json})"))?;
        Ok(())
    }
}

/// The popup's view of `url`. `cookies` is asked only for a web page —
/// a shell document has no jar worth counting. Grants are filled in by
/// the caller's registry; this builds the rest.
fn describe(url: &str, cookies: impl FnOnce(&str) -> usize) -> ChromeSiteInfo {
    let parsed = url::Url::parse(url).ok();
    let origin = parsed
        .as_ref()
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default();
    let internal = pages::is_internal_url(url);
    let security = match parsed.as_ref().map(|u| u.scheme()) {
        _ if internal => "internal",
        Some("https") => "secure",
        _ => "insecure",
    };
    let host = if internal {
        "Cryo".to_string()
    } else {
        parsed
            .as_ref()
            .and_then(|u| u.host_str())
            .map(str::to_string)
            .unwrap_or_else(|| url.to_string())
    };
    ChromeSiteInfo {
        cookies: if internal { 0 } else { cookies(url) },
        grants: Vec::new(),
        origin,
        host,
        security,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_web_page_is_named_by_host_and_scheme() {
        let info = describe("https://meet.google.com/abc-defg", |_| 3);
        assert_eq!(info.origin, "https://meet.google.com");
        assert_eq!(info.host, "meet.google.com");
        assert_eq!(info.security, "secure");
        assert_eq!(info.cookies, 3);
        let info = describe("http://example.test/", |_| 1);
        assert_eq!(info.security, "insecure");
    }

    #[test]
    fn a_shell_page_is_internal_and_counts_no_cookies() {
        let info = describe(pages::SETTINGS_URL, |_| panic!("no jar for a shell page"));
        assert_eq!(info.security, "internal");
        assert_eq!(info.host, "Cryo");
        assert_eq!(info.cookies, 0);
    }
}
