//! Recorder persistence service: loading and saving the in-memory aggregate.
use super::*;

/// The service the app drives: `History` stays authoritative in memory,
/// flushed through the `Store` — batched, not on every mutation, since a
/// navigation is far more frequent than a durable write is worth.
/// Whatever is unflushed goes out on drop.
pub struct Recorder {
    history: History,
    store: Box<dyn Store>,
    writer: Option<worker::WriteQueue<Entry>>,
    /// Mutations since the last write — the batch `FLUSH_EVERY` counts.
    unsaved: u32,
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
            writer: None,
            unsaved: 0,
        }
    }

    /// In-memory only — records and matches but never persists. The test
    /// seam; incognito doesn't construct a `Recorder` at all.
    pub fn session_only() -> Self {
        Self {
            history: History::default(),
            store: Box::new(Memory),
            writer: None,
            unsaved: 0,
        }
    }

    /// Remember `text` and mark the batch; the write lands every
    /// `FLUSH_EVERY` mutations. `seen_at` comes from the caller — no
    /// ambient clock inside the domain.
    pub fn record(
        &mut self,
        text: &str,
        title: Option<&str>,
        seen_at: u64,
    ) -> Result<(), PersistError> {
        self.history.record(text, title, seen_at);
        self.dirty()
    }

    /// Attach a document title to a remembered URL — and mark the batch
    /// only when the title actually changed, since title events repeat
    /// what the file already holds.
    pub fn note_title(&mut self, text: &str, title: &str) -> Result<(), PersistError> {
        if self.history.note_title(text, title) {
            self.dirty()?;
        }
        Ok(())
    }

    /// Attach a site's icon (`data:` URL) to a remembered URL and flush —
    /// but only when the icon actually changed, since favicon events
    /// repeat what the file already holds.
    pub fn note_favicon(&mut self, text: &str, data_url: &str) -> Result<(), PersistError> {
        if self.history.note_favicon(text, data_url) {
            self.dirty()?;
        }
        Ok(())
    }

    /// A mutation went in — write it out once the batch is full.
    fn dirty(&mut self) -> Result<(), PersistError> {
        self.unsaved += 1;
        if self.unsaved >= FLUSH_EVERY {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), PersistError> {
        if let Some(writer) = &self.writer {
            writer.submit(self.history.entries().to_vec());
        } else {
            self.store.save(self.history.entries())?;
        }
        self.unsaved = 0;
        Ok(())
    }

    pub(crate) fn background(
        &mut self,
        failed: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<worker::WriterShutdown> {
        let store = std::mem::replace(&mut self.store, Box::new(Memory));
        let (writer, shutdown) = worker::WriteQueue::spawn(
            move |rows| store.save(rows).map_err(|e| e.to_string()),
            failed,
        )?;
        self.writer = Some(writer);
        Ok(shutdown)
    }

    pub fn matches(&self, needle: &str, limit: usize) -> Vec<&Entry> {
        self.history.matches(needle, limit)
    }

    /// Forget one destination and flush — an explicit user action earns
    /// an immediate write, unlike routine recording.
    pub fn remove(&mut self, text: &str) -> Result<(), PersistError> {
        if self.history.remove(text) {
            self.flush()?;
        }
        Ok(())
    }

    /// Forget everything and flush — same reasoning as `remove`.
    pub fn clear(&mut self) -> Result<(), PersistError> {
        self.history.clear();
        self.flush()
    }

    /// Fold an imported history in and flush once — an explicit user
    /// action earns an immediate write, unlike routine recording.
    /// The caller has already built valid entries through `Entry::new`.
    pub fn import(&mut self, entries: Vec<Entry>) -> Result<MergeReport, PersistError> {
        let report = self.history.merge(entries);
        if report.added > 0 || report.merged > 0 {
            self.flush()?;
        }
        Ok(report)
    }

    /// Everything remembered — what an export writes out.
    pub fn entries(&self) -> &[Entry] {
        self.history.entries()
    }
}

impl Drop for Recorder {
    /// A crashed or closing window must not lose the pending batch —
    /// flush what `dirty` left behind. A failed write is reported, not
    /// `?`'d: `Drop` has nowhere to return an error to.
    fn drop(&mut self) {
        if self.unsaved > 0 {
            if let Err(e) = self.flush() {
                crate::report!("flushing history on drop: {e:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_round_trips_through_the_store() {
        let dir = std::env::temp_dir().join(format!("cryo-test-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut rec = Recorder::open(JsonFile::new(path.clone()));
        rec.record("https://a", Some("A"), 42).unwrap();
        // Recording batches its write — the drop pays what is pending.
        drop(rec);
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
