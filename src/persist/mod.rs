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
pub(crate) mod worker;

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

/// Longest icon kept, in `data:` URL characters.
const MAX_FAVICON: usize = 128_000;

/// Mutations a `Recorder` lets stack up before it pays a full write.
/// `record` fires on every navigation — rewriting a 5000-entry file
/// each time is disk churn on the event loop; a handful of pending
/// writes costs a crash at most the last few visits.
const FLUSH_EVERY: u32 = 16;

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

    /// The hydration door for stored rows (AGENTS §6): same rules as
    /// `new`, plus the icon our own file was allowed to keep. A row the
    /// door can't validate — a hand-edited file's empty `text`, say —
    /// is refused, and the store drops it rather than failing the file.
    pub fn hydrate(
        text: &str,
        title: Option<&str>,
        uses: u32,
        last_seen: u64,
        favicon: Option<&str>,
    ) -> Result<Self, PersistError> {
        let mut entry = Self::new(text, title, uses, last_seen)?;
        // Only a `data:` URL survives the door: the omnibox paints this
        // string verbatim, so a stored `https://` icon would be a fetch
        // to a hand-edited host. The row itself is fine — the icon is
        // cosmetic and the next visit refills it.
        entry.favicon = favicon
            .map(str::trim)
            .filter(|f| f.starts_with("data:") && f.len() <= MAX_FAVICON)
            .map(str::to_string);
        Ok(entry)
    }
}

mod history;
pub use history::History;

/// The persistence port: what the domain needs from the outside — write
/// the entries back, read them on open. Implementations live in `file`
/// and `mem`; the domain never names a path or a format.
pub trait Store: Send {
    fn load(&self) -> Result<Vec<Entry>, PersistError>;
    fn save(&self, entries: &[Entry]) -> Result<(), PersistError>;
}

mod recorder;
pub use recorder::Recorder;

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hydrate_drops_a_favicon_that_is_not_self_contained() {
        let e = Entry::hydrate(
            "https://a",
            None,
            1,
            1,
            Some("https://tracker.example/x.png"),
        )
        .unwrap();
        assert_eq!(e.favicon, None, "a remote icon is a fetch — refused");
        let e = Entry::hydrate("https://a", None, 1, 1, Some("data:image/png;base64,zz")).unwrap();
        assert_eq!(e.favicon.as_deref(), Some("data:image/png;base64,zz"));
    }
}
