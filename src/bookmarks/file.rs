//! File adapter: the registry as one versioned JSON document.
//!
//! The wire DTOs live here and only here — `Bookmark` never derives
//! serde (AGENTS §6). `version` rides in the file, so a future schema
//! upgrades at this boundary and domain code never sees an old format.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Bookmark, BookmarkError, BookmarkId, Store, CURRENT_VERSION};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookmarkFile {
    version: u32,
    rows: Vec<RowFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowFile {
    id: u64,
    title: String,
    url: String,
    folder: String,
    added_at: u64,
}

impl From<&Bookmark> for RowFile {
    fn from(b: &Bookmark) -> Self {
        Self {
            id: b.id().get(),
            title: b.title().to_string(),
            url: b.url().to_string(),
            folder: b.folder().to_string(),
            added_at: b.added_at(),
        }
    }
}

/// The registry as one JSON file — the store the real build uses.
pub struct JsonFile {
    path: PathBuf,
}

impl JsonFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `bookmarks.json` under the per-user data dir (`crate::data_dir`),
    /// beside the history file — same in a dev build and an installed one.
    pub fn default_path() -> PathBuf {
        crate::data_dir().join("bookmarks.json")
    }

    fn read_err(&self, source: std::io::Error) -> BookmarkError {
        BookmarkError::Read {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn write_err(&self, source: std::io::Error) -> BookmarkError {
        BookmarkError::Write {
            path: self.path.display().to_string(),
            source,
        }
    }
}

impl Store for JsonFile {
    fn load(&self) -> Result<Vec<Bookmark>, BookmarkError> {
        // A missing file is a fresh profile, not an error.
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.read_err(e)),
        };
        let file: BookmarkFile = serde_json::from_slice(&bytes).map_err(BookmarkError::Parse)?;
        if file.version != CURRENT_VERSION {
            return Err(BookmarkError::Version(file.version));
        }
        // Hydration goes through the domain's one door; a row that no
        // longer validates (a scheme this build refuses, say) is dropped
        // rather than failing the whole file.
        Ok(file
            .rows
            .into_iter()
            .filter_map(|r| {
                Bookmark::hydrate(
                    BookmarkId::from_raw(r.id),
                    &r.title,
                    &r.url,
                    &r.folder,
                    r.added_at,
                )
                .ok()
            })
            .collect())
    }

    fn save(&self, rows: &[Bookmark]) -> Result<(), BookmarkError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| self.write_err(e))?;
        }
        let file = BookmarkFile {
            version: CURRENT_VERSION,
            rows: rows.iter().map(RowFile::from).collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(BookmarkError::Parse)?;
        // Write-then-rename: a crash mid-write leaves the old file whole.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| self.write_err(e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| self.write_err(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("cryo-bmfile-{tag}-{}", std::process::id()))
            .join("bookmarks.json")
    }

    #[test]
    fn missing_file_loads_empty() {
        let store = JsonFile::new(temp("missing"));
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn wrong_version_is_an_error_not_a_guess() {
        let path = temp("ver");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":99,"rows":[]}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(BookmarkError::Version(99))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let path = temp("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":1,"rows":[],"mystery":1}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(BookmarkError::Parse(_))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_stored_row_the_domain_refuses_is_dropped_not_fatal() {
        let path = temp("refused");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":1,"rows":[
                 {"id":1,"title":"ok","url":"https://a.test","folder":"","added_at":1},
                 {"id":2,"title":"bad","url":"javascript:alert(1)","folder":"","added_at":1}
               ]}"#,
        )
        .unwrap();
        let rows = JsonFile::new(path.clone()).load().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url(), "https://a.test");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_then_load_round_trips_every_field() {
        let path = temp("round");
        let store = JsonFile::new(path.clone());
        let row =
            Bookmark::hydrate(BookmarkId::from_raw(4), "A", "https://a.test", "News", 42).unwrap();
        store.save(std::slice::from_ref(&row)).unwrap();
        let back = store.load().unwrap();
        assert_eq!(back, vec![row]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn default_path_lands_beside_the_history_file() {
        let path = JsonFile::default_path();
        assert_eq!(path.file_name().unwrap(), "bookmarks.json");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "cryo");
    }
}
