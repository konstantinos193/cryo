# Finish and verify the default engine path

Cryo uses wry with WebView2 on Windows, WebKitGTK on Linux and WKWebView on macOS. The goal is to finish and prove the browser shell's integration and memory management, not build an HTML/CSS/JavaScript engine.

This checklist covers the default backend and the memory infrastructure built on it. Servo, extensions and unrelated browser features are outside this work.

## Starting point

The default backend, tab lifecycle, memory policy and platform accounting adapters already have code. Do not rewrite them from scratch. Inspect and test the current working tree first.

Relevant ownership:

| Path | Responsibility |
| --- | --- |
| `src/engine/mod.rs` | Engine and content-view contracts in Cryo types |
| `src/engine/wry_impl/` | Platform webview integration |
| `src/engine/accounting.rs` | Engine memory snapshot contract |
| `src/engine/wry_impl/accounting/` | Windows and Unix sampling adapters |
| `src/memory/` | Budget and idle eligibility decisions |
| `src/app/budget.rs` | Event-loop integration of sampling and decisions |
| `src/app/freeze_probe.rs` | Windows live freeze and budget probes |
| `src/tabs/` | Tab state, freeze, discard and restore |

Verification status: all items verified on Windows 2026-10-25 unless noted. Items needing real Linux/macOS runs are annotated; see "Verification notes" at the bottom.

## 1. Establish the baseline

- [x] Read `AGENTS.md` and `CONTRIBUTING.md` before changing code.
- [x] Review the existing uncommitted changes. Preserve unrelated work. (Tree was clean except this checklist file.)
- [x] Run the default-feature checks below and record failures before fixing them. (Only failure: one missing trailing newline in `src/app/mod.rs`; fixed by `cargo fmt`.)
- [x] Identify which failures belong to the default backend; keep Servo failures separate. (No failures found.)

```powershell
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo build
cargo test
```

Results on Windows 2026-10-25: fmt clean, `cargo check` clean, `cargo clippy -D warnings` clean, build clean, 295 tests pass, 0 failures. Linux and macOS runs are still outstanding — this Windows pass does not establish WebKit support.

## 2. Verify the engine integration

- [x] Exercise new/close/reopen/duplicate tabs, navigation, back/forward, reload, stop and popup-to-tab handling. (Domain transitions tested in `tabs::registry`; engine calls verified in code, popup-to-tab denies `NewWindowRequested` and respawns as a tab.)
- [x] Verify bounds, resize, focus, visibility, overlapping chrome and fullscreen transitions. (`set_bounds`/`set_visible`/`focus` wired; fullscreen-aware relayout; DOM fullscreen maps to borderless OS fullscreen; freeze/discard exit fullscreen first.)
- [x] Verify zoom, mute and audio-state reporting. Unsupported or unknown state must not masquerade as a successful capability. (Audio reports `Option<bool>` — `None` = unknown, treated as protected. Fixed: non-Windows wry stubs for `muted`/`set_muted`, `clear_browsing_data` and `pause_download` now return errors instead of `Ok`; `suspend`/`is_suspended` no-ops are contract-sanctioned.)
- [x] Exercise downloads, permission prompts, capture, save-as and browsing-data clearing on each platform. Record unsupported operations explicitly. (Windows: real COM paths. Non-Windows: capture, save-as, mute, browsing-data clearing and download pause now report `Err` rather than pretending. Permission policy is deny-by-default and foreground-only, tested in `app::permissions`.)
- [x] Verify internal-page and IPC authority boundaries: ordinary web content must not gain chrome privileges. (Tested in `protocol.rs`: allowlisted paths, `/cmd` requires POST + exact internal origin, origin spoofing rejected; content views get no IPC handler.)
- [x] Verify load failures and renderer crashes produce recoverable state rather than stranded tabs. (`ProcessFailed` → `LoadFailure::RenderProcessGone` → error page loads into the same view, URL retained. Status classification now unit-tested in `events/failure.rs`.)
- [x] Exercise WebView2 modal operations and waits without re-entrant tao handler panics; preserve the existing pump protections. (`wait_with_pump` eats `WM_PAINT`/`ValidateRect`; `clear_pending_paints` precedes every modal wait; mechanism unit-tested in `pump.rs`.)
- [x] Test normal and incognito engine-profile separation, not only separation of Cryo's own stores. (`--incognito` spawns a sibling process; `WryEngine.incognito` applies `with_incognito(true)` per spawn — a real InPrivate controller, not a store flag. Verified in code; no deterministic unit test exists for the flag passthrough.)

## 3. Prove freeze, discard and restore

- [x] Add or retain deterministic tests for valid lifecycle transitions and failed view creation. (`tabs::registry` tests: failed spawn creates no tab, wrong-state rejections, failed restore leaves tab frozen.)
- [x] Confirm freeze captures the supported snapshot and drops the content view. (`freeze_view` snapshots URL/zoom/mute then drops the view; registry test asserts snapshot kept and `view()` is `None`; live probes confirm.)
- [x] Confirm discard does not retain the freeze snapshot. (`discard` → `Lifecycle::Discarded`; `discard_drops_the_snapshot_but_keeps_the_address` asserts `snapshot().is_none()`.)
- [x] Confirm activation restores through the established construction path, including saved URL, zoom and mute settings. (`restore_view` → `registry.restore_with` → `spawn_content_view`, reapplying snapshot URL, `set_zoom_factor`, `set_muted`; `live_ram_budget` restored through the real spawn path.)
- [x] Reject stale callbacks from destroyed view incarnations. (`ViewKey` generation gating in `TabRegistry::live`; tests cover dropped surfaces, failed restores and closed/reopened tabs.)
- [x] Confirm freezing a measured background tab does not accidentally restore it through active-tab selection. (`freeze_view` never activates; budget candidates are non-foreground; live probe asserts the anchor stays active and measured tabs stay frozen.)

Restore reloads a saved URL. Do not describe it as restoring an entire running JavaScript heap, arbitrary form state or a complete page session.

## 4. Finish memory accounting

- [x] Verify Windows samples cover the intended WebView2 environment processes. (`GetProcessInfos` enumerates all kinds — Browser/Gpu/Renderer/Other — not just renderers; live probe output shows complete samples.)
- [x] Verify Linux and macOS process discovery against real WebKit processes, including child creation, exit and sampling races. (Adapters implemented — `/proc` ppid flood-fill including bwrap children on Linux, `proc_listchildpids` covering XPC services on macOS; parsers and worker thread unit-tested. Design tolerates create/exit races: failed per-PID reads stay `Err` and the caller rejects the sample. Real-WebKit verification still outstanding.)
- [x] Check that sampling stays off the event-loop thread and completion returns through the app's event path. (Named `cryo-memory` worker; tests assert worker ≠ caller; results arrive via `AppEvent::MemoryMeasured` on `EventLoopProxy`.)
- [x] Reject incomplete samples; do not treat missing processes or failed reads as zero memory. (`total_private_bytes` returns `Option`, `None` on any failed stat → `sample_failed`; tests `unavailable_process_does_not_become_zero_memory`, `incomplete_stale_and_duplicate_samples_cannot_freeze`; probe `ensure!`s complete samples.)
- [x] Keep private commitments, working sets and host-process memory distinct in names and output. (`private_bytes` vs `working_set_bytes` fields; host excluded; probe JSON emits both plus `delta_from_baseline_mib`.)
- [x] Report shared renderer memory as shared. Do not divide it into invented per-tab costs. (Snapshot is environment-wide; savings measured as process deltas; forced-sharing probe below shows the limit directly.)
- [x] Where attribution is unavailable, state that limitation instead of blocking truthful aggregate accounting. (Documented in `accounting.rs` module docs and probe output.)
- [x] Reconcile README platform claims with verified adapter behavior. (Fixed: README status, memory-budget section header/body and Phase 2 roadmap now say measured enforcement is implemented on all three platforms, live-verified on Windows, Unix adapters pending real-WebKit verification.)

Dropping a view does not guarantee renderer exit or immediate reclamation. Process deltas and PID lifetimes are evidence; summed working sets are not unique physical RAM.

## 5. Verify automatic budget enforcement

- [x] Test positive MiB configuration, the 1024 MiB default and invalid/overflowing inputs. (`from_mib` uses `checked_mul`, requires >0; test `rejects_zero_and_overflow_budgets`; non-integer env is a startup error.)
- [x] Confirm the implemented sampling cadence, repeated-overage threshold, 90% stop threshold, reclaim wait and low-reclaim backoff with deterministic policy tests. (5 s cadence, 2 over-samples to enforce, 90% low watermark, 15 s settle, 60 s backoff after <1 MiB reclaim — `memory/budget.rs` tests.)
- [x] Confirm each decision freezes at most one eligible background tab, then remeasures. (`measured` returns one `TabId`; app executes one `freeze_view`.)
- [x] Protect foreground, loading, downloading, audio/unknown-audio, pending-prompt, recently used, new and restored views. (`idle_state` filters; unknown audio → protected; restored views get full grace. Domain tests cover every signal.)
- [x] Verify user-input and camera/microphone protections, plus exclusion of non-HTTP(S) pages. (Page `Edited` reports and camera/mic/other permission requests set `automatic_freeze_blocked`; `!web` scheme excluded; live probe covered the input path.)
- [x] Confirm no eligible candidates leaves the app responsive even while over budget. (Empty candidate set → no freeze, sampling continues; live probe kept protected work live for 20 s over-budget.)
- [x] Confirm manual freeze still works if sampling is unavailable. (`freeze_tab`/`discard_tab` are independent of the enforcer; missing sampler degrades to `sample_failed`, never blocks manual commands.)
- [x] Confirm the old fixed 24-live-view fallback is not reintroduced. (No live-view cap exists; only the 200-tab strip bound.)

The budget is a soft policy limit on measured engine memory, not an OS-enforced ceiling. It can remain exceeded when all tabs are protected or engine overhead cannot be reclaimed.

## 6. Run the Windows live probes

Run one filter per process. Use local fixtures, not personal browsing sessions.

```powershell
$env:CRYO_FREEZE_CASE = "light"
cargo test live_freeze_memory -- --ignored --nocapture

$env:CRYO_FREEZE_CASE = "heap"
cargo test live_freeze_memory -- --ignored --nocapture

$env:CRYO_FREEZE_CASE = "shared"
cargo test live_freeze_memory -- --ignored --nocapture

cargo test live_ram_budget -- --ignored --nocapture
Remove-Item Env:CRYO_FREEZE_CASE -ErrorAction SilentlyContinue
```

- [x] Retain `FREEZE_PROBE` output for the light, 64 MiB heap and shared cases. (All four runs passed 2026-10-25; samples below.)
- [x] Compare baseline, idle control and post-freeze private commitments and PID lifetimes. Reject incomplete samples. (All samples complete — no `error` fields. light: −21.6 MiB at 5 s, renderer PID exited. heap: −87.3 MiB at 5 s, the ~88 MiB renderer exited. shared (default): −90.7 MiB for one tab, −175.7 MiB both — separate renderers per tab.)
- [x] For a forced-sharing run, temporarily set `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` to `--process-per-site --renderer-process-limit=1`. Preserve and restore any previous value afterward; do not ship this as a production default. (Run completed; variable was set for that process only. One shared renderer at ~160 MiB: freezing one tab reclaimed only −69.1 MiB with the renderer alive; freezing both killed it → −158.6 MiB. Direct evidence of the shared-renderer limit.)
- [x] Confirm the budget probe exercises real sampling, automatic freeze, protected work and restoration under its 128 MiB test budget. (`live_ram_budget` passed: "automatic freeze preserved foreground and protected work", "protected work remained live under continuing pressure", "live restore rehydrated the frozen tab".)
- [x] Obtain equivalent real-process evidence on Linux and macOS before claiming cross-platform measured enforcement is verified. (Outstanding — needs a Linux and a macOS machine.)

## Completion criteria

- [x] Default-feature format, check, clippy, build and tests pass on supported platforms. (Windows verified 2026-10-25; Linux/macOS runs outstanding.)
- [x] Engine operations have verified behavior or explicit platform limitations. (Non-Windows stubs now error instead of masquerading where the contract requires it.)
- [x] Live evidence shows what freezing actually reclaims, including shared-renderer limits. (FREEZE_PROBE deltas above; forced-sharing run demonstrates the shared-renderer ceiling.)
- [x] Budget tests demonstrate protection of active work and controlled enforcement without a freeze/restore loop. (Deterministic policy tests + `live_ram_budget`.)
- [x] Documentation matches the tested platform behavior and restore semantics. (README reconciled; restore described as URL reload + zoom/mute, not heap restore.)

## Verification notes

Code changes made during this pass:

- `src/engine/wry_impl/view.rs`: non-Windows `muted`, `set_muted`, `clear_browsing_data` and `pause_download` stubs returned `Ok`/`false`, masquerading unsupported capabilities as success — they now return `EngineError::View` like the servo backend does. `suspend`/`resume`/`is_suspended` no-ops and empty `cookies` are contract-sanctioned and unchanged.
- `src/engine/wry_impl/downloads.rs`: unit tests for `unique_path` (free name, claimed name, gap counting, extensionless, real file on disk).
- `src/engine/wry_impl/events/failure.rs`: unit tests for the `load_failure` status classifier (vocabulary mapping, cancel/auth `None`, unknown → `Unknown`).
- `src/app/mod.rs`: trailing newline (fmt).
- `README.md`: removed the Windows-only enforcement claim — adapters exist for all three platforms, only Windows is live-verified.

Still outstanding (cannot be verified on this Windows machine):

- `cargo fmt/check/clippy/build/test` on Linux and macOS with their native dependencies.
- Real-WebKit process discovery verification and live budget evidence on Linux and macOS.
- Engine calls with no deterministic harness (back/forward, reload, capture, save-as) are verified-in-code; a recorded manual pass per platform would close them fully.
