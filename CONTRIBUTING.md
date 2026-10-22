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
