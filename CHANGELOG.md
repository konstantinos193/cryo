# Changelog

## 0.8.2

### Fixed

- No behavior change — deterministic coverage only. New tests pin the
  exact-budget band (pressure persists between 90% and 100%), candidate-order
  tiebreak for equal-age tabs, and the retained reclaim baseline after a failed
  sample. Idle-sleep tests cover the 59-second boundary and the no-repeat
  suspension gate.
- Adds the memory-policy and tab-lifecycle verification checklists.

## 0.8.1

### Fixed

- Non-Windows views no longer claim success on unsupported operations: mute,
  browsing-data clearing and download pause now report an error instead of
  silently doing nothing.
- Download filename deduplication is covered by tests, including files that
  already exist on disk.
- Web error statuses map onto the right failure kinds (offline, DNS, timeout,
  connection, certificate, redirect); cancels and auth challenges produce no
  error page, unknown codes report Unknown.

## 0.8.0

### Added

- Measured memory budgets on Windows. Every five seconds Cryo samples WebView2
  process memory; two over-budget samples freeze the coldest eligible tab until
  usage falls to 90% of the budget. Set `CRYO_MEMORY_BUDGET_MIB` (default 1024)
  to change it.
- Protection rules: foreground, loading, downloading, audio-playing, prompting,
  recently-used, user-input and permission-requesting views are never frozen
  automatically; shared renderers are not billed to individual tabs.
- `Engine::measure_memory` and `Engine::spin_event_loop` ports, with WebView2
  accounting adapters on Windows and macOS/Linux groundwork.
- Experimental Servo engine backend behind `--features servo-engine`.
- Opt-in live probes on Windows (`CRYO_FREEZE_CASE`, `live_ram_budget`) that
  exercise the real freeze and budget paths and report process memory deltas.

### Removed

- The 24-live-view cap; a measured budget replaces the count heuristic.

## 0.7.1

### Fixed

- Faster startup on Windows: all views share one WebView2 environment instead of each spawning its own browser process.
- The window stays hidden until its views exist, so the first frame is the finished layout rather than views appearing one by one.

## 0.7.0

### Added

- Saved-password autofill with origin checks and explicit fill/save cards in trusted chrome.
- Origin-bound, session-only permission prompts on Windows for microphone, camera, location, notifications, and clipboard access. Unsupported permission types and backends deny access.

### Fixed

- Dropped or failed webviews cannot send callbacks into replacement tabs. Popup and keyboard callbacks carry the same generation checks as page and download events.
- History, bookmark, and vault writes run on bounded background workers, retry temporary failures, report errors to the window, and drain on shutdown.
- Internal documents and browser commands require explicit view privileges; content views cannot fetch chrome documents or send privileged commands.
- Tab lifecycle transitions invalidate owned prompts and interrupt downloads. Closed-tab download records remain available on the shelf.
- Legacy persisted WebView2 permission grants are cleared before external navigation; new consent remains scoped to the window.

### Refactored

- Budget decisions live in the memory context, separate from surface construction and window layout.
- Chrome markup, styles, scripts, wire types, and navigation policy have separate owners while retaining ordered script initialization.
- Tab registries, persistence services, import parsing, and WebView2 event families are separated into focused modules.

### Verification

- Formatting, Clippy with warnings denied, locked build, and 268 Rust tests pass on Windows.
- JavaScript startup, library panes, and permission actions pass a DOM-stub smoke check.
- Native desktop smoke testing was interrupted; full end-to-end capture-device verification is not claimed.
