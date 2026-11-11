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

| Path                     | Crate                | Published | What                                                                                    |
| ------------------------ | -------------------- | --------- | --------------------------------------------------------------------------------------- |
| `rust/zero-client`       | `zero-client`        | not yet   | The client: JSON, hashes, AST, protocol, IVM, and later the engine                      |
| `rust/conformance`       | `zero-conformance`   | never     | The conformance runner: replays every vector file                                       |
| `rust/codegen-tests`     | `zero-codegen-tests` | never     | The Rust `zero-rust codegen` generates, checked in, and its tests                       |
| `packages/zero-rust-cli` | (npm) `zero-rust`    | not yet   | The `zero-rust` CLI: `codegen`, from the shared `packages/zerogen`; carries the version |

tmm chose `zero-client` for the crate name (`zero` is taken on crates.io;
`zero-client` was free on 2026-11-10).

## Releases

Like the Swift client's (`swift-release.yml`):

- Changesets versions the client. As for Swift, the CLI's npm package
  carries the version (tmm's decision): `zero-rust`
  (`packages/zero-rust-cli`) holds it and `CHANGELOG.md`, and the CLI and
  the crates share it. Add a changeset for `zero-rust` to release a change
  to either.
- Swift Release's "Version Packages" PR versions both clients; its script
  copies `zero-rust`'s version into `rust/Cargo.toml` (`[workspace.package]`)
  and `rust/Cargo.lock` with `rust/scripts/set-version.sh`. A second
  Changesets job would race it for the same branch.
- On `main`, `rust-release.yml` releases a version that isn't out yet, which
  happens when the Version Packages PR merges: merging it is the release
  gate, as for Swift. The `rust-release` environment limits the release jobs
  to `main` and holds their secrets, with no required reviewers. A release
  is a subtree split of `rust/` to
  the mirror, `wevm/zero-rust`, with a tag and a GitHub release from the
  changelog, `cargo publish` of `zero-client`, and `npm publish` of the
  `zero-rust` CLI. 0.0.0 is never released.
- The mirror token comes from github-sts, authorized by
  `rust/.github/sts/mirror.sts.yaml`, which the first mirror push publishes.
  wevm/zero-rust is in the `wevm-github-sts-production` app.
- crates.io's trusted publishing needs the crate to exist, so tmm makes the
  first publish by hand, at the first real release, from his machine with
  his own `cargo login` (`cargo publish -p zero-client` in `rust/` at the
  released commit), then sets up trusted publishing for `rust-release.yml`.
  CI holds no crates.io token: until the crate exists, the release run
  skips the crates.io job with a notice and still pushes the mirror and its
  release; a rerun after the manual publish finds the version published.
- npm's trusted publishing also needs the package to exist, so the first
  `zero-rust` publish is made by hand too (`pnpm --filter zero-rust build`,
  then `npm publish` in `packages/zero-rust-cli`); until then the npm job is
  skipped with a notice.

So the first mirror push and every crates.io and npm publish happen only when tmm
merges a Version Packages PR that carries a `zero-rust` version.

## Architecture (planned)

- A synchronous, single-owner engine with an injected clock and deterministic
  scheduling; no `await` inside a mutator or a rebase.
- **No third-party dependencies** (tmm's decision): `cargo tree -e normal`
  of `zero-client` with its default features shows `zero-client` alone, and
  CI fails otherwise. Third-party crates come only behind opt-in features,
  such as the reference transport and SQLite storage below. JSON objects and
  schemas use `map::OrderedMap`, an insertion-ordered map in the crate
  (`indexmap` was dropped), and no third-party type is in the public API.
- Queries read in-memory IVM sources. Persistence is behind a storage
  trait; see [Storage](#storage-m5).
- The socket is behind a transport trait that the core defines (tmm's
  decision), `SocketFactory` and `SocketConnection` with `SocketEvents`, the
  shape of the browser `WebSocket`; the clock and timers are behind
  `Scheduler`. By default `zero-client` depends on no async runtime and no
  WebSocket crate, so an app can bring its own transport without
  downloading crates it doesn't use; `zero_client::testing` has a manual
  scheduler and mock sockets.
- The reference transport (M4), `zero_client::tokio_transport` behind the
  `tokio-transport` feature, runs the client on a tokio `LocalSet`: timers
  and each socket are local tasks, and the follow-ups to an API call run as
  a local task spawned once the call returns, where JS runs microtasks. Its
  public API exposes no tokio or tungstenite types, so a tungstenite major
  bump doesn't make a `zero-client` one. `tokio-tungstenite` is declared
  with `default-features = false` and only `connect`, and TLS is the `tls`
  feature: rustls on `ring` with the Mozilla roots (`webpki-roots`), so it
  needs no cmake or system certificates, and an app on `ws://` or behind
  its own TLS pulls in none of it. A socket that can't open, or that drops
  without a close frame, closes with 1006, as a browser reports it.
  `tests/client_e2e/` runs the whole client on it against `zero-cache`.
- The walking skeleton (M1.5) speaks the protocol to `zero-cache` itself,
  over `tokio-tungstenite`, without the client.
- Snapshots and subscriptions that don't depend on a UI framework, bridged
  into GPUI entities and Rune state by the apps.

### Storage (M5)

Opt-in, like the transport (tmm's decision), and shaped after zero-swift's
(`swift/DESIGN.md`, Storage), which keeps Replicache's semantics but not its
IndexedDB-shaped machinery.

- The core owns everything but the bytes: the in-memory mirror of the
  client groups, clients, snapshots, and pending mutations; every decision,
  made in JS's order; and the client and client-group lifecycle and GC
  (heartbeats, deleting inactive clients and telling the server, dropping
  empty groups, disabling a group after `ClientNotFound`,
  `SchemaVersionNotSupported`, or a 1009 close, and dropping the database
  after the base cookie or last mutation ID errors).
- A storage backend implements two small public traits
  (in `zero_client::client`): `StorageBackend` opens a database by name, deletes
  databases by name (`Zero::delete`, `drop_all_databases`), and runs the
  database GC; `StoredDatabase` reads the client groups and clients when it
  opens and a group's snapshot and mutations when a client joins the group
  (as Swift reads them), then applies an ordered stream of writes (Swift's
  `StorageOp`: put or delete a group or clients, the deleted clients, a
  poke's snapshot changes with its cookie and last mutation IDs, a pending
  mutation, a rebase's aggregate writes, and dropping confirmed mutations),
  and flushes, closes, and drops. An app brings its own store with
  `ZeroStorage::Custom`, the "custom store" Swift planned but never built.
- The memory backend is in the core. It keeps databases for the life of the
  process, keyed by name, like JS `kvStore: 'mem'`, and it is also the
  fallback when a store can't be opened.
- A `sqlite` feature (`rusqlite` 0.40, the newest on Rust 1.88, on the
  system `libsqlite3`) implements only the traits and the database-file GC;
  `sqlite-bundled` compiles SQLite in, for platforms without a
  `libsqlite3`. WAL mode, `synchronous = NORMAL`. Each open database has a
  plain `std::thread` that owns its connection, fed by a channel: writes run
  in order, one `BEGIN IMMEDIATE` transaction per batch (a mutation, a
  poke), so no async runtime is needed; reads at open, flushes, and closes
  wait for it. The file layout, file names, and default directory are
  zero-swift's, so the two read each other's files.
- Versioning by name, as in Swift, with no migration framework: the
  database name covers the user, the storage key, the mutate and query
  URLs, the client schema's hash, the storage version, and the format, so a
  schema change starts a new database as in JS; the file name is a hash of
  it plus a Rust layout version.
- Failure handling as in Swift: if the store can't be opened the client
  runs on memory and calls `on_storage_failure`; a failed write stops
  writing and calls it too; opening waits at most 250 ms for another
  connection's lock before falling back; a write busy past the 5 s timeout
  is kept and written with the next one; stored data that doesn't decode
  is lost client state, so the client starts a new database and calls
  `on_client_state_not_found`. Swift hears of a failed write when it fails;
  Rust when the client next writes, flushes, or closes, which a heartbeat
  does within a minute. The database-file GC follows Swift's: five
  minutes after launch and then every 12 hours, databases no client used
  for two days are deleted, only ours, renamed away first.
- The client scenario vectors run on both backends: the report has a
  `(sqlite)` row for each client file, each scenario on a directory of its
  own. `tests/sqlite_storage.rs` checks the file itself (a relaunch reads
  an edit made on disk, a corrupt file, a directory that can't open,
  `delete`, the GC), and `tests/client_e2e/` relaunches on SQLite against
  `zero-cache`.
- The default build is memory-only (tmm's decision), so it keeps the
  no-third-party-dependencies rule; SQLite is opt-in. Swift differs: its
  default is SQLite. **A default build persists nothing across restarts**:
  the README and the crate docs say so, and the README's quick start
  enables `sqlite` with `tokio-transport` and `tls`, so following it gives
  data that survives a restart.
- The storage options match Swift's `ZeroStorage` by name: `Memory`,
  `Sqlite` (with an optional directory, behind the `sqlite` feature), and
  `Custom`, for a backend the app brings, which Swift planned but never
  built.

### Codegen (M3)

`zero-rust codegen` generates the Rust a Zero app needs from its TypeScript
schema, as `zero-swift codegen` does the Swift: one generator,
`packages/zerogen`, with a target per language (`targets/swift.ts`,
`targets/rust.ts`). Loading the schema and its column types and writing or
checking the output (`--check`) are shared; the Rust target has its own
emitter, not Swift's templates. Options come from `zero-rust.json`
(`schema`, `export`, `output`, `typePrefix`, and per-table `type` and
column `type: "int"` overrides, as in `zero-swift.json`).

The generated file holds, against `zero_client::typed`:

- a row struct per table, with a field per column (snake_case) and per
  relationship (`Option<Vec<Row>>` for many, `Option<Box<Row>>` for one,
  filled when a query asks with `related`), and the aggregates a query
  asked for in `related`; a `Column` constant per column and a
  `Relationship` constant per relationship (`Issue::OWNER_ID`,
  `Issue::COMMENTS`), which name them in queries;
- an enum per `enumeration<…>()` and a struct per typed `json<…>()` object,
  shared when the TS type is one named type;
- `schema()`, the runtime `Schema`, and `zql`, a function per table that
  starts a typed `Query<Row>`;
- the typed CRUD of a mutator (M4), as Swift's `tx.mutate.<table>`:
  `mutate`, a function per table that returns a `TypedTableMutator`
  (`insert` and `upsert` take the row, `update` a `<Row>Update`, `delete` a
  `<Row>Key`). `<Row>Update` has the primary key and an `Option` per other
  column, or a `ColumnUpdate` (`Unchanged`, `Null`, `Set`) for one that can
  be null, with `<Row>Update::new(key)` leaving every other column, so
  `IssueUpdate { closed: Some(true), ..IssueUpdate::new(id) }`. Swift nests
  `Update` and `Key` in the row; Rust can't, so they are named after the
  row, after the schema's own types claim theirs (`TKey2` when an enum is
  `TKey`).

`typed::Query<R, T>` wraps `AnyQuery`: each method calls the `AnyQuery`
method of the same name with the constant's name, so a typed query builds
exactly the AST, format, and hash the untyped one does, which the
conformance vectors check against JS. `T` is what the query materializes
to: `Vec<R>`, `Option<R>` after `one()`, `i64` for `count()`, `Option<f64>`
for `sum` and `avg`, the column's type for `min` and `max`, `GroupRow`s
after `group_by` and `DistinctRow`s after `distinct`. `Query::read` reads a
view as `T`. Rows read strictly (a value of the wrong type is an error that
names the column), except that a null in a column the schema says can't be
null reads as `""`, `0`, or `false`, as JS shows it; Swift does the same.

Identifiers are ASCII: names stay in strings, so any table or column name
works. A keyword is written `r#name`, or gets `_` when `r#` can't escape it
(`self_`); names that clash once they are identifiers, or with a prelude or
`zero_client::typed` type, get `_`, `Row`, or a number. Every schema of
`packages/zero-swift-cli/test/sweep-schema.ts` generates Rust that compiles
with clippy's warnings denied.

The generated files are rustfmt-clean only by accident, so they are
declared `#[rustfmt::skip] mod schema;` (rustfmt formats `@generated` files
by default).

Tests: `packages/zero-rust-cli/src/codegen.test.ts` checks that the
generated files in `rust/codegen-tests/src` (the zql test schema, the
codegen fixture, zbugs, and every sweep schema) are current;
`rust/codegen-tests/tests/typed.rs` compiles them and checks that typed
queries build the untyped queries' ASTs, formats, hashes, and errors, and
that views of them, hydrated from memory sources and pushed to, read as the
generated rows.

Not yet, with the milestones that need them: `drift record` for Rust ports
of app queries and mutators with the zbugs model (M6); publishing the CLI to
npm, as `zero-rust`, with the first release (M7).

## Conformance

`cargo run -p zero-conformance` replays every file in
`packages/zero-conformance/vectors` (checked in) and, when present,
`packages/zero-conformance/generated` (the seeded fuzz corpora that
`pnpm --filter zero-conformance run test` writes), and prints a table:
cases per file, how many match JS exactly, how many match only under a
documented exception, and how many fail.

- Every checked-in file, and every generated corpus that is there, is
  listed. A file no suite replays yet is reported as not implemented, never
  as passing.
- Each case is checked alone; a failing or panicking case is counted and the
  rest still run. CI fails on any failing case and, with
  `--require-generated`, on missing generated corpora.
- `cargo test -p zero-conformance` asserts the same thing.
- Vectors are never edited or skipped to get to green. When JS itself is
  wrong (the Swift client found such cases), the difference is listed below
  and counted as an exception, with the reason.

Deliberate differences from JS, each counted separately in the report, are
in [PARITY.md](PARITY.md).

### CI

`.github/workflows/rust.yml`, on `main`, `rust-client`, and every PR:

- **Conformance Vectors**: the checked-in vectors match current JS, and the
  seeded fuzz corpora are generated for the next job; the checked-in
  generated Rust matches `zero-rust codegen`, whose types and lints pass.
- **Rust (Linux)**: rustfmt, clippy with warnings denied and every
  feature, the tests with the default features and with all of them, the
  conformance report (in the job summary), rustdoc with warnings denied, and
  `cargo package`, which builds the crate as crates.io would get it, outside
  the workspace, a check that the default build of `zero-client` has no
  third-party normal or build dependencies, and a build on the
  `rust-version` the manifest names.
- **Rust end-to-end (Linux)**: Postgres and `zero-cache` from this repo's
  source over the fixture database (`packages/zero-conformance/src/e2e/`),
  and `zero-client/tests/walking_skeleton.rs` and `client_e2e/` (the
  whole client on the tokio transport: sync, optimistic mutations the
  server confirms, a relaunch, two clients of a group, synced aggregates)
  against them, with `ZERO_E2E_URL` set. Without it the tests skip, so
  `cargo test` needs no server. With SQLite, `client_e2e/` also runs
  Swift's chaos tests through a TCP proxy (`proxy.rs`, Swift's
  `ChaosProxy`) that refuses, blackholes, silences, throttles, stalls, and
  cuts connections, with `psql` writing behind zero-cache's back:
  `harden.rs` (`HardenE2ETests`) and two seeds of `fuzz.rs`
  (`ChaosFuzzTests`), whose seeds take the same steps as Swift's. One chaos
  test runs at a time, as Swift serializes them. Swift's tests also check
  the outcomes its `addMutationOutcomeListener` hears, which is Swift's own
  (JS has none); here exactly-once rests on Postgres's rows and each
  client's last mutation ID.
- **Rust API breaks**: `cargo semver-checks` against the last release, at the
  bump the pending changesets declare. It does nothing until `zero-client` is
  on crates.io.

`.github/workflows/rust-nightly.yml`, nightly and by hand: JS's answers to
new random queries, pushes and client scenarios over synced aggregates on
fresh seeds (the ones Swift Nightly uses), five to ten times the PR checks'
cases, replayed by the conformance runner; and 150 seeds of the chaos fuzz
against `zero-cache`, the seeds Swift Nightly's chaos fuzz takes.

Still to come, with the milestones that need them: macOS and Windows (M6,
with the apps).

## Milestones

Each milestone ends with green CI on `rust-client` and a short report.

| Milestone | Scope                                                                                                                                                                                      | Done when                                                        | Status |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------- | ------ |
| M0        | Scaffolding: the `rust/` workspace, the conformance runner and its report, CI                                                                                                              | CI green on `rust-client`, running the runner on real vectors    | done   |
| M1        | Primitives and wire: JSON, JS numbers, string orders, hashes, primary and aggregate keys, value comparison, LIKE, TTL, AST and normalization, query hashes, protocol messages, poke chunks | Every primitive, AST, and protocol vector file and corpus passes | done   |
| M1.5      | Walking skeleton: connect to a real `zero-cache`, handshake, receive pokes, log rows                                                                                                       | Talks to a real server in CI                                     | done   |
| M2        | IVM: memory sources, every client operator, pipeline builder, change application, aggregates                                                                                               | Curated and seeded-fuzz IVM vectors pass                         | done   |
| M3        | Codegen: a Rust target for the schema codegen CLI                                                                                                                                          | Generated code compiles and drives M2                            | done   |
| M4        | Client engine on memory storage: connection, query manager, pokes, mutators, rebase, auth, recovery                                                                                        | Client scenario vectors pass                                     | done   |
| M5        | The storage trait and lifecycle in the core, the memory backend, and SQLite behind the `sqlite` feature; scenarios on both; end-to-end against Postgres and `zero-cache`                   | Scenarios pass on memory and SQLite; end-to-end passes           | done   |
| M6        | The shared zbugs model, then `apps/zbugs-gpui` and `apps/zbugs-rune`                                                                                                                       | Both apps run against a real backend                             |        |
| M7        | Hardening, docs, packaging, the first crates.io release (with tmm's approval)                                                                                                              | `0.1.0` ready to publish                                         |        |
