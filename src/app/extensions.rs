//! The extensions pane's app side: list what the catalogue holds, add a
//! folder the user picks, remove one by its slug.
//!
//! Loading is the engine's job and happens when a content view is built, so
//! nothing here installs into a running view. An install or a removal
//! therefore reaches open tabs only when they next build — which the pane
//! says, instead of leaving someone to wonder why nothing changed.

use crate::shell::chrome::ChromeExtension;

use super::App;

impl App {
    /// Whether extensions can be loaded here at all. `None` covers both
    /// reasons — a platform whose engine has no Chrome-extension support,
    /// and an incognito window, which deliberately loads none.
    pub(super) fn extensions_supported(&self) -> bool {
        self.extensions.is_some()
    }

    /// The pane's rows. A catalogue that cannot be read lists as empty and
    /// reports the failure rather than taking the panel down with it: the
    /// directory is the user's, and someone else may have been in it.
    pub(super) fn extension_rows(&self) -> Vec<ChromeExtension> {
        let Some(catalogue) = &self.extensions else {
            return Vec::new();
        };
        match catalogue.list() {
            Ok(found) => found
                .into_iter()
                .map(|e| ChromeExtension {
                    slug: e.slug().to_string(),
                    name: e.name().to_string(),
                    version: e.version().to_string(),
                    manifest_version: e.manifest_version(),
                    dir: e.dir().display().to_string(),
                })
                .collect(),
            Err(e) => {
                crate::report!("{e:?}");
                Vec::new()
            }
        }
    }

    /// Add an unpacked extension. The folder picker is an OS dialog, so the
    /// person standing at the machine chooses what gets copied in — cryo
    /// never reads a path the document supplied.
    pub(super) fn add_extension(&mut self) -> anyhow::Result<()> {
        let Some(catalogue) = &self.extensions else {
            return self.notice(
                false,
                "Extensions are not available here",
                vec![extensions_unsupported_reason(self.incognito)],
            );
        };
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose an unpacked extension folder")
            .pick_folder()
        else {
            return Ok(());
        };
        match catalogue.install(&folder) {
            Ok(installed) => {
                let name = format!("{} {}", installed.name(), installed.version());
                self.notice(true,
                    "Extension added",
                    vec![
                        name,
                        "It loads into tabs opened from now on. Do not edit or move the copied folder — WebView2 drops an extension whose files change."
                            .to_string(),
                    ],
                )?;
                self.push_library()
            }
            Err(e) => self.notice(false, "That folder was not added", vec![e.to_string()]),
        }
    }

    /// Remove an installed extension. The slug is resolved against the
    /// catalogue's own listing, so a handle from the document can only ever
    /// name something that is actually installed.
    pub(super) fn remove_extension(&mut self, slug: &str) -> anyhow::Result<()> {
        let Some(catalogue) = &self.extensions else {
            return Ok(());
        };
        match catalogue.remove(slug) {
            Ok(()) => {
                self.notice(
                    true,
                    "Extension removed",
                    vec!["Tabs already open keep it until they reload.".to_string()],
                )?;
                self.push_library()
            }
            Err(e) => self.notice(false, "That extension was not removed", vec![e.to_string()]),
        }
    }
}

/// Why extensions are off, in the words that fit the reason. Incognito is a
/// deliberate choice and says so; anything else is the platform.
fn extensions_unsupported_reason(incognito: bool) -> String {
    if incognito {
        "An incognito window loads no extensions. Open one in a normal window instead.".to_string()
    } else {
        "This build loads unpacked extensions through WebView2, which is Windows only.".to_string()
    }
}
