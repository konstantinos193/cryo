# Anti-spaghetti rules for cryo

Enforceable rules, not vibes. If a rule can't be checked in review or by
clippy, it doesn't belong here. CI gates (`fmt`, `clippy -D warnings`,
`build`, `test`) are in CONTRIBUTING.md — these rules sit on top.

"Spaghetti" has a testable meaning here: code is spaghetti when answering
"where does X happen?" or "what breaks if I touch this?" requires reading
the whole program — everything is everywhere and the reader makes a
mental jump per line. Hygiene is the inverse: every question has exactly
one place that answers it.

## 1. `main.rs` is a shell, not a program

`main` may only: parse args, load config, construct the app, run the event
loop, print the final error. Everything else lives in the library.

- All logic goes in `src/lib.rs` + modules. `main.rs` stays under ~50 lines.
- `main` calls one `run() -> anyhow::Result<()>` and nothing else.
- If you can't test it, it doesn't belong in `main.rs`.

## 2. Module boundaries

One file, one concept. A module owns one data type plus its `impl`s and
helpers, or one small coherent set of functions.

- **Banned names:** `util`, `utils`, `helpers`, `common`, `misc`, `shared`,
  `stuff`. If you can't name it, the concept isn't carved right yet.
- **No `*Manager` / `*Handler` god objects.** `TabManager` that also does
  navigation, freezing, budgets and IPC is spaghetti in a trench coat.
  Split by responsibility: `tabs::lifecycle`, `tabs::registry`, etc.
- **Tripwires** (not hard limits, but past these you justify it in the PR):
  - file > ~500 lines
  - function > ~60 lines
  - `impl` block > ~400 lines
  - nesting depth > 4
- Public items first, private after — files read top-down.
- Every module gets a `//!` doc comment saying what it owns.
- One abstraction level per function. Plumbing next to policy in one body
  makes the reader switch contexts per line — that texture *is*
  spaghetti, even in a short function.
- **Duplication.** The second copy of a trivial block is fine; the third
  means extract. Two copies of *non-trivial* logic (parsing, validation,
  state transitions) is already a bug farm — copies drift.
- **Shotgun surgery names a misplaced responsibility.** One logical
  change needing matching edits across three modules means the concept
  lives in the wrong place — move it, don't scatter it.

## 3. Dependency direction is one-way

The engine is plumbing and must be swappable (wry today, Servo in Phase 5).
Policy is the product. Policy never imports plumbing.

```
            ┌─────────────┐
            │    main     │
            └──────┬──────┘
                   ▼
            ┌─────────────┐      ┌──────────────┐
            │     app     │─────▶│   memory/    │
            │ (event loop)│      │   budgets    │
            └──────┬──────┘      └──────────────┘
                   ▼
            ┌─────────────┐
            │    tabs     │   lifecycle, registry, state
            └──────┬──────┘
                   ▼
            ┌─────────────┐
            │   engine    │   trait defined HERE, in terms of OUR types
            │  (trait +   │   ─ implemented by wry backend
            │   wry impl) │
            └─────────────┘
```

- `wry`/WebView2 types may appear **only** inside `engine/`. `tao` types
  may appear only in `app`/`shell`/`lib.rs` (the event-loop owner) — never
  in `tabs`, `memory` or `persist`. Anywhere else they appear, the PR gets
  rejected.
- The `Engine`/`Webview` traits are defined in `engine/mod.rs` using our own
  types (`TabId`, `NavigationRequest`, `MemoryStats`). The wry backend
  implements them; nothing outside `engine/` knows wry exists.
- No module may reach *through* a sibling into its internals. Go through the
  sibling's public API or up to the parent.
- No circular module deps. If A needs B and B needs A, one of them owns a
  trait the other implements.

## 4. Domain structure — DDD without the ceremony

Use the parts of DDD that prevent spaghetti; skip the parts that generate
boilerplate. No `IRepositoryFactoryBuilder` nonsense — contexts, entities,
value objects, aggregates, domain services. That's it.

- **Bounded contexts = top-level modules.** `tabs`, `engine`, `shell`
  (window/UI), `persist` (history recorder), `bookmarks`, `vault`
  (passwords), `transfer` (foreign-file import/export), `instance`
  (single-instance handoff), `update` (self-update). `memory` is the
  planned budgets context — it does not exist yet, but when it does a
  `Budget` lives there and `tabs` *asks* memory rather than computing
  budgets itself. Each context owns its vocabulary and its types. New
  top-level modules need a PR justification — and a line in this list.
- **Entities vs value objects — know which you're writing.**
  - *Entity*: has identity, changes over time → `Tab` (keyed by `TabId`),
    `Window`.
  - *Value object*: immutable, equal by contents → `ByteBudget`,
    `MemoryStats`, `FreezeReason`, `TabSnapshot`. Copied/cloned freely,
    never shared-mutable, no setters — you replace them wholesale.
- **Aggregates guard their internals.** `Tab` is the aggregate root for tab
  state. Outsiders change it only through methods (`tab.freeze()`,
  `tab.navigate(url)`), never by poking `tab.history.push()` from outside.
  Fields stay private or `pub(crate)` within the context. If a field needs
  mutating from another module, the aggregate is missing a method.
- **Domain services own cross-entity policy.** Which tab freezes when RAM
  is tight is *policy*, not a `Tab` method — it lives in a service
  (`memory::BudgetEnforcer`) that takes entities as input and returns
  decisions. Services decide; entities execute their own transitions.
- **Ubiquitous language — one word per concept.** The README fixed the
  vocabulary: **freeze, discard, restore, budget, accounting**. Use exactly
  those in type and fn names. Code that says `suspend` while docs say
  `freeze` gets renamed. No synonyms, no `TabThing`/`TabData`/`TabInfo`.
- **No anemic pass-through layers.** A module that only forwards calls
  unchanged is indirection earning nothing — delete it. A layer earns its
  keep by owning a decision.
- **Ports defined by the domain, adapters outside.** Same rule as §3 seen
  from the other side: `engine::Engine`, `persist::Store`, `memory::Sampler`
  are traits the domain declares for what it needs; `wry`/sqlite/OS impls
  are replaceable adapters.

## 5. State: one owner, messages between parts

The event loop owns the app state. Everything else sends it messages.

- Exactly one `struct App` owns top-level state and lives on the event loop
  thread. No second place where "the truth" lives.
- Cross-part communication goes through channels/`EventLoopProxy`, not
  shared `Rc<RefCell<...>>` webs. `Rc<RefCell>` is allowed inside one
  module's internals; it may not appear in a public signature.
- **No global mutable state.** No `static mut`, no `lazy_static`/`once_cell`
  singletons holding app state. Config and state are passed down from `main`.
- Per-tab state lives in one `Tab` struct inside one ordered collection
  (`Vec<Tab>` in the registry) — not scattered across parallel
  `HashMap<TabId, X>` fields or index-parallel vectors.

## 6. Hydration — bytes in, valid domain objects out

Hydration = turning stored/wire bytes into domain objects. For cryo this is
the core mechanic: **freeze = snapshot the domain state and drop the webview;
restore = hydrate a `Tab` back from its snapshot.** Get this boundary right
or every persistence change becomes spaghetti.

- **DTOs live at the boundary only.** Serde types (`TabSnapshotV1`,
  `BudgetConfigFile`, IPC payloads) live inside `persist`/IPC modules. They
  may be dumb `pub` field bags — that's their job. **Domain types never
  derive `Serialize`/`Deserialize`.** The day `Tab` derives serde, the
  schema becomes the domain and every refactor is a migration.
- **One door in.** `TabSnapshot -> Tab` happens in exactly one place
  (`Tab::restore` / `tabs::snapshot`). If two call sites can build a `Tab`
  from raw data, invariants will diverge — guaranteed.
- **Constructors validate — you cannot build an invalid object.**
  `Tab::restore(snap) -> Result<Tab, RestoreError>`. No "hydrate then fix
  up": a half-valid `Tab` must not exist even briefly. Validation lives in
  the constructor, not in a `validate()` the caller might forget.
- **Version the wire, not the domain.** Snapshots carry `version: u32`.
  Migration code sits next to the DTOs, upgrades old payloads to the
  *current* DTO, and only then hydrates. Domain code never sees an old
  format — no `if version == 1` inside `Tab`.
- **Strict schemas.** `#[serde(deny_unknown_fields)]` where sensible. Don't
  round-trip unknown fields "just in case" — preserved mystery data is
  hidden coupling that explodes later.
- **Time and ids come from ports, not ambient calls.** Restore takes the
  snapshot's own timestamps or an injected `&dyn Clock` — never
  `Instant::now()` buried in hydration. "Frozen since" must be the real
  freeze time, and tests must control the clock.
- **Hydrate-then-emit, not emit-during.** Build the full `Tab`, then publish
  one `TabRestored` event. No events fired halfway through construction —
  observers must never see a partially-hydrated entity.

## 7. Errors: `Result` for conditions, panic for bugs

- Anything that can fail for an expected reason returns `Result` + `?`.
- Library-style modules (`tabs`, `memory`, `engine`, `persist`) return
  concrete `thiserror` enums — one error type per module,
  `#[non_exhaustive]`, variants named for the operation (`Navigate`,
  `Freeze`, `ReadStats`, `Restore`).
- `anyhow::Result` only at the shell edge: `main`, `run()`, top-level
  wiring, tests. Never in a signature another module matches on.
- `unwrap`/`expect` only where the invariant is obvious (lock poisoning,
  "registry contains id we just inserted"). `expect` messages state the
  invariant, not the operation.
- Never silently `let _ =` a `Result`. Handle it or `?` it.
- No dependency's error type leaks through a public signature — wrap it.

## 8. Types do the policing

- Newtypes for ids: `TabId(u64)`, `ProcessId(u32)`, `ByteBudget(u64)` —
  never bare `u64`s passed between modules.
- Illegal states are unrepresentable: a tab is `Active | Frozen | Discarded`
  (enum), not three booleans. If two fields can't disagree in a valid state,
  merge them into an enum.
- `pub` is a commitment. Default to private / `pub(crate)`; widen only when
  a sibling module genuinely needs it.
- Traits for seams (engine, memory sampler, clock), structs for data. Don't
  make a trait "in case we need it" — make it when a second impl exists.

## 9. Async / platform discipline

- The `tao` event loop is the only loop. No second runtime (tokio etc.)
  without a PR that justifies it and explains the bridge.
- Blocking work (process stat reads, anything > a few ms) goes off the event
  loop thread; results come back via `EventLoopProxy`.
- No `thread::sleep` in production paths — use the event loop's timer.

## 10. Tests and dead code

- Unit tests live in `#[cfg(test)] mod tests` at the bottom of the file they
  test; integration tests in `tests/`.
- Engine and persistence seams get fake/in-memory impls (`engine::fake`,
  `persist::mem`) so policy and hydration are testable without a real
  WebView or disk.
- Dead code gets deleted, not `#[allow(dead_code)]`'d. `#[allow]` of any
  lint needs a comment saying why.
- **No broken windows.** The tree always compiles warning-free: a
  half-applied refactor is finished or reverted, never parked — a
  visible mess licenses the next one. A file no `mod` path reaches is
  dead code regardless of intent; it lives on a branch, not in `src/`.
- A comment the diff made stale is corrected or deleted in that diff —
  a wrong comment costs more than none.
- No `dbg!`/`println!` debugging left in merges — logging goes through one
  facade (`log`/`tracing`) once it's added.

## Quick checklist for review

1. Does `main.rs` still fit on one screen?
2. Did a `wry` import appear outside `engine/`, or a `tao` import
   outside `app`/`shell`/`lib.rs`? Did a serde derive appear on a
   domain type?
3. Did any file cross a tripwire without justification?
4. Any new `Rc<RefCell>` in a signature, global `static`, or `unwrap`
   outside tests/`main`?
5. Does the change add a `HashMap<TabId, _>` parallel to the tab registry?
6. Is there a `util`/`manager`/`helper` name hiding an uncarved concept,
   or a synonym for freeze/discard/restore/budget?
7. Was a domain object built from raw data outside its one constructor?
   Does a snapshot's version reach domain code?
8. Did an outside module mutate an entity's fields instead of calling a
   method on the aggregate?
9. Does the tree still compile clean (`cargo check`, zero warnings — the
   CI gate is `-D warnings`)? Any file in `src/` no `mod` path reaches?
10. Third copy of a non-trivial block left unextracted, or one logical
    change scattered as matching edits across three modules?
11. A comment this diff made stale, or a function mixing plumbing and
    policy in one body?
