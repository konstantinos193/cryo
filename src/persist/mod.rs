//! Stored state: the memory the shell keeps across sessions.
//!
//! `History` is the domain object — remembered typed input and visits,
//! deduped, ranked, and bounded (memory is the product; an unbounded
//! history file is exactly the bug this project exists to fight).
//! `Store` is the persistence port the domain declares; `JsonFile` and
//! `Memory` are the adapters. `Recorder` is the domain service the app
//! drives — callers stamp `seen_at` themselves so tests control time
//! (AGENTS §6).
//!
//! Nothing here is reachable over the internal protocol: history is
//! private, and `pages::internal_page` answers every origin — a fetchable
//! endpoint would leak it to any website. Suggestions reach the chrome
//! document by eval only.

mod file;
mod mem;

pub use file::JsonFile;
pub use mem::Memory;

/// One remembered entry — a visited URL or a committed omnibox input.
/// A value object: replaced wholesale, never mutated through a reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What gets matched: the URL or the text as typed.
    pub text: String,
    /// The document title when one arrived — matched and shown.
    pub title: Option<String>,
    /// Times committed. Frequency outranks recency.
    pub uses: u32,
    /// Unix seconds of the last visit — stamped by the caller.
    pub last_seen: u64,
    /// The site's icon as a `data:` URL — the omnibox paints it beside
    /// the row. Only ever set for visited URLs; typed input has none.
    pub favicon: Option<String>,
}

/// What `merge` did to the history — reported to the user after an
/// import, the same shape `bookmarks` and `vault` report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Destinations the history had never seen.
    pub added: usize,
    /// Destinations already remembered: the stronger count and the
    /// later visit win.
    pub merged: usize,
    /// Rows with nothing to remember.
    pub rejected: usize,
    /// Rows dropped because the history is full and they were weaker
    /// than what was already there.
    pub overflowed: usize,
}

/// Hard bound on kept history. Past it, the weakest entry (fewest uses,
/// then oldest) makes room for the new one.
const MAX_ENTRIES: usize = 5_000;

/// The most a single suggest query returns.
const MAX_MATCHES: usize = 8;

/// The wire version `JsonFile` writes and reads.
pub(crate) const CURRENT_VERSION: u32 = 1;

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PersistError {
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
    #[error("parsing stored state: {0}")]
    Parse(#[source] serde_json::Error),
    #[error("stored state has version {0}, this build reads {CURRENT_VERSION}")]
    Version(u32),
    /// An imported row carried no destination to remember.
    #[error("an entry needs text")]
    NoText,
}

impl Entry {
    /// The door imports come through (AGENTS §6): an `Entry` built from
    /// a foreign file is validated here, so `History::merge` can trust
    /// everything it is handed. Visits recorded in the browser go
    /// through `History::record` instead, which stamps them itself.
    pub fn new(
        text: &str,
        title: Option<&str>,
        uses: u32,
        last_seen: u64,
    ) -> Result<Self, PersistError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(PersistError::NoText);
        }
        Ok(Self {
            text: text.to_string(),
            title: title
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(std::string::ToString::to_string),
            // A visit that happened at least once — an imported zero
            // would rank below everything, including unvisited input.
            uses: uses.max(1),
            last_seen,
            // Foreign formats carry no icons we can read — the first
            // visit after the import fills it in.
            favicon: None,
        })
    }
}

/// Ordered visit memory. Dedupes by `text`, ranks matches by uses then
/// recency, never exceeds `MAX_ENTRIES`.
#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    /// Hydration door — the one place stored bytes become a `History`.
    /// The caller has already migrated the wire format.
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Commit `text` — a visited URL or a typed input. Re-committing the
    /// same text bumps `uses` and refreshes `last_seen`; a non-empty
    /// title overwrites the old one.
    pub fn record(&mut self, text: &str, title: Option<&str>, seen_at: u64) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match self.entries.iter_mut().find(|e| e.text == text) {
            Some(e) => {
                e.uses += 1;
                e.last_seen = seen_at;
                if let Some(t) = title.filter(|t| !t.is_empty()) {
                    e.title = Some(t.to_string());
                }
            }
            None => {
                if self.entries.len() >= MAX_ENTRIES {
                    let weakest = self
                        .entries
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| (e.uses, e.last_seen))
                        .map(|(i, _)| i);
                    if let Some(i) = weakest {
                        self.entries.remove(i);
                    }
                }
                self.entries.push(Entry {
                    text: text.to_string(),
                    title: title
                        .filter(|t| !t.is_empty())
                        .map(std::string::ToString::to_string),
                    uses: 1,
                    last_seen: seen_at,
                    favicon: None,
                });
            }
        }
    }

    /// Attach a title to an entry already remembered — navigations record
    /// the URL before the document's title arrives. No entry, no write.
    pub fn note_title(&mut self, text: &str, title: &str) {
        if title.is_empty() {
            return;
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.text == text) {
            e.title = Some(title.to_string());
        }
    }

    /// Attach a site's icon to a remembered URL — favicons arrive after
    /// the visit was recorded, like titles. No entry, no write. Returns
    /// whether anything changed so the caller only flushes on a real
    /// update — icon events repeat a `data:` URL we may already hold.
    pub fn note_favicon(&mut self, text: &str, data_url: &str) -> bool {
        if data_url.is_empty() {
            return false;
        }
        match self.entries.iter_mut().find(|e| e.text == text) {
            Some(e) if e.favicon.as_deref() != Some(data_url) => {
                e.favicon = Some(data_url.to_string());
                true
            }
            _ => false,
        }
    }

    /// Fold an imported history into this one.
    ///
    /// Counts take the stronger of the two rather than adding: importing
    /// the same file twice must not inflate what the omnibox ranks
    /// first. Titles already here are kept — ours came from a document
    /// we actually loaded. Trimming to `MAX_ENTRIES` happens once, at
    /// the end, by strength, so a 50,000-row file costs one sort rather
    /// than 50,000 evictions.
    pub fn merge(&mut self, imported: Vec<Entry>) -> MergeReport {
        let mut report = MergeReport::default();
        for row in imported {
            match self.entries.iter_mut().find(|e| e.text == row.text) {
                Some(mine) => {
                    mine.uses = mine.uses.max(row.uses);
                    mine.last_seen = mine.last_seen.max(row.last_seen);
                    if mine.title.is_none() {
                        mine.title = row.title;
                    }
                    if mine.favicon.is_none() {
                        mine.favicon = row.favicon;
                    }
                    report.merged += 1;
                }
                None => {
                    self.entries.push(row);
                    report.added += 1;
                }
            }
        }
        if self.entries.len() > MAX_ENTRIES {
            // Strongest first, then cut: same rule as `record`'s
            // eviction — fewest uses, then oldest, goes.
            self.entries
                .sort_by(|a, b| (b.uses, b.last_seen).cmp(&(a.uses, a.last_seen)));
            // What falls off can be an entry that was already here, so
            // this counts rows dropped, not rows refused.
            report.overflowed = self.entries.len() - MAX_ENTRIES;
            self.entries.truncate(MAX_ENTRIES);
        }
        report
    }

    /// Case-insensitive substring match over text and title, best first:
    /// more uses, then more recent. Capped at `MAX_MATCHES`.
    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Entry> {
        let needle = needle.to_lowercase();
        let mut hits: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| {
                e.text.to_lowercase().contains(&needle)
                    || e.title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        hits.sort_by(|a, b| (b.uses, b.last_seen).cmp(&(a.uses, a.last_seen)));
        hits.truncate(limit.min(MAX_MATCHES));
        hits
    }
}

/// The persistence port: what the domain needs from the outside — write
/// the entries back, read them on open. Implementations live in `file`
/// and `mem`; the domain never names a path or a format.
pub trait Store {
    fn load(&self) -> Result<Vec<Entry>, PersistError>;
    fn save(&self, entries: &[Entry]) -> Result<(), PersistError>;
}

/// The service the app drives: `History` stays authoritative in memory,
/// flushed through the `Store` after every mutation.
pub struct Recorder {
    history: History,
    store: Box<dyn Store>,
}

impl Recorder {
    /// Load what the store has and keep writing back to it. A load
    /// failure (missing file is fine — corruption is not) starts empty
    /// rather than stopping the browser; the next save heals the file.
    pub fn open(store: impl Store + 'static) -> Self {
        let entries = match store.load() {
            Ok(entries) => entries,
            Err(e) => {
                crate::report!("{e:?}");
                Vec::new()
            }
        };
        Self {
            history: History::from_entries(entries),
            store: Box::new(store),
        }
    }

    /// In-memory only — records and matches but never persists. The test
    /// seam; incognito doesn't construct a `Recorder` at all.
    pub fn session_only() -> Self {
        Self {
            history: History::default(),
            store: Box::new(Memory),
        }
    }

    /// Remember `text` and flush. `seen_at` comes from the caller — no
    /// ambient clock inside the domain.
    pub fn record(
        &mut self,
        text: &str,
        title: Option<&str>,
        seen_at: u64,
    ) -> Result<(), PersistError> {
        self.history.record(text, title, seen_at);
        self.store.save(self.history.entries())
    }

    /// Attach a document title to a remembered URL and flush.
    pub fn note_title(&mut self, text: &str, title: &str) -> Result<(), PersistError> {
        self.history.note_title(text, title);
        self.store.save(self.history.entries())
    }

    /// Attach a site's icon (`data:` URL) to a remembered URL and flush —
    /// but only when the icon actually changed, since favicon events
    /// repeat what the file already holds.
    pub fn note_favicon(&mut self, text: &str, data_url: &str) -> Result<(), PersistError> {
        if self.history.note_favicon(text, data_url) {
            self.store.save(self.history.entries())?;
        }
        Ok(())
    }

    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Entry> {
        self.history.matches(needle, limit)
    }

    /// Fold an imported history in and flush once. The caller has
    /// already built valid entries through `Entry::new`.
    pub fn import(&mut self, entries: Vec<Entry>) -> Result<MergeReport, PersistError> {
        let report = self.history.merge(entries);
        if report.added > 0 || report.merged > 0 {
            self.store.save(self.history.entries())?;
        }
        Ok(report)
    }

    /// Everything remembered — what an export writes out.
    pub fn entries(&self) -> &[Entry] {
        self.history.entries()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_dedupes_bumps_uses_and_refreshes_time() {
        let mut h = History::default();
        h.record("https://a", None, 10);
        h.record("https://b", None, 20);
        h.record("https://a", Some("Site A"), 30);
        let a = &h.entries()[0];
        assert_eq!(a.uses, 2);
        assert_eq!(a.last_seen, 30);
        assert_eq!(a.title.as_deref(), Some("Site A"));
        assert_eq!(h.entries().len(), 2);
    }

    #[test]
    fn empty_and_whitespace_input_is_not_recorded() {
        let mut h = History::default();
        h.record("   ", None, 1);
        h.record("", None, 1);
        assert!(h.entries().is_empty());
    }

    #[test]
    fn matches_rank_by_uses_then_recency_and_cap() {
        let mut h = History::default();
        h.record("https://old-match", None, 1);
        for _ in 0..5 {
            h.record("https://hot-match", None, 2);
        }
        h.record("https://new-match", None, 99);
        let hits = h.matches("match", 10);
        assert_eq!(hits[0].text, "https://hot-match");
        assert_eq!(hits[1].text, "https://new-match");
        assert_eq!(hits[2].text, "https://old-match");
        assert!(h.matches("match", 2).len() <= 2);
    }

    #[test]
    fn matches_search_titles_too() {
        let mut h = History::default();
        h.record("https://example.com", Some("Rust Language Site"), 1);
        let hits = h.matches("rust", 10);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn note_title_only_updates_existing_entries() {
        let mut h = History::default();
        h.note_title("https://ghost", "Nope");
        assert!(h.entries().is_empty());
        h.record("https://a", None, 1);
        h.note_title("https://a", "Title");
        assert_eq!(h.entries()[0].title.as_deref(), Some("Title"));
    }

    #[test]
    fn note_favicon_only_writes_real_changes() {
        let mut h = History::default();
        assert!(
            !h.note_favicon("https://ghost", "data:x"),
            "no entry, no write"
        );
        h.record("https://a", None, 1);
        assert!(h.note_favicon("https://a", "data:one"));
        assert_eq!(h.entries()[0].favicon.as_deref(), Some("data:one"));
        assert!(
            !h.note_favicon("https://a", "data:one"),
            "same icon is a no-op"
        );
        assert!(h.note_favicon("https://a", "data:two"), "a new icon wins");
        assert_eq!(h.entries()[0].favicon.as_deref(), Some("data:two"));
        assert!(
            !h.note_favicon("https://a", ""),
            "an empty icon clears nothing"
        );
    }

    #[test]
    fn full_history_evicts_the_weakest() {
        let mut h = History::default();
        for i in 0..MAX_ENTRIES {
            h.record(&format!("https://e{i}"), None, i as u64);
        }
        h.record("https://often", None, 0);
        for _ in 0..3 {
            h.record("https://often", None, 1);
        }
        h.record("https://new", None, 5000);
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert!(h.entries().iter().any(|e| e.text == "https://often"));
        assert!(!h.entries().iter().any(|e| e.text == "https://e0"));
    }

    #[test]
    fn recorder_round_trips_through_the_store() {
        let dir = std::env::temp_dir().join(format!("cryo-test-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut rec = Recorder::open(JsonFile::new(path.clone()));
        rec.record("https://a", Some("A"), 42).unwrap();
        let rec2 = Recorder::open(JsonFile::new(path.clone()));
        assert_eq!(rec2.matches("https://a", 5)[0].uses, 1);
        assert_eq!(rec2.matches("https://a", 5)[0].last_seen, 42);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_only_records_but_writes_nowhere() {
        let mut rec = Recorder::session_only();
        rec.record("https://a", None, 1).unwrap();
        assert_eq!(rec.matches("a", 5).len(), 1);
    }

    #[test]
    fn entry_new_validates_what_imports_hand_it() {
        assert!(matches!(
            Entry::new("   ", None, 1, 1),
            Err(PersistError::NoText)
        ));
        let e = Entry::new("  https://a  ", Some("  "), 0, 7).unwrap();
        assert_eq!(e.text, "https://a");
        assert_eq!(e.title, None);
        assert_eq!(e.uses, 1, "an imported visit happened at least once");
        assert_eq!(e.last_seen, 7);
    }

    #[test]
    fn merge_adds_new_rows_and_strengthens_known_ones() {
        let mut h = History::default();
        h.record("https://a", Some("Mine"), 10);
        let report = h.merge(vec![
            Entry::new("https://a", Some("Theirs"), 9, 5).unwrap(),
            Entry::new("https://b", None, 3, 20).unwrap(),
        ]);
        assert_eq!(report.added, 1);
        assert_eq!(report.merged, 1);
        let a = h.entries().iter().find(|e| e.text == "https://a").unwrap();
        assert_eq!(a.uses, 9, "the stronger count wins");
        assert_eq!(a.last_seen, 10, "the later visit wins");
        assert_eq!(a.title.as_deref(), Some("Mine"), "our own title stays");
    }

    #[test]
    fn merge_fills_in_a_title_we_never_had() {
        let mut h = History::default();
        h.record("https://a", None, 1);
        h.merge(vec![Entry::new("https://a", Some("Theirs"), 1, 1).unwrap()]);
        assert_eq!(h.entries()[0].title.as_deref(), Some("Theirs"));
    }

    #[test]
    fn merging_the_same_file_twice_changes_nothing_the_second_time() {
        let rows = || {
            vec![
                Entry::new("https://a", Some("A"), 4, 10).unwrap(),
                Entry::new("https://b", Some("B"), 2, 20).unwrap(),
            ]
        };
        let mut h = History::default();
        h.merge(rows());
        let once: Vec<Entry> = h.entries().to_vec();
        let report = h.merge(rows());
        assert_eq!(report.added, 0);
        assert_eq!(report.merged, 2);
        assert_eq!(h.entries(), once.as_slice(), "an import is idempotent");
    }

    #[test]
    fn merge_keeps_the_bound_and_the_strongest_rows() {
        let mut h = History::default();
        for _ in 0..9 {
            h.record("https://mine-and-busy", None, 1);
        }
        let flood: Vec<Entry> = (0..MAX_ENTRIES + 100)
            .map(|i| Entry::new(&format!("https://e{i}"), None, 1, i as u64).unwrap())
            .collect();
        let report = h.merge(flood);
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert_eq!(report.overflowed, 101);
        assert!(
            h.entries()
                .iter()
                .any(|e| e.text == "https://mine-and-busy"),
            "the busiest row is never the one cut"
        );
    }

    #[test]
    fn recorder_import_flushes_once_and_survives_a_reopen() {
        let dir = std::env::temp_dir().join(format!("cryo-test-imp-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut rec = Recorder::open(JsonFile::new(path.clone()));
        let report = rec
            .import(vec![Entry::new("https://a", Some("A"), 5, 42).unwrap()])
            .unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(rec.entries().len(), 1);
        let reopened = Recorder::open(JsonFile::new(path.clone()));
        assert_eq!(reopened.matches("https://a", 5)[0].uses, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn importing_nothing_is_not_an_error() {
        let mut rec = Recorder::session_only();
        assert_eq!(rec.import(Vec::new()).unwrap(), MergeReport::default());
    }
}
