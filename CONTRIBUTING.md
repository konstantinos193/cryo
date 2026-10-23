# Contributing to cryo

## Ground rules

- `main` is protected. All changes land through pull requests — no direct pushes,
  no force pushes, no branch deletion.
- CI must be green: `cargo fmt`, `cargo clippy -D warnings`, `cargo build`,
  `cargo test`.
- Commit messages describe **why**, not just what.
- Keep PRs small and scoped. One feature or fix per PR.

## Scope discipline

cryo is a browser **shell**, not a rendering engine. PRs that try to implement
HTML/CSS/JS parsing will be rejected — that work belongs to upstream engines
(WebView2, Servo). Our territory: tab lifecycle, memory policy, budgets,
accounting, UI, and eventually the WebExtensions API surface.

## Memory is the product

Any change that increases baseline idle memory needs a benchmark in the PR
description (before/after RSS on a fixed tab set). Features that cost RAM must
be opt-in or pay for themselves by reclaiming more elsewhere.

## Releasing

The installer is Inno Setup (`installer/cryo.iss`) — a per-user install to
`%LOCALAPPDATA%\Programs\cryo`, no admin prompt.

1. Bump `version` in `Cargo.toml`, land it on `main`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag must equal the
   Cargo.toml version; the release pipeline refuses mismatches.
3. `release.yml` builds `cryo.exe`, compiles `cryo-setup.exe`, writes
   `latest.json` (version + pinned installer URL + sha256) and publishes
   the GitHub Release.

Self-update: release builds fetch
`releases/latest/download/latest.json` at launch on a worker thread. A
newer semver downloads the installer to `%TEMP%`, verifies the sha256,
and shows the ⋮ badge — "Restart to update" runs it with `/VERYSILENT`
and relaunches. The `latest.json` schema is the update wire format —
change it and every released build needs a matching `src/update` version.

### Code signing

Unsigned builds hit a SmartScreen warning on first install. To sign, put
a PFX in repo secrets `CRYO_CERT_BASE64` + `CRYO_CERT_PASSWORD` and the
pipeline signs both `cryo.exe` and `cryo-setup.exe` — no workflow change
needed. Note a self-signed cert proves integrity but does **not** clear
SmartScreen; only a real code-signing cert builds reputation.
