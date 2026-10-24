# Security Policy

## Supported Versions

cryo is pre-1.0 and moves. Only the latest commit on `main` receives
security fixes; releases are snapshots of a moving target, not support
contracts.

| Version | Supported |
| ------- | --------- |
| `main`  | Yes       |
| releases| No        |

## Reporting a Vulnerability

Report vulnerabilities privately:

- **Private vulnerability reporting** — open a report under
  [Security → Advisories → Report a vulnerability](https://github.com/konstantinos193/cryo/security/advisories/new).
- Do **not** open a public issue. A public issue is a zero-day with a
  mailing list.

Include a description, steps to reproduce, and the affected commit if
known. Reports are triaged on a best-effort basis; you will get an
acknowledgement, and credit in the advisory if a fix ships (unless you'd
rather stay anonymous).

## Scope

cryo is a browser shell that embeds WebView2. Bugs in the webview engine
itself (Chromium/WebView2) belong upstream at Microsoft — if Chromium has
a bad day, that is their ticket queue. Bugs in how cryo *uses* the engine
are ours: navigation policy, IPC between shell and webview,
snapshot/restore of tab state, the update/installer path, and the vault.
