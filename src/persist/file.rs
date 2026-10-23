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

impl From<EntryFile> for Entry {
    fn from(e: EntryFile) -> Self {
        Self {
            text: e.text,
            title: e.title,
            uses: e.uses,
            last_seen: e.last_seen,
            favicon: e.favicon,
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
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.read_err(e)),
        };
        let file: HistoryFile = serde_json::from_slice(&bytes).map_err(PersistError::Parse)?;
        if file.version != CURRENT_VERSION {
            return Err(PersistError::Version(file.version));
        }
        Ok(file.entries.into_iter().map(Entry::from).collect())
    }

    fn save(&self, entries: &[Entry]) -> Result<(), PersistError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| self.write_err(e))?;
        }
        let file = HistoryFile {
            version: CURRENT_VERSION,
            entries: entries.iter().map(EntryFile::from).collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(PersistError::Parse)?;
        // Write-then-rename: a crash mid-write leaves the old file intact.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| self.write_err(e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| self.write_err(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_empty() {
        let path = std::env::temp_dir()
            .join(format!("cryo-test-missing-{}", std::process::id()))
            .join("history.json");
        let store = JsonFile::new(path);
        assert_eq!(store.load().unwrap(), Vec::<Entry>::new());
    }

    #[test]
    fn wrong_version_is_an_error_not_a_guess() {
        let dir = std::env::temp_dir().join(format!("cryo-test-ver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        std::fs::write(&path, r#"{"version":99,"entries":[]}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(PersistError::Version(99))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let dir = std::env::temp_dir().join(format!("cryo-test-unk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        std::fs::write(&path, r#"{"version":1,"entries":[],"mystery":1}"#).unwrap();
        let store = JsonFile::new(path.clone());
        assert!(matches!(store.load(), Err(PersistError::Parse(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
