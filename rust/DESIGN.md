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

tmm chose `zero-client` for the crate name (`zero` is taken on crates.io;
`zero-client` was free on 2026-11-10).

## Releases

Like the Swift client's (`swift-release.yml`):

- Changesets versions the client. `packages/zero-rust` is a version carrier,
  not published to npm: it holds the version and `CHANGELOG.md`. Add a
  changeset for `zero-rust` to release a change.
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
  changelog, and `cargo publish` of `zero-client`. 0.0.0 is never released.
- The mirror token comes from github-sts, authorized by
  `rust/.github/sts/mirror.sts.yaml`, which the first mirror push publishes.
  crates.io's trusted publishing needs the crate to exist, so the first
  publish uses a `CARGO_REGISTRY_TOKEN` in the environment.

So the first mirror push and every crates.io publish happen only when tmm
merges a Version Packages PR that carries a `zero-rust` version.

## Architecture (planned)

- A synchronous, single-owner engine with an injected clock and deterministic
  scheduling; no `await` inside a mutator or a rebase.
- Queries read in-memory IVM sources. Persistence (snapshots, pending
  mutations, cookies, and client and client-group lifecycle, with
  Replicache's semantics but not its IndexedDB-shaped storage) is planned on
  SQLite (`rusqlite`). Whether it is opt-in, behind a trait or a feature, is
  tmm's to decide; until then `rusqlite` is not a required dependency.
- The socket is behind a `Transport` trait that the core defines (tmm's
  decision): by default `zero-client` depends on no async runtime and no
  WebSocket crate, so an app can bring its own transport without
  downloading crates it doesn't use. A reference transport,
  `tokio-tungstenite` with rustls, comes behind an opt-in Cargo feature of
  `zero-client` for a quick start. Its public API exposes no tungstenite
  types, so a tungstenite major bump doesn't make a `zero-client` one. To
  keep downloads small, `tokio-tungstenite` is declared with
  `default-features = false` and only the features the transport uses, and
  TLS is a feature of its own: rustls pulls in a crypto provider (`ring`,
  or `aws-lc-rs`, whose `aws-lc-sys` needs cmake and a C toolchain), so an
  app on `ws://` or behind its own TLS pulls in none of it. The quick-start
  path (`cargo add zero-client --features tokio-transport,tls`) goes in the
  README with M4.
- The tests already speak to a real `zero-cache` through
  `tokio-tungstenite`, as a dev-dependency only (M1.5).
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

Deliberate differences from JS, each counted separately in the report, are
in [PARITY.md](PARITY.md).

### CI

`.github/workflows/rust.yml`, on `main`, `rust-client`, and every PR:

- **Conformance Vectors**: the checked-in vectors match current JS, and the
  seeded fuzz corpora are generated for the next job.
- **Rust (Linux)**: rustfmt, clippy with warnings denied, the tests, the
  conformance report (in the job summary), rustdoc with warnings denied, and
  `cargo package`, which builds the crate as crates.io would get it, outside
  the workspace, and a build on the `rust-version` the manifest names.
- **Rust end-to-end (Linux)**: Postgres and `zero-cache` from this repo's
  source over the fixture database (`packages/zero-conformance/src/e2e/`),
  and `zero-client/tests/walking_skeleton.rs` against them, with
  `ZERO_E2E_URL` set. Without it the test skips, so `cargo test` needs no
  server.
- **Rust API breaks**: `cargo semver-checks` against the last release, at the
  bump the pending changesets declare. It does nothing until `zero-client` is
  on crates.io.

Still to come, with the milestones that need them: a nightly differential run on fresh seeds
(M2, like `swift-nightly.yml`), macOS and Windows (M6, with the apps).

## Milestones

Each milestone ends with green CI on `rust-client` and a short report.

| Milestone | Scope                                                                                                                                                                                      | Done when                                                        | Status |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------- | ------ |
| M0        | Scaffolding: the `rust/` workspace, the conformance runner and its report, CI                                                                                                              | CI green on `rust-client`, running the runner on real vectors    | done   |
| M1        | Primitives and wire: JSON, JS numbers, string orders, hashes, primary and aggregate keys, value comparison, LIKE, TTL, AST and normalization, query hashes, protocol messages, poke chunks | Every primitive, AST, and protocol vector file and corpus passes | done   |
| M1.5      | Walking skeleton: connect to a real `zero-cache`, handshake, receive pokes, log rows                                                                                                       | Talks to a real server in CI                                     | done   |
| M2        | IVM: memory sources, every client operator, pipeline builder, change application, aggregates                                                                                               | Curated and seeded-fuzz IVM vectors pass                         |        |
| M3        | Codegen: a Rust target for the schema codegen CLI                                                                                                                                          | Generated code compiles and drives M2                            |        |
| M4        | Client engine on memory storage: connection, query manager, pokes, mutators, rebase, auth, recovery                                                                                        | Client scenario vectors pass                                     |        |
| M5        | SQLite persistence and lifecycle; scenarios on SQLite; end-to-end against Postgres and `zero-cache`                                                                                        | Scenarios pass on SQLite; end-to-end passes                      |        |
| M6        | The shared zbugs model, then `apps/zbugs-gpui` and `apps/zbugs-rune`                                                                                                                       | Both apps run against a real backend                             |        |
| M7        | Hardening, docs, packaging, the first crates.io release (with tmm's approval)                                                                                                              | `0.1.0` ready to publish                                         |        |
