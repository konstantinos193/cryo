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
    /// Whether the store's contents are in `history` yet. A recorder
    /// opened `pending` records into an empty history while a worker
    /// reads the file, and must not write until it has — a flush before
    /// then would replace the whole file with this session's handful.
    loaded: Loaded,
}

enum Loaded {
    Done,
    /// What the user forgot before the file arrived, replayed over it
    /// in `hydrate`: a clear drops the file outright, removals are
    /// applied to it before this session's rows go back on top.
    Pending {
        cleared: bool,
        removals: Vec<Removal>,
    },
}

enum Removal {
    Text(String),
    Since(u64),
}

impl Recorder {
    /// Load what the store has and keep writing back to it. A load
    /// failure (missing file is fine — corruption is not) starts empty
    /// rather than stopping the browser; the next save heals the file.
    pub fn open(store: impl Store + 'static) -> Self {
        let entries = Self::loaded_or_empty(store.load());
        Self {
            history: History::from_entries(entries),
            store: Box::new(store),
            writer: None,
            unsaved: 0,
            loaded: Loaded::Done,
        }
    }

    /// Open without reading: the caller loads the file on a worker and
    /// hands the result to `hydrate`. Reading a multi-megabyte history
    /// the previous window just rewrote can block for seconds (the
    /// antivirus scans the fresh file), and the launch path must not
    /// wait on that. Until hydrated the recorder remembers and matches
    /// what this session records, and writes nothing.
    pub fn pending(store: impl Store + 'static) -> Self {
        Self {
            history: History::default(),
            store: Box::new(store),
            writer: None,
            unsaved: 0,
            loaded: Loaded::Pending {
                cleared: false,
                removals: Vec::new(),
            },
        }
    }

    /// The file's contents, in: what this session recorded meanwhile
    /// goes back on top (`History::merge` — the stronger count and the
    /// later visit win), forgetting done meanwhile is replayed, and a
    /// batch that was waiting on this is written. On an `open` or
    /// already hydrated recorder this is a no-op.
    pub fn hydrate(
        &mut self,
        loaded: Result<Vec<Entry>, PersistError>,
    ) -> Result<(), PersistError> {
        let Loaded::Pending { cleared, removals } =
            std::mem::replace(&mut self.loaded, Loaded::Done)
        else {
            return Ok(());
        };
        if !cleared {
            let since = std::mem::take(&mut self.history);
            self.history = History::from_entries(Self::loaded_or_empty(loaded));
            for removal in removals {
                match removal {
                    Removal::Text(text) => {
                        self.history.remove(&text);
                    }
                    Removal::Since(cutoff) => {
                        self.history.remove_since(cutoff);
                    }
                }
            }
            self.history.merge(since.entries().to_vec());
        }
        if self.unsaved > 0 {
            self.flush()?;
        }
        Ok(())
    }

    fn loaded_or_empty(loaded: Result<Vec<Entry>, PersistError>) -> Vec<Entry> {
        loaded.unwrap_or_else(|e| {
            crate::report!("{e:?}");
            Vec::new()
        })
    }

    /// In-memory only — records and matches but never persists. The test
    /// seam; incognito doesn't construct a `Recorder` at all.
    pub fn session_only() -> Self {
        Self {
            history: History::default(),
            store: Box::new(Memory),
            writer: None,
            unsaved: 0,
            loaded: Loaded::Done,
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
        // Not until the file is in: `unsaved` keeps counting, and the
        // write happens from `hydrate`.
        if matches!(self.loaded, Loaded::Pending { .. }) {
            return Ok(());
        }
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
        if let Loaded::Pending { removals, .. } = &mut self.loaded {
            removals.push(Removal::Text(text.to_string()));
            self.unsaved += 1;
        }
        if self.history.remove(text) {
            self.flush()?;
        }
        Ok(())
    }

    /// Forget everything and flush — same reasoning as `remove`.
    pub fn clear(&mut self) -> Result<(), PersistError> {
        if let Loaded::Pending { cleared, .. } = &mut self.loaded {
            *cleared = true;
            self.unsaved += 1;
        }
        self.history.clear();
        self.flush()
    }

    /// Forget everything last seen at or after `cutoff` and flush —
    /// same reasoning as `remove`.
    pub fn remove_since(&mut self, cutoff: u64) -> Result<usize, PersistError> {
        if let Loaded::Pending { removals, .. } = &mut self.loaded {
            removals.push(Removal::Since(cutoff));
            self.unsaved += 1;
        }
        let forgotten = self.history.remove_since(cutoff);
        if forgotten > 0 {
            self.flush()?;
        }
        Ok(forgotten)
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

    #[test]
    fn a_pending_recorder_writes_nothing_until_the_file_is_in() {
        let dir = std::env::temp_dir().join(format!("cryo-test-pend-{}", std::process::id()));
        let path = dir.join("history.json");
        Recorder::open(JsonFile::new(path.clone()))
            .import(vec![Entry::new("https://old", Some("Old"), 3, 7).unwrap()])
            .unwrap();
        let mut rec = Recorder::pending(JsonFile::new(path.clone()));
        rec.record("https://new", None, 50).unwrap();
        rec.clear().unwrap();
        // The clear reached memory and was *not* written over the file:
        // reopening still finds the old row.
        assert!(rec.matches("old", 5).is_empty());
        assert_eq!(
            Recorder::open(JsonFile::new(path.clone())).entries().len(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hydration_keeps_the_file_and_what_was_recorded_meanwhile() {
        let dir = std::env::temp_dir().join(format!("cryo-test-hyd-{}", std::process::id()));
        let path = dir.join("history.json");
        let file = JsonFile::new(path.clone());
        let mut rec = Recorder::pending(JsonFile::new(path.clone()));
        rec.record("https://new", Some("New"), 50).unwrap();
        rec.hydrate(Ok(vec![
            Entry::new("https://old", Some("Old"), 3, 7).unwrap(),
            Entry::new("https://gone", None, 1, 8).unwrap(),
        ]))
        .unwrap();
        assert_eq!(rec.matches("old", 5)[0].uses, 3);
        assert_eq!(rec.matches("new", 5)[0].last_seen, 50);
        // The waiting batch went out with the file's rows in it.
        drop(rec);
        assert_eq!(Recorder::open(file).entries().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_before_hydration_is_replayed_over_the_file() {
        let mut rec = Recorder::pending(Memory);
        rec.remove("https://gone").unwrap();
        rec.remove_since(100).unwrap();
        rec.hydrate(Ok(vec![
            Entry::new("https://gone", None, 1, 8).unwrap(),
            Entry::new("https://recent", None, 1, 150).unwrap(),
            Entry::new("https://kept", None, 1, 9).unwrap(),
        ]))
        .unwrap();
        let kept: Vec<_> = rec.entries().iter().map(|e| e.text.as_str()).collect();
        assert_eq!(kept, ["https://kept"]);
        // A cleared session drops the file outright.
        let mut rec = Recorder::pending(Memory);
        rec.clear().unwrap();
        rec.hydrate(Ok(vec![Entry::new("https://old", None, 1, 1).unwrap()]))
            .unwrap();
        assert!(rec.entries().is_empty());
    }
}
