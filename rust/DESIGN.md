# Zero Rust client — design

A native Rust client for Zero, at parity with the JS client
(`packages/zero-client`), proven by the conformance vectors that
`packages/zero-conformance` generates from the JS implementation. It follows
the Swift client (`swift/DESIGN.md`): the same vector layers, the same
documented exceptions, and the same milestone discipline.

Two dogfood apps come later, after the client: `apps/zbugs-gpui` (GPUI) and
`apps/zbugs-rune` (Rune). They share the client, the schema, queries,
mutators, and a zbugs model crate, but not their UI.

## Layout

| Path               | Crate              | Published | What                                                           |
| ------------------ | ------------------ | --------- | -------------------------------------------------------------- |
| `rust/zero-client` | `zero-client`      | not yet   | The client: JSON, hashes, AST, protocol, and later IVM, engine |
| `rust/conformance` | `zero-conformance` | never     | The conformance runner: replays every vector file              |

The crate names are provisional; `zero` is taken on crates.io and
`zero-client` was free on 2026-11-10. Publishing needs tmm's approval.

## Architecture (planned)

- A synchronous, single-owner engine with an injected clock and deterministic
  scheduling; no `await` inside a mutator or a rebase.
- Queries read in-memory IVM sources. SQLite (`rusqlite`) holds snapshots,
  pending mutations, cookies, and client and client-group lifecycle, with
  Replicache's semantics but not its IndexedDB-shaped storage.
- Tokio for background I/O, `tokio-tungstenite` with rustls for the socket.
- Snapshots and subscriptions that don't depend on a UI framework, bridged
  into GPUI entities and Rune state by the apps.

## Conformance

`cargo run -p zero-conformance` replays every file in
`packages/zero-conformance/vectors` (checked in) and, when present,
`packages/zero-conformance/generated` (the seeded fuzz corpora that
`pnpm --filter zero-conformance run test` writes), and prints a table:
cases per file, how many match JS exactly, how many match only under a
documented exception, and how many fail.

- Every checked-in file is listed. A file no suite replays yet is reported as
  not implemented, never as passing.
- Each case is checked alone; a failing or panicking case is counted and the
  rest still run. CI fails on any failing case and, with
  `--require-generated`, on missing generated corpora.
- `cargo test -p zero-conformance` asserts the same thing.
- Vectors are never edited or skipped to get to green. When JS itself is
  wrong (the Swift client found such cases), the difference is listed below
  and counted as an exception, with the reason.

### Documented exceptions

Each is counted separately in the report.

| Exception      | Why                                                                                                                                                                                                                                                                                                               |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lone surrogate | A Rust `String` can't hold a lone surrogate, which a JS string can. The parser reads an escaped one (`"\ud800"`) as U+FFFD, as the Swift client does; the case matches when JS's output, with each such escape read as U+FFFD, is what Rust writes. Real servers send valid UTF-8, so this only shows in fuzzing. |
| Nesting        | JSON nested deeper than `json::MAX_DEPTH` (1,500) is rejected. Dropping, comparing, and writing a value recurse, and a debug build overflows a 2 MB thread stack by 3,000 levels. JS has no limit short of its own stack (zero-cache writes about 4,100 levels). Swift has the same limit.                        |

## Milestones

Each milestone ends with green CI on `rust-client` and a short report.

| Milestone | Scope                                                                                                                                                                                      | Done when                                                        | Status      |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------- | ----------- |
| M0        | Scaffolding: the `rust/` workspace, the conformance runner and its report, CI                                                                                                              | CI green on `rust-client`, running the runner on real vectors    | in progress |
| M1        | Primitives and wire: JSON, JS numbers, string orders, hashes, primary and aggregate keys, value comparison, LIKE, TTL, AST and normalization, query hashes, protocol messages, poke chunks | Every primitive, AST, and protocol vector file and corpus passes | in progress |
| M1.5      | Walking skeleton: connect to a real `zero-cache`, handshake, receive pokes, log rows                                                                                                       | Talks to a real server in CI                                     |             |
| M2        | IVM: memory sources, every client operator, pipeline builder, change application, aggregates                                                                                               | Curated and seeded-fuzz IVM vectors pass                         |             |
| M3        | Codegen: a Rust target for the schema codegen CLI                                                                                                                                          | Generated code compiles and drives M2                            |             |
| M4        | Client engine on memory storage: connection, query manager, pokes, mutators, rebase, auth, recovery                                                                                        | Client scenario vectors pass                                     |             |
| M5        | SQLite persistence and lifecycle; scenarios on SQLite; end-to-end against Postgres and `zero-cache`                                                                                        | Scenarios pass on SQLite; end-to-end passes                      |             |
| M6        | The shared zbugs model, then `apps/zbugs-gpui` and `apps/zbugs-rune`                                                                                                                       | Both apps run against a real backend                             |             |
| M7        | Hardening, docs, packaging, the first crates.io release (with tmm's approval)                                                                                                              | `0.1.0` ready to publish                                         |             |
