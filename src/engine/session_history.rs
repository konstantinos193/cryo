//! A view's back/forward list as the engine reports it: where the view is
//! and what lies either side. Not the `persist` history — this one is one
//! surface's trail, and it dies with the surface.

use super::EngineError;

/// The engine's name for one entry. Only meaningful to the view that
/// listed it — another tab's list can reuse the number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEntryId(i64);

impl SessionEntryId {
    pub fn from_raw(raw: i64) -> Self {
        Self(raw)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// One stop on the trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: SessionEntryId,
    pub url: String,
    pub title: String,
}

/// The whole trail, oldest first, with a position that is always on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHistory {
    entries: Vec<SessionEntry>,
    current: usize,
}

impl SessionHistory {
    pub fn new(entries: Vec<SessionEntry>, current: usize) -> Result<Self, EngineError> {
        if current >= entries.len() {
            return Err(EngineError::View(format!(
                "Session history position {current} is outside its {} entries",
                entries.len()
            )));
        }
        Ok(Self { entries, current })
    }

    /// What Back steps through, nearest first.
    pub fn behind(&self) -> impl Iterator<Item = &SessionEntry> {
        self.entries[..self.current].iter().rev()
    }

    /// What Forward steps through, nearest first.
    pub fn ahead(&self) -> impl Iterator<Item = &SessionEntry> {
        self.entries[self.current + 1..].iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trail(urls: &[&str], current: usize) -> Result<SessionHistory, EngineError> {
        let entries = urls
            .iter()
            .enumerate()
            .map(|(i, url)| SessionEntry {
                id: SessionEntryId::from_raw(i as i64),
                url: (*url).to_string(),
                title: String::new(),
            })
            .collect();
        SessionHistory::new(entries, current)
    }

    fn urls<'a>(entries: impl Iterator<Item = &'a SessionEntry>) -> Vec<&'a str> {
        entries.map(|e| e.url.as_str()).collect()
    }

    #[test]
    fn both_directions_read_nearest_first_and_skip_the_current_page() {
        let history = trail(&["a", "b", "c", "d"], 2).unwrap();
        assert_eq!(urls(history.behind()), ["b", "a"]);
        assert_eq!(urls(history.ahead()), ["d"]);
    }

    #[test]
    fn the_ends_of_the_trail_have_nothing_beyond_them() {
        let first = trail(&["a", "b"], 0).unwrap();
        assert_eq!(first.behind().count(), 0);
        let last = trail(&["a", "b"], 1).unwrap();
        assert_eq!(last.ahead().count(), 0);
    }

    #[test]
    fn a_position_off_the_trail_is_refused() {
        assert!(matches!(trail(&["a"], 1), Err(EngineError::View(_))));
        assert!(matches!(trail(&[], 0), Err(EngineError::View(_))));
    }
}
