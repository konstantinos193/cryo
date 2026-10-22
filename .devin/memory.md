# cryo — project memory

Context for anyone (human or agent) picking up work on this repo.
Last updated: 2026-10-22 (late session — Phase 0 functional).

## What this is

**cryo** is an open-source, memory-obsessed browser shell written in Rust.
Repo: https://github.com/konstantinos193/cryo (public, MIT).
Owner: konstantinos193.

Tagline: "Tabs freeze. Budgets hold."

## The core thesis (don't lose this)

The browser RAM problem is a **policy problem, not an engine problem**. Sites
hold the memory; browsers manage it badly. cryo does NOT write a rendering
engine — that is a multi-decade effort (see Servo, Ladybird). cryo builds the
shell: tab lifecycle, memory budgets, accounting, dashboard, and eventually the
WebExtensions (MV3) API surface.

Contributions that try to parse HTML/CSS/JS are out of scope by design.

## Stack decisions

- Rust, edition 2021.
- Engine backend: system WebView (WebView2 on Windows) via `wry` — Phase 0.
- Servo is a possible future backend (Phase 5), not the current plan.
- No heavy dependencies yet — keep it that way until Phase 0 lands.

## Roadmap status

- [x] Phase 0 — minimal shell: window + tabs + navigation (wry/WebView2) —
      VERIFIED working 2026-10-22: tab strip, +/×, address bar, real
      navigation, titles, Ctrl+T/W/Tab/L/F5/F12 keys
- [ ] Phase 1 — tab lifecycle: freeze, discard, restore
- [ ] Phase 2 — per-tab memory accounting + budgets
- [ ] Phase 3 — memory dashboard UI
- [ ] Phase 4 — WebExtensions MV3 API surface
- [ ] Phase 5 — evaluate Servo as alternate engine

## Workflow / repo rules

- `main` is protected: PRs only, linear history (squash/rebase, no merge
  commits), no force pushes, no deletions.
- Required status check: `check` (CI job = fmt + clippy -D warnings + build +
  test, Windows runner). Must be green and branch up-to-date before merge.
- `enforce_admins` is OFF — the admin can technically bypass protections.
  Self-discipline applies; flip to true if wanted.
- Merged branches auto-delete on GitHub.
- Drill: branch off main → work → push → `gh pr create` → CI →
  `gh pr merge --squash` → pull main.

## Product rule

Memory is the product. Any PR that raises baseline idle RSS needs before/after
benchmarks in the description. Features that cost RAM must be opt-in or reclaim
more than they cost.

## WebView2 / wry traps (cost hours to find — do not re-learn)

1. **`NavigateToString` documents are dead surfaces.** `with_html` lands on a
   `data:` URI where WebView2 does not execute the document's own `<script>`
   (the element exists in the DOM, complete, but never runs). wry's IPC
   bridge also silently drops their messages: `Request::builder().uri(source)`
   fails on `data:` URIs. → Never use `with_html`/`load_html` for interactive
   surfaces.
2. **`with_ipc_handler` kills page scripts.** On wry 0.57 / WebView2 155,
   attaching it leaves the document's `<script>` unexecuted even on real
   `http:` documents (verified: title never set, `__cryo_update` undefined).
   We do NOT use it.
3. **The working design**: custom protocol `cryo` → pages at
   `http://cryo.localhost/<page>` (the form wry's Windows workaround rewrites
   to; `load_url` does NOT rewrite, so always pass the `http://` form).
   Page→app commands = `fetch('/cmd', {method:'POST', body:JSON})` — the
   protocol handler routes `/cmd` request bodies to `ViewSpec::on_ipc`.
   App→page = `eval()`. Both proven live.
4. GUI verification helpers live in `.devin/tools/`: `capture.ps1`
   (window screenshot) and `click.ps1` (window-relative mouse click).
5. `target/debug/cryo.exe` gets file-locked while running — `taskkill //F
   //IM cryo.exe` BEFORE cargo build, or the build fails EACCESS.
6. WebView2 user data dir: `target/debug/cryo.exe.WebView2/` — briefly locked
   after the process dies ("resource is in use" on immediate relaunch).

## Environment notes

- Windows dev box, repo lives at `E:\programming\cryo`.
- `gh` CLI authenticated as konstantinos193.
- Git commits locally use `-c user.name="konstantinos193"` (no global config
  identity set).
