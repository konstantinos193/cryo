# Changelog

## 0.10.0

### Added

- Downloads pane in the library. It shows the same rows as the ⬇ shelf
  at page scale, with a count, "Clear finished" and live repaints while
  a transfer runs; "Show all downloads" on the shelf opens it. Finished
  rows keep the shelf's verbs — click to open, drag out to copy, show in
  folder.

### Changed

- Download progress moved from the bar under the ⬇ glyph to a ring
  around it. A transfer with a declared size sets the arc; one without
  runs an orbiting segment. Row controls are 28px hit targets instead of
  12px glyphs.
- The bookmarks bar drops to icon-only chips once there are more entries
  than the row is wide, rather than clipping the tail. Titles stay in the
  tooltips.
- A launch with no remembered placement opens maximized — the bare
  1280x800 floating window read as broken on a desktop monitor. A
  remembered placement is still restored as before.
- `tests/tab_strip.cjs` runs against jsdom instead of a hand-written DOM
  stub, so the tests exercise real event dispatch and `closest()`. Run
  `npm ci` before `node --test tests/tab_strip.cjs`.

## 0.9.0

### Added

- Window placement persistence: size, position and maximized state are
  written to `window.json` at close and restored on the next launch. A
  remembered position is only re-seated on a monitor that still exists;
  incognito reads the size but never writes placement.
- cryo.exe and the installer now carry VERSIONINFO from the crate
  metadata.

### Fixed

- Tab-strip activation moved to `pointerdown`, so a press on a frozen or
  discarded tab lands before a re-render can move the target out from
  under the click. Regression coverage in `tests/tab_strip.cjs`.

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
