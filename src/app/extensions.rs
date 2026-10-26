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

    /// Fetch an extension from the Chrome Web Store, by store address or id.
    ///
    /// The store's own "Add to Chrome" cannot reach us — WebView2 has no
    /// endpoint for it and the error page explains that — so this is the way
    /// in. The fetch runs on a worker; the event loop is never held for a
    /// network round trip (AGENTS §9).
    pub(super) fn install_from_store(&mut self, input: &str) -> anyhow::Result<()> {
        if self.extensions.is_none() {
            return self.notice(
                false,
                "Extensions are not available here",
                vec![extensions_unsupported_reason(self.incognito)],
            );
        }
        let request = match crate::extensions::web_store::parse_input(input) {
            Ok(request) => request,
            Err(_) => {
                return self.notice(
                    false,
                    "That is not a Chrome Web Store extension",
                    vec![
                        "Paste the store page address, or the 32-letter extension id out of it."
                            .to_string(),
                    ],
                );
            }
        };
        self.notice(
            true,
            "Fetching the extension",
            vec![
                request.id.to_string(),
                "cryo checks the package's signature against the key its id is derived from before installing anything.".to_string(),
            ],
        )?;
        let proxy = self.proxy.clone();
        crate::extensions::web_store::spawn_fetch(request, move |result| {
            super::post(
                &proxy,
                super::AppEvent::StoreExtension(result.map(Box::new).map_err(Box::new)),
            );
        });
        Ok(())
    }

    /// A worker finished fetching and verifying a package. Everything that
    /// could be checked away from the event loop already has been: what is
    /// left is writing the folder out.
    pub(super) fn on_store_extension(
        &mut self,
        result: Result<
            Box<crate::extensions::crx::VerifiedCrx>,
            Box<crate::extensions::ExtensionError>,
        >,
    ) -> anyhow::Result<()> {
        let package = match result {
            Ok(package) => package,
            Err(e) => {
                return self.notice(
                    false,
                    "That extension was not installed",
                    vec![e.to_string()],
                );
            }
        };
        let Some(catalogue) = &self.extensions else {
            return Ok(());
        };
        match catalogue.install_package(&package) {
            Ok(installed) => {
                self.notice(
                    true,
                    "Extension installed",
                    vec![
                        format!("{} {}", installed.name(), installed.version()),
                        "Signature verified. It loads into tabs opened from now on.".to_string(),
                    ],
                )?;
                self.push_library()
            }
            Err(e) => self.notice(
                false,
                "That extension was not installed",
                vec![e.to_string()],
            ),
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
