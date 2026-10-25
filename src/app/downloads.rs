//! The download shelf: this window's record of files the engine is
//! saving. `AppEvent::Download` upserts rows; the chrome's ⬇ popover
//! renders them and its buttons send the commands back (`open`, `pause`,
//! `resume`, `retry`, `cancel`, `show in folder`, `clear`).

use std::path::Path;

use tao::event_loop::ControlFlow;

use crate::engine::{DownloadId, DownloadState, DownloadUpdate};
use crate::shell::chrome::{ChromeCommand, ChromeDownload};
use crate::tabs::{SurfaceResources, TabId, ViewKey};

use super::dragout;
use super::App;

/// Finished rows accumulate without bound otherwise — cap the shelf.
const SHELF_CAP: usize = 50;

/// One row on the shelf. The engine identifies a download by (surface,
/// view-local id) — the surface incarnation, not just the tab: a
/// restored tab's new view numbers its downloads from scratch, and must
/// not land on the old view's rows. The wire key is an app counter so the
/// chrome never carries the pair.
pub(super) struct Entry {
    pub key: u64,
    pub view: ViewKey,
    pub engine_id: DownloadId,
    pub name: String,
    pub path: String,
    /// The address the file came from — Retry navigates a throwaway tab
    /// to it. Empty when the engine didn't report one.
    pub uri: String,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

/// A finished row clicked — the OS default handler opens the file.
#[cfg(target_os = "windows")]
fn open_path(path: &str) {
    use windows::core::w;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
    unsafe {
        // ShellExecute's error channel is the ≤32 HINSTANCE convention.
        let handle = ShellExecuteW(
            None,
            w!("open"),
            &windows::core::HSTRING::from(path),
            None,
            None,
            SW_SHOW,
        );
        if handle.0 as usize <= 32 {
            crate::report!("open failed ({}) for {path}", handle.0 as usize);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn open_path(path: &str) {
    let tool = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(err) = std::process::Command::new(tool).arg(path).spawn() {
        crate::report!("{err:?}");
    }
}

/// A `file:///` URL for a local path — the shape `navigate` takes.
pub(super) fn file_url(path: &Path) -> String {
    format!("file:///{}", path.display().to_string().replace('\\', "/"))
}

impl App {
    fn download_entries(&self) -> impl Iterator<Item = &Entry> {
        self.download_archive.iter().chain(
            self.registry
                .iter()
                .flat_map(|tab| tab.resources().downloads.iter()),
        )
    }

    pub(super) fn archive_tab_downloads(&mut self, id: TabId) {
        if let Some(tab) = self.registry.get_mut(id) {
            tab.resources_mut().drop_surface();
            let rows = std::mem::take(&mut tab.resources_mut().downloads);
            self.download_archive.extend(rows);
        }
        if self.download_archive.len() > SHELF_CAP {
            self.download_archive.sort_by_key(|e| e.key);
            self.download_archive
                .drain(..self.download_archive.len() - SHELF_CAP);
        }
    }

    pub(super) fn download_folder_setting(&self, id: TabId, choose: bool) -> anyhow::Result<()> {
        if self
            .registry
            .get(id)
            .is_none_or(|tab| tab.url() != crate::shell::pages::SETTINGS_URL)
        {
            return Ok(());
        }
        let Some(view) = self.view_of(id) else {
            return Ok(());
        };
        let result = (|| -> Result<_, crate::engine::EngineError> {
            let current = view.download_folder()?;
            if choose {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Download folder")
                    .set_directory(&current)
                    .pick_folder()
                {
                    view.set_download_folder(&path)?;
                }
            }
            view.download_folder()
        })();
        let (path, error) = match result {
            Ok(path) => (Some(path.to_string_lossy().into_owned()), None),
            Err(error) => (
                view.download_folder()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned()),
                Some(error.to_string()),
            ),
        };
        view.eval(&format!(
            "window.__cryo_download_folder({}, {})",
            serde_json::to_string(&path)?,
            serde_json::to_string(&error)?
        ))?;
        Ok(())
    }

    /// A view reported download progress — upsert the shelf row and
    /// repaint chrome.
    pub(super) fn download_update(
        &mut self,
        view: ViewKey,
        u: DownloadUpdate,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        let terminal = !matches!(u.state, DownloadState::InProgress | DownloadState::Paused);
        let Some(tab) = self.registry.get_mut(view.tab()) else {
            return Ok(());
        };
        let rows = &mut tab.resources_mut().downloads;
        let inserted = match rows
            .iter_mut()
            .find(|e| e.view == view && e.engine_id == u.id)
        {
            Some(e) => {
                e.received = u.received;
                e.total = u.total;
                e.state = u.state;
                e.path = u.path;
                false
            }
            None => {
                self.download_seq += 1;
                rows.push(Entry {
                    key: self.download_seq,
                    view,
                    engine_id: u.id,
                    name: u.name,
                    path: u.path,
                    uri: u.uri,
                    received: u.received,
                    total: u.total,
                    state: u.state,
                });
                if rows.len() > SHELF_CAP {
                    let drop = rows.len() - SHELF_CAP;
                    rows.drain(..drop);
                }
                true
            }
        };
        // A retry tab exists only to hold the navigation — no engine has
        // a "download this URL" call, so a real view fetches it. Once the
        // transfer settles the tab is litter: close it like Chrome's
        // invisible retry would. Kept if it's the last tab standing —
        // closing then would exit the window mid-action.
        let tab = view.tab();
        if terminal
            && self
                .registry
                .get_mut(tab)
                .is_some_and(|t| t.release_retry())
            && self.registry.len() > 1
        {
            return self.close_tab(tab, control_flow);
        }
        // A new row changes more than the shelf — the ⬇ button appears —
        // so it earns a full sync. Progress ticks at ~12 Hz only repaint
        // the shelf; serializing every tab, icon and bookmark for that
        // is the churn the split exists to avoid.
        if inserted {
            self.sync_chrome()
        } else {
            self.push_downloads()
        }
    }

    /// Push just the shelf rows — the light sibling of `sync_chrome`,
    /// for progress updates that change nothing else.
    pub(super) fn push_downloads(&self) -> anyhow::Result<()> {
        let json = serde_json::to_string(&self.download_rows())?;
        self.chrome.eval(&format!("window.__cryo_dl({json})"))?;
        Ok(())
    }

    /// The live surface that owns row `e`'s transfer — `None` once that
    /// incarnation is gone, even if its tab has a newer one.
    fn owning_view(&self, e: &Entry) -> Option<&dyn crate::engine::ContentView> {
        self.registry
            .live(e.view)
            .and_then(|t| t.view())
            .map(|v| &**v)
    }

    /// The shelf's command family — `on_chrome` hands the whole
    /// `Download*` arm set here so the strip dispatch stays a table of
    /// one-liners. `key` is the app-issued row number the wire carries.
    pub(super) fn download_command(&mut self, cmd: ChromeCommand) -> anyhow::Result<()> {
        match cmd {
            ChromeCommand::DownloadCancel { key } => self.cancel_download(key),
            ChromeCommand::DownloadOpen { key } => {
                self.open_download(key);
                Ok(())
            }
            ChromeCommand::DownloadPause { key } => self.pause_download(key),
            ChromeCommand::DownloadResume { key } => self.resume_download(key),
            ChromeCommand::DownloadRetry { key } => self.retry_download(key),
            ChromeCommand::DownloadDrag { key } => {
                self.drag_download(key);
                Ok(())
            }
            ChromeCommand::DownloadShow { key } => {
                self.reveal_download(key);
                Ok(())
            }
            ChromeCommand::DownloadsClear => self.clear_finished_downloads(),
            _ => unreachable!("dispatch only hands Download* commands here"),
        }
    }

    /// The ✕ on a live row — reaches back through the owning view. A
    /// row whose view already left has nothing to cancel; the shelf
    /// entry already shows its terminal state.
    pub(super) fn cancel_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.download_entries().find(|e| e.key == key) {
            if let Some(view) = self.owning_view(e) {
                view.cancel_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// The ⏸ on a live row — the transfer holds, resumable, until ▶
    /// or ✕. Same reach-back path as cancel.
    pub(super) fn pause_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.download_entries().find(|e| e.key == key) {
            if let Some(view) = self.owning_view(e) {
                view.pause_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// The ▶ on a paused row.
    pub(super) fn resume_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.download_entries().find(|e| e.key == key) {
            if let Some(view) = self.owning_view(e) {
                view.resume_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// Click on a finished row — the file opens with its default app.
    pub(super) fn open_download(&self, key: u64) {
        if let Some(e) = self
            .download_entries()
            .find(|e| e.key == key && e.state == DownloadState::Done)
        {
            open_path(&e.path);
        }
    }

    /// A finished row pointer-dragged out of the shelf — a real OLE
    /// drag carries the file, so Explorer copies it and a drop back on
    /// the strip opens it (the `FilesDropped` path).
    pub(super) fn drag_download(&self, key: u64) {
        if let Some(e) = self
            .download_entries()
            .find(|e| e.key == key && e.state == DownloadState::Done)
        {
            dragout::drag_file(Path::new(&e.path));
        }
    }

    /// "Retry" on a dead transfer — a background tab re-navigates to the
    /// recorded URI, which restarts the download; the tab closes itself
    /// when the transfer settles (see `download_update`).
    pub(super) fn retry_download(&mut self, key: u64) -> anyhow::Result<()> {
        let Some(e) = self.download_entries().find(|e| e.key == key) else {
            return Ok(());
        };
        if e.uri.is_empty() {
            return Ok(());
        }
        let uri = e.uri.clone();
        let name = e.name.clone();
        let id = self.open_tab(uri.clone(), &uri)?;
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title(name);
            tab.host_retry();
        }
        self.sync_chrome()
    }

    /// The 📁 on a finished row — Explorer selects the file. Errors go
    /// to the reporting seam: the row stays put either way.
    pub(super) fn reveal_download(&self, key: u64) {
        let Some(e) = self.download_entries().find(|e| e.key == key) else {
            return;
        };
        #[cfg(target_os = "windows")]
        if let Err(err) = std::process::Command::new("explorer")
            .arg(format!("/select,{}", e.path))
            .spawn()
        {
            crate::report!("{err:?}");
        }
        #[cfg(not(target_os = "windows"))]
        open_path(&e.path);
    }

    /// "Clear finished" drops every settled row; live ones (including
    /// paused — still resumable) stay.
    pub(super) fn clear_finished_downloads(&mut self) -> anyhow::Result<()> {
        self.download_archive.clear();
        for tab in self.registry.iter_mut() {
            tab.resources_mut()
                .downloads
                .retain(|e| matches!(e.state, DownloadState::InProgress | DownloadState::Paused));
        }
        self.sync_chrome()
    }

    /// Newest first — the order the popover paints.
    pub(super) fn download_rows(&self) -> Vec<ChromeDownload> {
        let mut rows: Vec<_> = self.download_entries().collect();
        rows.sort_by_key(|e| std::cmp::Reverse(e.key));
        rows.into_iter()
            .take(SHELF_CAP)
            .map(|e| ChromeDownload {
                key: e.key,
                name: e.name.clone(),
                path: e.path.clone(),
                received: e.received,
                total: e.total,
                state: match e.state {
                    DownloadState::InProgress => "progress",
                    DownloadState::Paused => "paused",
                    DownloadState::Done => "done",
                    DownloadState::Interrupted => "interrupted",
                    DownloadState::Cancelled => "cancelled",
                },
            })
            .collect()
    }
}
