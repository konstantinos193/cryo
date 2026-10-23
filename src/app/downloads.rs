//! The download shelf: this window's record of files the engine is
//! saving. `AppEvent::Download` upserts rows; the chrome's ⬇ popover
//! renders them and its buttons send the commands back (`open`, `pause`,
//! `resume`, `retry`, `cancel`, `show in folder`, `clear`).

use std::path::Path;

use tao::event_loop::ControlFlow;

use crate::engine::{DownloadId, DownloadState, DownloadUpdate};
use crate::shell::chrome::ChromeDownload;
use crate::tabs::TabId;

use super::dragout;
use super::App;

/// Finished rows accumulate without bound otherwise — cap the shelf.
const SHELF_CAP: usize = 50;

/// One row on the shelf. The engine identifies a download by (tab,
/// view-local id); the wire key is an app counter so the chrome never
/// carries the pair.
pub(super) struct Entry {
    pub key: u64,
    pub tab: TabId,
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
    /// A view reported download progress — upsert the shelf row and
    /// repaint chrome.
    pub(super) fn download_update(
        &mut self,
        tab: TabId,
        u: DownloadUpdate,
        control_flow: &mut ControlFlow,
    ) -> anyhow::Result<()> {
        let terminal = !matches!(u.state, DownloadState::InProgress | DownloadState::Paused);
        match self
            .downloads
            .iter_mut()
            .find(|e| e.tab == tab && e.engine_id == u.id)
        {
            Some(e) => {
                e.received = u.received;
                e.total = u.total;
                e.state = u.state;
                e.path = u.path;
            }
            None => {
                self.download_seq += 1;
                self.downloads.push(Entry {
                    key: self.download_seq,
                    tab,
                    engine_id: u.id,
                    name: u.name,
                    path: u.path,
                    uri: u.uri,
                    received: u.received,
                    total: u.total,
                    state: u.state,
                });
                if self.downloads.len() > SHELF_CAP {
                    let drop = self.downloads.len() - SHELF_CAP;
                    self.downloads.drain(..drop);
                }
            }
        }
        // A retry tab exists only to hold the navigation — no engine has
        // a "download this URL" call, so a real view fetches it. Once the
        // transfer settles the tab is litter: close it like Chrome's
        // invisible retry would. Kept if it's the last tab standing —
        // closing then would exit the window mid-action.
        if terminal && self.retry_tabs.remove(&tab) && self.registry.len() > 1 {
            return self.close_tab(tab, control_flow);
        }
        self.sync_chrome()
    }

    /// `id`'s view is gone — anything it was downloading died with it.
    /// The surface teardown paths call this before the view drops.
    pub(super) fn drop_downloads_of(&mut self, tab: TabId) {
        self.retry_tabs.remove(&tab);
        for e in &mut self.downloads {
            if e.tab == tab && matches!(e.state, DownloadState::InProgress | DownloadState::Paused)
            {
                e.state = DownloadState::Interrupted;
            }
        }
    }

    /// The ✕ on a live row — reaches back through the owning view. A
    /// row whose view already left has nothing to cancel; the shelf
    /// entry already shows its terminal state.
    pub(super) fn cancel_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.downloads.iter().find(|e| e.key == key) {
            if let Some(view) = self.views.get(&e.tab) {
                view.cancel_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// The ⏸ on a live row — the transfer holds, resumable, until ▶
    /// or ✕. Same reach-back path as cancel.
    pub(super) fn pause_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.downloads.iter().find(|e| e.key == key) {
            if let Some(view) = self.views.get(&e.tab) {
                view.pause_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// The ▶ on a paused row.
    pub(super) fn resume_download(&mut self, key: u64) -> anyhow::Result<()> {
        if let Some(e) = self.downloads.iter().find(|e| e.key == key) {
            if let Some(view) = self.views.get(&e.tab) {
                view.resume_download(e.engine_id)?;
            }
        }
        Ok(())
    }

    /// Click on a finished row — the file opens with its default app.
    pub(super) fn open_download(&self, key: u64) {
        if let Some(e) = self
            .downloads
            .iter()
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
            .downloads
            .iter()
            .find(|e| e.key == key && e.state == DownloadState::Done)
        {
            dragout::drag_file(Path::new(&e.path));
        }
    }

    /// "Retry" on a dead transfer — a background tab re-navigates to the
    /// recorded URI, which restarts the download; the tab closes itself
    /// when the transfer settles (see `download_update`).
    pub(super) fn retry_download(&mut self, key: u64) -> anyhow::Result<()> {
        let Some(e) = self.downloads.iter().find(|e| e.key == key) else {
            return Ok(());
        };
        if e.uri.is_empty() {
            return Ok(());
        }
        let uri = e.uri.clone();
        let name = e.name.clone();
        let id = self.registry.create(uri.clone());
        if let Some(tab) = self.registry.get_mut(id) {
            tab.set_title(name);
        }
        self.spawn_view_at(id, &uri)?;
        self.retry_tabs.insert(id);
        self.sync_chrome()
    }

    /// The 📁 on a finished row — Explorer selects the file. Errors go
    /// to the reporting seam: the row stays put either way.
    pub(super) fn reveal_download(&self, key: u64) {
        let Some(e) = self.downloads.iter().find(|e| e.key == key) else {
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
        self.downloads
            .retain(|e| matches!(e.state, DownloadState::InProgress | DownloadState::Paused));
        self.sync_chrome()
    }

    /// Newest first — the order the popover paints.
    pub(super) fn download_rows(&self) -> Vec<ChromeDownload> {
        self.downloads
            .iter()
            .rev()
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
