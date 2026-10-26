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
    /// What to show for this extension.
    ///
    /// A localized manifest writes `__MSG_appName__` and keeps the real
    /// string in `_locales/<default_locale>/messages.json`. Most extensions
    /// worth installing are localized — MetaMask among them — so a label
    /// reading "Localized extension" next to every one of them would be
    /// useless. The message file is read from the extension's own folder;
    /// if it is missing or says nothing, the honest label is still there.
    pub fn display_name(&self, dir: &Path) -> String {
        let name = self.name.trim();
        if let Some(key) = name
            .strip_prefix("__MSG_")
            .and_then(|rest| rest.strip_suffix("__"))
        {
            if let Some(resolved) = self.message(dir, key) {
                return resolved;
            }
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

    /// One message out of the default locale's `messages.json`. Chrome
    /// matches these keys without regard to case, so this does too.
    ///
    /// The file belongs to the extension, so it is untrusted input: a
    /// missing file, bad JSON or a message that is not a string all return
    /// `None` rather than failing the install, and the result is trimmed and
    /// length-capped so a manifest cannot push a wall of text into the pane.
    fn message(&self, dir: &Path, key: &str) -> Option<String> {
        let locale = self.default_locale.as_deref()?;
        // The locale names a directory, so it may not travel out of one.
        if locale.is_empty()
            || !locale
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return None;
        }
        let path = dir.join("_locales").join(locale).join("messages.json");
        let bytes = std::fs::read(path).ok()?;
        let messages: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let wanted = key.to_lowercase();
        let (_, entry) = messages
            .as_object()?
            .iter()
            .find(|(name, _)| name.to_lowercase() == wanted)?;
        let message = entry.get("message")?.as_str()?.trim();
        if message.is_empty() {
            return None;
        }
        Some(message.chars().take(120).collect())
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
        assert_eq!(file.display_name(&dir), "Tab Tidier");
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
    fn a_localized_name_comes_out_of_the_locale_file() {
        // What MetaMask actually ships: the manifest names a message and the
        // real name lives in _locales.
        let dir = written(
            "locale",
            r#"{"name":"__MSG_appName__","version":"1","manifest_version":3,"default_locale":"en"}"#,
        );
        let locale = dir.join("_locales").join("en");
        std::fs::create_dir_all(&locale).unwrap();
        std::fs::write(
            locale.join("messages.json"),
            r#"{"appName":{"message":"MetaMask","description":"The name"}}"#,
        )
        .unwrap();
        let file = read(&dir).expect("reads");
        assert_eq!(file.display_name(&dir), "MetaMask");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn message_keys_match_without_regard_to_case() {
        let dir = written(
            "case",
            r#"{"name":"__MSG_APPNAME__","version":"1","manifest_version":3,"default_locale":"en"}"#,
        );
        let locale = dir.join("_locales").join("en");
        std::fs::create_dir_all(&locale).unwrap();
        std::fs::write(
            locale.join("messages.json"),
            r#"{"appName":{"message":"Tidier"}}"#,
        )
        .unwrap();
        assert_eq!(read(&dir).unwrap().display_name(&dir), "Tidier");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unresolvable_localized_name_falls_back_to_an_honest_label() {
        let dir = std::env::temp_dir().join("cryo-manifest-nolocale");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No _locales directory at all.
        let localized = ManifestFile {
            name: "__MSG_extName__".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: Some("en".into()),
        };
        assert_eq!(localized.display_name(&dir), "Localized extension (en)");
        let no_locale = ManifestFile {
            name: "__MSG_extName__".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: None,
        };
        assert_eq!(no_locale.display_name(&dir), "Localized extension");
        let blank = ManifestFile {
            name: "   ".into(),
            version: "1".into(),
            manifest_version: 3,
            default_locale: None,
        };
        assert_eq!(blank.display_name(&dir), "Unnamed extension");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locale_that_would_leave_the_folder_is_not_read() {
        let dir = std::env::temp_dir().join("cryo-manifest-badlocale");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for locale in ["../../../etc", "..", "en/../..", ""] {
            let file = ManifestFile {
                name: "__MSG_extName__".into(),
                version: "1".into(),
                manifest_version: 3,
                default_locale: Some(locale.into()),
            };
            assert!(file.message(&dir, "extName").is_none(), "{locale:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_message_file_that_is_not_usable_is_ignored_rather_than_fatal() {
        for body in [
            "{",
            r#"{"appName":{"description":"no message key"}}"#,
            r#"{"appName":{"message":42}}"#,
            r#"{"appName":{"message":"   "}}"#,
            r#"[]"#,
        ] {
            let dir = written(
                "badmsg",
                r#"{"name":"__MSG_appName__","version":"1","manifest_version":3,"default_locale":"en"}"#,
            );
            let locale = dir.join("_locales").join("en");
            std::fs::create_dir_all(&locale).unwrap();
            std::fs::write(locale.join("messages.json"), body).unwrap();
            assert_eq!(
                read(&dir).unwrap().display_name(&dir),
                "Localized extension (en)",
                "{body}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_very_long_name_is_capped_before_it_reaches_the_pane() {
        let dir = written(
            "long",
            r#"{"name":"__MSG_appName__","version":"1","manifest_version":3,"default_locale":"en"}"#,
        );
        let locale = dir.join("_locales").join("en");
        std::fs::create_dir_all(&locale).unwrap();
        let long = "x".repeat(500);
        std::fs::write(
            locale.join("messages.json"),
            format!(r#"{{"appName":{{"message":"{long}"}}}}"#),
        )
        .unwrap();
        assert_eq!(read(&dir).unwrap().display_name(&dir).chars().count(), 120);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
