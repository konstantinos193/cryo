//! The chrome strip: a self-contained HTML/JS UI plus the wire types it
//! speaks with the app over IPC (`window.ipc.postMessage` in, one
//! `__cryo_update(state)` eval out).

use serde::{Deserialize, Serialize};

use crate::shell::search::SearchEngine;

/// Commands the chrome UI can send. Wire format: JSON tagged by `cmd`.
/// Unknown fields are rejected — a typo'd key inside a known command
/// must fail loudly, not parse with the extra data dropped.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChromeCommand {
    /// Sent by the chrome document when it finishes parsing — the signal to
    /// push state. The page announces itself instead of the app racing the
    /// document's load.
    Ready {
        /// The search-engine pick the document read from the shell
        /// origin's localStorage — the app cannot see the storage area,
        /// so it travels on the wire: a preset slug or a custom `%s`
        /// template. `None` means "not reported" and keeps whatever the
        /// app already has, never a reset.
        #[serde(default)]
        engine: Option<String>,
    },
    NewTab,
    Activate {
        id: u64,
    },
    Close {
        id: u64,
    },
    Navigate {
        input: String,
        /// Modifier state at Enter — decides where the navigation lands
        /// (current tab, new tab, new window). Wire fields, policy in app.
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
        /// The stored search-engine pick, read fresh at send time — it
        /// may have changed in the settings tab since the last command.
        /// `None` keeps the app's current engine.
        #[serde(default)]
        engine: Option<String>,
    },
    Reload,
    /// The reload button's ✕ state — aborts the in-flight load.
    Stop,
    Back,
    Forward,
    /// Open Google sign-in in a new tab — the profile cookie jar is shared,
    /// so signing in once applies to every site and survives restarts.
    SignIn,
    NewIncognito,
    DevTools,
    ClearData,
    /// Open the internal settings page in a new tab.
    Settings,
    /// Menu's "Check for updates" — result surfaces in a dialog.
    CheckUpdates,
    /// "Restart to update" — runs the staged installer and exits.
    UpdateApply,
    /// Freeze a tab: snapshot kept, surface dropped. `id` absent means
    /// the active tab — the ⋮ menu doesn't know tab ids, the strip's
    /// per-tab buttons do.
    Freeze {
        #[serde(default)]
        id: Option<u64>,
    },
    /// Discard a tab: the harsher drop — only the address survives.
    Discard {
        #[serde(default)]
        id: Option<u64>,
    },
    /// A chrome overlay (⋮ menu, omnibox dropdown) opened or closed —
    /// while open the app grows this view over the whole window so the
    /// popup can paint above the content views (a view can't draw
    /// outside its own bounds).
    Overlay {
        open: bool,
    },
    /// The omnibox asks for suggestions per keystroke. The app answers
    /// with a `__cryo_suggest` eval — history never travels the other
    /// direction: `/cmd` reaches the app only from this view (content
    /// views carry no `on_ipc`), and the page resolver would hand a
    /// fetchable list to any origin.
    Suggest {
        q: String,
        /// Same piggyback as `navigate` — the stored engine, so the
        /// "Search …" verdict row names what a commit would use.
        #[serde(default)]
        engine: Option<String>,
    },
    /// The ✕ on a live download row.
    DownloadCancel {
        key: u64,
    },
    /// Clicking a finished row — the file opens with its default app.
    DownloadOpen {
        key: u64,
    },
    /// The ⏸ on a live row — the transfer holds, resumable.
    DownloadPause {
        key: u64,
    },
    /// The ▶ on a paused row.
    DownloadResume {
        key: u64,
    },
    /// "Retry" on an interrupted or cancelled row — the app re-navigates
    /// a throwaway tab to the file's URI.
    DownloadRetry {
        key: u64,
    },
    /// A finished row pointer-dragged out of the shelf — the app starts
    /// a native OLE drag carrying the file, so a drop on a folder copies
    /// it and a drop back on the strip opens it.
    DownloadDrag {
        key: u64,
    },
    /// The 📁 on a finished row — Explorer selects the file.
    DownloadShow {
        key: u64,
    },
    /// "Clear finished" drops every settled row off the shelf.
    DownloadsClear,
    /// The ★ button, and Ctrl+D: keep the active page, or drop it again
    /// if it is already kept. One toggle, like every other browser's.
    BookmarkPage,
    /// Open a kept page — a bar row or a library row. Modifiers decide
    /// where it lands, same wire fields as `Navigate`.
    BookmarkOpen {
        id: u64,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
    },
    BookmarkRemove {
        id: u64,
    },
    /// The library panel wants a pane's data pushed: "bookmarks",
    /// "passwords", "import" or "export". The panel is a chrome overlay,
    /// so its bounds still travel through `Overlay`.
    Library {
        pane: String,
    },
    /// Show one saved password in the clear. The app asks the user to
    /// confirm before it answers — a click in this document is not on
    /// its own enough to put a secret on screen.
    PasswordReveal {
        origin: String,
        username: String,
    },
    PasswordRemove {
        origin: String,
        username: String,
    },
    /// "Import from a file…": one picker, and the format is sniffed.
    /// A struct variant with no fields — a unit variant would parse
    /// `{"cmd":"import_file","path":"…"}` and smuggle a path past
    /// `deny_unknown_fields`.
    ImportFile {},
    /// Import from a browser profile the app found. The index is into
    /// the source list the app pushed — a path never travels back
    /// through the document.
    ImportProfile {
        index: usize,
    },
    /// Write bookmarks, passwords or history out to a file the user
    /// picks: "bookmarks" | "passwords" | "history".
    Export {
        what: String,
    },
}

/// `__cryo_suggest` payload — `q` is the query echoed back so the
/// document can drop a reply that lands after the user typed on.
#[derive(Debug, Serialize)]
pub struct SuggestReply {
    pub q: String,
    pub rows: Vec<Suggestion>,
}

/// One omnibox row.
#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub label: String,
    /// What goes back through `navigate` — a URL for sites, the raw
    /// text for remembered input (the app normalizes it again).
    pub input: String,
    /// The row's icon as a `data:` URL — `None` falls back to a glyph
    /// (search/go rows) or a letter dot (sites with no remembered icon).
    pub icon: Option<String>,
    /// What ArrowRight/End fills in — the URL minus its scheme when it
    /// extends the typed text. `None` on rows that can't complete (the
    /// search row, substring-only matches).
    pub complete: Option<String>,
    /// "search" | "go" | "page" — picks the fallback glyph.
    pub kind: &'static str,
}

/// The inline completion a suggestion offers, in the address bar's own
/// vocabulary: what the user typed stays, the tail arrives selected. A
/// URL the query prefixes as-typed wins first (so "https://git" keeps
/// its scheme); otherwise the scheme and `www.` come off and the bare
/// form is tried — that's the text the box fills, so the row completes
/// the way the user expects to type it.
pub fn completion_for(url: &str, q: &str) -> Option<String> {
    let ql = q.to_lowercase();
    if url.len() > q.len() && url.to_lowercase().starts_with(&ql) {
        return Some(url.to_string());
    }
    let mut bare = url;
    for prefix in ["https://", "http://", "www."] {
        if let Some(rest) = bare.strip_prefix(prefix) {
            bare = rest;
        }
    }
    if bare.len() > q.len() && bare.to_lowercase().starts_with(&ql) {
        Some(bare.to_string())
    } else {
        None
    }
}

/// One download shelf row — pushed inside `ChromeState` so progress
/// repaints with every update.
#[derive(Debug, Serialize)]
pub struct ChromeDownload {
    /// App-assigned shelf key — row commands send it back unchanged.
    pub key: u64,
    pub name: String,
    /// Absolute path — the row's tooltip, and `reveal` resolves it.
    pub path: String,
    pub received: u64,
    /// Declared size; `null` until the server says (some never do).
    pub total: Option<u64>,
    /// "progress" | "paused" | "done" | "interrupted" | "cancelled"
    pub state: &'static str,
}

/// State pushed to the chrome UI after every change.
#[derive(Debug, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<ChromeTab>,
    pub active_url: String,
    /// Session-history depth of the active tab's surface — the arrows
    /// grey out when there's nowhere to go.
    pub can_back: bool,
    pub can_fwd: bool,
    /// A downloaded, verified installer is staged — the menu shows
    /// "Restart to update" and the ⋮ button gets a badge.
    pub update_version: Option<String>,
    /// The download shelf, newest first. The ⬇ button hides while empty.
    pub downloads: Vec<ChromeDownload>,
    /// The bookmarks bar: kept pages with no folder, in the order they
    /// were kept. Empty means the strip falls back to the speed dial.
    pub bookmarks: Vec<ChromeBookmark>,
    /// The active tab's address is kept — the ★ fills in.
    pub starred: bool,
    /// The Google session the account button shows — `None` keeps the
    /// "Sign in" pill.
    pub account: Option<ChromeAccount>,
}

/// The account button's face: what `crate::account` found in the
/// profile's jar.
#[derive(Debug, Serialize)]
pub struct ChromeAccount {
    /// Display name — the tooltip and the initial-letter fallback.
    pub name: String,
    /// The tooltip's second line.
    pub email: String,
    /// Picture as a `data:` URL; `None` draws the initial instead.
    pub avatar: Option<String>,
}

/// One bookmark on the wire — bar rows and library rows alike.
#[derive(Debug, Serialize)]
pub struct ChromeBookmark {
    pub id: u64,
    pub title: String,
    pub url: String,
    /// `""` for a bar row; the library shows the path.
    pub folder: String,
    /// Unix seconds, used by the library's newest-first view.
    pub added_at: u64,
    /// The site's icon as a `data:` URL — `None` falls back to the
    /// letter tile, same as the omnibox.
    pub favicon: Option<String>,
}

/// The library panel's payload, pushed by `__cryo_library` when a pane
/// is opened or after an import changes what it shows. It travels apart
/// from `ChromeState` because the panel is usually shut and the lists
/// are long.
#[derive(Debug, Serialize)]
pub struct ChromeLibrary {
    /// "bookmarks" | "passwords" | "import" | "export"
    pub pane: String,
    pub bookmarks: Vec<ChromeBookmark>,
    /// Kept bookmarks the push left out — the pane says how many rather
    /// than pretending the list is all of them.
    pub more_bookmarks: usize,
    /// Saved sign-ins, never their secrets: a password reaches this
    /// document only through `__cryo_secret`, after the user confirms.
    pub passwords: Vec<ChromePassword>,
    /// Saved sign-ins the push left out.
    pub more_passwords: usize,
    /// Browser profiles found on this machine, in the order the import
    /// commands index them.
    pub sources: Vec<ChromeSource>,
    /// The look for installed browsers has finished. Until it has, the
    /// pane says it is looking rather than claiming there are none.
    pub sources_ready: bool,
    /// The vault could not be opened (another Windows account, a
    /// damaged file) — the pane says so instead of looking empty.
    pub locked: Option<String>,
    /// Incognito windows keep nothing: the panes say so and the import
    /// buttons stay off.
    pub incognito: bool,
}

#[derive(Debug, Serialize)]
pub struct ChromePassword {
    pub origin: String,
    pub username: String,
    /// How many dots to draw. The secret itself stays in the app.
    pub length: usize,
    pub note: String,
}

/// One importable browser profile.
#[derive(Debug, Serialize)]
pub struct ChromeSource {
    /// Index the `import_profile` command sends back.
    pub index: usize,
    pub browser: String,
    pub profile: String,
    /// How many bookmarks it holds.
    pub count: usize,
}

/// `__cryo_secret` payload: one revealed password, for one click.
#[derive(Debug, Serialize)]
pub struct SecretReply {
    pub origin: String,
    pub username: String,
    pub secret: String,
}

/// `__cryo_notice` payload: what an import or export did, in the words
/// the user reads. `ok` picks the colour; `lines` are already phrased.
#[derive(Debug, Serialize)]
pub struct Notice {
    pub ok: bool,
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ChromeTab {
    pub id: u64,
    pub title: String,
    pub active: bool,
    /// "active" | "frozen" | "discarded" — the strip dims and badges by it.
    pub state: &'static str,
    /// The page icon as a `data:` URL — the pushed state is the wire, so
    /// the PNG travels inline instead of over a second request.
    pub favicon: Option<String>,
    /// A navigation is in flight — the strip swaps the icon for a
    /// spinner and the reload button for ✕.
    pub loading: bool,
}

/// A tab's icon (PNG bytes) → the `data:` URL `ChromeTab.favicon` carries.
pub(crate) fn favicon_data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", crate::base64(png))
}

/// Turn address-bar text into a URL: full URLs pass through, domain-looking
/// input gets `https://`, anything else becomes a search on `engine`.
pub fn normalize_nav_input(input: &str, engine: &SearchEngine) -> String {
    let s = input.trim();
    if s.is_empty() {
        return "about:blank".to_string();
    }
    if s.contains("://")
        || s.starts_with("about:")
        || s.starts_with("data:")
        || s.starts_with("file:")
    {
        return s.to_string();
    }
    // A pasted Windows path becomes a file URL, like in any real
    // browser — without this the drive letter's dot (in `file.html`)
    // reads as a host and the path is asked of the web.
    if let Some(url) = local_path_url(s) {
        return url;
    }
    if !s.contains(' ') && s.contains('.') {
        return format!("https://{s}");
    }
    engine.search_url(s)
}

/// Chrome's Ctrl+Enter: a bare word becomes `www.<word>.com`; anything
/// already dotted or multi-word falls back to normal normalization.
pub fn canonize_nav_input(input: &str, engine: &SearchEngine) -> String {
    let s = input.trim();
    if let Some(url) = local_path_url(s) {
        return url;
    }
    if !s.is_empty() && !s.contains(' ') && !s.contains('.') && !s.contains("://") {
        return format!("https://www.{s}.com");
    }
    normalize_nav_input(s, engine)
}

/// `C:\dir\f` or `C:/dir/f` → `file:///C:/dir/f`; `\\srv\dir` →
/// `file://srv/dir`. Anything else is not a path.
fn local_path_url(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b[2], b'\\' | b'/');
    if drive {
        return Some(format!("file:///{}", encode_path(s)));
    }
    if let Some(unc) = s.strip_prefix(r"\\") {
        return Some(format!("file://{}", encode_path(unc)));
    }
    None
}

/// Percent-encode what a `file:` URL cannot carry — path separators are
/// normalized to `/` first, and non-ASCII bytes escape per octet so a
/// Greek or Cyrillic folder name still opens.
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.replace('\\', "/").bytes() {
        match b {
            b' ' | b'%' | b'#' | b'?' | b'<' | b'>' | b'"' | b'|' | 0x00..=0x1F | 0x7F..=0xFF => {
                out.push_str(&format!("%{b:02X}"))
            }
            _ => out.push(b as char),
        }
    }
    out
}

/// The strip's document — kept in its own file next to the module.
pub const CHROME_HTML: &str = include_str!("chrome.html");

#[cfg(test)]
mod tests {
    use super::*;

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
            r#"{"cmd":"password_remove","origin":"https://a.test","username":""}"#,
        )
        .unwrap();
        assert!(matches!(cmd, ChromeCommand::PasswordRemove { .. }));
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
}
