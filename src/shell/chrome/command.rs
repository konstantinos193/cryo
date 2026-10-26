//! Commands accepted from the trusted chrome document.
use serde::Deserialize;

/// Commands the chrome UI can send. Wire format: JSON tagged by `cmd`.
/// Unknown fields are rejected — a typo'd key inside a known command
/// must fail loudly, not parse with the extra data dropped.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChromeCommand {
    PermissionAnswer {
        token: u64,
        allow: bool,
    },
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
    /// An account picked off the avatar's dropdown — the app opens that
    /// account's Google home under its `authuser` index, which is its
    /// position in the ListAccounts order the state carries.
    AccountOpen {
        index: u64,
    },
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
    /// "passwords", "history", "import" or "export". The panel is a
    /// chrome overlay, so its bounds still travel through `Overlay`.
    Library {
        pane: String,
    },
    /// Add an unpacked extension. Carries no path: the folder comes from
    /// an OS picker the app raises, never from this document.
    ExtensionAdd,
    /// Remove an installed extension. `slug` is a catalogue handle, and
    /// the app resolves it against the listing rather than as a path.
    ExtensionRemove {
        slug: String,
    },
    /// Fetch one from the Chrome Web Store. `input` is whatever was typed —
    /// a store address or an id — and the app is what decides whether it
    /// names an extension at all.
    ExtensionInstall {
        input: String,
    },
    /// A history row clicked — where it lands follows the modifiers,
    /// same rule as `BookmarkOpen`. The URL travels instead of an id:
    /// history's key is the remembered text itself.
    HistoryOpen {
        url: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
    },
    /// The row's ✕ — forget one destination.
    HistoryRemove {
        text: String,
    },
    /// "Clear history" — forgets every recorded visit and typed input.
    /// A struct variant so a stray `{"cmd":"history_clear","x":…}`
    /// fails `deny_unknown_fields` rather than parsing anyway.
    HistoryClear {},
    /// Show one saved password in the clear. The app asks the user to
    /// confirm before it answers — a click in this document is not on
    /// its own enough to put a secret on screen.
    PasswordReveal {
        origin: String,
        username: String,
    },
    /// Copy one saved password to the clipboard without showing it.
    /// Goes through the same confirm gate as `PasswordReveal` — a
    /// clipboard is a disclosure too.
    PasswordCopy {
        origin: String,
        username: String,
    },
    PasswordRemove {
        origin: String,
        username: String,
    },
    /// Fill one account selected from the trusted password card.
    PasswordFill {
        username: String,
    },
    /// Accept the pending save/update proposal.
    PasswordSave {},
    /// Dismiss the password card without changing the vault.
    PasswordPromptClose {},
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
