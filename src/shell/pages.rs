//! Internal pages served over the engine's `cryo` protocol.
//!
//! Shell surfaces are real `http://cryo.localhost/...` documents, not
//! `NavigateToString` HTML: a `data:`-document runs no scripts and its IPC
//! messages never reach the host (wry drops unparseable Source URIs).
//!
//! Each surface owns a folder — `chrome/`, `newtab/`, `settings/`,
//! `error/`, with the shared palette in `theme/` — holding its document,
//! stylesheet and script. This module maps a request path to one of them
//! and owns nothing else: no page's markup, copy or style lives here.

use crate::shell::{chrome, dial, error, mail, newtab, search, settings, theme};

/// Origin that all internal pages share. `is_internal_url` keys off this —
/// keep the two in sync if the scheme registration ever changes.
pub(crate) const INTERNAL_ORIGIN: &str = "http://cryo.localhost";

pub const CHROME_URL: &str = "http://cryo.localhost/chrome";
pub const NEW_TAB_URL: &str = "http://cryo.localhost/newtab";
pub const SETTINGS_URL: &str = "http://cryo.localhost/settings";
pub const MAIL_URL: &str = "http://cryo.localhost/mail";

/// A URL that belongs to the shell, not to any web destination.
pub fn is_internal_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.origin().ascii_serialization() == INTERNAL_ORIGIN)
}

/// The shell page a navigation landed on, when it is one a tab can
/// *be at*: the home page, settings, mail. Anything else internal —
/// an error card carrying the failed address in its query, the
/// settings page's request routes, the chrome — is `None`: a tab that
/// shows those keeps the address it had.
pub fn page_address(url: &str) -> Option<&'static str> {
    [NEW_TAB_URL, SETTINGS_URL, MAIL_URL]
        .into_iter()
        .find(|page| *page == url)
}

/// Engine-facing resolver for the `cryo` scheme. wry hands us the request
/// path (`/chrome`), we hand back the static file behind it. Every path a
/// shell document references has to appear here, or the page renders
/// unstyled or dead — `documents_reference_only_served_assets` holds that.
pub fn internal_page(path: &str) -> Option<&'static str> {
    // The palette every document links ahead of its own sheet, the head
    // script that applies the stored pick, and each theme's favicon and
    // mark — `theme` owns those paths, since it owns what is behind them.
    if let Some(asset) = theme::asset(path) {
        return Some(asset);
    }
    match path {
        "/chrome" => Some(chrome::HTML),
        "/chrome/chrome.css" => Some(chrome::STYLE),
        "/chrome/chrome.js" => Some(chrome::SCRIPT),
        "/newtab" => Some(newtab::HTML),
        "/newtab/newtab.css" => Some(newtab::STYLE),
        "/newtab/newtab.js" => Some(newtab::SCRIPT),
        "/settings" => Some(settings::HTML),
        "/settings/settings.css" => Some(settings::STYLE),
        "/settings/settings.js" => Some(settings::SCRIPT),
        "/mail" => Some(mail::HTML),
        "/mail/mail.css" => Some(mail::STYLE),
        "/mail/mail.js" => Some(mail::SCRIPT),
        // The error page reads its parameters from the query string, which
        // never reaches this resolver — one document covers every failure.
        error::ERROR_PATH => Some(error::HTML),
        "/error/error.css" => Some(error::STYLE),
        "/error/error.js" => Some(error::SCRIPT),
        // The dial list both shell surfaces fetch at load.
        dial::PATH => Some(dial::json()),
        // The engine list the settings page offers and the new-tab page
        // resolves its search box through.
        search::PATH => Some(search::json()),
        // The theme list the settings page offers.
        theme::PATH => Some(theme::json()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every document that a surface serves, for the whole-set checks below.
    const DOCUMENTS: [&str; 5] = [
        "/chrome",
        "/newtab",
        "/settings",
        "/mail",
        error::ERROR_PATH,
    ];

    #[test]
    fn chrome_document_assets_resolve_as_one_ordered_script() {
        let html = internal_page("/chrome").unwrap();
        assert!(html.contains("href=\"/chrome/chrome.css\""));
        assert!(html.contains("src=\"/chrome/chrome.js\""));
        // One execution unit for the strip's own features. The shared
        // theme script is the only other one, and it is in the head
        // because it has to run before the first frame.
        assert_eq!(html.matches("src=\"/chrome/chrome.js\"").count(), 1);
        assert_eq!(html.matches("<script").count(), 2);
        assert!(html.contains("src=\"/theme/theme.js\""));
        assert!(internal_page("/chrome/chrome.css").is_some());
        let script = internal_page("/chrome/chrome.js").unwrap();
        assert!(script.find("const ipc").unwrap() < script.find("window.__cryo_update =").unwrap());
        for name in [
            "paintBookmarks",
            "paintPasswords",
            "paintHistory",
            "paintImport",
            "paintExport",
        ] {
            assert!(script.contains(&format!("const {name} =")));
        }
    }

    /// A page's own stylesheet and script are separate requests, so a path
    /// the resolver does not answer is a 404 the reader sees as a broken
    /// page. Checking the documents' own links catches a renamed file.
    #[test]
    fn documents_reference_only_served_assets() {
        for path in DOCUMENTS {
            let html = internal_page(path).expect("built-in page exists");
            let mut assets = 0;
            for (at, _) in html.match_indices("=\"/") {
                let rest = &html[at + 2..];
                let asset = &rest[..rest.find('"').expect("attribute value is quoted")];
                assert!(
                    internal_page(asset).is_some(),
                    "{path} references {asset}, which nothing serves"
                );
                assets += 1;
            }
            assert!(assets >= 3, "{path} links no icon, sheet or script");
        }
    }

    /// Markup, style and script each have one home: a document that still
    /// carries a `<style>` block has grown a second place to look.
    #[test]
    fn no_document_inlines_its_own_style_or_script() {
        for path in DOCUMENTS {
            let html = internal_page(path).expect("built-in page exists");
            assert!(!html.contains("<style"), "{path} inlines a stylesheet");
            assert!(
                !html.contains("<script>"),
                "{path} inlines a script; it belongs in the page's .js"
            );
        }
    }

    #[test]
    fn every_document_links_the_shared_palette() {
        for path in DOCUMENTS {
            let html = internal_page(path).expect("built-in page exists");
            assert!(
                html.contains("href=\"/theme/tokens.css\""),
                "{path} does not link the shared palette"
            );
        }
    }

    /// Every document ships the default theme's icon and the script that
    /// swaps it for the stored pick's — a document that linked neither
    /// would wear the engine's blank page icon.
    #[test]
    fn built_in_pages_use_the_cryo_favicon_and_can_reach_every_theme_s() {
        for path in DOCUMENTS {
            let html = internal_page(path).expect("built-in page exists");
            assert!(html.contains(&format!(
                "<link rel=\"icon\" type=\"image/svg+xml\" href=\"/theme/icon-{}.svg\">",
                theme::Theme::DEFAULT.slug()
            )));
            assert!(html.contains("src=\"/theme/theme.js\""));
        }
        for shell_theme in theme::ALL {
            let slug = shell_theme.slug();
            assert_eq!(
                internal_page(&format!("/theme/icon-{slug}.svg")),
                Some(shell_theme.icon_svg())
            );
            assert_eq!(
                internal_page(&format!("/theme/mark-{slug}.svg")),
                Some(shell_theme.mark_svg())
            );
        }
        // A slug no theme answers to is a 404, not the default's art.
        assert!(internal_page("/theme/icon-solarized.svg").is_none());
        assert!(internal_page("/theme/icon.svg").is_none());
        assert!(internal_page("/theme/glacier.svg").is_none());
    }

    #[test]
    fn resolves_known_pages_only() {
        for path in DOCUMENTS {
            assert!(internal_page(path).is_some());
        }
        assert!(internal_page(dial::PATH).is_some());
        assert!(internal_page(search::PATH).is_some());
        assert!(internal_page(theme::PATH).is_some());
        assert!(internal_page("/etc/passwd").is_none());
        assert!(internal_page("/newtab/newtab.html").is_none());
    }

    #[test]
    fn only_the_three_pages_are_addresses_a_tab_can_hold() {
        assert_eq!(page_address(NEW_TAB_URL), Some(NEW_TAB_URL));
        assert_eq!(page_address(SETTINGS_URL), Some(SETTINGS_URL));
        assert_eq!(page_address(MAIL_URL), Some(MAIL_URL));
        for not_a_place in [
            "http://cryo.localhost/error?kind=dns&url=https%3A%2F%2Fx",
            "http://cryo.localhost/settings/theme/light",
            "http://cryo.localhost/mail?x=1",
            "http://cryo.localhost/mail/",
            CHROME_URL,
            "https://example.com",
        ] {
            assert_eq!(page_address(not_a_place), None, "{not_a_place}");
        }
    }

    #[test]
    fn internal_urls_match_protocol_origin() {
        assert!(is_internal_url(NEW_TAB_URL));
        assert!(is_internal_url(CHROME_URL));
        assert!(!is_internal_url("https://example.com"));
        assert!(!is_internal_url("http://cryo.evil.com"));
    }
}
