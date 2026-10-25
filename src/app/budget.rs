//! Measured-memory enforcement and idle observations. The memory context owns
//! selection and pacing; this adapter gathers engine observations and executes decisions.

use crate::engine::accounting::MemorySnapshot;
use crate::engine::{ContentView, DownloadState};
use crate::memory::{BudgetEnforcer, ByteBudget, Candidate, IdleState};
use crate::tabs::TabId;

use super::{post, App, AppEvent};

impl App {
    pub(crate) fn set_memory_budget(&mut self, budget: ByteBudget) {
        self.memory_budget = BudgetEnforcer::new(budget, self.clock.now());
    }

    pub(crate) fn resource_tick(&mut self) {
        if self.exiting {
            return;
        }
        self.suspend_idle_views();
        if !cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "macos"
        )) {
            return;
        }
        let Some(sequence) = self.memory_budget.begin_sample(self.clock.now()) else {
            return;
        };
        let proxy = self.proxy.clone();
        if let Err(e) = self.engine.measure_memory(Box::new(move |snapshot| {
            post(&proxy, AppEvent::MemoryMeasured(sequence, snapshot));
        })) {
            self.memory_budget.sample_failed();
            crate::report!("memory accounting: {e}");
        }
    }

    pub(super) fn adopt_view(&mut self, id: TabId) -> anyhow::Result<()> {
        if let Some(tab) = self.registry.get_mut(id) {
            tab.note_unfocused(self.clock.now());
        }
        Ok(())
    }

    pub(super) fn on_memory_measured(
        &mut self,
        sequence: u64,
        snapshot: MemorySnapshot,
    ) -> anyhow::Result<()> {
        let now = self.clock.now();
        let fresh = now
            .checked_duration_since(snapshot.started_at())
            .is_some_and(|age| age < std::time::Duration::from_secs(30));
        let bytes = if fresh && !snapshot.processes().is_empty() {
            snapshot.total_private_bytes()
        } else {
            None
        };
        let candidates: Vec<_> = self
            .registry
            .views()
            .map(|(tab, view)| Candidate {
                id: tab.id(),
                idle: self.idle_state(tab, &**view),
            })
            .collect();
        if let Some(id) = self
            .memory_budget
            .measured(sequence, bytes, now, candidates)
        {
            self.freeze_view(id)?;
            self.sync_chrome()?;
        }
        Ok(())
    }

    pub(super) fn suspend_idle_views(&mut self) {
        let now = self.clock.now();
        for (tab, view) in self.registry.views() {
            if self.idle_state(tab, &**view).should_suspend(now) {
                if let Err(e) = view.suspend() {
                    crate::report!("suspend tab {}: {e:?}", tab.id());
                }
            }
        }
    }

    fn idle_state(
        &self,
        tab: &crate::tabs::Tab<Box<dyn ContentView>, super::resources::TabResources>,
        view: &dyn ContentView,
    ) -> IdleState {
        let resources = tab.resources();
        let downloading = resources
            .downloads
            .iter()
            .any(|e| matches!(e.state, DownloadState::InProgress | DownloadState::Paused));
        let web =
            url::Url::parse(tab.url()).is_ok_and(|url| matches!(url.scheme(), "http" | "https"));
        IdleState {
            foreground: self.registry.active_id() == Some(tab.id()),
            retry_host: tab.is_retry_host(),
            loading: tab.is_loading(),
            downloading,
            playing_audio: view.playing_audio().unwrap_or(true),
            suspended: view.is_suspended().unwrap_or(false),
            protected: !web
                || resources.automatic_freeze_blocked
                || resources.password_prompt.is_some()
                || resources.permission_prompt.is_some(),
            unfocused_since: tab.unfocused_since(),
        }
    }
}
