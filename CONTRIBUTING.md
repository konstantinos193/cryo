# Contributing to cryo

## Ground rules

- `main` is protected. Everything lands through pull requests — no direct
  pushes, no force pushes, no "just this once."
- CI must be green: `cargo fmt`, `cargo clippy -D warnings`, `cargo build`,
  `cargo test`. clippy is the parole officer on this project.
- Commit messages say **why**, not what. The diff already covers the what.
- Keep PRs small and scoped. One feature or fix per PR. A PR that does three
  things gets reviewed at one-third speed and blamed for all three.

## Scope discipline

cryo is a browser **shell**, not a rendering engine. PRs that implement
HTML/CSS/JS parsing will be rejected — that work belongs to upstream engines
(WebView2, Servo). If your PR needs a CSS parser, the PR is in the wrong
repository. Our territory: tab lifecycle, memory policy, budgets, accounting,
UI, and eventually the WebExtensions API surface.

## Memory is the product

Any change that increases baseline idle memory needs a benchmark in the PR
description (before/after RSS on a fixed tab set). Features that cost RAM must
be opt-in or pay for themselves by reclaiming more elsewhere. A browser shell
that eats memory to save memory is a space heater with tabs.

## Releasing

The installer is Inno Setup (`installer/cryo.iss`) — a per-user install to
`%LOCALAPPDATA%\Programs\cryo`, no admin prompt.

1. Bump `version` in `Cargo.toml`, land it on `main`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag must equal the
   Cargo.toml version; the release pipeline refuses mismatches, so you don't
   have to remember to.
3. `release.yml` builds every desktop target and publishes the GitHub
   Release: `cryo-setup.exe` + `latest.json` (Windows),
   `cryo_<ver>_amd64.deb` + `cryo-x86_64-unknown-linux-gnu.tar.gz`
   (Linux), `Cryo.app` packed as `cryo-<target>.tar.gz` for both
   `aarch64-apple-darwin` and `x86_64-apple-darwin` (macOS). Only the
   Windows job writes `latest.json` — the updater is Windows-only.

Self-update: release builds fetch
`releases/latest/download/latest.json` at launch on a worker thread. A
newer semver downloads the installer to `%TEMP%`, verifies the sha256,
and shows the ⋮ badge — "Restart to update" runs it with `/VERYSILENT`
and relaunches. The `latest.json` schema is the update wire format —
change it and every released build needs a matching `src/update` version,
or the updater starts installing tomorrow's build over yesterday's
expectations.

### Code signing

Unsigned builds hit a SmartScreen warning on first install. To sign, put
a PFX in repo secrets `CRYO_CERT_BASE64` + `CRYO_CERT_PASSWORD` and the
pipeline signs both `cryo.exe` and `cryo-setup.exe` — no workflow change
needed. A self-signed cert proves integrity but does **not** clear
SmartScreen; reputation is only built by a real code-signing cert, and
SmartScreen is not impressed by homework.
