//! The download shelf: this window's record of files the engine is
//! saving. `AppEvent::Download` upserts rows; the chrome's ⬇ popover
//! renders them and its buttons send the commands back (`cancel`,
//! `show in folder`, `clear`).

use crate::engine::{DownloadId, DownloadState, DownloadUpdate};
use crate::shell::chrome::ChromeDownload;
use crate::tabs::TabId;

use super::{report, App};

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
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

impl App {
    /// A view reported download progress — upsert the shelf row and
    /// repaint chrome.
    pub(super) fn download_update(
        &mut self,
        tab: TabId,
        u: DownloadUpdate,
    ) -> anyhow::Result<()> {
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
        self.sync_chrome()
    }

    /// `id`'s view is gone — anything it was downloading died with it.
    /// The surface teardown paths call this before the view drops.
    pub(super) fn drop_downloads_of(&mut self, tab: TabId) {
        for e in &mut self.downloads {
            if e.tab == tab && e.state == DownloadState::InProgress {
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
            report(err);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = e;
    }

    /// "Clear finished" drops every settled row; in-flight ones stay.
    pub(super) fn clear_finished_downloads(&mut self) -> anyhow::Result<()> {
        self.downloads
            .retain(|e| e.state == DownloadState::InProgress);
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
                    DownloadState::Done => "done",
                    DownloadState::Interrupted => "interrupted",
                    DownloadState::Cancelled => "cancelled",
                },
            })
            .collect()
    }
}
