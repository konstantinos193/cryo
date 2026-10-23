//! In-memory adapter: the test seam. `Recorder::session_only` uses it;
//! incognito builds no `Recorder` at all — forgetting is its point.

use super::{Entry, PersistError, Store};

/// A store that keeps nothing: load is empty, save is a no-op.
pub struct Memory;

impl Store for Memory {
    fn load(&self) -> Result<Vec<Entry>, PersistError> {
        Ok(Vec::new())
    }

    fn save(&self, _entries: &[Entry]) -> Result<(), PersistError> {
        Ok(())
    }
}
