# Security Policy

## Supported Versions

cryo is pre-1.0 and under active development. Only the latest commit on
`main` receives security fixes.

| Version | Supported |
| ------- | --------- |
| `main`  | Yes       |
| releases| No        |

## Reporting a Vulnerability

Please report vulnerabilities privately via GitHub:

- **Private vulnerability reporting** — open a report under
  [Security → Advisories → Report a vulnerability](https://github.com/konstantinos193/cryo/security/advisories/new).
- Do **not** open a public issue for a security problem.

Include a description, steps to reproduce, and the affected commit if
known. Reports are triaged on a best-effort basis; you will get a
response acknowledging the report, and credit in the advisory if a fix
ships (unless you prefer otherwise).

## Scope

cryo is a browser shell that embeds WebView2. Issues in the webview
engine itself (Chromium/WebView2) belong upstream at Microsoft. Issues
in how cryo *uses* the engine — navigation policy, IPC between shell
and webview, snapshot/restore of tab state, the update/installer path —
are in scope.
