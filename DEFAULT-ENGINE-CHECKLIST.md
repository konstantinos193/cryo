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

Unchecked items mean verification or follow-up is required; they do not mean the feature has no implementation. No fresh build or live test results are claimed by this document.

## 1. Establish the baseline

- [ ] Read `AGENTS.md` and `CONTRIBUTING.md` before changing code.
- [ ] Review the existing uncommitted changes. Preserve unrelated work.
- [ ] Run the default-feature checks below and record failures before fixing them.
- [ ] Identify which failures belong to the default backend; keep Servo failures separate.

```powershell
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo build
cargo test
```

Run these on Windows, Linux and macOS with their required native dependencies. A passing Windows build does not establish WebKit support.

## 2. Verify the engine integration

- [ ] Exercise new/close/reopen/duplicate tabs, navigation, back/forward, reload, stop and popup-to-tab handling.
- [ ] Verify bounds, resize, focus, visibility, overlapping chrome and fullscreen transitions.
- [ ] Verify zoom, mute and audio-state reporting. Unsupported or unknown state must not masquerade as a successful capability.
- [ ] Exercise downloads, permission prompts, capture, save-as and browsing-data clearing on each platform. Record unsupported operations explicitly.
- [ ] Verify internal-page and IPC authority boundaries: ordinary web content must not gain chrome privileges.
- [ ] Verify load failures and renderer crashes produce recoverable state rather than stranded tabs.
- [ ] Exercise WebView2 modal operations and waits without re-entrant tao handler panics; preserve the existing pump protections.
- [ ] Test normal and incognito engine-profile separation, not only separation of Cryo's own stores.

## 3. Prove freeze, discard and restore

- [ ] Add or retain deterministic tests for valid lifecycle transitions and failed view creation.
- [ ] Confirm freeze captures the supported snapshot and drops the content view.
- [ ] Confirm discard does not retain the freeze snapshot.
- [ ] Confirm activation restores through the established construction path, including saved URL, zoom and mute settings.
- [ ] Reject stale callbacks from destroyed view incarnations.
- [ ] Confirm freezing a measured background tab does not accidentally restore it through active-tab selection.

Restore reloads a saved URL. Do not describe it as restoring an entire running JavaScript heap, arbitrary form state or a complete page session.

## 4. Finish memory accounting

- [ ] Verify Windows samples cover the intended WebView2 environment processes.
- [ ] Verify Linux and macOS process discovery against real WebKit processes, including child creation, exit and sampling races.
- [ ] Check that sampling stays off the event-loop thread and completion returns through the app's event path.
- [ ] Reject incomplete samples; do not treat missing processes or failed reads as zero memory.
- [ ] Keep private commitments, working sets and host-process memory distinct in names and output.
- [ ] Report shared renderer memory as shared. Do not divide it into invented per-tab costs.
- [ ] Where attribution is unavailable, state that limitation instead of blocking truthful aggregate accounting.
- [ ] Reconcile README platform claims with verified adapter behavior. Unix adapter files exist locally, while the README still says measured enforcement is Windows-only.

Dropping a view does not guarantee renderer exit or immediate reclamation. Process deltas and PID lifetimes are evidence; summed working sets are not unique physical RAM.

## 5. Verify automatic budget enforcement

- [ ] Test positive MiB configuration, the 1024 MiB default and invalid/overflowing inputs.
- [ ] Confirm the implemented sampling cadence, repeated-overage threshold, 90% stop threshold, reclaim wait and low-reclaim backoff with deterministic policy tests.
- [ ] Confirm each decision freezes at most one eligible background tab, then remeasures.
- [ ] Protect foreground, loading, downloading, audio/unknown-audio, pending-prompt, recently used, new and restored views.
- [ ] Verify user-input and camera/microphone protections, plus exclusion of non-HTTP(S) pages.
- [ ] Confirm no eligible candidates leaves the app responsive even while over budget.
- [ ] Confirm manual freeze still works if sampling is unavailable.
- [ ] Confirm the old fixed 24-live-view fallback is not reintroduced.

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

- [ ] Retain `FREEZE_PROBE` output for the light, 64 MiB heap and shared cases.
- [ ] Compare baseline, idle control and post-freeze private commitments and PID lifetimes. Reject incomplete samples.
- [ ] For a forced-sharing run, temporarily set `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` to `--process-per-site --renderer-process-limit=1`. Preserve and restore any previous value afterward; do not ship this as a production default.
- [ ] Confirm the budget probe exercises real sampling, automatic freeze, protected work and restoration under its 128 MiB test budget.
- [ ] Obtain equivalent real-process evidence on Linux and macOS before claiming cross-platform measured enforcement is verified.

## Completion criteria

- [ ] Default-feature format, check, clippy, build and tests pass on supported platforms.
- [ ] Engine operations have verified behavior or explicit platform limitations.
- [ ] Live evidence shows what freezing actually reclaims, including shared-renderer limits.
- [ ] Budget tests demonstrate protection of active work and controlled enforcement without a freeze/restore loop.
- [ ] Documentation matches the tested platform behavior and restore semantics.

Start with the baseline checks, then run the Windows live probes. Use the failures and measurements to choose the next code change. Do not make Servo a prerequisite for finishing this path.
