# Changelog

## 0.14.0

### Added

- "Add to Chrome" installs. The store button could not reach cryo before, so
  the address had to be pasted into the Extensions pane by hand; now the click
  raises a card that asks, fetches, verifies and installs. cryo asks first
  because a navigation is not proof of a click — any page can send a view at a
  crafted store address — and each answer carries a token the app issued, so a
  stale or invented one installs nothing.
- A real progress bar while the package comes down, determinate when the store
  declares a length and an orbiting segment when it does not. A 25 MB
  extension used to be twenty seconds of silence, which reads as a hang.
- Per-extension On/Off in the Extensions pane, from the engine rather than
  from the folder. A row says "Not loaded yet" where the folder is installed
  but nothing has loaded it, instead of implying it runs.

### Changed

- The question, the download and the outcome now happen in one card that puts
  itself away, rather than a dialog, a toast and an error page in three
  places. The throwaway tab the store opens for the package is closed — that
  address was a download target, never a page — or becomes a new tab when it
  is the only one.
- The Web Store error page is kept only for what cryo genuinely cannot do — a
  private window, or a platform that loads no extensions — and says that,
  rather than describing a manual workaround that is no longer needed.
- Uninstalling asks the engine to unregister the extension before deleting the
  folder. Deleting alone did work, because WebView2 drops an extension whose
  files change under it, but by side effect rather than by asking.

### Fixed

- Navigating a frozen tab moved the tab's address without moving the snapshot
  it would restore at, so the restore reloaded the frozen page and dropped the
  navigation.

## 0.13.0

### Added

- Install Chrome extensions from the Web Store. `Ctrl+Shift+O` →
  **Extensions** takes a store page address or an extension id, and cryo
  fetches the `.crx` from Google's own endpoint, because WebView2 carries no
  endpoint for "Add to Chrome" at all.
- CRX3 verification. The package's signature is checked over the whole
  archive, and — the part that matters — only against a key whose SHA-256
  produces the extension id being installed. A signature from any other key,
  however valid, is refused. RSA goes through Windows CNG rather than a new
  crypto dependency, and the archive is only reachable by going through the
  check.
- A localized extension is named properly. `__MSG_appName__` is resolved from
  the extension's own `_locales/<default_locale>/messages.json`, so MetaMask
  lists as MetaMask rather than as "Localized extension".

### Fixed

- A failed "Add to Chrome" said "This site cannot be found", which was true of
  the placeholder address the engine was sent to and no use to anyone. The
  error page now names the real reason, points at the Extensions pane, and
  offers no Try again, because retrying a placeholder can only fail again.

### Known limits

- Still Windows only, and incognito windows still load nothing.
- An extension reaches a tab when that tab's view is built, so it starts
  working in tabs opened after the install.
- cryo verifies that a package is signed by the key its id comes from. It does
  not review what the extension then does, any more than Chrome does.

## 0.12.0

### Added

- Chrome extensions, unpacked, on Windows. `Ctrl+Shift+O` → **Extensions**
  adds the folder holding a `manifest.json`, lists what is installed with its
  version and manifest version, and removes one. MV2 and MV3 both load;
  verified against a real content script running on a live page.
- A new `extensions` context owns the catalogue — what is installed and what
  a manifest says — while loading stays the engine's job behind
  `engine::Engine`. An install copies the folder into the profile and never
  touches the copy again, because WebView2 drops an extension whose files
  change under it.

### Known limits

- Windows only. WebView2 is what loads extensions; wry's extension path on
  WebKitGTK means native WebKit extensions, which are a different thing, so
  the pane says so rather than pretending.
- Unpacked folders only — no `.crx` and no Web Store, which WebView2 does not
  offer.
- An extension reaches a tab when that tab's view is built, so it starts
  working in tabs opened after it is added.
- Incognito windows load none, deliberately: an extension sees the pages it
  is granted, and a private window is not the place to decide that silently.
- Requires WebView2 runtime 120.0.2210.55 or newer; older runtimes ignore the
  option and no extension appears.

## 0.11.0

### Added

- Tab search (`Ctrl+Shift+A`): an overlay that filters every open tab by
  name or address and takes one with the arrows or a click. Frozen and
  discarded tabs are listed like any other and picking one restores it
  through the ordinary activate path — the overlay is a selector and owns
  no restore logic. The strip stops being a way to find a tab long before
  freezing stops making tabs cheap to keep.
- CI runs the chrome-script tests. They were only ever run by hand, so the
  tab-strip pointer regression they cover was ungated.

### Fixed

- `npm test` ran nothing. `node --test tests/` resolves `tests` as a module
  instead of scanning the directory, so the script shipped in 0.10.0 failed
  rather than running the tests. Chrome tests are now named
  `tests/*.test.cjs` — the pattern `node --test` discovers — and the script
  passes that glob explicitly.

### Documentation

- The install section says what Windows SmartScreen is actually reporting on
  an unsigned installer, and how to check the download against the `sha256`
  every release already publishes in `latest.json` — the same hash the
  self-updater verifies. Signing itself still needs a certificate.
- What a per-tab memory figure is allowed to mean is decided and recorded:
  sole-owner attribution plus one separately reported shared pool, no
  division and no estimates. The memory dashboard has to print something per
  row, so the rule exists before the surface does.

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
