//! Settings: theme, search preferences, download folder, and browser
//! reference.
//!
//! Document, stylesheet and script live in `settings/`, one file each.
//! Search and the theme use the shell origin's localStorage (see
//! `search.rs` and `theme.rs`). Folder and theme requests use
//! intercepted navigations, accepted only from a settings tab; content
//! views still have no general-purpose command channel.

use crate::shell::pages::SETTINGS_URL;
use crate::shell::theme::Theme;

/// The settings document, served at [`SETTINGS_URL`].
pub(crate) const HTML: &str = include_str!("settings/settings.html");

/// Its stylesheet, served at `/settings/settings.css`.
pub(crate) const STYLE: &str = include_str!("settings/settings.css");

/// Its script, served at `/settings/settings.js`.
pub(crate) const SCRIPT: &str = include_str!("settings/settings.js");

/// `Some(choose)` when `url` is the settings page asking about the download
/// folder: `choose` picks the folder dialog, otherwise it wants the current
/// path. Anything else — another origin, a near-miss host, a trailing query
/// — is not a request.
pub(crate) fn download_folder_request(url: &str) -> Option<bool> {
    match url.strip_prefix(SETTINGS_URL)? {
        "/download-folder" => Some(false),
        "/download-folder/choose" => Some(true),
        _ => None,
    }
}

/// `Some(theme)` when `url` is the settings page reporting a new theme
/// pick. The page has already stored it and painted every open shell
/// document; this tells the app, which owns the window icon and the
/// colour a fresh view paints before its first frame. Unknown slugs and
/// other origins are not reports — the pick never reaches the app as
/// raw text (AGENTS §6).
pub(crate) fn theme_request(url: &str) -> Option<Theme> {
    Theme::from_slug(url.strip_prefix(SETTINGS_URL)?.strip_prefix("/theme/")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_reports_require_a_settings_route_and_a_known_theme() {
        for theme in crate::shell::theme::ALL {
            assert_eq!(
                theme_request(&format!("{SETTINGS_URL}/theme/{}", theme.slug())),
                Some(*theme)
            );
        }
        for url in [
            "https://example.com/settings/theme/light",
            "http://cryo.localhost.evil/settings/theme/light",
            "http://cryo.localhost/settings/theme/light?extra=1",
            "http://cryo.localhost/settings/theme/solarized",
            "http://cryo.localhost/settings/theme/",
            SETTINGS_URL,
        ] {
            assert_eq!(theme_request(url), None, "accepted {url}");
        }
    }

    /// The page drives the pick by navigating to that route, so the two
    /// names have to agree.
    #[test]
    fn the_page_reports_the_pick_on_the_route_the_parser_accepts() {
        assert!(SCRIPT.contains("\"/settings/theme/\" + slug"));
        assert!(SCRIPT.contains("window.__cryo_theme(slug)"));
        assert!(HTML.contains("id=\"theme-pop\""));
    }

    #[test]
    fn download_folder_requests_require_exact_settings_routes() {
        assert_eq!(
            download_folder_request(&format!("{SETTINGS_URL}/download-folder")),
            Some(false)
        );
        assert_eq!(
            download_folder_request(&format!("{SETTINGS_URL}/download-folder/choose")),
            Some(true)
        );
        for url in [
            "https://example.com/settings/download-folder/choose",
            "http://cryo.localhost.evil/settings/download-folder/choose",
            "http://cryo.localhost/settings/download-folder/choose?extra=1",
            SETTINGS_URL,
        ] {
            assert_eq!(download_folder_request(url), None);
        }
    }

    /// The page drives the folder flow by navigating to the routes above,
    /// so the two names have to agree.
    #[test]
    fn the_page_asks_on_the_routes_the_parser_accepts() {
        for route in [
            "/settings/download-folder",
            "/settings/download-folder/choose",
        ] {
            assert!(SCRIPT.contains(route), "the page never asks for {route}");
        }
    }
}
