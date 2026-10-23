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

/// The wire version `JsonFile` writes and reads.
pub(crate) const CURRENT_VERSION: u32 = 1;

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

/// The ordered registry. One `Vec`, no parallel maps (AGENTS §5).
#[derive(Default)]
pub struct Bookmarks {
    rows: Vec<Bookmark>,
    next_id: u64,
}

impl Bookmarks {
    /// Hydration door for stored state — the caller has already migrated
    /// the wire format. Duplicates and overflow are dropped here rather
    /// than failing the load: one bad row must not cost the whole file.
    pub fn hydrate(rows: Vec<Bookmark>) -> Self {
        let next_id = rows.iter().map(|b| b.id.0).max().map_or(1, |max| max + 1);
        let mut out = Self {
            rows: Vec::with_capacity(rows.len().min(MAX_BOOKMARKS)),
            next_id,
        };
        for row in rows.into_iter().take(MAX_BOOKMARKS) {
            if !out.contains(&row.folder, &row.url) {
                out.rows.push(row);
            }
        }
        out
    }

    pub fn rows(&self) -> &[Bookmark] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The bar's rows in registry order — what the chrome strip paints.
    pub fn bar(&self) -> impl Iterator<Item = &Bookmark> {
        self.rows.iter().filter(|b| b.in_bar())
    }

    /// Every folder holding at least one bookmark, sorted, without the
    /// bar itself.
    pub fn folders(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .rows
            .iter()
            .map(|b| b.folder.as_str())
            .filter(|f| !f.is_empty())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn get(&self, id: BookmarkId) -> Option<&Bookmark> {
        self.rows.iter().find(|b| b.id == id)
    }

    /// Is this exact destination already kept in that folder?
    pub fn contains(&self, folder: &str, url: &str) -> bool {
        let folder = normalize_folder(folder);
        self.rows
            .iter()
            .any(|b| b.folder == folder && b.url == url.trim())
    }

    /// Kept in any folder — what the star button asks before it fills.
    pub fn starred(&self, url: &str) -> Option<BookmarkId> {
        let url = url.trim();
        self.rows.iter().find(|b| b.url == url).map(|b| b.id)
    }

    /// Keep one page. A duplicate reports the row already there instead
    /// of growing the registry.
    pub fn add(&mut self, draft: &Draft) -> Result<BookmarkId, BookmarkError> {
        let mark = Bookmark::hydrate(
            BookmarkId(self.next_id),
            &draft.title,
            &draft.url,
            &draft.folder,
            draft.added_at,
        )?;
        if let Some(kept) = self
            .rows
            .iter()
            .find(|b| b.folder == mark.folder && b.url == mark.url)
        {
            return Ok(kept.id);
        }
        if self.rows.len() >= MAX_BOOKMARKS {
            return Err(BookmarkError::Full);
        }
        let id = mark.id;
        self.next_id += 1;
        self.rows.push(mark);
        Ok(id)
    }

    /// Bulk import. Every draft goes through the same door `add` uses;
    /// the report is what the user sees afterwards.
    pub fn add_all(&mut self, drafts: &[Draft]) -> MergeReport {
        let mut report = MergeReport::default();
        for draft in drafts {
            let before = self.rows.len();
            match self.add(draft) {
                Ok(_) if self.rows.len() > before => report.added += 1,
                Ok(_) => report.duplicates += 1,
                Err(BookmarkError::Full) => report.overflowed += 1,
                Err(_) => report.rejected += 1,
            }
        }
        report
    }

    /// Drop one row. Reports whether anything was there — the caller
    /// skips the flush when nothing changed.
    pub fn remove(&mut self, id: BookmarkId) -> bool {
        let before = self.rows.len();
        self.rows.retain(|b| b.id != id);
        self.rows.len() != before
    }

    /// Case-insensitive match over title, url and folder — the library
    /// panel's filter and the omnibox's bookmark rows. An empty needle
    /// matches everything, which is how the panel lists.
    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Bookmark> {
        let needle = needle.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|b| {
                needle.is_empty()
                    || b.title.to_lowercase().contains(&needle)
                    || b.url.to_lowercase().contains(&needle)
                    || b.folder.to_lowercase().contains(&needle)
            })
            .take(limit)
            .collect()
    }
}

/// The persistence port this context declares. Adapters live in `file`
/// and `mem`; the domain never names a path or a format.
pub trait Store {
    fn load(&self) -> Result<Vec<Bookmark>, BookmarkError>;
    fn save(&self, rows: &[Bookmark]) -> Result<(), BookmarkError>;
}

/// The service the app drives: the registry stays authoritative in
/// memory, flushed through the `Store` after every mutation — the same
/// shape as `persist::Recorder`.
pub struct Keeper {
    marks: Bookmarks,
    store: Box<dyn Store>,
}

impl Keeper {
    /// Load what the store has and keep writing back to it. A failed
    /// load starts empty rather than stopping the browser; the next
    /// save heals the file.
    pub fn open(store: impl Store + 'static) -> Self {
        let rows = match store.load() {
            Ok(rows) => rows,
            Err(e) => {
                crate::report!("{e:?}");
                Vec::new()
            }
        };
        Self {
            marks: Bookmarks::hydrate(rows),
            store: Box::new(store),
        }
    }

    /// In-memory only — keeps and matches but never persists. The test
    /// seam, and what an incognito window gets.
    pub fn session_only() -> Self {
        Self {
            marks: Bookmarks::default(),
            store: Box::new(Memory),
        }
    }

    pub fn marks(&self) -> &Bookmarks {
        &self.marks
    }

    pub fn add(&mut self, draft: &Draft) -> Result<BookmarkId, BookmarkError> {
        let id = self.marks.add(draft)?;
        self.flush()?;
        Ok(id)
    }

    pub fn add_all(&mut self, drafts: &[Draft]) -> Result<MergeReport, BookmarkError> {
        let report = self.marks.add_all(drafts);
        if report.added > 0 {
            self.flush()?;
        }
        Ok(report)
    }

    pub fn remove(&mut self, id: BookmarkId) -> Result<bool, BookmarkError> {
        let removed = self.marks.remove(id);
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    fn flush(&self) -> Result<(), BookmarkError> {
        self.store.save(self.marks.rows())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(title: &str, url: &str, folder: &str) -> Draft {
        Draft {
            title: title.to_string(),
            url: url.to_string(),
            folder: folder.to_string(),
            added_at: 100,
        }
    }

    #[test]
    fn add_mints_ids_and_keeps_order() {
        let mut marks = Bookmarks::default();
        let a = marks.add(&draft("A", "https://a.test", "")).unwrap();
        let b = marks.add(&draft("B", "https://b.test", "News")).unwrap();
        assert_ne!(a, b);
        assert_eq!(marks.len(), 2);
        assert_eq!(marks.rows()[0].title(), "A");
        assert_eq!(marks.rows()[1].folder(), "News");
    }

    #[test]
    fn same_url_in_same_folder_is_one_bookmark() {
        let mut marks = Bookmarks::default();
        let first = marks.add(&draft("A", "https://a.test", "")).unwrap();
        let again = marks.add(&draft("A again", "https://a.test", "")).unwrap();
        assert_eq!(first, again);
        assert_eq!(marks.len(), 1);
        // The same url filed elsewhere is a different bookmark.
        marks.add(&draft("A", "https://a.test", "News")).unwrap();
        assert_eq!(marks.len(), 2);
    }

    #[test]
    fn bookmarklets_and_blank_urls_are_refused() {
        let mut marks = Bookmarks::default();
        assert!(matches!(
            marks.add(&draft("evil", "javascript:alert(1)", "")),
            Err(BookmarkError::Scheme(_))
        ));
        assert!(matches!(
            marks.add(&draft("blank", "   ", "")),
            Err(BookmarkError::NoUrl)
        ));
        assert!(matches!(
            marks.add(&draft("data", "data:text/html,<script>", "")),
            Err(BookmarkError::Scheme(_))
        ));
        assert!(marks.is_empty());
    }

    #[test]
    fn untitled_bookmarks_show_their_url() {
        let mut marks = Bookmarks::default();
        marks.add(&draft("  ", "https://a.test", "")).unwrap();
        assert_eq!(marks.rows()[0].title(), "https://a.test");
    }

    #[test]
    fn folder_paths_are_normalized_and_bounded() {
        let mut marks = Bookmarks::default();
        marks
            .add(&draft("A", "https://a.test", " /News/ / Rust/ "))
            .unwrap();
        assert_eq!(marks.rows()[0].folder(), "News/Rust");
        let deep = (0..20)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/");
        marks.add(&draft("B", "https://b.test", &deep)).unwrap();
        assert_eq!(marks.rows()[1].folder().split('/').count(), MAX_DEPTH);
    }

    #[test]
    fn long_titles_cut_on_char_boundaries() {
        let mut marks = Bookmarks::default();
        let title = "é".repeat(MAX_TITLE + 50);
        marks.add(&draft(&title, "https://a.test", "")).unwrap();
        assert_eq!(marks.rows()[0].title().chars().count(), MAX_TITLE);
    }

    #[test]
    fn bar_rows_are_the_folderless_ones() {
        let mut marks = Bookmarks::default();
        marks.add(&draft("A", "https://a.test", "")).unwrap();
        marks.add(&draft("B", "https://b.test", "News")).unwrap();
        assert_eq!(marks.bar().count(), 1);
        assert_eq!(marks.folders(), vec!["News"]);
    }

    #[test]
    fn add_all_reports_added_duplicates_and_rejects() {
        let mut marks = Bookmarks::default();
        let report = marks.add_all(&[
            draft("A", "https://a.test", ""),
            draft("A", "https://a.test", ""),
            draft("bad", "javascript:0", ""),
        ]);
        assert_eq!(report.added, 1);
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.overflowed, 0);
    }

    #[test]
    fn a_full_registry_overflows_instead_of_growing() {
        let mut marks = Bookmarks::default();
        let drafts: Vec<Draft> = (0..MAX_BOOKMARKS + 5)
            .map(|i| draft("x", &format!("https://x{i}.test"), ""))
            .collect();
        let report = marks.add_all(&drafts);
        assert_eq!(marks.len(), MAX_BOOKMARKS);
        assert_eq!(report.added, MAX_BOOKMARKS);
        assert_eq!(report.overflowed, 5);
    }

    #[test]
    fn starred_finds_a_url_in_any_folder() {
        let mut marks = Bookmarks::default();
        let id = marks.add(&draft("A", "https://a.test", "News")).unwrap();
        assert_eq!(marks.starred("https://a.test"), Some(id));
        assert_eq!(marks.starred("https://b.test"), None);
    }

    #[test]
    fn remove_reports_whether_anything_went() {
        let mut marks = Bookmarks::default();
        let id = marks.add(&draft("A", "https://a.test", "")).unwrap();
        assert!(marks.remove(id));
        assert!(!marks.remove(id));
        assert!(marks.is_empty());
    }

    #[test]
    fn matches_search_title_url_and_folder() {
        let mut marks = Bookmarks::default();
        marks
            .add(&draft("Rust Blog", "https://blog.rust-lang.org", "Dev"))
            .unwrap();
        assert_eq!(marks.matches("rust", 5).len(), 1);
        // A substring, not a fuzzy match: the words must be in order.
        assert_eq!(marks.matches("rust bl", 5).len(), 1);
        assert_eq!(marks.matches("blog rust", 5).len(), 0);
        assert_eq!(marks.matches("rust-lang", 5).len(), 1);
        assert_eq!(marks.matches("dev", 5).len(), 1);
        assert_eq!(marks.matches("", 5).len(), 1);
        assert_eq!(marks.matches("nope", 5).len(), 0);
    }

    #[test]
    fn hydrate_keeps_ids_and_mints_above_the_highest() {
        let kept = vec![
            Bookmark::hydrate(BookmarkId(7), "A", "https://a.test", "", 1).unwrap(),
            Bookmark::hydrate(BookmarkId(3), "B", "https://b.test", "", 1).unwrap(),
        ];
        let mut marks = Bookmarks::hydrate(kept);
        assert_eq!(marks.rows()[0].id(), BookmarkId(7));
        let next = marks.add(&draft("C", "https://c.test", "")).unwrap();
        assert_eq!(next, BookmarkId(8));
    }

    #[test]
    fn hydrate_drops_stored_duplicates() {
        let kept = vec![
            Bookmark::hydrate(BookmarkId(1), "A", "https://a.test", "", 1).unwrap(),
            Bookmark::hydrate(BookmarkId(2), "A", "https://a.test", "", 1).unwrap(),
        ];
        assert_eq!(Bookmarks::hydrate(kept).len(), 1);
    }

    #[test]
    fn keeper_round_trips_through_the_store() {
        let dir = std::env::temp_dir().join(format!("cryo-bm-{}", std::process::id()));
        let path = dir.join("bookmarks.json");
        let mut keeper = Keeper::open(JsonFile::new(path.clone()));
        let id = keeper.add(&draft("A", "https://a.test", "News")).unwrap();
        let reopened = Keeper::open(JsonFile::new(path.clone()));
        assert_eq!(reopened.marks().len(), 1);
        assert_eq!(reopened.marks().get(id).unwrap().folder(), "News");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_only_keeps_but_writes_nowhere() {
        let mut keeper = Keeper::session_only();
        keeper.add(&draft("A", "https://a.test", "")).unwrap();
        assert_eq!(keeper.marks().len(), 1);
    }
}
