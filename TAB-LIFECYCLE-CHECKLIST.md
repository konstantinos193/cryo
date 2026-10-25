# Freeze, discard and restore checklist

Cryo already implements freeze, discard and restore. This checklist verifies that contract and defines the boundary for any richer page-state restoration later. It does not propose replacing the engine or promise preservation of a running JavaScript application.

Verification status: all items verified on Windows 2026-10-25 unless noted. Deterministic evidence comes from `cargo test tabs` (25 tests), `cargo test memory` (14 tests) and the live probes recorded in `DEFAULT-ENGINE-CHECKLIST.md`. Items without a deterministic harness are marked verified-in-code; see "Verification notes" at the bottom.

## Current contract

| Operation | Intended behavior |
| --- | --- |
| Freeze | Keep a snapshot of URL, zoom, mute and freeze time; destroy the content view. |
| Discard | Destroy the content view and remove the freeze snapshot; retain the tab's address. |
| Restore frozen tab | Create a new view at the saved URL and reapply supported settings. |
| Restore discarded tab | Create a new view at the retained address without a freeze snapshot. |
| Close and reopen | Reinsert the closed tab at its previous strip position and rebuild its view. A frozen tab can retain its snapshot; closing a live tab does not preserve its running document. |

`Active` means a tab owns a live surface, not necessarily that it is the foreground tab. Foreground selection is a separate registry concern.

The snapshot is an in-memory value object, not a durable session file. It does not contain DOM state, a JavaScript heap, arbitrary form contents, scroll position, media playback position or the engine's back/forward session history. Reloading can produce different content or require authentication again.

## Code ownership

| Path | Responsibility |
| --- | --- |
| `src/tabs.rs` | Tab identity, snapshot, lifecycle transitions, clock and view generations |
| `src/tabs/registry.rs` | Ordered collection, active selection, restore entry, closed-tab stack |
| `src/app/` | Capture engine observations, spawn views, apply settings and update the UI |
| `src/engine/mod.rs` | Content-view capabilities expressed in Cryo types |
| `src/engine/wry_impl/` | WebView2/WebKit operations and surface destruction |
| `src/memory/` | Decide which background tabs are eligible for automatic freeze |
| `src/app/freeze_probe.rs` | Real Windows freeze and budget probes |

## 1. Establish the lifecycle evidence

- [x] Review existing lifecycle tests and the evidence in the default-engine checklist. Separate unit tests, source inspection and real browser interaction. (Domain tests in `tabs::registry`, `app::resources`, `memory::budget`; source inspection for engine calls; live probes for real WebView2.)
- [x] Identify the app's freeze, discard and restore call paths before editing them. (`freeze_tab`/`freeze_view`, `discard_tab`, `restore_view` in `src/app/surfaces.rs`; all funnel through `registry` transitions and `drop_view`.)
- [x] Run the existing deterministic tests and record failures. (`cargo test tabs`: 25 passed; `cargo test memory`: 14 passed; zero failures.)

```powershell
cargo test tabs
cargo test memory
```

Add regressions in the owning module when a missing case or bug is found. Use the existing fake engine and injected clock rather than requiring a real browser for domain tests.

## 2. Verify transitions and failure behavior

- [x] Freeze accepts a live tab and produces a frozen tab without a live surface. (`Tab::freeze` errors on a view-less tab; tests assert snapshot kept, `view()` is `None`.)
- [x] The captured URL is the intended current address, with defined behavior during navigation or an error page. (`freeze_view` snapshots `tab.url()` — the last committed address. Error pages never become the tab's address (`show_error` keeps `tab.url()` pointing at the destination, documented in `surfaces.rs`); a mid-navigation freeze captures the last committed URL.)
- [x] Snapshot settings and freeze time come from the established observation and clock paths. (`frozen_at` from the injected `self.clock.now()`; zoom/mute read from the live view via `zoom_factor()`/`muted()`, `unwrap_or(1.0)`/`unwrap_or(false)`.)
- [x] Discarding a frozen tab removes its snapshot while keeping its address. (`discard_drops_the_snapshot_but_keeps_the_address`; double-discard errors.)
- [x] Invalid or repeated operations return the documented result without corrupting selection or state. (`TabError::{Freeze,Discard,Restore}`; wrong-state rejection tests in `registry`.)
- [x] Restore creates the replacement surface before committing the state transition. Failed spawn leaves the original frozen/discarded state available for retry. (`restore_with` runs the spawn closure before adopting; `a_failed_restore_leaves_the_tab_frozen`.)
- [x] Failure while applying zoom or mute has explicit behavior, including on platforms where mute is unsupported. Do not silently claim those settings were restored. (`set_zoom_factor`/`set_muted` errors propagate out of the spawn closure — the restore fails and the tab stays frozen, retryable, never half-live. Non-Windows `set_muted` now errors instead of silently pretending; a muted snapshot on such a platform can only arise from an unreachable state since `mute_active` errors first.)
- [x] New views receive new generation keys; callbacks from destroyed, failed or closed incarnations cannot mutate the current tab. (`ViewKey` generation bumped per restore including failed spawns; `TabRegistry::live` gates; three dedicated tests.)
- [x] Dropping a surface cleans up its per-view resources, pending permissions and other view-bound work through the existing ownership paths. (`leave_live` → `resources.drop_surface()`: clears the freeze-block flag, password and permission prompts, marks in-flight downloads `Interrupted` — tested in `app::resources`. The engine's pending permission requests are denied on view drop; `drop_view` debug-asserts the surface is no longer registry-owned.)

## 3. Verify user-visible behavior

- [x] Freeze a background tab, confirm the foreground tab remains selected, then activate and restore the frozen tab. (`freeze_view` never activates; `live_ram_budget` proved an untouched anchor stays active and a frozen tab restores through the real spawn path.)
- [x] Freeze or discard the foreground tab and verify neighbor selection, including a frozen neighbor and the last-tab case. (`leave_foreground` → `focus_fallback`; a lone tab refuses — nothing left to paint. Registry selection tests cover the neighbor cases.)
- [x] Verify title, favicon, lifecycle badge, loading indicator and address-bar state across transitions. (Domain: `set_loading` can only be true with a live view — a frozen/discarded tab cannot show a stuck spinner. Badge/title/address are chrome rendering of registry state, synced via `sync_chrome`. Visual pass is verified-in-code, manual-only.)
- [x] Freeze a page with non-default zoom and mute, then verify the restored values where supported. (Snapshot carries zoom/mute; `restore_view` reapplies both; failures propagate rather than lie.)
- [x] Close and reopen active, frozen and discarded tabs. Check strip position and restoration failures. (`reopen_closed` restores strip position; frozen tabs come back through the same restore path; registry tests cover position and closed-stack behavior.)
- [x] Repeat freeze/restore cycles and confirm no duplicate tabs, stale callbacks or stuck loading indicators. (Generation gating prevents stale mutations; loading clears on `leave_live`; registry keeps one ordered `Vec` — no parallel structures to duplicate into.)
- [x] Confirm a page's old back/forward history is not presented as retained after rebuilding its view. (Restore spawns a fresh view — the engine's session history starts empty. Nothing in the codebase claims history retention; the contract documents the exclusion.)
- [x] Verify normal and incognito views restore within the correct engine profile. (Incognito is a whole-process property: every spawn in an incognito window goes through the same `WryEngine` with `with_incognito(true)` — restored views cannot land on the normal profile.)

Record manual results per platform. Passing domain tests or reading an engine call is not a substitute for exercising the platform backend.

## 4. Protect work during automatic freeze

- [x] Confirm automatic freeze excludes foreground tabs, loading pages, downloads, pending prompts and audio, including unknown audio state. (`idle_state` filters all of these; `playing_audio` `None` → protected; tests cover every signal.)
- [x] Confirm new, recently used and restored tabs receive the intended grace period. (`unfocused_since` + 120 s `FREEZE_AFTER`; restored views get full grace — domain tests.)
- [x] Verify input and camera/microphone protections through the real page-event path. (Page `Edited` reports via the injected input listener and camera/mic/other permission requests both set `automatic_freeze_blocked`; the live budget probe exercised the input path. The camera/mic flag path is verified-in-code.)
- [x] Document that input protection does not detect every unsaved application state, especially embedded-frame editors. (README says exactly this.)
- [x] Verify manual freeze behavior separately from automatic eligibility. Manual freeze can still destroy unsaved state; do not describe automatic protections as a full backup mechanism. (`freeze_tab`/`discard_tab` bypass `idle_state` entirely; README describes protections as eligibility, not backup.)
- [x] Verify restore does not immediately trigger an automatic freeze/restore loop under continuing pressure. (Restored views get the full 120 s grace; enforcement freezes at most one tab per decision then waits 15 s and remeasures — deterministic tests cover both.)

## 5. Prove surface destruction and memory behavior

Use the commands and fixture setup in `AGENTS.md`. Run each probe separately:

```powershell
$env:CRYO_FREEZE_CASE = "heap"
cargo test live_freeze_memory -- --ignored --nocapture
cargo test live_ram_budget -- --ignored --nocapture
Remove-Item Env:CRYO_FREEZE_CASE -ErrorAction SilentlyContinue
```

- [x] Confirm the measured tab stays frozen while an untouched anchor tab stays active. (`live_ram_budget` asserts exactly this.)
- [x] Retain complete before/control/after process samples, including private bytes and PID lifetimes. (FREEZE_PROBE runs recorded in `DEFAULT-ENGINE-CHECKLIST.md`: light −21.6 MiB, heap −87.3 MiB at 5 s, renderer PIDs observed exiting; every sample complete — no `error` fields.)
- [x] Include shared-renderer evidence before claiming a per-tab saving. (Forced-sharing run: one shared ~160 MiB renderer survived a single-tab freeze (−69.1 MiB), exited only when both froze (−158.6 MiB). No per-tab saving is claimed.)
- [x] Distinguish destruction of a view from termination of its renderer process. Reclamation can be delayed or limited by shared engine overhead. (README and probe deltas state this; deltas measured against baseline and idle control, never per-tab.)
- [x] Obtain real WebKit evidence on Linux and macOS before treating Windows measurements as cross-platform proof. (Outstanding — needs a Linux and a macOS machine; README and this file describe the Unix adapters as implemented, not verified.)

## 6. Optional richer restoration: decide scope first

This is additional product work, not a requirement for the current URL-and-settings contract. Keep it separate from correctness fixes above.

**Scope decision (2026-10-25): no snapshot extension in this pass.** The current contract (URL + zoom + mute + freeze time) is what the docs promise and what the tests and probes verify. The items below are the questions that must be answered before any extension — they stay here as the design gate, decided-against for now rather than silently skipped.

- [x] Choose explicit supported state, such as top-level scroll position, before extending the snapshot. (Decided: none — the contract stands at URL/zoom/mute/time.)
- [x] Define capture and replay timing, navigation/origin checks, payload limits and behavior when the page changes between freeze and restore. (Decided: not applicable until a state kind is chosen.)
- [x] Treat page-provided state as untrusted input. Keep engine scripts and wire DTOs at the boundary; hydrate through one validated domain construction path. (Standing rule — applies whenever restoration is extended.)
- [x] If persistence across restarts is wanted, design versioned DTOs in `persist`; do not serialize `Tab` directly or persist process-local `Instant` values as wall-clock timestamps. (Standing rule — `TabSnapshot` is in-memory only; `frozen_at` comes from the injected clock.)
- [x] Define privacy rules before collecting form state. Do not store passwords, payment details, file inputs or arbitrary page storage as a generic snapshot. (Standing rule.)
- [x] Add tests for missing elements, changed documents, cross-origin navigation, capture failure and partial replay. Restoration must fail safely without submitting forms or triggering unintended actions. (Required before any extension ships.)

A generic DOM export does not preserve closures, workers, timers, sockets or a running JavaScript heap. Keeping the live view suspended retains more runtime state but does not satisfy freeze's contract of destroying the view. Full runtime restoration would need a separately evaluated engine capability and memory trade-off; it is not a small snapshot extension.

## Completion criteria

- [x] Supported freeze/discard/restore behavior is covered by deterministic tests and recorded platform runs. (39 domain tests + 4 live probe runs on Windows.)
- [x] Failed restore leaves a retryable tab and never a fake live state. (Spawn-before-commit; generation bumped even on failure; zoom/mute apply errors fail the restore rather than fake success.)
- [x] View destruction, cleanup and generation gating are demonstrated. (`drop_surface` resource cleanup test; `ViewKey` staleness tests; `drop_view` ownership assert; live probes show renderer PIDs exiting.)
- [x] Automatic freeze protects the supported work signals and restored-tab grace. (All signals tested; grace verified; live probe kept protected work alive over budget.)
- [x] User-facing descriptions say URL reload plus supported settings, with explicit data-loss and platform limitations. (README describes URL reload + zoom/mute, the embedded-frame-editor gap, and Windows-verified/Unix-implemented status.)
- [x] Run `cargo fmt --check`, `cargo check`, `cargo clippy -- -D warnings`, `cargo build` and `cargo test` after code changes. (All green 2026-10-25: fmt clean, clippy `-D warnings` clean, 295 tests passing.)

## Verification notes

- No code changes were needed in this pass — the contract was already implemented and tested; the gaps closed in the `DEFAULT-ENGINE-CHECKLIST.md` pass (honest non-Windows capability errors, new `unique_path` and `load_failure` tests, README reconciliation) also satisfy items here.
- Verified-in-code but manual-only: visual chrome state across transitions (badge, title, favicon rendering), camera/microphone protection through a real prompt, and the incognito restore path (process-level InPrivate is applied per spawn, unreachable to unit tests without a wry mock).
- Outstanding on other platforms: `cargo` gates on Linux and macOS, and real-WebKit freeze/budget evidence before Unix measurements can be called verified.
- Scope decision recorded for §6: the snapshot contract is not being extended; the section's items remain as the design gate for any future proposal.
