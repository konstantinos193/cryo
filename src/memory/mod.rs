//! Resource policy: choose background tabs to freeze and decide when an
//! idle renderer may sleep. The app gathers observations and executes decisions.

use std::time::{Duration, Instant};

use crate::tabs::{TabId, TabRegistry};

const MAX_VIEWS: usize = 24;
const SUSPEND_AFTER: Duration = Duration::from_secs(60);

/// Select the coldest eligible tab when the live surface budget is exceeded.
pub(crate) fn freeze_candidate<V, A: crate::tabs::SurfaceResources>(
    tabs: &TabRegistry<V, A>,
    keep: TabId,
    now: Instant,
) -> Option<TabId> {
    if tabs.views().count() <= MAX_VIEWS {
        return None;
    }
    let active = tabs.active_id();
    tabs.views()
        .map(|(tab, _)| tab)
        .filter(|tab| tab.id() != keep && Some(tab.id()) != active)
        .min_by_key(|tab| tab.unfocused_since().unwrap_or(now))
        .map(|tab| tab.id())
}

/// Observations needed for idle policy, collected without engine dependencies.
pub(crate) struct IdleState {
    pub foreground: bool,
    pub retry_host: bool,
    pub loading: bool,
    pub downloading: bool,
    pub playing_audio: bool,
    pub suspended: bool,
    pub unfocused_since: Option<Instant>,
}

impl IdleState {
    pub(crate) fn should_suspend(&self, now: Instant) -> bool {
        !self.foreground
            && !self.retry_host
            && !self.loading
            && !self.downloading
            && !self.playing_audio
            && !self.suspended
            && self
                .unfocused_since
                .is_some_and(|since| now.saturating_duration_since(since) >= SUSPEND_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_preserves_foreground_and_new_surface_and_freezes_oldest() {
        let now = Instant::now();
        let mut tabs = TabRegistry::<()>::new();
        let mut ids = Vec::new();
        for i in 0..MAX_VIEWS {
            let id = tabs
                .create_with(format!("https://{i}.test"), |_| Ok::<_, ()>(()))
                .unwrap();
            tabs.get_mut(id)
                .unwrap()
                .note_unfocused(now + Duration::from_secs(i as u64));
            ids.push(id);
        }
        tabs.set_active(ids[0]).unwrap();
        assert_eq!(freeze_candidate(&tabs, ids[1], now), None);
        let new = tabs
            .create_with("https://new.test".into(), |_| Ok::<_, ()>(()))
            .unwrap();
        assert_eq!(freeze_candidate(&tabs, new, now), Some(ids[1]));
        assert_eq!(freeze_candidate(&tabs, ids[1], now), Some(new));
    }

    #[test]
    fn idle_policy_protects_work_and_audio() {
        let now = Instant::now();
        let mut state = IdleState {
            foreground: false,
            retry_host: false,
            loading: false,
            downloading: false,
            playing_audio: false,
            suspended: false,
            unfocused_since: Some(now - SUSPEND_AFTER),
        };
        assert!(state.should_suspend(now));
        state.playing_audio = true;
        assert!(!state.should_suspend(now));
        state.playing_audio = false;
        state.downloading = true;
        assert!(!state.should_suspend(now));
        state.downloading = false;
        state.foreground = true;
        assert!(!state.should_suspend(now));
    }
}
