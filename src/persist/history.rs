//! History aggregate: bounded collection, queries and mutations.
use super::*;

/// Ordered visit memory. Dedupes by `text`, ranks matches by uses then
/// recency, never exceeds `MAX_ENTRIES`.
#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    /// Hydration door — the one place stored bytes become a `History`.
    /// The caller has already migrated the wire format.
    pub fn from_entries(mut entries: Vec<Entry>) -> Self {
        // The file can't be trusted to hold the bound either — a
        // hand-edited 50,000-row history enters memory at its size
        // unless the door enforces it. Same eviction as `merge`:
        // strongest first, then cut.
        if entries.len() > MAX_ENTRIES {
            entries.sort_by_key(|e| std::cmp::Reverse((e.uses, e.last_seen)));
            entries.truncate(MAX_ENTRIES);
        }
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
    /// Returns whether anything changed, so the caller only flushes on a
    /// real update — title events repeat what the file already holds.
    pub fn note_title(&mut self, text: &str, title: &str) -> bool {
        if title.is_empty() {
            return false;
        }
        match self.entries.iter_mut().find(|e| e.text == text) {
            Some(e) if e.title.as_deref() != Some(title) => {
                e.title = Some(title.to_string());
                true
            }
            _ => false,
        }
    }

    /// Attach a site's icon to a remembered URL — favicons arrive after
    /// the visit was recorded, like titles. No entry, no write. Returns
    /// whether anything changed so the caller only flushes on a real
    /// update — icon events repeat a `data:` URL we may already hold.
    /// An oversized payload is refused: a favicon is a few kilobytes,
    /// not a megabyte.
    pub fn note_favicon(&mut self, text: &str, data_url: &str) -> bool {
        if data_url.is_empty() || data_url.len() > MAX_FAVICON {
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

    /// Forget one destination — the history pane's per-row delete.
    /// Returns whether anything was there.
    pub fn remove(&mut self, text: &str) -> bool {
        match self.entries.iter().position(|e| e.text == text) {
            Some(i) => {
                self.entries.remove(i);
                true
            }
            None => false,
        }
    }

    /// Forget everything — the history pane's "clear all".
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Fold an imported history into this one.
    ///
    /// Counts take the stronger of the two rather than adding: importing
    /// the same file twice must not inflate what the omnibox ranks
    /// first. Titles already here are kept — ours came from a document
    /// we actually loaded. Trimming to `MAX_ENTRIES` happens once, at
    /// the end, by strength, so a 50,000-row file costs one sort rather
    /// than 50,000 evictions. Duplicates resolve through a position
    /// index — a linear find per row would square the import's size.
    pub fn merge(&mut self, imported: Vec<Entry>) -> MergeReport {
        let mut report = MergeReport::default();
        let mut pos: std::collections::HashMap<String, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.text.clone(), i))
            .collect();
        for row in imported {
            match pos.get(&row.text) {
                Some(&i) => {
                    let mine = &mut self.entries[i];
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
                    pos.insert(row.text.clone(), self.entries.len());
                    self.entries.push(row);
                    report.added += 1;
                }
            }
        }
        if self.entries.len() > MAX_ENTRIES {
            // Strongest first, then cut: same rule as `record`'s
            // eviction — fewest uses, then oldest, goes.
            self.entries
                .sort_by_key(|e| std::cmp::Reverse((e.uses, e.last_seen)));
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
        hits.sort_by_key(|e| std::cmp::Reverse((e.uses, e.last_seen)));
        hits.truncate(limit.min(MAX_MATCHES));
        hits
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
    fn remove_and_clear_forget() {
        let mut h = History::default();
        h.record("https://a", None, 1);
        h.record("https://b", None, 2);
        assert!(!h.remove("https://ghost"), "nothing there, nothing done");
        assert!(h.remove("https://a"));
        assert_eq!(h.entries()[0].text, "https://b");
        h.clear();
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
    fn hydration_enforces_the_bound_a_file_could_not_hold() {
        let rows: Vec<Entry> = (0..MAX_ENTRIES + 10)
            .map(|i| Entry::new(&format!("https://{i}"), None, i as u32 + 1, i as u64).unwrap())
            .collect();
        let h = History::from_entries(rows);
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert!(
            h.entries().iter().all(|e| e.uses > 10),
            "the weakest ten were evicted"
        );
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
}
