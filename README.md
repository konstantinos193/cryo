<p align="center">
  <img src="assets/icon.svg" alt="cryo" width="200">
</p>

<h1 align="center">cryo</h1>

<p align="center"><b>A memory-obsessed browser shell written in Rust.<br>Tabs freeze. Budgets hold.</b></p>

cryo is a browser built around one thesis: the RAM problem in modern browsers
is not an engine problem, it's a **policy** problem. Engines render; shells
decide what stays alive. cryo writes no HTML parser — the engine is plumbing
(WebView2 today, Servo later). All engineering effort goes into the shell:
tab lifecycle, memory budgets, accounting.

## Status: Phase 0 — working shell

Window, tab strip, navigation, internal pages, and a full keyboard-shortcut
layer — running on WebView2 via `wry`/`tao`. The freeze/budget machinery is
Phase 1+; the architecture is already built for it.

What exists today:

- Ordered tab registry with closed-tab reopen stack (Ctrl+Shift+T)
- Incognito windows as sibling processes (`--incognito` → WebView2
  `IsInPrivateModeEnabled` — the real InPrivate, not a cosmetic flag)
- Shell UI rendered by the same engine (`cryo` internal scheme →
  `http://cryo.localhost/...` on Windows): chrome strip, new-tab page
- Engine seam: everything WebView2/COM lives behind `engine::{Engine,
  ContentView}` traits in our own types — a fake in-memory engine exists
  for tests, Servo can slot in without touching policy code

## Keyboard shortcuts

Two input paths — WebView2's `AcceleratorKeyPressed` while a view has focus,
tao's `KeyboardInput` otherwise — feed one `key_command()` mapping, so keys
behave identically whether the page or the chrome owns focus.

| Keys | Action |
|---|---|
| `Ctrl+T` / `Ctrl+W` / `Ctrl+F4` | New tab / close tab |
| `Ctrl+Shift+T` | Reopen closed tab (bounded stack, restores strip position) |
| `Ctrl+Tab` / `Ctrl+PgDn` · `Ctrl+Shift+Tab` / `Ctrl+PgUp` | Next / previous tab |
| `Ctrl+1..8` / `Ctrl+9` | Jump to tab N / last tab |
| `Ctrl+Shift+PgUp` / `PgDn` | Move tab left / right |
| `Ctrl+Shift+Home` / `End` | Move tab to start / end |
| `Ctrl+Shift+K` | Duplicate tab |
| `Ctrl+M` | Mute / unmute tab |
| `Ctrl+N` / `Ctrl+Shift+N` | New window / new **incognito** window |
| `Ctrl+Shift+W` / `Ctrl+Shift+Q` / `Alt+F4` | Close window / quit |
| `Ctrl+L` / `Alt+D` / `Ctrl+K` / `Ctrl+E` | Focus address bar |
| `F6` / `Ctrl+F6` | Cycle focus address bar ↔ page / jump to page |
| `Alt+Enter` / `Alt+Shift+Enter` | Open address-bar input in new fg / bg tab |
| `Ctrl+Enter` | `www.<word>.com` canonize + open |
| `Ctrl+Shift+Enter` | Open input in a new window |
| `Ctrl+Shift+L` | Paste-and-go (Edge) |
| `Alt+←` / `Alt+→` | Back / forward |
| `F5` / `Ctrl+R` | Reload |
| `Ctrl+F5` / `Ctrl+Shift+R` / `Shift+F5` | Hard reload (DevTools-protocol cache bypass) |
| `Esc` | Stop loading (still reaches the page for dialogs/menus) |
| `Ctrl+P` / `Ctrl+S` / `Ctrl+O` | Print / save-as UI / open file dialog → `file://` |
| `Ctrl+U` | `view-source:` in a new tab |
| `Ctrl+=` `+` / `Ctrl+-` / `Ctrl+0` | Zoom in / out / reset (×1.25 steps, 25–500%) |
| `Ctrl+Shift+S` | Screenshot → `Pictures\cryo\shot-*.png` |
| `Ctrl+Shift+Del` | Clear all browsing data for the profile |
| `F11` | Borderless fullscreen |
| `F12` / `Ctrl+Shift+I` / `J` / `C` | DevTools |
| `Alt+Home` | Home (new-tab page) |

Mouse: `target=_blank`, ctrl/shift/middle-click and `window.open` are all
captured via `NewWindowRequested`, denied as popups, and reopened as
foreground tabs. Middle-click a tab to close it.

## Architecture

```
            ┌─────────────┐
            │    main     │   arg parsing only (--incognito, URL seed)
            └──────┬──────┘
                   ▼
            ┌─────────────┐      ┌──────────────┐
            │     app     │─────▶│   memory/    │   (Phase 1+)
            │ (event loop)│      │   budgets    │
            └──────┬──────┘      └──────────────┘
                   ▼
            ┌─────────────┐
            │    tabs     │   Tab entity, ordered registry, closed stack
            └──────┬──────┘
                   ▼
            ┌─────────────┐
            │   engine    │   Engine/ContentView traits in OUR types;
            │  (wry impl) │   fake engine for tests; WebView2 isolated here
            └─────────────┘
```

Dependency direction is one-way: policy never imports plumbing. `wry` types
exist only inside `engine/`; `tao` types only in `app`/`lib`. Per-tab state
lives in one `Tab` struct in one ordered `Vec` — no parallel
`HashMap<TabId, _>` scatter.

The enforceable version of this is [AGENTS.md](AGENTS.md) — module
boundaries, banned names, hydration rules, error taxonomy. Read it before
opening a PR.

## Build & run

Windows + Rust stable. WebView2 runtime is preinstalled on Windows 10/11.

```powershell
cargo run                      # normal window
cargo run -- --incognito       # InPrivate window (in-memory profile)
cargo run -- https://site.com  # seed the first tab with an address
cargo run --release            # no console window
```

CI gates: `cargo fmt`, `cargo clippy -D warnings`, `cargo build`,
`cargo test`. `.devin/tools/keys.ps1` drives the running window with
SendKeys for manual shortcut testing (`keys.ps1 -Keys "^t"` = Ctrl+T).

## Roadmap

- [x] **Phase 0** — Shell: window, tabs, navigation, shortcuts, incognito
- [ ] **Phase 1** — Tab lifecycle: freeze, discard, restore
- [ ] **Phase 2** — Per-tab memory accounting + budgets
- [ ] **Phase 3** — Memory dashboard (the real `Shift+Esc`)
- [ ] **Phase 4** — WebExtensions (MV3) surface
- [ ] **Phase 5** — Evaluate Servo as alternate engine backend

Known gaps by design (need subsystems that don't exist yet): find-in-page,
bookmarks/history/downloads, tab search overlay, task manager. Track them
in issues, not in wishful code.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the ground rules (PRs only,
`main` is protected) and [AGENTS.md](AGENTS.md) for the anti-spaghetti
architecture rules — both are enforced in review.

## License

MIT — see [LICENSE](LICENSE).
