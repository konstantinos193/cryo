//! Browser extensions: the unpacked extension folders cryo hands the engine.
//!
//! This context owns the catalogue — what is installed, where it sits on
//! disk, and what its manifest says it is. It does not load anything: the
//! engine does that, and `engine::Engine` declares the port. `Extension` is
//! built in exactly one place (`Extension::hydrate`), so a folder without a
//! readable manifest never becomes one (AGENTS §6).
//!
//! The on-disk layout is one directory holding one subdirectory per
//! extension, which is exactly the shape the engine adapter loads. Nothing
//! here edits an installed folder after it is written: WebView2 drops an
//! extension whose files change under it, so an install copies once and then
//! leaves the copy alone.

mod manifest;
pub mod web_store;

use std::path::{Path, PathBuf};

pub use manifest::ManifestFile;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtensionError {
    #[error("the extensions directory could not be read: {0}")]
    List(String),
    #[error(
        "no manifest.json in the chosen folder — an unpacked extension has one at its top level"
    )]
    NoManifest,
    #[error("manifest.json is not readable extension metadata: {0}")]
    Manifest(String),
    #[error("an extension named {0} is already installed")]
    AlreadyInstalled(String),
    #[error("the extension could not be copied in: {0}")]
    Install(String),
    #[error("the extension could not be removed: {0}")]
    Remove(String),
    #[error("{0} is not an installed extension")]
    Unknown(String),
    #[error("{0:?} is not a Chrome extension id")]
    Id(String),
}

/// One installed extension. A value object: equal by contents, replaced
/// wholesale, never mutated in place — the folder on disk is the truth and
/// this is a reading of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension {
    slug: String,
    name: String,
    version: String,
    manifest_version: u32,
    dir: PathBuf,
}

impl Extension {
    /// The one door in. A folder becomes an `Extension` only by having a
    /// manifest that parses and names itself; everything else is refused
    /// rather than shown as a half-read row.
    pub fn hydrate(dir: &Path, slug: String) -> Result<Self, ExtensionError> {
        let file = manifest::read(dir)?;
        Ok(Self {
            slug,
            name: file.display_name(),
            version: file.version,
            manifest_version: file.manifest_version,
            dir: dir.to_path_buf(),
        })
    }

    /// The installed folder's name, which is also the handle the chrome
    /// document sends back. Minted by `install` from the manifest name, so
    /// it is stable for as long as the folder is.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// 2 or 3. Reported rather than enforced: which one the engine honours
    /// is the engine's business, and saying so beats guessing for the user.
    pub fn manifest_version(&self) -> u32 {
        self.manifest_version
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// The installed set, rooted at one directory. Holds no state of its own —
/// every answer is read from disk, because the engine reads the same folders
/// and a cached list would be a second truth (AGENTS §5).
pub struct Catalogue {
    root: PathBuf,
}

impl Catalogue {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Where the engine adapter is pointed. It may not exist yet — nothing
    /// creates it until something is installed.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Everything installed, by name. A subdirectory whose manifest will not
    /// hydrate is skipped rather than failing the whole list: one bad folder
    /// must not hide the others, and the engine skips it too.
    pub fn list(&self) -> Result<Vec<Extension>, ExtensionError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries =
            std::fs::read_dir(&self.root).map_err(|e| ExtensionError::List(e.to_string()))?;
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| ExtensionError::List(e.to_string()))?;
            if !entry.path().is_dir() {
                continue;
            }
            let slug = entry.file_name().to_string_lossy().to_string();
            if let Ok(extension) = Extension::hydrate(&entry.path(), slug) {
                found.push(extension);
            }
        }
        found.sort_by_key(|e| e.name().to_lowercase());
        Ok(found)
    }

    /// Copy an unpacked folder in. The source is validated before anything is
    /// written, so a rejected pick leaves no partial install behind, and the
    /// copy is never touched again — WebView2 drops an extension whose files
    /// change under it.
    pub fn install(&self, source: &Path) -> Result<Extension, ExtensionError> {
        let file = manifest::read(source)?;
        let slug = slugify(&file.display_name());
        let target = self.root.join(&slug);
        if target.exists() {
            return Err(ExtensionError::AlreadyInstalled(file.display_name()));
        }
        std::fs::create_dir_all(&self.root).map_err(|e| ExtensionError::Install(e.to_string()))?;
        copy_tree(source, &target).map_err(|e| {
            // A half-written copy is not an extension. Take it back out
            // rather than leaving a folder that lists as broken.
            let _ = std::fs::remove_dir_all(&target);
            ExtensionError::Install(e.to_string())
        })?;
        Extension::hydrate(&target, slug)
    }

    /// Remove an installed extension by its slug. The slug is resolved
    /// against the listing rather than joined onto the root, so a crafted
    /// handle from the chrome document cannot address a path outside it.
    pub fn remove(&self, slug: &str) -> Result<(), ExtensionError> {
        let found = self
            .list()?
            .into_iter()
            .find(|e| e.slug() == slug)
            .ok_or_else(|| ExtensionError::Unknown(slug.to_string()))?;
        std::fs::remove_dir_all(found.dir()).map_err(|e| ExtensionError::Remove(e.to_string()))
    }
}

/// A folder name from a display name: lowercase, ASCII word characters and
/// dashes, nothing that can traverse or collide with a device name. Empty
/// input and names that reduce to nothing both fall back, so a slug is
/// always a usable single path segment.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if (ch == ' ' || ch == '-' || ch == '_') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "extension".to_string()
    } else {
        slug
    }
}

/// Recursive directory copy. Symlinks are not followed — an unpacked
/// extension is a tree of files, and following links out of it would copy
/// something the user did not pick.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cryo-ext-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        dir
    }

    fn unpacked(dir: &Path, name: &str, manifest_version: u32) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"name":"{name}","version":"1.2.3","manifest_version":{manifest_version}}}"#
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        std::fs::write(dir.join("icons").join("16.png"), b"not really a png").unwrap();
    }

    #[test]
    fn an_unpacked_folder_is_copied_in_whole_and_then_lists() {
        let root = temp_root("install");
        let source = root.join("source");
        unpacked(&source, "Tab Tidier", 3);
        let catalogue = Catalogue::new(root.join("extensions"));

        let installed = catalogue.install(&source).expect("installs");
        assert_eq!(installed.name(), "Tab Tidier");
        assert_eq!(installed.version(), "1.2.3");
        assert_eq!(installed.manifest_version(), 3);
        assert_eq!(installed.slug(), "tab-tidier");
        // The whole tree travels, not just the manifest.
        assert!(installed.dir().join("icons").join("16.png").exists());

        let listed = catalogue.list().expect("lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], installed);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_without_a_manifest_is_refused_and_writes_nothing() {
        let root = temp_root("nomanifest");
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("readme.txt"), b"not an extension").unwrap();
        let catalogue = Catalogue::new(root.join("extensions"));

        assert!(matches!(
            catalogue.install(&source),
            Err(ExtensionError::NoManifest)
        ));
        // Validation happens before the directory is created, so a refused
        // pick leaves no trace at all.
        assert!(!catalogue.root().exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installing_the_same_extension_twice_is_refused_rather_than_merged() {
        let root = temp_root("twice");
        let source = root.join("source");
        unpacked(&source, "Tab Tidier", 3);
        let catalogue = Catalogue::new(root.join("extensions"));

        catalogue.install(&source).expect("first install");
        assert!(matches!(
            catalogue.install(&source),
            Err(ExtensionError::AlreadyInstalled(name)) if name == "Tab Tidier"
        ));
        assert_eq!(catalogue.list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_will_not_hydrate_is_skipped_instead_of_failing_the_list() {
        let root = temp_root("broken");
        let catalogue = Catalogue::new(root.join("extensions"));
        let good = root.join("source");
        unpacked(&good, "Good One", 3);
        catalogue.install(&good).unwrap();
        // A folder someone dropped in by hand, with nothing cryo can read.
        std::fs::create_dir_all(catalogue.root().join("junk")).unwrap();
        std::fs::write(catalogue.root().join("junk").join("manifest.json"), b"{").unwrap();

        let listed = catalogue.list().expect("still lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name(), "Good One");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removal_resolves_the_slug_against_the_listing_not_the_path() {
        let root = temp_root("remove");
        let source = root.join("source");
        unpacked(&source, "Tab Tidier", 3);
        let catalogue = Catalogue::new(root.join("extensions"));
        catalogue.install(&source).unwrap();

        // A handle that would traverse out of the root if it were joined on.
        for crafted in ["..", "../source", "..\\source", "nope"] {
            assert!(matches!(
                catalogue.remove(crafted),
                Err(ExtensionError::Unknown(_))
            ));
        }
        assert!(source.exists());

        catalogue.remove("tab-tidier").expect("removes");
        assert!(catalogue.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_directory_lists_empty_rather_than_erroring() {
        let catalogue = Catalogue::new(std::env::temp_dir().join("cryo-ext-does-not-exist"));
        assert!(catalogue.list().expect("empty list").is_empty());
    }

    #[test]
    fn slugs_stay_one_safe_path_segment() {
        assert_eq!(slugify("Tab Tidier"), "tab-tidier");
        assert_eq!(slugify("uBlock Origin"), "ublock-origin");
        assert_eq!(slugify("../../etc/passwd"), "etcpasswd");
        assert_eq!(slugify("C:\\Windows"), "cwindows");
        assert_eq!(slugify("...."), "extension");
        assert_eq!(slugify(""), "extension");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
    }
}
