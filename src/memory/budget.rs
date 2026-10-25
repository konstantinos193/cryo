//! Measured private-memory budget, pressure hysteresis and sequential freeze
//! pacing. WebView2 shares renderers between views, so nothing is per-tab:
//! the only signal is the process delta around a freeze — the first empty
//! delta usually means a sibling still references the renderer, so the next
//! eligible tab still freezes on the settle cadence; consecutive empty
//! deltas mean freezing stopped working, and that backs off.

use std::time::{Duration, Instant};

use super::{IdleState, MemoryError};
use crate::tabs::TabId;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(30);
const FREEZE_SETTLE: Duration = Duration::from_secs(15);
const NO_RECLAIM_BACKOFF: Duration = Duration::from_secs(60);
const FREEZE_AFTER: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
pub(crate) struct ByteBudget(u64);

impl Default for ByteBudget {
    fn default() -> Self {
        Self(1024 * 1024 * 1024)
    }
}

impl ByteBudget {
    pub(crate) fn from_mib(mib: u64) -> Result<Self, MemoryError> {
        mib.checked_mul(1024 * 1024)
            .filter(|n| *n > 0)
            .map(Self)
            .ok_or(MemoryError::ConfigureBudget)
    }
}

pub(crate) struct Candidate {
    pub id: TabId,
    pub idle: IdleState,
}

pub(crate) struct BudgetEnforcer {
    budget: ByteBudget,
    sequence: u64,
    pending: Option<(u64, Instant)>,
    next_sample: Instant,
    next_freeze: Instant,
    high_samples: u8,
    /// Consecutive freezes whose follow-up sample showed <1 MiB reclaimed.
    /// Shared renderers make the first empty delta normal — the sibling
    /// still holds the process — so only a streak backs off.
    no_reclaim: u8,
    pressure: bool,
    before_freeze: Option<u64>,
}

impl BudgetEnforcer {
    pub(crate) fn new(budget: ByteBudget, now: Instant) -> Self {
        Self {
            budget,
            sequence: 0,
            pending: None,
            next_sample: now,
            next_freeze: now,
            high_samples: 0,
            no_reclaim: 0,
            pressure: false,
            before_freeze: None,
        }
    }

    pub(crate) fn begin_sample(&mut self, now: Instant) -> Option<u64> {
        if now < self.next_sample
            || self
                .pending
                .is_some_and(|(_, since)| now.saturating_duration_since(since) < SAMPLE_TIMEOUT)
        {
            return None;
        }
        if self.pending.is_some() {
            self.sample_failed();
        }
        self.sequence += 1;
        self.pending = Some((self.sequence, now));
        self.next_sample = now + SAMPLE_INTERVAL;
        Some(self.sequence)
    }

    pub(crate) fn sample_failed(&mut self) {
        self.pending = None;
        self.high_samples = 0;
        self.no_reclaim = 0;
        self.pressure = false;
    }

    pub(crate) fn measured(
        &mut self,
        sequence: u64,
        bytes: Option<u64>,
        now: Instant,
        candidates: impl IntoIterator<Item = Candidate>,
    ) -> Option<TabId> {
        let (pending, since) = self.pending?;
        if pending != sequence {
            return None;
        }
        self.pending = None;
        if now.saturating_duration_since(since) >= SAMPLE_TIMEOUT {
            self.sample_failed();
            return None;
        }
        let Some(bytes) = bytes else {
            self.sample_failed();
            return None;
        };
        if let Some(before) = self.before_freeze.take() {
            if before.saturating_sub(bytes) < 1024 * 1024 {
                self.no_reclaim = self.no_reclaim.saturating_add(1);
                if self.no_reclaim >= 2 {
                    self.next_freeze = now + NO_RECLAIM_BACKOFF;
                }
            } else {
                self.no_reclaim = 0;
            }
        }
        if bytes <= self.budget.0 / 10 * 9 {
            self.pressure = false;
            self.high_samples = 0;
            self.no_reclaim = 0;
        } else if bytes > self.budget.0 {
            self.high_samples = self.high_samples.saturating_add(1);
            self.pressure |= self.high_samples >= 2;
        } else {
            self.high_samples = 0;
        }
        if !self.pressure || now < self.next_freeze {
            return None;
        }
        let id = candidates
            .into_iter()
            .filter(|c| c.idle.eligible(now, FREEZE_AFTER))
            .min_by_key(|c| c.idle.unfocused_since)
            .map(|c| c.id)?;
        self.before_freeze = Some(bytes);
        self.next_freeze = now + FREEZE_SETTLE;
        self.next_sample = self.next_freeze;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(now: Instant, age: u64, protected: bool) -> Candidate {
        Candidate {
            id: TabId::from_raw(age),
            idle: IdleState {
                foreground: false,
                retry_host: false,
                loading: false,
                downloading: false,
                playing_audio: false,
                suspended: false,
                protected,
                unfocused_since: Some(now - Duration::from_secs(age)),
            },
        }
    }

    fn sample(
        policy: &mut BudgetEnforcer,
        now: Instant,
        mib: Option<u64>,
        candidates: Vec<Candidate>,
    ) -> Option<TabId> {
        let seq = policy.begin_sample(now).unwrap();
        policy.measured(seq, mib.map(|n| n * 1024 * 1024), now, candidates)
    }

    #[test]
    fn restored_tab_cannot_be_refrozen_until_a_full_new_idle_grace_period() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(200), vec![]);
        let frozen = now + SAMPLE_INTERVAL;
        assert!(sample(
            &mut p,
            frozen,
            Some(200),
            vec![candidate(frozen, 300, false)]
        )
        .is_some());
        let restored = frozen + FREEZE_SETTLE;
        let mut tab = candidate(restored, 300, false);
        tab.idle.unfocused_since = Some(restored);
        assert!(sample(&mut p, restored, Some(150), vec![tab]).is_none());
        let later = restored + Duration::from_secs(119);
        let mut tab = candidate(later, 300, false);
        tab.idle.unfocused_since = Some(restored);
        assert!(sample(&mut p, later, Some(150), vec![tab]).is_none());
        let later = restored + Duration::from_secs(125);
        let mut tab = candidate(later, 300, false);
        tab.idle.unfocused_since = Some(restored);
        assert!(sample(&mut p, later, Some(150), vec![tab]).is_some());
    }

    #[test]
    fn every_active_work_signal_blocks_automatic_freeze() {
        let now = Instant::now();
        for signal in 0..7 {
            let mut c = candidate(now, 300, false);
            match signal {
                0 => c.idle.foreground = true,
                1 => c.idle.retry_host = true,
                2 => c.idle.loading = true,
                3 => c.idle.downloading = true,
                4 => c.idle.playing_audio = true,
                5 => c.idle.protected = true,
                _ => c.idle.unfocused_since = None,
            }
            assert!(!c.idle.eligible(now, FREEZE_AFTER));
        }
        assert!(!candidate(now, 119, false).idle.eligible(now, FREEZE_AFTER));
        assert!(candidate(now, 120, false).idle.eligible(now, FREEZE_AFTER));
        let mut sleeping = candidate(now, 300, false);
        sleeping.idle.suspended = true;
        assert!(sleeping.idle.eligible(now, FREEZE_AFTER));
    }

    #[test]
    fn under_budget_never_freezes_regardless_of_tab_count() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        for i in 0..5 {
            let now = now + SAMPLE_INTERVAL * i;
            assert_eq!(
                sample(
                    &mut p,
                    now,
                    Some(80),
                    (200..400).map(|age| candidate(now, age, false)).collect()
                ),
                None
            );
        }
    }

    #[test]
    fn timed_out_request_can_be_replaced_without_accepting_its_late_reply() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        let old = p.begin_sample(now).unwrap();
        let later = now + SAMPLE_TIMEOUT;
        let new = p.begin_sample(later).unwrap();
        assert_ne!(old, new);
        assert!(p.measured(old, Some(u64::MAX), later, vec![]).is_none());
        assert_eq!(p.pending.unwrap().0, new);
        assert!(p.measured(new, Some(0), later, vec![]).is_none());
        assert!(p.pending.is_none());
    }

    #[test]
    fn rejects_zero_and_overflow_budgets() {
        assert!(ByteBudget::from_mib(0).is_err());
        assert!(ByteBudget::from_mib(u64::MAX).is_err());
        assert!(ByteBudget::from_mib(1024).is_ok());
    }

    #[test]
    fn requires_sustained_pressure_and_selects_oldest_eligible_tab() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        assert_eq!(
            sample(&mut p, now, Some(110), vec![candidate(now, 300, false)]),
            None
        );
        let later = now + SAMPLE_INTERVAL;
        assert_eq!(
            sample(
                &mut p,
                later,
                Some(110),
                vec![
                    candidate(later, 500, true),
                    candidate(later, 20, false),
                    candidate(later, 200, false),
                    candidate(later, 300, false)
                ]
            ),
            Some(TabId::from_raw(300))
        );
        assert!(p.begin_sample(later + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn hysteresis_stops_below_low_watermark_and_rechecks_after_each_freeze() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        assert!(sample(
            &mut p,
            now + SAMPLE_INTERVAL,
            Some(110),
            vec![candidate(now, 300, false)]
        )
        .is_some());
        let settled = now + SAMPLE_INTERVAL + FREEZE_SETTLE;
        assert!(sample(
            &mut p,
            settled,
            Some(95),
            vec![candidate(settled, 400, false)]
        )
        .is_some());
        assert!(sample(
            &mut p,
            settled + FREEZE_SETTLE,
            Some(89),
            vec![candidate(settled, 500, false)]
        )
        .is_none());
        assert!(!p.pressure);
    }

    #[test]
    fn incomplete_stale_and_duplicate_samples_cannot_freeze() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        let seq = p.begin_sample(now).unwrap();
        assert!(p.begin_sample(now + SAMPLE_INTERVAL).is_none());
        assert!(p.measured(seq + 1, Some(u64::MAX), now, vec![]).is_none());
        assert!(p.measured(seq, None, now, vec![]).is_none());
        let later = now + SAMPLE_INTERVAL;
        let seq = p.begin_sample(later).unwrap();
        assert!(p
            .measured(seq, Some(u64::MAX), later + SAMPLE_TIMEOUT, vec![])
            .is_none());
        assert!(p
            .measured(seq, Some(u64::MAX), later + SAMPLE_TIMEOUT, vec![])
            .is_none());
        assert!(!p.pressure);
    }

    /// At exactly the budget a sample is not over, so the over-streak
    /// resets — but existing pressure inside the 90–100% band persists
    /// and an exact-budget sample still freezes.
    #[test]
    fn exactly_at_budget_resets_the_streak_without_lifting_pressure() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        for i in 0..2u32 {
            let now = now + SAMPLE_INTERVAL * i;
            assert_eq!(
                sample(&mut p, now, Some(100), vec![candidate(now, 300, false)]),
                None
            );
        }
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        let later = now + SAMPLE_INTERVAL;
        assert!(sample(&mut p, later, Some(110), vec![candidate(later, 300, false)]).is_some());
        let settled = later + FREEZE_SETTLE;
        assert!(sample(
            &mut p,
            settled,
            Some(100),
            vec![candidate(settled, 400, false)]
        )
        .is_some());
        assert!(p.pressure);
    }

    /// Equal-age candidates are decided by candidate order — the
    /// registry's order — not by whichever id sorts first.
    #[test]
    fn equal_age_candidates_freeze_in_candidate_order() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        let later = now + SAMPLE_INTERVAL;
        let pair = |a: u64, b: u64| {
            vec![
                Candidate {
                    id: TabId::from_raw(a),
                    ..candidate(later, 300, false)
                },
                Candidate {
                    id: TabId::from_raw(b),
                    ..candidate(later, 300, false)
                },
            ]
        };
        assert_eq!(
            sample(&mut p, later, Some(110), pair(2, 1)),
            Some(TabId::from_raw(2))
        );
    }

    /// A failed follow-up sample resets pressure but keeps the
    /// pre-freeze baseline: the next complete sample still answers
    /// whether freezing that tab reclaimed anything.
    #[test]
    fn a_failed_sample_after_freeze_keeps_the_reclaim_baseline() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        let later = now + SAMPLE_INTERVAL;
        assert!(sample(&mut p, later, Some(110), vec![candidate(later, 300, false)]).is_some());
        assert!(p.begin_sample(later + FREEZE_SETTLE).is_some());
        p.sample_failed();
        assert!(!p.pressure);
        let recovered = later + FREEZE_SETTLE + SAMPLE_INTERVAL;
        // 105 is still over budget but 5 MiB under the pre-freeze
        // baseline — a real reclaim, not an empty delta.
        assert!(sample(&mut p, recovered, Some(105), vec![]).is_none());
        assert_eq!(p.no_reclaim, 0);
    }

    /// Shared renderers make one empty delta normal — the frozen tab's
    /// sibling still holds the process — so the next eligible tab freezes
    /// on the settle cadence, not after a backoff.
    #[test]
    fn one_empty_delta_still_freezes_the_next_candidate_on_schedule() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        sample(
            &mut p,
            now + SAMPLE_INTERVAL,
            Some(110),
            vec![candidate(now, 300, false)],
        );
        let later = now + SAMPLE_INTERVAL + FREEZE_SETTLE;
        assert!(sample(&mut p, later, Some(111), vec![candidate(later, 400, false)]).is_some());
    }

    /// Two consecutive empty deltas mean draining the process tree no longer
    /// reclaims — back off instead of churning through every remaining tab.
    #[test]
    fn consecutive_empty_deltas_back_off() {
        let now = Instant::now();
        let mut p = BudgetEnforcer::new(ByteBudget::from_mib(100).unwrap(), now);
        sample(&mut p, now, Some(110), vec![]);
        sample(
            &mut p,
            now + SAMPLE_INTERVAL,
            Some(110),
            vec![candidate(now, 300, false)],
        );
        let second = now + SAMPLE_INTERVAL + FREEZE_SETTLE;
        assert!(sample(
            &mut p,
            second,
            Some(111),
            vec![candidate(second, 400, false)]
        )
        .is_some());
        let later = second + FREEZE_SETTLE;
        assert!(sample(&mut p, later, Some(112), vec![candidate(later, 500, false)]).is_none());
        assert!(sample(
            &mut p,
            later + NO_RECLAIM_BACKOFF,
            Some(112),
            vec![candidate(later, 500, false)]
        )
        .is_some());
    }
}
