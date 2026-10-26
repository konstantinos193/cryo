//! Outbound chrome state and library payloads.
use serde::Serialize;

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
    /// The Google session the account button shows: every live account
    /// in jar order — the dropdown lists them all. Empty keeps the
    /// "Sign in" pill.
    pub accounts: Vec<ChromeAccount>,
    /// Which account the window browses as — the button wears its face
    /// and the dropdown marks its row. `0` is Google's own default.
    pub active_account: u64,
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
    /// "bookmarks" | "passwords" | "history" | "import" | "export"
    pub pane: String,
    pub bookmarks: Vec<ChromeBookmark>,
    /// Kept bookmarks the push left out — the pane says how many rather
    /// than pretending the list is all of them.
    pub more_bookmarks: usize,
    /// Remembered visits and typed input — the history pane's rows,
    /// newest first. Empty (with `incognito` set) means "this window
    /// keeps nothing", which the pane says instead of looking broken.
    pub history: Vec<ChromeHistoryEntry>,
    /// Remembered entries the push left out.
    pub more_history: usize,
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

/// One remembered entry on the wire — a visited URL or a committed
/// omnibox input. `text` is the identity `history_remove` sends back.
#[derive(Debug, Serialize)]
pub struct ChromeHistoryEntry {
    pub text: String,
    /// Document title; `None` on typed input and iconless visits.
    pub title: Option<String>,
    pub uses: u32,
    /// Unix seconds of the last visit — the pane groups and sorts on it.
    pub last_seen: u64,
    pub favicon: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChromePassword {
    pub origin: String,
    pub username: String,
    /// How many dots to draw. The secret itself stays in the app.
    pub length: usize,
    pub note: String,
}

/// Username-only row in the fill chooser. Secrets never enter chrome state.
#[derive(Debug, Serialize)]
pub struct PasswordAccount {
    pub username: String,
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

/// `__cryo_secret` payload: one password, for one click. `copy` says
/// the document hands it to the clipboard instead of putting it on
/// screen.
#[derive(Debug, Serialize)]
pub struct SecretReply {
    pub origin: String,
    pub username: String,
    pub secret: String,
    pub copy: bool,
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
    /// The tab's address, so the tab-search overlay can match on it.
    /// It never leaves this document — the chrome view is the shell
    /// origin, and only this window's tabs are in it.
    pub url: String,
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
