//! The tasks pane: every tab, its lifecycle state, and what the last
//! memory sample could honestly bill to it — Shift+Esc's answer.
//!
//! Rows ride the same door every privileged surface uses: the chrome
//! document's command channel in, an eval push out. No endpoint — the
//! attribution policy refuses a per-tab number that is not exact, and
//! the exposure rule refuses one any origin could read (see
//! `persist`'s doc comment for the shared reasoning).
//!
//! `memory::attribute` owns the sole-owner/shared/absent call; this
//! adapter gathers the live views' frame ids, maps rows onto tabs, and
//! pushes `ChromeTasks`. Samples come from two places — the budget
//! tick's own measurement, and `request_memory_sample`, which asks the
//! engine directly when the pane is open rather than waiting on the
//! enforcer's pacing.

use crate::memory::{attribute, Attribution, LiveView, Pools};
use crate::shell::chrome::{favicon_data_url, ChromeTask, ChromeTasks};
use crate::tabs::TabState;

use super::{post, unix_now, App, AppEvent};

impl App {
    /// Shift+Esc: the tasks pane, same door as the library — the
    /// document owns its open/shut state, so this asks it to show the
    /// pane and the overlay bounds follow from its `overlay` report.
    pub(super) fn toggle_tasks(&mut self) -> anyhow::Result<()> {
        self.chrome
            .eval("window.__cryo_library_toggle && window.__cryo_library_toggle('tasks')")?;
        Ok(())
    }

    /// A sample the pane asked for — `sequence` 0, so the budget
    /// enforcer ignores it: the enforcer bills samples it ordered.
    pub(super) fn request_memory_sample(&mut self) {
        let proxy = self.proxy.clone();
        if let Err(e) = self.engine.measure_memory(Box::new(move |snapshot| {
            post(&proxy, AppEvent::MemoryMeasured(0, snapshot));
        })) {
            crate::report!("tasks memory sample: {e}");
        }
    }

    /// Push the pane's rows. A shut pane — or a different pane — is
    /// skipped: the push is only worth its JSON while someone is
    /// looking at it.
    pub(super) fn push_tasks(&mut self) -> anyhow::Result<()> {
        if self.library_pane.as_deref() != Some("tasks") {
            return Ok(());
        }
        let live: Vec<LiveView> = self
            .registry
            .views()
            .map(|(tab, view)| LiveView {
                id: tab.id(),
                frame: view.frame_id(),
            })
            .collect();
        let (rows, pools) = self
            .last_memory
            .as_ref()
            .map(|(_, snapshot)| attribute(snapshot, &live))
            .unwrap_or_else(|| {
                (
                    live.iter()
                        .map(|v| (v.id, Attribution::Unattributed))
                        .collect(),
                    Pools::default(),
                )
            });
        let claimed: std::collections::HashMap<_, _> = rows.into_iter().collect();
        let active = self.registry.active_id();
        let tasks = self
            .registry
            .iter()
            .map(|tab| {
                let (bytes, reason) = if tab.state() == TabState::Active {
                    match claimed.get(&tab.id()) {
                        Some(Attribution::Sole(b)) => (Some(*b), None),
                        Some(Attribution::Shared) => (None, Some("shared")),
                        Some(Attribution::Unreadable) => (None, Some("unreadable")),
                        _ => (None, Some("unattributed")),
                    }
                } else {
                    (None, None)
                };
                ChromeTask {
                    id: tab.id().get(),
                    title: tab.title().to_string(),
                    url: tab.url().to_string(),
                    state: tab.state().as_str(),
                    current: active == Some(tab.id()),
                    bytes,
                    reason,
                    favicon: tab.favicon().map(favicon_data_url),
                }
            })
            .collect();
        let processes = self
            .last_memory
            .as_ref()
            .map(|(_, snapshot)| {
                snapshot
                    .processes()
                    .iter()
                    .map(|p| crate::shell::chrome::ChromeProcess {
                        pid: p.id().0,
                        kind: match p.kind() {
                            crate::engine::accounting::ProcessKind::Browser => "browser",
                            crate::engine::accounting::ProcessKind::Gpu => "gpu",
                            crate::engine::accounting::ProcessKind::Renderer => "renderer",
                            _ => "other",
                        },
                        bytes: p.stats().ok().map(|s| s.private_bytes()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let payload = ChromeTasks {
            tasks,
            processes,
            overhead: pools.overhead,
            shared: pools.shared_renderers,
            partial: pools.partial,
            sampled_at: self.last_memory.as_ref().map(|(at, _)| *at).unwrap_or(0),
        };
        let json = serde_json::to_string(&payload)?;
        self.chrome.eval(&format!("window.__cryo_tasks({json})"))?;
        Ok(())
    }

    /// A sample landed — keep it for the pane and repaint if the pane
    /// is looking. Called for every `MemoryMeasured`, budget-ordered
    /// or pane-ordered.
    pub(super) fn note_memory_sample(
        &mut self,
        snapshot: crate::engine::accounting::MemorySnapshot,
    ) -> anyhow::Result<()> {
        self.last_memory = Some((unix_now(), snapshot));
        self.push_tasks()
    }
}
