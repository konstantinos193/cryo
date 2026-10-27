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
        let found = match catalogue.list() {
            Ok(found) => found,
            Err(e) => {
                crate::report!("{e:?}");
                return Vec::new();
            }
        };
        // Two different questions: the catalogue says what is on disk, the
        // engine says what it loaded and whether it is on. Where they
        // disagree the pane shows it rather than picking one to believe.
        let loaded = self.engine.loaded_extensions().unwrap_or_else(|e| {
            crate::report!("{e:?}");
            Vec::new()
        });
        found
            .into_iter()
            .map(|e| {
                // A store install's folder is named by its id; a folder added
                // by hand is not, so it may have no engine row to match.
                let engine_row = loaded.iter().find(|l| l.id == e.slug());
                ChromeExtension {
                    slug: e.slug().to_string(),
                    name: e.name().to_string(),
                    version: e.version().to_string(),
                    manifest_version: e.manifest_version(),
                    dir: e.dir().display().to_string(),
                    loaded: engine_row.is_some(),
                    enabled: engine_row.is_some_and(|l| l.enabled),
                }
            })
            .collect()
    }

    /// Switch one on or off without uninstalling it. The engine owns this
    /// state — the folder on disk says nothing about it — so the answer is
    /// read back from the engine rather than assumed from the request.
    pub(super) fn set_extension_enabled(
        &mut self,
        slug: &str,
        enabled: bool,
    ) -> anyhow::Result<()> {
        // Resolved against the catalogue first, so a handle from the document
        // can only name something installed.
        let known = self
            .extensions
            .as_ref()
            .and_then(|c| c.list().ok())
            .is_some_and(|all| all.iter().any(|e| e.slug() == slug));
        if !known {
            return Ok(());
        }
        match self.engine.set_extension_enabled(slug, enabled) {
            Ok(()) => self.push_library(),
            Err(e) => self.notice(
                false,
                if enabled {
                    "That extension was not switched on"
                } else {
                    "That extension was not switched off"
                },
                vec![e.to_string()],
            ),
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

    /// A navigation turned out to be "Add to Chrome". Ask before fetching
    /// anything.
    ///
    /// The click on the store page is intent, but a navigation is not proof
    /// of a click: any page can send a view at a crafted store address, and
    /// installing on that alone would be a drive-by install. So the app
    /// asks, in the chrome view, which is the only place a page cannot
    /// answer from.
    pub(super) fn offer_store_install(
        &mut self,
        request: crate::extensions::web_store::InstallRequest,
    ) -> anyhow::Result<()> {
        if self.extensions.is_none() {
            return Ok(());
        }
        // Already installed: answer in the same card the install would have
        // used, rather than a toast that fades while the page behind it
        // still looks like a failure.
        let installed = self
            .extensions
            .as_ref()
            .and_then(|c| c.list().ok())
            .and_then(|all| {
                all.into_iter()
                    .find(|e| e.slug() == request.id.get())
                    .map(|e| format!("{} {}", e.name(), e.version()))
            });
        if let Some(name) = installed {
            self.install_seq += 1;
            let prompt = crate::shell::chrome::ChromeInstallPrompt {
                token: self.install_seq,
                id: request.id.to_string(),
                name: Some(name.clone()),
                fetching: true,
            };
            let json = serde_json::to_string(&prompt)?;
            self.chrome
                .eval(&format!("window.__cryo_install_prompt({json})"))?;
            self.set_overlay_open(true)?;
            return self.finish_install("Already installed", Ok(name));
        }
        self.install_seq += 1;
        let prompt = crate::shell::chrome::ChromeInstallPrompt {
            token: self.install_seq,
            id: request.id.to_string(),
            name: None,
            fetching: false,
        };
        let json = serde_json::to_string(&prompt)?;
        self.pending_install = Some((self.install_seq, request));
        self.chrome
            .eval(&format!("window.__cryo_install_prompt({json})"))?;
        self.set_overlay_open(true)
    }

    /// The answer to that prompt. The token has to match the request still
    /// pending, so an answer that arrives late — or one a page invented —
    /// installs nothing.
    pub(super) fn answer_store_install(&mut self, token: u64, install: bool) -> anyhow::Result<()> {
        let pending = match self.pending_install.take() {
            Some((pending_token, request)) if pending_token == token => request,
            // Not ours: put back whatever was there and ignore the answer.
            other => {
                self.pending_install = other;
                return Ok(());
            }
        };
        self.chrome.eval("window.__cryo_install_prompt(null)")?;
        self.set_overlay_open(false)?;
        if !install {
            return Ok(());
        }
        self.start_store_fetch(pending)
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
        self.start_store_fetch(request)
    }

    /// Start the worker. One place, so the pane's field and an answered
    /// "Add to Chrome" cannot drift apart in what they do.
    fn start_store_fetch(
        &mut self,
        request: crate::extensions::web_store::InstallRequest,
    ) -> anyhow::Result<()> {
        // The prompt stays up and becomes the progress surface, so the whole
        // install happens in one place instead of a dialog that vanishes and
        // a toast that appears somewhere else.
        self.installing = true;
        let prompt = crate::shell::chrome::ChromeInstallPrompt {
            token: self.install_seq,
            id: request.id.to_string(),
            name: None,
            fetching: true,
        };
        let json = serde_json::to_string(&prompt)?;
        self.chrome
            .eval(&format!("window.__cryo_install_prompt({json})"))?;
        self.set_overlay_open(true)?;
        let proxy = self.proxy.clone();
        let progress_proxy = self.proxy.clone();
        crate::extensions::web_store::spawn_fetch(
            request,
            move |received, total| {
                super::post(
                    &progress_proxy,
                    super::AppEvent::StoreExtensionProgress(received, total),
                );
            },
            move |result| {
                super::post(
                    &proxy,
                    super::AppEvent::StoreExtension(result.map(Box::new).map_err(Box::new)),
                );
            },
        );
        Ok(())
    }

    /// Bytes in. Goes straight to the prompt, which is already on screen —
    /// a toast that said "fetching" and then went quiet for twenty seconds
    /// read as a hang.
    pub(super) fn on_store_progress(
        &mut self,
        received: u64,
        total: Option<u64>,
    ) -> anyhow::Result<()> {
        if !self.installing {
            return Ok(());
        }
        let total = total.map(|t| t.to_string()).unwrap_or("null".into());
        self.chrome.eval(&format!(
            "window.__cryo_install_progress && window.__cryo_install_progress({received},{total})"
        ))?;
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
        self.installing = false;
        let package = match result {
            Ok(package) => package,
            Err(e) => return self.finish_install("Not installed", Err(e.to_string())),
        };
        let Some(catalogue) = &self.extensions else {
            return Ok(());
        };
        match catalogue.install_package(&package) {
            Ok(installed) => {
                let done = format!("{} {}", installed.name(), installed.version());
                self.finish_install("Extension installed", Ok(done))?;
                self.push_library()
            }
            Err(e) => self.finish_install("Not installed", Err(e.to_string())),
        }
    }

    /// Land the prompt on its last state. It shows the outcome in place and
    /// closes itself, rather than vanishing and leaving a toast to explain
    /// what happened somewhere else on screen.
    fn finish_install(
        &mut self,
        title: &str,
        outcome: Result<String, String>,
    ) -> anyhow::Result<()> {
        let (ok, text) = match outcome {
            Ok(name) => (true, name),
            Err(reason) => (false, reason),
        };
        let payload = serde_json::json!({ "ok": ok, "title": title, "text": text });
        self.chrome.eval(&format!(
            "window.__cryo_install_done && window.__cryo_install_done({payload})"
        ))?;
        // The document dismisses itself once the result has been read; the
        // bounds go back now so the page underneath is live again.
        self.set_overlay_open(false)
    }

    /// Remove an installed extension. The slug is resolved against the
    /// catalogue's own listing, so a handle from the document can only ever
    /// name something that is actually installed.
    pub(super) fn remove_extension(&mut self, slug: &str) -> anyhow::Result<()> {
        let Some(catalogue) = &self.extensions else {
            return Ok(());
        };
        // Ask the engine to unregister it before the folder goes. Deleting
        // the folder alone does make WebView2 drop the extension, but by
        // side effect; this is the actual uninstall. A failure here is not
        // fatal — an extension the engine never loaded has nothing to
        // unregister — so it is noted and the folder still goes.
        if let Err(e) = self.engine.remove_extension(slug) {
            crate::report!("engine did not unregister {slug}: {e}");
        }
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
