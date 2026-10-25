//! Bookmarks aggregate: bounded collection, queries and mutations.
use super::*;

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

    /// Draft → `Bookmark`: the validation half of `add`, before dedupe
    /// and capacity — `add` and `add_all` share it.
    fn hydrated(&self, draft: &Draft) -> Result<Bookmark, BookmarkError> {
        Bookmark::hydrate(
            BookmarkId(self.next_id),
            &draft.title,
            &draft.url,
            &draft.folder,
            draft.added_at,
            draft.favicon.as_deref(),
        )
    }

    /// Bound, then store — the insert both add paths share once the row
    /// is known not to duplicate.
    fn push(&mut self, mark: Bookmark) -> Result<BookmarkId, BookmarkError> {
        if self.rows.len() >= MAX_BOOKMARKS {
            return Err(BookmarkError::Full);
        }
        let id = mark.id;
        self.next_id += 1;
        self.rows.push(mark);
        Ok(id)
    }

    /// Keep one page. A duplicate reports the row already there instead
    /// of growing the registry.
    pub fn add(&mut self, draft: &Draft) -> Result<BookmarkId, BookmarkError> {
        let mark = self.hydrated(draft)?;
        if let Some(kept) = self
            .rows
            .iter()
            .find(|b| b.folder == mark.folder && b.url == mark.url)
        {
            return Ok(kept.id);
        }
        self.push(mark)
    }

    /// Bulk import. Every draft goes through the same doors `add` uses —
    /// `hydrated` validates, `push` bounds and stores — but dedupe runs
    /// against a key set built once, so the import costs its own size,
    /// not its size squared. The report is what the user sees afterwards.
    pub fn add_all(&mut self, drafts: &[Draft]) -> MergeReport {
        let mut report = MergeReport::default();
        let mut keys: std::collections::HashSet<(String, String)> = self
            .rows
            .iter()
            .map(|b| (b.folder.clone(), b.url.clone()))
            .collect();
        for draft in drafts {
            match self.hydrated(draft) {
                Err(_) => report.rejected += 1,
                Ok(mark) => {
                    let key = (mark.folder.clone(), mark.url.clone());
                    if keys.contains(&key) {
                        report.duplicates += 1;
                        continue;
                    }
                    match self.push(mark) {
                        Ok(_) => {
                            keys.insert(key);
                            report.added += 1;
                        }
                        Err(BookmarkError::Full) => report.overflowed += 1,
                        Err(_) => report.rejected += 1,
                    }
                }
            }
        }
        report
    }

    /// Attach a site's icon to every row pointing at `url` — the star
    /// can be pressed before the icon arrives, and an imported bookmark
    /// only gets one on its first visit. Same URL in different folders
    /// is the same site, so all of them wear it. Returns whether
    /// anything changed so the caller only flushes on a real update.
    pub fn set_favicon(&mut self, url: &str, data_url: &str) -> bool {
        let Some(icon) = clean_favicon(Some(data_url)) else {
            return false;
        };
        let mut changed = false;
        for b in &mut self.rows {
            if b.url == url && b.favicon.as_deref() != Some(icon.as_str()) {
                // Value objects are replaced wholesale, not patched.
                *b = Bookmark {
                    favicon: Some(icon.clone()),
                    ..b.clone()
                };
                changed = true;
            }
        }
        changed
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

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(title: &str, url: &str, folder: &str) -> Draft {
        Draft {
            title: title.to_string(),
            url: url.to_string(),
            folder: folder.to_string(),
            added_at: 100,
            favicon: None,
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
            Bookmark::hydrate(BookmarkId(7), "A", "https://a.test", "", 1, None).unwrap(),
            Bookmark::hydrate(BookmarkId(3), "B", "https://b.test", "", 1, None).unwrap(),
        ];
        let mut marks = Bookmarks::hydrate(kept);
        assert_eq!(marks.rows()[0].id(), BookmarkId(7));
        let next = marks.add(&draft("C", "https://c.test", "")).unwrap();
        assert_eq!(next, BookmarkId(8));
    }

    #[test]
    fn hydrate_drops_stored_duplicates() {
        let kept = vec![
            Bookmark::hydrate(BookmarkId(1), "A", "https://a.test", "", 1, None).unwrap(),
            Bookmark::hydrate(BookmarkId(2), "A", "https://a.test", "", 1, None).unwrap(),
        ];
        assert_eq!(Bookmarks::hydrate(kept).len(), 1);
    }

    #[test]
    fn set_favicon_wears_the_site_icon_in_every_folder() {
        let mut marks = Bookmarks::default();
        marks.add(&draft("A", "https://a.test", "")).unwrap();
        marks.add(&draft("A", "https://a.test", "News")).unwrap();
        marks.add(&draft("B", "https://b.test", "")).unwrap();
        assert!(marks.set_favicon("https://a.test", "data:image/png;base64,xx"));
        assert_eq!(marks.rows()[0].favicon(), Some("data:image/png;base64,xx"));
        assert_eq!(marks.rows()[1].favicon(), Some("data:image/png;base64,xx"));
        assert_eq!(marks.rows()[2].favicon(), None);
        assert!(
            !marks.set_favicon("https://a.test", "data:image/png;base64,xx"),
            "the same icon again is a no-op"
        );
    }

    #[test]
    fn set_favicon_rejects_what_is_not_an_icon() {
        let mut marks = Bookmarks::default();
        marks.add(&draft("A", "https://a.test", "")).unwrap();
        assert!(!marks.set_favicon("https://a.test", "javascript:0"));
        assert!(!marks.set_favicon("https://a.test", ""));
        assert_eq!(marks.rows()[0].favicon(), None);
    }
}
