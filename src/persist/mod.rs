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
                eprintln!("cryo: {e:?}");
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

    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Entry> {
        self.history.matches(needle, limit)
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
}
