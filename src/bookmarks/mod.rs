//! Bookmarks: the pages the user keeps, and the bar that shows them.
//!
//! `Bookmarks` is the aggregate root — an ordered registry that mints
//! `BookmarkId`s, dedupes by (folder, url) and stays bounded. `Bookmark`
//! is built in exactly one place (`Bookmark::hydrate`); a stored row and
//! an imported draft both arrive through it, so an invalid bookmark
//! cannot exist even briefly (AGENTS §6). `Store` is the port this
//! context declares; `JsonFile` and `Memory` are its adapters.
//!
//! Timestamps come from the caller — no ambient clock lives here.

mod file;
mod mem;

pub use file::JsonFile;
pub use mem::Memory;

/// Registry identity. Minted by `Bookmarks`, carried on the wire to the
/// chrome document, and the handle every mutation names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BookmarkId(u64);

impl BookmarkId {
    pub fn get(self) -> u64 {
        self.0
    }

    /// Rebuild an id the chrome document sent back. The registry is the
    /// authority on which ids exist — an unknown one simply misses.
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl std::fmt::Display for BookmarkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One kept page. Immutable once built: a change replaces it wholesale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    id: BookmarkId,
    title: String,
    url: String,
    /// `""` is the bar itself; otherwise a `/`-joined path such as
    /// `News/Rust`. Normalized by the constructor.
    folder: String,
    /// Unix seconds. Imports keep the source's date when it has one.
    added_at: u64,
    /// The site's icon as a `data:` URL — what the bar and the library
    /// paint instead of a letter tile. `None` until a visit brings one.
    favicon: Option<String>,
}

/// A bookmark as a foreign source proposes it: unvalidated strings with
/// no identity. Importers build these — plain data, so a worker thread
/// can hand a whole file's worth back to the event loop (AGENTS §9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub url: String,
    pub folder: String,
    pub added_at: u64,
    /// Foreign formats carry no icons we can read — importers leave
    /// `None` and the first visit fills it in.
    pub favicon: Option<String>,
}

/// What `add_all` did: enough to tell the user, without naming urls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// New rows now in the registry.
    pub added: usize,
    /// Rows already kept under the same folder and url.
    pub duplicates: usize,
    /// Rows that failed validation (no url, unusable scheme).
    pub rejected: usize,
    /// Rows dropped because the registry is full.
    pub overflowed: usize,
}

/// Hard bound on kept bookmarks. Memory is the product — an unbounded
/// registry is the bug this project exists to fight.
const MAX_BOOKMARKS: usize = 10_000;

/// Longest title and url kept. A megabyte-long `data:` url in an
/// imported file is a payload, not a destination.
const MAX_TITLE: usize = 300;
const MAX_URL: usize = 2_048;

/// Longest folder path kept, and the deepest nesting. Past these the
/// path is cut, not rejected — the bookmark still belongs somewhere.
const MAX_FOLDER: usize = 300;
const MAX_DEPTH: usize = 8;

/// Longest icon kept, in `data:` URL characters. A favicon PNG is a few
/// kilobytes; a megabyte payload in a bookmark file is not an icon.
const MAX_FAVICON: usize = 128_000;

/// The wire version `JsonFile` writes and reads. v1 rows had no
/// `favicon` — the file adapter migrates them at the boundary.
pub(crate) const CURRENT_VERSION: u32 = 2;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum BookmarkError {
    #[error("a bookmark needs a url")]
    NoUrl,
    #[error("{0} is not a bookmarkable address")]
    Scheme(String),
    #[error("the bookmark registry is full ({MAX_BOOKMARKS} kept)")]
    Full,
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing stored bookmarks: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("stored bookmarks have version {0}, this build reads {CURRENT_VERSION}")]
    Version(u32),
}

impl Bookmark {
    /// The one door (AGENTS §6): every `Bookmark` — stored row, imported
    /// draft or starred page — is built here and validated here. There
    /// is no "hydrate then fix up".
    fn hydrate(
        id: BookmarkId,
        title: &str,
        url: &str,
        folder: &str,
        added_at: u64,
        favicon: Option<&str>,
    ) -> Result<Self, BookmarkError> {
        let url = url.trim();
        if url.is_empty() {
            return Err(BookmarkError::NoUrl);
        }
        if url.len() > MAX_URL || !bookmarkable(url) {
            return Err(BookmarkError::Scheme(truncate(url, 40)));
        }
        let title = truncate(title.trim(), MAX_TITLE);
        Ok(Self {
            id,
            // A bookmark with no title shows its url — never a blank row.
            title: if title.is_empty() {
                url.to_string()
            } else {
                title
            },
            url: url.to_string(),
            folder: normalize_folder(folder),
            added_at,
            favicon: clean_favicon(favicon),
        })
    }

    pub fn id(&self) -> BookmarkId {
        self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn folder(&self) -> &str {
        &self.folder
    }

    pub fn added_at(&self) -> u64 {
        self.added_at
    }

    /// The site's icon as a `data:` URL, when one has arrived.
    pub fn favicon(&self) -> Option<&str> {
        self.favicon.as_deref()
    }

    /// Bar rows are the ones with no folder — what the strip shows.
    pub fn in_bar(&self) -> bool {
        self.folder.is_empty()
    }
}

/// Schemes worth keeping. `javascript:` bookmarklets are the classic
/// import payload: the shell's own documents run on the internal
/// origin, so one must never arrive as a kept destination.
fn bookmarkable(url: &str) -> bool {
    const KEEP: [&str; 4] = ["http://", "https://", "file:///", "about:"];
    let lower = url.to_lowercase();
    KEEP.iter().any(|p| lower.starts_with(p))
}

/// An icon worth keeping: an image `data:` URL under the bound. A row
/// whose icon is anything else still loads — the icon is decorative,
/// so it is dropped rather than costing the bookmark.
fn clean_favicon(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    (raw.len() <= MAX_FAVICON && raw.starts_with("data:image/")).then(|| raw.to_string())
}

/// Keep the path readable and bounded: trim each segment, drop empties,
/// cap the depth, cap the whole.
fn normalize_folder(folder: &str) -> String {
    let joined = folder
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(MAX_DEPTH)
        .collect::<Vec<_>>()
        .join("/");
    truncate(&joined, MAX_FOLDER)
}

/// Cut at a char boundary — `&s[..n]` panics mid-codepoint.
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => s[..cut].to_string(),
        None => s.to_string(),
    }
}

mod registry;
pub use registry::Bookmarks;

/// The persistence port this context declares. Adapters live in `file`
/// and `mem`; the domain never names a path or a format.
pub trait Store: Send {
    fn load(&self) -> Result<Vec<Bookmark>, BookmarkError>;
    fn save(&self, rows: &[Bookmark]) -> Result<(), BookmarkError>;
}

mod keeper;
pub use keeper::Keeper;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_row_with_a_bogus_icon_loads_without_one() {
        let row = Bookmark::hydrate(
            BookmarkId(1),
            "A",
            "https://a.test",
            "",
            1,
            Some("https://x.evil"),
        )
        .unwrap();
        assert_eq!(row.favicon(), None);
    }
}
