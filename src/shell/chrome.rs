//! Chrome document and its public wire API. Commands, state, suggestions,
//! and address policy each have one owning module.
mod command;
mod navigation;
mod state;
mod suggestions;
pub use command::ChromeCommand;
pub use navigation::{canonize_nav_input, normalize_nav_input};
pub use state::*;
pub use suggestions::{completion_for, SuggestReply, Suggestion};

/// A tab's icon (PNG bytes) → the `data:` URL `ChromeTab.favicon` carries.
pub(crate) fn favicon_data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", crate::base64(png))
}

/// The strip's document — kept in its own file next to the module.
pub const CHROME_HTML: &str = include_str!("chrome.html");

pub(crate) const CHROME_STYLE: &str = concat!(
    include_str!("chrome/chrome.css"),
    "\n",
    include_str!("chrome/library.css"),
);

/// One execution unit preserves initialization order and prevents callbacks
/// from running between feature files while the document is still loading.
pub(crate) const CHROME_SCRIPT: &str = concat!(
    include_str!("chrome/bootstrap.js"),
    "\n",
    include_str!("chrome/permissions.js"),
    "\n",
    include_str!("chrome/omnibox.js"),
    "\n",
    include_str!("chrome/downloads.js"),
    "\n",
    include_str!("chrome/library.js"),
    "\n",
    include_str!("chrome/bookmarks.js"),
    "\n",
    include_str!("chrome/passwords.js"),
    "\n",
    include_str!("chrome/history.js"),
    "\n",
    include_str!("chrome/transfer.js"),
    "\n",
    include_str!("chrome/render.js"),
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::SearchEngine;

    #[test]
    fn normalize_passthroughs_and_upgrades() {
        let ddg = SearchEngine::DEFAULT;
        assert_eq!(normalize_nav_input("", &ddg), "about:blank");
        assert_eq!(normalize_nav_input("  ", &ddg), "about:blank");
        assert_eq!(
            normalize_nav_input("https://a.b/c?d=e", &ddg),
            "https://a.b/c?d=e"
        );
        assert_eq!(normalize_nav_input("about:blank", &ddg), "about:blank");
        assert_eq!(
            normalize_nav_input("example.com", &ddg),
            "https://example.com"
        );
        assert_eq!(
            normalize_nav_input("rust borrow checker", &ddg),
            "https://duckduckgo.com/?q=rust%20borrow%20checker"
        );
    }

    #[test]
    fn a_pasted_windows_path_becomes_a_file_url() {
        let ddg = SearchEngine::DEFAULT;
        assert_eq!(
            normalize_nav_input(r"C:\Users\konst\Desktop\cryo-bookmarks.html", &ddg),
            "file:///C:/Users/konst/Desktop/cryo-bookmarks.html"
        );
        assert_eq!(
            normalize_nav_input("C:/dir/with space/f.txt", &ddg),
            "file:///C:/dir/with%20space/f.txt"
        );
        assert_eq!(
            normalize_nav_input(r"\\srv\share\f.txt", &ddg),
            "file://srv/share/f.txt"
        );
        // And Ctrl+Enter cannot www.com-ify a path either.
        assert_eq!(
            canonize_nav_input(r"D:\dir\nodot", &ddg),
            "file:///D:/dir/nodot"
        );
        // A URL with a scheme is still untouched.
        assert_eq!(
            normalize_nav_input("file:///C:/dir/f.txt", &ddg),
            "file:///C:/dir/f.txt"
        );
    }

    #[test]
    fn normalize_searches_on_the_given_engine() {
        let google = SearchEngine::by_slug("google").unwrap();
        assert_eq!(
            normalize_nav_input("two words", &google),
            "https://www.google.com/search?q=two%20words"
        );
        // The engine only sees search input — URLs pass through untouched.
        assert_eq!(
            normalize_nav_input("example.com", &google),
            "https://example.com"
        );
        let custom = SearchEngine::from_pick("https://kagi.com/search?q=%s").unwrap();
        assert_eq!(
            normalize_nav_input("two words", &custom),
            "https://kagi.com/search?q=two%20words"
        );
    }

    #[test]
    fn completion_extends_prefixes_and_strips_the_scheme() {
        // The bare form is what the box fills — "git" completes to
        // "github.com", not "https://github.com".
        assert_eq!(
            completion_for("https://github.com", "git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            completion_for("https://www.reddit.com/r/rust", "redd").as_deref(),
            Some("reddit.com/r/rust")
        );
        // Typed input stored without a scheme completes as-is.
        assert_eq!(
            completion_for("github.com/explore", "github").as_deref(),
            Some("github.com/explore")
        );
        // A scheme already typed keeps it — the tail continues the text.
        assert_eq!(
            completion_for("https://github.com", "https://gi").as_deref(),
            Some("https://github.com")
        );
    }

    #[test]
    fn completion_needs_a_real_extension() {
        // Equal strings complete nothing — the box is already there.
        assert_eq!(completion_for("https://github.com", "github.com"), None);
        // Substring matches are rows, not completions.
        assert_eq!(completion_for("https://github.com", "hub"), None);
        assert_eq!(completion_for("https://github.com", "gitx"), None);
    }

    #[test]
    fn canonize_wraps_bare_words_only() {
        let ddg = SearchEngine::DEFAULT;
        assert_eq!(canonize_nav_input("cnn", &ddg), "https://www.cnn.com");
        assert_eq!(
            canonize_nav_input("example.com", &ddg),
            "https://example.com"
        );
        assert_eq!(
            canonize_nav_input("two words", &ddg),
            "https://duckduckgo.com/?q=two%20words"
        );
    }

    #[test]
    fn command_wire_format_round_trips() {
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"navigate","input":"x"}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Navigate {
                input,
                ctrl: false,
                alt: false,
                shift: false,
                ..
            } if input == "x"
        ));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"navigate","input":"x","alt":true}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Navigate { alt: true, .. }));
        // The stored engine rides along on the commands that consult it.
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"navigate","input":"x","engine":"google"}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Navigate { engine, .. } if engine.as_deref() == Some("google")
        ));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"suggest","q":"x","engine":"bing"}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Suggest { q, engine } if q == "x" && engine.as_deref() == Some("bing")
        ));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"ready","engine":"brave"}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::Ready { engine } if engine.as_deref() == Some("brave")
        ));
        // Absent means "not reported" — the field defaults, never errors.
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"ready"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Ready { engine: None }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"activate","id":3}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Activate { id: 3 }));
        // Freeze/discard carry an optional id — absent means active tab.
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"freeze","id":3}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Freeze { id: Some(3) }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"freeze"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Freeze { id: None }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"discard","id":2}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Discard { id: Some(2) }));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"stop"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Stop));
        assert!(serde_json::from_str::<ChromeCommand>(r#"{"cmd":"bogus"}"#).is_err());
    }

    #[test]
    fn library_command_wire_format_round_trips() {
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"bookmark_page"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::BookmarkPage));
        // Modifiers are optional, like `navigate`'s.
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"bookmark_open","id":4}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::BookmarkOpen {
                id: 4,
                ctrl: false,
                shift: false
            }
        ));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"bookmark_open","id":4,"ctrl":true}"#).unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::BookmarkOpen { ctrl: true, .. }
        ));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"bookmark_remove","id":7}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::BookmarkRemove { id: 7 }));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"library","pane":"import"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Library { pane } if pane == "import"));
        let cmd: ChromeCommand = serde_json::from_str(
            r#"{"cmd":"password_reveal","origin":"https://a.test","username":"bob"}"#,
        )
        .unwrap();
        assert!(matches!(
            cmd,
            ChromeCommand::PasswordReveal { origin, username }
                if origin == "https://a.test" && username == "bob"
        ));
        let cmd: ChromeCommand = serde_json::from_str(
            r#"{"cmd":"password_copy","origin":"https://a.test","username":"bob"}"#,
        )
        .unwrap();
        assert!(matches!(cmd, ChromeCommand::PasswordCopy { .. }));
        let cmd: ChromeCommand = serde_json::from_str(
            r#"{"cmd":"password_remove","origin":"https://a.test","username":""}"#,
        )
        .unwrap();
        assert!(matches!(cmd, ChromeCommand::PasswordRemove { .. }));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"password_fill","username":"bob"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::PasswordFill { username } if username == "bob"));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"password_save"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::PasswordSave {}));
        let cmd: ChromeCommand = serde_json::from_str(r#"{"cmd":"import_file"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::ImportFile {}));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"import_profile","index":2}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::ImportProfile { index: 2 }));
        let cmd: ChromeCommand =
            serde_json::from_str(r#"{"cmd":"export","what":"passwords"}"#).unwrap();
        assert!(matches!(cmd, ChromeCommand::Export { what } if what == "passwords"));
    }

    #[test]
    fn a_library_command_cannot_smuggle_a_path() {
        // The document indexes the source list; it never names a file.
        assert!(serde_json::from_str::<ChromeCommand>(
            r#"{"cmd":"import_profile","index":1,"path":"C:\\x"}"#
        )
        .is_err());
        assert!(
            serde_json::from_str::<ChromeCommand>(r#"{"cmd":"import_file","path":"C:\\x"}"#)
                .is_err()
        );
    }

    #[test]
    fn unknown_fields_inside_a_known_command_are_rejected() {
        assert!(
            serde_json::from_str::<ChromeCommand>(r#"{"cmd":"activate","id":3,"idb":4}"#).is_err()
        );
        assert!(serde_json::from_str::<ChromeCommand>(
            r#"{"cmd":"navigate","input":"x","ctrll":true}"#
        )
        .is_err());
        // serde's limit: unit variants have no fields to deny, so extras
        // on them ({"cmd":"stop","id":3}) still parse. Everything the
        // wire could corrupt is a struct variant.
        assert!(serde_json::from_str::<ChromeCommand>(r#"{"cmd":"stop","id":3}"#).is_ok());
    }

    #[test]
    fn favicon_png_becomes_a_data_url() {
        assert_eq!(
            favicon_data_url(b"\x89PNG"),
            "data:image/png;base64,iVBORw=="
        );
    }

    #[test]
    fn chrome_contains_the_trusted_password_card() {
        assert!(CHROME_HTML.contains("id=\"password-pop\""));
        let bootstrap = include_str!("chrome/bootstrap.js");
        assert!(bootstrap.contains("window.__cryo_password_prompt"));
        assert!(bootstrap.contains("cmd: 'password_fill'"));
        assert!(bootstrap.contains("cmd: 'password_save'"));
    }
}
