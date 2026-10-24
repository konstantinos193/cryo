//! File adapter: `History` entries as one versioned JSON document.
//!
//! The wire DTOs live here and only here — `Entry` never derives serde
//! (AGENTS §6). `version` rides in the file so a future schema upgrades
//! at this boundary before domain objects ever see it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Entry, PersistError, Store, CURRENT_VERSION};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryFile {
    version: u32,
    entries: Vec<EntryFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFile {
    text: String,
    title: Option<String>,
    uses: u32,
    last_seen: u64,
    /// Added after v1 shipped — `default` keeps older files loadable
    /// without a version bump; absent means no icon was ever seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    favicon: Option<String>,
}

impl From<&Entry> for EntryFile {
    fn from(e: &Entry) -> Self {
        Self {
            text: e.text.clone(),
            title: e.title.clone(),
            uses: e.uses,
            last_seen: e.last_seen,
            favicon: e.favicon.clone(),
        }
    }
}

/// History as one JSON file — the store the real build uses.
pub struct JsonFile {
    path: PathBuf,
}

impl JsonFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `history.json` under the per-user data dir (`crate::data_dir`) —
    /// the same in a dev `target/debug` build and an installed one.
    pub fn default_path() -> PathBuf {
        crate::data_dir().join("history.json")
    }

    fn read_err(&self, source: std::io::Error) -> PersistError {
        PersistError::Read {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn write_err(&self, source: std::io::Error) -> PersistError {
        PersistError::Write {
            path: self.path.display().to_string(),
            source,
        }
    }
}

impl Store for JsonFile {
    fn load(&self) -> Result<Vec<Entry>, PersistError> {
        // A missing file is a fresh start, not an error.
        let Some(bytes) = crate::read_file(&self.path).map_err(|e| self.read_err(e))? else {
            return Ok(Vec::new());
        };
        let file: HistoryFile = serde_json::from_slice(&bytes).map_err(PersistError::Parse)?;
        if file.version != CURRENT_VERSION {
            return Err(PersistError::Version(file.version));
        }
        // Hydration goes through the domain's one door; a row that no
        // longer validates (a hand-edited file's empty `text`, say) is
        // dropped rather than failing the whole file.
        Ok(file
            .entries
            .into_iter()
            .filter_map(|e| {
                Entry::hydrate(
                    &e.text,
                    e.title.as_deref(),
                    e.uses,
                    e.last_seen,
                    e.favicon.as_deref(),
                )
                .ok()
            })
            .collect())
    }

    fn save(&self, entries: &[Entry]) -> Result<(), PersistError> {
        let file = HistoryFile {
            version: CURRENT_VERSION,
            entries: entries.iter().map(EntryFile::from).collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(PersistError::Parse)?;
        crate::write_file_atomic(&self.path, &bytes).map_err(|e| self.write_err(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        crate::test_dir(&format!("histfile-{tag}")).join("history.json")
    }

    #[test]
    fn missing_file_loads_empty() {
        let store = JsonFile::new(temp("missing"));
        assert_eq!(store.load().unwrap(), Vec::<Entry>::new());
    }

    #[test]
    fn wrong_version_is_an_error_not_a_guess() {
        let path = temp("ver");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":99,"entries":[]}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(PersistError::Version(99))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let path = temp("unk");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":1,"entries":[],"mystery":1}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(PersistError::Parse(_))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_stored_row_the_domain_refuses_is_dropped_not_fatal() {
        let path = temp("refused");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"version":1,"entries":[
                 {"text":"https://a.test","title":null,"uses":2,"last_seen":9},
                 {"text":"  ","title":null,"uses":5,"last_seen":9}
               ]}"#,
        )
        .unwrap();
        let rows = JsonFile::new(path.clone()).load().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "https://a.test");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
