//! View-budget policy: choosing which background surfaces to freeze and
//! which idle renderers to suspend. Surface construction stays in `surfaces`.

use crate::engine::DownloadState;
use crate::memory::{self, IdleState};
use crate::tabs::TabId;

use super::App;

impl App {
    pub(super) fn adopt_view(&mut self, id: TabId) -> anyhow::Result<()> {
        if let Some(tab) = self.registry.get_mut(id) {
            tab.note_unfocused(self.clock.now());
        }
        self.enforce_view_budget(id)
    }

    fn enforce_view_budget(&mut self, keep: TabId) -> anyhow::Result<()> {
        while let Some(coldest) = memory::freeze_candidate(&self.registry, keep, self.clock.now()) {
            self.freeze_view(coldest)?;
        }
        Ok(())
    }

    pub(super) fn suspend_idle_views(&mut self) {
        let now = self.clock.now();
        let active = self.registry.active_id();
        for (tab, view) in self.registry.views() {
            let id = tab.id();
            let downloading = tab.resources().downloads.iter().any(|e| {
                e.view.tab() == id
                    && matches!(e.state, DownloadState::InProgress | DownloadState::Paused)
            });
            let state = IdleState {
                foreground: Some(id) == active,
                retry_host: tab.is_retry_host(),
                loading: tab.is_loading(),
                downloading,
                playing_audio: view.playing_audio().unwrap_or(true),
                suspended: view.is_suspended().unwrap_or(false),
                unfocused_since: tab.unfocused_since(),
            };
            if !state.should_suspend(now) {
                continue;
            }
            if let Err(e) = view.suspend() {
                crate::report!("suspend tab {id}: {e:?}");
            }
        }
    }
}
