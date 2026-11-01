# Contributing to cryo

## Ground rules

- `main` is protected. Everything lands through pull requests — no direct
  pushes, no force pushes, no "just this once."
- CI must be green: `cargo fmt`, `cargo clippy -D warnings`, `cargo build`,
  `cargo test`, and the live probes (`live_freeze_memory`,
  `live_ram_budget` on Windows — real WebView2, real freeze/restore).
  clippy is the parole officer on this project.
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

Unsigned builds hit a SmartScreen warning on first install. A self-signed
cert proves integrity but does **not** clear it; reputation is only built by
a certificate that chains to a public root, and SmartScreen is not impressed
by homework.

`installer/sign.ps1` is the only place that signs anything. The release
workflow calls it twice — once on `target\release\cryo.exe` before Inno packs
it, once on `cryo-setup.exe` after — and `latest.json` is hashed after that,
because signing rewrites the file. The script reads its certificate from the
environment, so no password reaches a command line:

| variable | source |
| --- | --- |
| `CRYO_CERT_BASE64` + `CRYO_CERT_PASSWORD` | a base64 PFX, from repo secrets |
| `CRYO_CERT_THUMBPRINT` | a cert already in the user's store — hardware token, cloud CSP |
| `CRYO_SIGNTOOL` | explicit `signtool.exe` path, for a runner with no SDK |

With neither certificate source set it prints why and exits 0, so the step
runs on every release rather than waking up for the first signed one. What it
will not do is pass a signature Windows would reject: it runs
`signtool verify /pa` afterwards and fails the release if the chain does not
validate. Shipping a broken signature is worse than shipping none — it looks
fixed from the inside and is still broken on a user's desktop.

`powershell -File installer\sign.tests.ps1` exercises all of that against
throwaway self-signed certs — preflight failures, the verify gate, and a
signed-and-timestamped artefact. CI runs it, because signing is the one path
a release cannot rehearse.

**Getting a certificate is the open half of
[#17](https://github.com/konstantinos193/cryo/issues/17).** Note that
`CRYO_CERT_BASE64` fits fewer certificates than it looks: since June 2023 the
CA/Browser Forum has required code-signing keys to be generated and held on
FIPS-certified hardware, so CAs no longer issue an exportable PFX. A PFX
secret suits a pre-2023 certificate or a test one; anything bought today
arrives as a USB token or a cloud HSM.

**SignPath Foundation is the chosen route** — free for open source, and cryo
fits the stated bar: MIT throughout, public repo, real release history. The
publisher string users see becomes the Foundation's rather than ours, which is
the price of a free certificate. In order:

1. The code signing policy is published — see
   [README](README.md#code-signing-policy). Their terms require the exact term
   "Code signing policy" on the home and download pages, the attribution
   sentence, the author/reviewer/approver roles, and a privacy statement.
2. Apply at [signpath.org](https://signpath.org/). Every team member needs MFA
   on their accounts before signing is enabled.
3. Approval issues the organization id, project slug, signing policy slug and
   an API token. None of them can be guessed, which is why no SignPath step is
   committed yet.
4. Then wire it: signing moves off `signtool` onto
   `signpath/github-action-submit-signing-request`, which takes an uploaded
   GitHub artifact, signs it on their service and hands back the signed file.
   That is two round trips for a release, because `cryo.exe` must be signed
   *before* Inno packs it and `cryo-setup.exe` only exists afterwards. Write
   that step against the real account and prove it on a prerelease tag — the
   one thing `sign.tests.ps1` cannot cover is someone else's service.

`sign.ps1` stays either way: `CRYO_CERT_THUMBPRINT` is the path for a CA
certificate on a USB token (Certum's open-source offering, Sectigo), which a
GitHub-hosted runner cannot reach — that route means a self-hosted runner or
signing locally. Azure Artifact Signing (formerly Trusted Signing) is the
other alternative, about $10/month and Microsoft-rooted, but individual
sign-up is limited to the USA and Canada.
