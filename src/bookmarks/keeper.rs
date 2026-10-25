//! Keeper persistence service: loading and saving the in-memory aggregate.
use super::*;

/// The service the app drives: the registry stays authoritative in
/// memory, flushed through the `Store` after every mutation — the same
/// shape as `persist::Recorder`.
pub struct Keeper {
    marks: Bookmarks,
    store: Box<dyn Store>,
    writer: Option<crate::persist::worker::WriteQueue<Bookmark>>,
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
            writer: None,
        }
    }

    /// In-memory only — keeps and matches but never persists. The test
    /// seam, and what an incognito window gets.
    pub fn session_only() -> Self {
        Self {
            marks: Bookmarks::default(),
            store: Box::new(Memory),
            writer: None,
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

    /// Attach a site's icon (`data:` URL) to every row at `url` and
    /// flush — but only when the icon actually changed, since favicon
    /// events repeat what the file already holds.
    pub fn set_favicon(&mut self, url: &str, data_url: &str) -> Result<bool, BookmarkError> {
        if self.marks.set_favicon(url, data_url) {
            self.flush()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn flush(&self) -> Result<(), BookmarkError> {
        if let Some(writer) = &self.writer {
            writer.submit(self.marks.rows().to_vec());
            Ok(())
        } else {
            self.store.save(self.marks.rows())
        }
    }

    pub(crate) fn background(
        &mut self,
        failed: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<crate::persist::worker::WriterShutdown> {
        let store = std::mem::replace(&mut self.store, Box::new(Memory));
        let (writer, shutdown) = crate::persist::worker::WriteQueue::spawn(
            move |rows| store.save(rows).map_err(|e| e.to_string()),
            failed,
        )?;
        self.writer = Some(writer);
        Ok(shutdown)
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
