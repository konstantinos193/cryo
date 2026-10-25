//! Resource policy: choose background tabs to freeze and decide when an
//! idle renderer may sleep. The app gathers observations and executes decisions.

mod budget;
pub(crate) use budget::{BudgetEnforcer, ByteBudget, Candidate};

use std::time::{Duration, Instant};

const SUSPEND_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum MemoryError {
    #[error("memory budget must be a positive MiB value that fits in bytes")]
    ConfigureBudget,
}

/// Observations needed for idle policy, collected without engine dependencies.
pub(crate) struct IdleState {
    pub foreground: bool,
    pub retry_host: bool,
    pub loading: bool,
    pub downloading: bool,
    pub playing_audio: bool,
    pub suspended: bool,
    pub protected: bool,
    pub unfocused_since: Option<Instant>,
}

impl IdleState {
    pub(crate) fn should_suspend(&self, now: Instant) -> bool {
        !self.suspended && self.eligible(now, SUSPEND_AFTER)
    }

    fn eligible(&self, now: Instant, minimum_idle: Duration) -> bool {
        !self.foreground
            && !self.retry_host
            && !self.loading
            && !self.downloading
            && !self.playing_audio
            && !self.protected
            && self
                .unfocused_since
                .is_some_and(|since| now.saturating_duration_since(since) >= minimum_idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            protected: false,
            unfocused_since: Some(now - SUSPEND_AFTER),
        };
        assert!(state.should_suspend(now));
        // 59 seconds idle is not yet sleepable; an already-sleeping
        // view is not asked again.
        state.unfocused_since = Some(now - SUSPEND_AFTER + Duration::from_secs(1));
        assert!(!state.should_suspend(now));
        state.unfocused_since = Some(now - SUSPEND_AFTER);
        state.suspended = true;
        assert!(!state.should_suspend(now));
        state.suspended = false;
        state.playing_audio = true;
        assert!(!state.should_suspend(now));
        state.playing_audio = false;
        state.downloading = true;
        assert!(!state.should_suspend(now));
        state.downloading = false;
        state.foreground = true;
        assert!(!state.should_suspend(now));
        state.foreground = false;
        state.protected = true;
        assert!(!state.should_suspend(now));
    }
}
