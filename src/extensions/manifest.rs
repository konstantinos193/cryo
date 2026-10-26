//! `manifest.json` at the boundary. Foreign bytes written by someone else
//! come in here and leave as validated metadata; nothing downstream sees
//! this shape (AGENTS §6).
//!
//! Deliberately **not** `deny_unknown_fields`. A real extension manifest
//! carries dozens of keys cryo has no opinion about — permissions, service
//! workers, content scripts, locale keys — and the engine is the thing that
//! reads them. Refusing a manifest for keys we do not model would reject
//! every genuine extension. The fields below are the only ones cryo claims
//! to understand, and they are the only ones it reads.

use std::path::Path;

use serde::Deserialize;

use super::ExtensionError;

/// The handful of manifest keys cryo displays. `default_locale` is read only
/// to explain a name it cannot resolve, not to resolve it: message
/// substitution is the engine's job, and guessing at it would put a wrong
/// name next to a real extension.
#[derive(Debug, Deserialize)]
pub struct ManifestFile {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub manifest_version: u32,
    #[serde(default)]
    pub default_locale: Option<String>,
}

impl ManifestFile {
    /// What to show for this extension. A localized manifest writes
    /// `__MSG_name__` and keeps the real string in `_locales`; rather than
    /// half-resolving that, the raw placeholder is replaced with something
    /// honest the user can act on.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.starts_with("__MSG_") && name.ends_with("__") {
            return match &self.default_locale {
                Some(locale) => format!("Localized extension ({locale})"),
                None => "Localized extension".to_string(),
            };
        }
        if name.is_empty() {
            "Unnamed extension".to_string()
        } else {
            name.to_string()
        }
    }
}

/// Read the manifest at the top level of an unpacked folder. A missing file
/// and an unreadable one are different answers: the first is "that folder is
/// not an extension", which is what a user who picked the wrong directory
/// needs to hear.
pub fn read(dir: &Path) -> Result<ManifestFile, ExtensionError> {
    let path = dir.join("manifest.json");
    if !path.is_file() {
        return Err(ExtensionError::NoManifest);
    }
    let bytes = std::fs::read(&path).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    let file: ManifestFile =
        serde_json::from_slice(&bytes).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    if file.manifest_version != 2 && file.manifest_version != 3 {
        return Err(ExtensionError::Manifest(format!(
            "manifest_version is {}, and only 2 and 3 exist",
            file.manifest_version
        )));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cryo-manifest-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), body).unwrap();
        dir
    }

    #[test]
    fn the_keys_cryo_reads_survive_a_manifest_full_of_ones_it_does_not() {
        let dir = written(
            "rich",
            r#"{
              "name": "Tab Tidier", "version": "2.0", "manifest_version": 3,
              "permissions": ["tabs", "storage"],
              "background": {"service_worker": "sw.js"},
              "content_scripts": [{"matches": ["<all_urls>"], "js": ["c.js"]}],
              "some_key_from_2031": {"nested": true}
            }"#,
        );
        let file = read(&dir).expect("unknown keys are not an error");
        assert_eq!(file.display_name(), "Tab Tidier");
        assert_eq!(file.version, "2.0");
        assert_eq!(file.manifest_version, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_manifest_says_so_rather_than_reporting_a_parse_failure() {
        let dir = std::env::temp_dir().join("cryo-manifest-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(read(&dir), Err(ExtensionError::NoManifest)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_version_that_does_not_exist_is_refused() {
        for body in [
            r#"{"name":"X","version":"1","manifest_version":1}"#,
            r#"{"name":"X","version":"1","manifest_version":4}"#,
            r#"{"name":"X","version":"1"}"#,
        ] {
            let dir = written("badver", body);
            assert!(matches!(read(&dir), Err(ExtensionError::Manifest(_))));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn truncated_json_is_a_manifest_error_not_a_missing_file() {
        let dir = written("trunc", "{\"name\": \"X\"");
        assert!(matches!(read(&dir), Err(ExtensionError::Manifest(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_localized_name_is_labelled_instead_of_shown_as_its_placeholder() {
        let localized = ManifestFile {
            name: "__MSG_extName__".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: Some("en".into()),
        };
        assert_eq!(localized.display_name(), "Localized extension (en)");
        let no_locale = ManifestFile {
            name: "__MSG_extName__".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: None,
        };
        assert_eq!(no_locale.display_name(), "Localized extension");
        let blank = ManifestFile {
            name: "   ".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: None,
        };
        assert_eq!(blank.display_name(), "Unnamed extension");
    }
}
