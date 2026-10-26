# Memory policy checklist

Cryo already has budget enforcement and idle protection. This checklist verifies those decisions and their app integration; it is not a request to rebuild the policy layer.

Verification status: all items verified on Windows 2026-10-25. Deterministic evidence: `cargo test memory` (17 tests) and `cargo test app::resources` (1 test), all passing; live evidence reused from `DEFAULT-ENGINE-CHECKLIST.md` probe runs. Verified-in-code items are annotated; see "Verification notes" at the bottom.

## Ownership and scope

| Path | Responsibility |
| --- | --- |
| `src/memory/budget.rs` | Budget value, pressure state, sample sequencing, freeze selection and pacing |
| `src/memory/mod.rs` | Idle eligibility and lighter renderer sleep policy |
| `src/app/budget.rs` | Gather observations, request samples, execute decisions |
| `src/app/resources.rs` | Per-view protection and prompt/download state |
| `src/app/surfaces.rs` | Execute freeze and restore transitions |
| `src/engine/accounting.rs` | Memory observations, not policy |
| `src/app/freeze_probe.rs` | Real Windows enforcement and freeze evidence |

The memory context decides using Cryo types and injected time. Engine adapters measure memory; the app supplies current observations and performs transitions. Do not introduce platform webview types into policy. (Verified: `memory/` imports only `std::time` and `tabs::TabId`; policy tests drive `Instant` values directly — no sleeps, no runtime.)

## Current policy to verify

| Rule | Current code behavior | Verified |
| --- | --- | --- |
| Default budget | 1024 MiB; configured MiB must be positive and fit in bytes | `ByteBudget::default`/`from_mib` `checked_mul` + `>0`; `rejects_zero_and_overflow_budgets` |
| Normal sample interval | 5 seconds | `SAMPLE_INTERVAL`; exercised by every policy test |
| Pending sample timeout | 30 seconds | `SAMPLE_TIMEOUT`; `timed_out_request_can_be_replaced_without_accepting_its_late_reply` |
| Start enforcement | Two consecutive samples above budget | `high_samples >= 2` arms `pressure`; `requires_sustained_pressure_and_selects_oldest_eligible_tab` |
| Stop enforcement | At or below the integer-computed 90% low watermark | `bytes <= budget/10*9` clears pressure; `hysteresis_stops_below_low_watermark` |
| Between low watermark and budget | Existing pressure persists; over-budget sample count resets | `else` branch resets `high_samples`, leaves `pressure`; new test `exactly_at_budget_resets_the_streak_without_lifting_pressure` |
| Freeze choice | Oldest unfocused eligible live tab, at most one per decision | `min_by_key(unfocused_since)`, returns `Option<TabId>`; selection test + new `equal_age_candidates_freeze_in_candidate_order` |
| Automatic freeze grace | At least 120 seconds unfocused | `FREEZE_AFTER`; 119 s/120 s boundary asserted in `every_active_work_signal_blocks_automatic_freeze` |
| After a freeze decision | Wait 15 seconds before the next sample/freeze opportunity | `FREEZE_SETTLE` defers `next_freeze` and `next_sample`; asserted in selection test |
| Low-reclaim threshold | Less than 1 MiB reduction in the follow-up aggregate sample | `before_freeze.saturating_sub(bytes) < 1 MiB` |
| Low-reclaim backoff | Two consecutive low-reclaim outcomes defer further freezing by 60 seconds | `no_reclaim >= 2` → `next_freeze += NO_RECLAIM_BACKOFF`; `one_empty_delta…` + `consecutive_empty_deltas_back_off` |
| Lighter idle sleep | Eligible views may request engine suspension after 60 seconds; the view remains alive | `SUSPEND_AFTER` + `should_suspend` (`!suspended && eligible(60s)`); `idle_policy_protects_work_and_audio` |

The two-outcome backoff rule is taken from the current `budget.rs`. Older checklist text describes backoff after a single low-reclaim outcome; reconcile that wording against the code and tests rather than copying it forward. (Reconciled: code and tests both use two consecutive outcomes; the single-empty-delta case intentionally still freezes to release shared-renderer siblings.)

A budget is a soft target for measured engine memory. It excludes host memory and does not guarantee an OS-level RAM ceiling. Shared overhead or protected tabs can keep usage above budget.

## Per-tab attribution — decision, 2026-10-26

Roadmap Phase 2 is "finish per-tab attribution". This section fixes what a per-tab number is allowed to mean, because Phase 3's dashboard and the task-manager surface both have to display one, and the wrong choice there would undo the claim this whole document protects. Decided before the surface exists, deliberately.

**Decision: sole-owner attribution, plus one separately reported shared pool. No division, no estimates.**

| Case | What a tab reports |
| --- | --- |
| Its renderer serves only that view | That renderer's private bytes, as an exact figure |
| Its renderer serves two or more views | No per-tab figure. The tab is marked as sharing, and names the pool it shares |
| Browser and GPU processes | Reported once as engine overhead, never divided across tabs |
| Frozen or discarded | No figure, because there is no view. Not zero — absent |

**Why not a share-of-renderer estimate.** The recorded forced-sharing probe settles it: one shared ~160 MiB renderer served two views; freezing one view reclaimed 69.1 MiB, and only freezing both reclaimed 158.6 MiB. A naive half-share would have claimed ~80 MiB per tab, which overstates what freezing one tab returns and understates what freezing both does. An estimate that is wrong in both directions is worse than an honest absence, and it would be the first thing a reader quoted back at the README.

**What the sampler is missing today.** `MemorySnapshot` carries `ProcessId` and `ProcessKind` and no view association at all, which is why the code currently refuses to bill any renderer to any tab. Sole-owner attribution needs exactly one new fact: which views a renderer serves.

On Windows that fact is available in the pinned `webview2-com 0.39` without new dependencies:

- `ICoreWebView2Environment13::GetProcessExtendedInfos` returns the same process set as `GetProcessInfos` plus each renderer's `AssociatedFrameInfos`. The collection is populated for renderer processes only; non-renderers are always empty, and a renderer with no live frames can be empty too.
- `ICoreWebView2FrameInfo2::FrameId` is non-zero only when the frame came from `AssociatedFrameInfos`, and it is the same id space as the webview's own frame id.
- `ICoreWebView2_20::FrameId` gives each view its own top-level frame id, so a renderer's frame-id set intersected against the view set yields sole-owner or shared, with no URL matching. Two tabs on the same URL must not be confused, which rules out matching on `Source`.

Constraints on the implementation, both already rules elsewhere in the tree:

- `GetProcessExtendedInfos` is asynchronous with a completion handler. It must not be waited on inside a tao handler. Either complete through the handler and post the result to the event loop like `measure` already does, or go through `engine::wry_impl::pump` — see the re-entrancy note in `AGENTS.md`.
- Frame ids are a snapshot. A view created or destroyed mid-call invalidates them, so a sample that cannot be reconciled against the current view set is discarded, not patched up. A discarded sample is an existing, tested path.

**Platforms without that mapping** report shared for every view rather than guessing. The honest-unsupported rule already applies to mute and download pause; this is the same rule.

**Consequences to hold on to.** No per-tab saving is claimed, in the UI or anywhere else — a sole-owner figure is what that renderer currently holds, not what freezing the tab will return to the OS. Working sets are still never summed as unique physical RAM. The budget stays an aggregate over the engine's process set; attribution is for display and must not become an input to freeze selection, which is the registry's business.

## 1. Baseline and deterministic coverage

- [x] Read the existing policy tests before changing constants or state transitions. (No constants or transitions changed; three tests added.)
- [x] Run the relevant tests and retain results; do not infer a live platform pass from unit tests. (`cargo test memory`: 17 passed; `cargo test app::resources`: 1 passed. Live results cited separately below.)
- [x] Use controlled time for boundary tests, without sleeps or a second runtime. (All tests compute `Instant` offsets; none sleep.)

```powershell
cargo test memory
cargo test app::resources
```

- [x] Verify zero, overflow and malformed launch configuration are rejected, and the default is applied only when appropriate. (0 and `u64::MAX` rejected by test; malformed `CRYO_MEMORY_BUDGET_MIB` is a startup `anyhow` error in `lib.rs`; absent var → 1024 MiB default.)
- [x] Verify under-budget usage never triggers automatic freeze regardless of tab count. (`under_budget_never_freezes_regardless_of_tab_count` — 200 candidates, 5 samples, none frozen.)
- [x] Verify exact-budget and low-watermark boundaries, interrupted over-budget sequences and persistent pressure between the thresholds. (New `exactly_at_budget_resets_the_streak_without_lifting_pressure`: exact-budget samples never arm pressure, but inside the band an exact-budget sample still freezes while pressure persists; 90% boundary covered by `hysteresis_stops_below_low_watermark`.)
- [x] Verify 119/120-second freeze eligibility and 60-second idle-sleep timing. (119/120 asserted in the signal test; 59 s/60 s suspend boundary added to `idle_policy_protects_work_and_audio`.)
- [x] Verify deterministic oldest-unfocused selection and the behavior of equal-age candidates. (Oldest-unfocused test exists; new `equal_age_candidates_freeze_in_candidate_order` pins candidate-order — i.e. registry-order — tiebreak.)
- [x] Verify an empty eligible set returns no decision while sampling can continue. (`measured` `?`s on an empty candidate iterator; hysteresis test's first sample uses an empty set and later samples proceed.)

## 2. Protect active work

- [x] Foreground tabs and retry-download hosts are ineligible. (Both are `eligible()` rejections, exercised in the 7-signal test.)
- [x] Loading views, active downloads and paused downloads are protected. (`loading` flag + `downloading` = `InProgress | Paused`, mapped in `idle_state`; signal test covers the policy side.)
- [x] Playing audio and unknown audio state are protected; mute alone must not be assumed to mean no active audio. (`playing_audio.unwrap_or(true)` — unknown → protected; mute state is never consulted, so a muted-but-decoding tab stays protected. Verified-in-code.)
- [x] Pending password and permission prompts block automatic freeze. (`protected` includes `password_prompt.is_some() || permission_prompt.is_some()`; policy-side `protected` rejection tested.)
- [x] Non-HTTP(S) pages are excluded from automatic decisions. (`!web` → `protected`; scheme parsed per tab in `idle_state`.)
- [x] User-input reports and camera/microphone permission paths set the intended per-view protection. (Page `Edited` reports and camera/mic/other permission requests set `automatic_freeze_blocked`; input path exercised live by `live_ram_budget`, camera/mic verified-in-code.)
- [x] View destruction clears view-bound protection through the existing resource lifecycle. (`drop_surface` clears the flag and both prompts, marks transfers `Interrupted` — tested in `app::resources`.)
- [x] New, recently used and restored views get the intended unfocused timestamp and full grace period. (`adopt_view` stamps `note_unfocused` at spawn and restore; `restored_tab_cannot_be_refrozen_until_a_full_new_idle_grace_period` asserts 119 s ineligible / 125 s eligible.)
- [x] Returning a tab to the foreground and then background starts the appropriate new idle interval. (`activate` calls `note_focused` on the incoming tab (clears the timestamp) and `note_unfocused(now)` on the outgoing one — a fresh interval. Verified in `app/actions.rs`.)

Input detection is not a backup mechanism. It cannot identify every unsaved editor or embedded-frame state. Manual freeze/discard remains separate from automatic eligibility and may lose unsaved work. (README documents exactly this.)

## 3. Sampling and failure recovery

- [x] Prevent overlapping requests until the pending request completes or times out. (`begin_sample` returns `None` while a pending request is younger than `SAMPLE_TIMEOUT`; asserted in the incomplete-samples test.)
- [x] Ignore wrong-sequence, duplicate and late completions without freezing a tab or consuming the current request incorrectly. (Sequence mismatch returns early without clearing the real pending slot; `timed_out_request_can_be_replaced_without_accepting_its_late_reply` and `incomplete_stale_and_duplicate_samples_cannot_freeze`.)
- [x] Reject empty, incomplete and stale snapshots rather than treating unavailable memory as zero. (`total_private_bytes` → `Option`; `None` → `sample_failed`. App adapter additionally requires non-empty process list and <30 s age.)
- [x] Verify the exact 30-second freshness boundary in both the app adapter and policy. (Policy: `>= SAMPLE_TIMEOUT` → `sample_failed` (boundary-inclusive, tested via `later + SAMPLE_TIMEOUT`). Adapter: `age < 30s` via `checked_duration_since` — a future-dated snapshot also fails. Verified-in-code on the app side.)
- [x] Verify failed samples reset pressure safely and allow future sampling. (`sample_failed` clears pending, over-streak, no-reclaim streak and pressure; `!p.pressure` asserted; `begin_sample` still issues new sequences.)
- [x] Test sampling failure immediately after a freeze, followed by recovery. Decide whether the retained pre-freeze baseline and timers still produce the intended reclaim/backoff behavior. (New test `a_failed_sample_after_freeze_keeps_the_reclaim_baseline` pins the decision: **retaining `before_freeze` is intentional** — the next complete sample still answers "did that freeze reclaim anything" against the pre-freeze baseline; pressure state resets so no unsafe freezing happens mid-gap. Verified: a 5 MiB reclaim against the baseline does not start the empty-delta streak.)
- [x] Verify unsupported or failed engine accounting leaves manual tab operations usable. (`measure_memory` error → `sample_failed` + `report!`; `freeze_tab`/`discard_tab` never consult the enforcer. The servo backend hits the default error path by design.)
- [x] Keep process enumeration and stat reads off the event-loop thread; deliver completion through the existing app event path. (Named `cryo-memory` worker on all adapters — tests assert worker ≠ caller; completion posts `AppEvent::MemoryMeasured` through `EventLoopProxy`.)

## 4. Freeze pacing and reclaim feedback

- [x] A decision selects one tab, not a batch; the app executes the real freeze path and updates chrome. (`measured` → `Option<TabId>`; `on_memory_measured` runs the same `freeze_view` + `sync_chrome` as manual freeze.)
- [x] No further freeze occurs during the 15-second settle interval. (`next_freeze`/`next_sample` deferred by `FREEZE_SETTLE`; `begin_sample` throttled — asserted.)
- [x] One low-reclaim outcome still permits the next eligible candidate after settling, allowing shared-renderer siblings to be released. (`one_empty_delta_still_freezes_the_next_candidate_on_schedule`.)
- [x] Two consecutive low-reclaim outcomes trigger the 60-second backoff. (`consecutive_empty_deltas_back_off` — including resumption after the backoff elapses.)
- [x] A sufficient reclaim outcome resets the low-reclaim streak. (`else { no_reclaim = 0 }`; exercised in the hysteresis test's 110→95 reclaim.)
- [x] Memory growth after a freeze does not underflow into a fictitious saving. (`saturating_sub` → growth counts as a 0-byte, i.e. low-reclaim, delta; the empty-delta tests drive 111/112 MiB follow-ups — growth — and get honest no-reclaim behavior.)
- [x] Verify behavior when the selected freeze fails: the policy must not permanently stall or claim the view was destroyed. Review whether decision-time feedback bookkeeping needs adjustment. (Reviewed: `freeze_view` runs on the same event-loop tick as `measured`, so the candidate's view cannot disappear between selection and execution — failure is effectively unreachable. If it did error, `?` propagates, `sync_chrome` is skipped (no claimed destruction), and the retained `before_freeze` baseline makes the next sample record an honest no-reclaim rather than stalling. No adjustment needed.)
- [x] Restored views cannot immediately enter a freeze/restore loop under sustained pressure. (Full 120 s grace on restore — dedicated test; one-freeze-per-decision pacing bounds churn regardless.)

Aggregate memory changes include unrelated engine activity. Treat a measured delta as feedback, not proof that a particular tab exclusively owned those bytes. (Module doc and README say this; probe output reports deltas, never per-tab costs.)

## 5. Lighter idle sleep versus freeze

- [x] Verify idle sleep uses the protection observations and does not target foreground or active-work views. (`should_suspend` reuses the same `eligible()` checks — foreground, loading, downloads, audio, protected all block.)
- [x] Verify already-suspended views are not repeatedly requested to sleep unnecessarily. (`!self.suspended` gate; assertion added to `idle_policy_protects_work_and_audio`.)
- [x] Verify activation resumes a sleeping view before use. (`activate` calls `view.resume()` on the incoming tab, errors reported not fatal — `app/actions.rs`. Verified-in-code.)
- [x] Confirm unsupported suspension behaves according to the engine contract and does not imply measured savings. (Contract: `suspend` is a no-op where the engine can't suspend, `is_suspended` answers `false` — errors are logged, no accounting is inferred from it.)
- [x] Keep the distinction explicit: suspension retains the view; freeze captures the supported snapshot and destroys it. (Separate paths: `suspend_idle_views` vs `freeze_view`; only freeze drops the view and produces a snapshot.)

## 6. Live integration evidence

Use the fixtures and environment instructions in `AGENTS.md`. Run probes separately:

```powershell
cargo test live_ram_budget -- --ignored --nocapture

$env:CRYO_FREEZE_CASE = "shared"
cargo test live_freeze_memory -- --ignored --nocapture
Remove-Item Env:CRYO_FREEZE_CASE -ErrorAction SilentlyContinue
```

- [x] Reuse existing Windows evidence where the tested policy is unchanged; rerun affected probes after behavioral changes. (Policy code unchanged this pass — only tests added — so the 2026-10-25 `live_ram_budget` and `live_freeze_memory` runs stand.)
- [x] Confirm real sampling triggers automatic freeze under the test budget while the foreground and protected work remain live. (`live_ram_budget` passed: automatic freeze under 128 MiB, "protected work remained live under continuing pressure".)
- [x] Confirm restore uses the normal spawn path and restored-tab grace is covered deterministically. (Probe: "live restore rehydrated the frozen tab" via `restore_view`/`spawn_content_view`; grace covered by `restored_tab_cannot_be_refrozen…`.)
- [x] Retain complete aggregate process measurements, including shared-renderer behavior. Default same-origin pages do not guarantee a shared renderer; follow the opt-in forced-sharing instructions in `AGENTS.md` when testing that case. (Default shared run: separate renderers, −175.7 MiB for both. Forced `--process-per-site --renderer-process-limit=1` run: shared ~160 MiB renderer survived one freeze (−69.1 MiB), exited when both froze (−158.6 MiB). Env var was process-scoped and cleared.)
- [x] Exercise continued over-budget operation when every remaining tab is protected. The app must remain responsive without bypassing protections. (`live_ram_budget` held protected work live for 20 s of continuing pressure; empty-candidate path returns no decision while sampling continues.)
- [x] Obtain real WebKit evidence on Linux and macOS before claiming those platforms are live-verified. (Outstanding — needs a Linux and a macOS machine; only Windows is live-verified.)

## Completion criteria

- [x] Budget thresholds, sequencing, time boundaries and failure recovery have deterministic coverage. (17 policy tests including new boundary, equal-age and failed-sample-after-freeze cases.)
- [x] App observation mapping preserves every protection signal and unknown-state fallback. (`idle_state` maps all eight signals; unknown audio → protected, unknown suspension → not suspended.)
- [x] Real-backend evidence demonstrates automatic freeze without sacrificing the supported protected work. (`live_ram_budget` on real WebView2.)
- [x] Timing and backoff descriptions in README and the other checklists agree with the tested policy. (README's "less than 1 MiB → backs off 60 s" is preceded by "then waits 15 seconds and remeasures" — it describes the streak outcome, consistent with the two-outcome code path; both checklists now cite the two-consecutive-empty-delta rule explicitly.)
- [x] Unsupported sampling and unreclaimable overhead degrade without unsafe fallback freezing or a fixed live-view cap. (`sample_failed` resets; no-reclaim backoff stops churn; no live-view cap exists.)
- [x] After code changes, run `cargo fmt --check`, `cargo check`, `cargo clippy -- -D warnings`, `cargo build` and `cargo test`. (All green 2026-10-25: fmt clean, clippy `-D warnings` clean, 298 tests passing.)

## Verification notes

Tests added this pass (`src/memory/budget.rs`, `src/memory/mod.rs`):

- `exactly_at_budget_resets_the_streak_without_lifting_pressure` — exact-budget never arms enforcement; band pressure survives an exact-budget sample.
- `equal_age_candidates_freeze_in_candidate_order` — ties break by candidate (registry) order, not id.
- `a_failed_sample_after_freeze_keeps_the_reclaim_baseline` — pins the retained-`before_freeze` decision: reclaim is still judged against the pre-freeze baseline after a failed sample.
- `idle_policy_protects_work_and_audio` extended — 59 s suspend boundary and the `suspended` re-request gate.

Decisions recorded (no code change needed):

- **Retained `before_freeze` after `sample_failed`**: pressure bookkeeping resets, but reclaim feedback is still measured against the last decision's baseline whenever the next complete sample lands — a missed sample must not erase the question "did that freeze work?".
- **Freeze-failure feedback**: unreachable on the single event-loop thread between selection and execution; if it ever erred, the retained baseline degrades to an honest no-reclaim outcome — no stall, no claimed destruction.

Outstanding on other platforms: `cargo` gates and live WebKit sampling/budget evidence on Linux and macOS. Everything verified-in-code above (audio/mute independence, camera/mic protection flag, resume-on-activate, adapter freshness) is noted as lacking a deterministic harness rather than silently claimed as tested.
