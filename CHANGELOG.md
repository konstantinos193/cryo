# Changelog

## 0.20.2

### Fixed

- Linux: cryo opens its window instead of exiting with "the underlying
  handle is not available". tao queues `set_visible` as a request the
  event loop pumps later, and a GTK widget owns no GdkWindow — so no
  window handle — until it is realized; the engine asked for the handle
  in between. The window is now realized without mapping first, and CI
  keeps it honest: an Xvfb smoke test launches the binary and requires
  it still be alive a few seconds later.

## 0.20.1

### Fixed

- Cloudflare challenges and similar self-solving pages survive. A load
  that got any response code is judged as that answer: WebView2 reports
  a 403 with a body as "unknown", and the error page cryo painted over
  it killed the verification the page was running. Only a navigation
  with no response at all goes through transport classification, its
  silent retry and the download race.

## 0.20.0

### Added

- Right-clicking the back or forward arrow opens that tab's session
  history, Chrome-style: pick any entry to jump straight to it. The
  list is read from the engine on every open (WebView2 exposes it only
  over the DevTools protocol), so it is never stale and never copied
  into the app.

### Fixed

- IMAP sessions that answer out of step (a Dovecot-style `INBOX.`
  host, or a reply the library cannot parse) no longer poison every
  later command: the folder list still shows, and the next request
  opens a fresh session. Unseen counts come from `STATUS`, not
  `SELECT`'s first-unseen position.

## 0.19.0

### Added

- Sender pictures in the mail list and reading pane: the picture the
  reader chose for an address, then Gravatar, then the organization's
  own site icon. Site icons are fetched only over `https` to hosts that
  resolve to public addresses, every redirect hop judged the same way,
  so a message cannot aim a request at the reader's network.
- Mail signatures: kept per account, sanitized with the same allowlist
  as a body, and sent as `multipart/alternative` with the RFC 3676
  `-- ` marker in the plain half. Attachments go out as
  `multipart/mixed`. Account settings are edited from the mail page.
- The address box follows the active tab; a draft typed into it holds
  only on the tab it was typed on and lasts until that page moves.

### Fixed

- The browser profile no longer lives wherever the program is installed.
  On an external USB drive every page load read its cache from that disk
  (cold reads ~41 ms against ~0.8 ms on NVMe; the same pages about 45%
  slower). It now lives in `%LOCALAPPDATA%\cryo\webview`. An existing
  profile is copied by the warm keeper after the last window closes,
  without its rebuildable caches, so nobody waits on it and sign-ins carry
  over. The old folder is left in place. Turning the keeper off with
  `CRYO_WARM_MINUTES=0` also leaves the profile where it is.
- Linux builds carry glib 0.18.5 with the upstream `VariantStrIter`
  fix (GHSA-wrw7-89jp-8q8g, Dependabot alert 1) as a `[patch.crates-io]`
  override in `patches/glib`. No release of wry or tao reaches glib
  0.20, where the fix shipped: both bind GTK3 through gtk-rs 0.18. The
  patch goes the day they move.

## 0.18.1

### Fixed

- The IMAP client moves to `imap` 3.0.0-alpha.15, which drops `nom` 5
  and with it `lexical-core` 0.7 (RUSTSEC-2024-0377, Dependabot alert
  2). Special-use folder roles now come from the typed RFC 6154
  attributes; Gmail's `\Inbox` and `\Spam` still match as extensions.

## 0.18.0

### Added

- A mail client at `/mail` (`Ctrl+Shift+M`): IMAP folders, threaded
  envelopes, a reading pane and SMTP send, with provider autoconfig and
  a guess for the rest. Everything on the wire lives on one worker
  thread that connects per batch and drops its sessions after an idle
  spell; bodies are sanitized (`ammonia`) before they reach the page,
  inline `cid:` images become data URLs, and the engine never sees a
  socket. The home page shows a mail-at-a-glance card with unread
  counts and sync status; incognito windows hide it.
- Site information popup behind the lock in the address bar: what the
  page is, whether the connection is secure, its cookie count, and the
  permission decisions remembered for it, each resettable there.
- Permission decisions are remembered per origin in `permissions.json`
  (an explicit Allow or Block answers the next request without asking;
  a dismissed prompt denies once). Notifications join the permission
  kinds. Incognito keeps grants in memory only.
- Tabs reorder by drag, Chrome-style: a horizontal pull picks the tab
  up, the drop relocates it.
- A warm keeper (`--warm`, `CRYO_WARM_MINUTES`, default 30) holds
  WebView2's browser process open after the last window closes, so the
  next launch attaches in ~200 ms instead of booting it in ~650 ms.

### Changed

- The window shows right after placement, before the first view exists,
  so a launch no longer looks hung while WebView2 boots.
- History is read on a worker thread and hydrated when it arrives; a
  multi-megabyte file the antivirus is still scanning no longer blocks
  the launch. Clears and removals made before it arrives replay over it.

## 0.16.1

### Fixed

- Certificate detail no longer reaches process output. A
  `CertificateRejected` carries the TLS reason, and the fetch tests were
  formatting the whole translated error into panic messages — which is a
  write to stderr, and the reason `rust/cleartext-logging` raised two
  code-scanning alerts. The assertions still say what they expected;
  they no longer print the error.

## 0.16.0

### Added

- Find in page on Windows: `Ctrl+F` opens a private find field over the
  active document — WebView2's native Find session does the searching,
  with highlighting, match counts, case matching and next/previous.
  `Enter` / `Shift+Enter`, `F3` / `Shift+F3` and `Ctrl+G` /
  `Ctrl+Shift+G` step between matches; Escape closes and clears the
  query. Results carry the revision and the tab they were searched on,
  so a late callback cannot repaint the bar after a switch, and a
  search never restores a frozen background tab. Older WebView2 runtimes
  and other backends say find is unavailable rather than pretending.
- MSIX packaging for the Microsoft Store (`installer/msix/`): manifest,
  tile art, `pack.ps1` with a `-DevSign` path for local installs, and
  `pack.tests.ps1` under the CI signing gate. Once the package identity
  is configured, tagged releases emit `cryo.msix` — the unsigned package
  Partner Center accepts and Microsoft signs — alongside `cryo-setup.exe`.

### Changed

- A build running under package identity leaves updates to the Store:
  `update::store_managed()` probes `GetCurrentPackageId` once, and the
  launch-time feed check plus the menu's "Check for updates" stay out of
  a packaged install's way.
- Privacy policy published as `PRIVACY.md` — the Store listing requires
  the URL; the content is the same facts the README has always stated.

## 0.14.1

### Fixed

- With an extension installed, a new tab would not go to any address. The
  permission reset holds navigation until every stored grant has been
  cleared, and WebView2 refuses to clear a `chrome-extension://` one, so the
  wait never ended. Those grants are no longer ours to revoke — they came
  with an extension the user installed, and the manifest declares them.
- A stored grant that cannot be cleared for any other reason now leaves the
  address bar working. The failure is named in the log with the permission
  and the origin, and the rest of the sweep still runs: one stale row a
  reader can act on beats a browser that answers no address at all.

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
