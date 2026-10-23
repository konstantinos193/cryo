//! In-memory adapter: the test seam. `Keeper::session_only` uses it, and
//! so does an incognito window — a bookmark starred there lasts as long
//! as the window does and no longer.

use super::{Bookmark, BookmarkError, Store};

/// A store that keeps nothing: load is empty, save is a no-op.
pub struct Memory;

impl Store for Memory {
    fn load(&self) -> Result<Vec<Bookmark>, BookmarkError> {
        Ok(Vec::new())
    }

    fn save(&self, _rows: &[Bookmark]) -> Result<(), BookmarkError> {
        Ok(())
    }
}
