# cryo

**A memory-obsessed browser shell written in Rust. Tabs freeze. Budgets hold.**

cryo is an open-source browser project built around one thesis: the RAM problem
in modern browsers is not an engine problem, it's a policy problem. The engine
is plumbing — the shell is the brain. cryo puts all its engineering effort into
the brain.

## The problem

- 5 heavy tabs (YouTube, Gmail, Reddit) can hold 3–5 GB of RAM.
- Browsers cache aggressively and return memory to the OS late and reluctantly.
- Sites leak memory over time (detached DOM, orphaned listeners, runaway timers).
- On a 16 GB machine, a browser + an IDE + a game is a losing fight.

## The approach

Instead of writing a rendering engine from scratch (a multi-decade team effort —
see Servo, Ladybird), cryo builds the **browser shell** in Rust on top of an
embeddable engine, and focuses on what existing browsers won't do:

- **Aggressive tab freezing** — idle tabs get fully suspended and their memory
  reclaimed, not just throttled.
- **Per-tab RAM budgets** — a tab that exceeds its budget gets reloaded or
  discarded, with a visible history of who ate what.
- **A real memory dashboard** — per-tab, per-extension, per-process accounting.
- **Chrome extension support** — via the WebExtensions API (MV3) where the
  underlying engine allows it.

## Roadmap

- [x] Phase 0 — Minimal shell: window, tabs, navigation (WebView2 via `wry`)
- [ ] Phase 1 — Tab lifecycle: freeze, discard, restore
- [ ] Phase 2 — Per-tab memory accounting + budgets
- [ ] Phase 3 — Memory dashboard UI
- [ ] Phase 4 — WebExtensions (MV3) API surface
- [ ] Phase 5 — Evaluate Servo as an alternate engine backend

## Tech

- Rust
- Engine backend: system WebView (WebView2) via `wry`, with Servo as a future option

## License

MIT — see [LICENSE](LICENSE).
