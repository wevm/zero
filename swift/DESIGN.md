# Zero Swift client — design

A native Swift client for Zero on iOS and macOS, with behavioral parity with
the JS client (`packages/zero-client`). Parity is enforced by a conformance
suite whose expected values come from running the JS implementation.

The server side (`zero-cache`, the app's API server, Postgres) is unchanged.
The Swift client speaks the existing sync protocol to `zero-cache`.

This document records the decisions made while scoping the project. Each
decision is listed with the reason that settled it.

## Goals and non-goals

Goals:

- Behavioral parity with the JS client for every non-deprecated public API.
- Swift-idiomatic API that keeps the JS concept names (`query`, `mutate`,
  `materialize`, `preload`, `connection`, TTL) so the JS docs map 1:1.
- Works with SwiftUI, UIKit, and AppKit (Mac Catalyst through UIKit).
- A conformance suite that the JS implementation generates and both
  implementations pass.
- Public-SDK quality, driven by a real app (`apps/zbugs-swift`).

Non-goals:

- Running or configuring servers from Swift.
- Web-only features: IndexedDB, BroadcastChannel and Web Locks multi-tab
  coordination, `hiddenTabDisconnectDelay`, page reloads.
- Deprecated JS APIs (see [Scope](#scope)).
- A production transport on Linux. The core builds and tests on Linux, but only
  the Apple layer ships a real WebSocket transport.

## Platforms and toolchain

| Item             | Decision                                                                                                                          |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| OS floor         | iOS 26, macOS 26                                                                                                                  |
| Toolchain        | Swift 6.2 or newer (`swift-tools-version: 6.2`), Swift 6 language mode, strict concurrency                                        |
| Linux            | `Zero` (core) and `ZeroTesting` build and test on Linux; `ZeroUI` is Apple-only                                                   |
| Other Apple OSes | Not promised; nothing in the core should block them                                                                               |
| Dependencies     | No third-party runtime dependencies. Test-only dependencies are allowed (for example SwiftNIO for the Linux end-to-end transport) |
| Tests            | Swift Testing; XCTest only where Apple requires it (UI tests)                                                                     |
| Formatting       | `swift-format` (ships with the toolchain), enforced in CI. No SwiftLint                                                           |

iOS 26 and macOS 26 give `Observations` and default UIKit/AppKit observation
tracking (`updateProperties()`), so one `@Observable` type drives SwiftUI,
UIKit, and AppKit. Only the UI layer depends on the OS version, so the floor can
be lowered later with `@available` shims.

## Repository layout and distribution

```
swift/                      Swift package (source of truth)
  Package.swift
  Sources/Zero/             core engine, platform-agnostic
  Sources/ZeroUI/           SwiftUI, UIKit, AppKit helpers; URLSession transport
  Sources/ZeroTesting/      drift-harness replay API and test helpers
  Tests/...
  VERSION
packages/zero-conformance/  private JS package: fixtures, vector generators, JS runners
packages/zero-swift-cli/    npm package `zero-swift`: `codegen`, `drift record`
apps/zbugs-swift/           dogfood app (iOS + macOS)
```

- SwiftPM needs `Package.swift` at a repository root and clones the whole
  repository. This monorepo's `.git` is about 436 MB, so the Swift package is
  distributed through a mirror repository, `wevm/zero-swift`. CI fills it with a
  `git subtree split` of `swift/` and tags releases there. Nobody commits to the
  mirror directly.
- The mirror is private until launch, then public. The Swift source is visible
  in this public monorepo either way.
- SwiftPM products: `Zero`, `ZeroUI`, `ZeroTesting`.
- The CLI ships as the unscoped npm package `zero-swift` (we don't own the
  `@rocicorp` scope) with a peer dependency on `@rocicorp/zero`. It is
  versioned with the Swift releases.
- License: Apache-2.0, like this repository.
- Docs: DocC, plus a JS→Swift parity matrix.

### Versioning and releases

- Independent semver starting at `0.1.0`. `1.0` once zbugs dogfooding shows the
  API is stable.
- A PR bumps `swift/VERSION`. On merge, CI pushes the subtree split to the
  mirror and tags it.
- The README carries a compatibility table: Swift version → sync protocol
  version → minimum `zero-cache` version → the Zero JS release whose vectors
  it passes.

### Fork hygiene and CI policy

- JS-side changes are additive only: new packages and files, no edits to
  existing tests. This keeps upstream merges painless and the work upstreamable.
- The Swift client tracks this fork's `main`.
- CI fails when the checked-in conformance vectors are stale, or when Swift
  fails them. Those are signs of real behavioral divergence.
- CI does not fail on a JS protocol bump. `zero-cache` must keep supporting
  every sync protocol from `MIN_SERVER_SUPPORTED_SYNC_PROTOCOL` (30) through
  `PROTOCOL_VERSION` indefinitely
  (`packages/zero-protocol/src/protocol-version.ts`), so an older Swift client
  keeps working. The Swift client declares its own protocol version, starting
  at 53.

## Architecture

The JS client has three layers, and the Swift client ports them as follows.

| JS subsystem                                                                                                | Size (non-test LOC) | Swift approach                                    |
| ----------------------------------------------------------------------------------------------------------- | ------------------- | ------------------------------------------------- |
| `zql` IVM operators and pipeline builder (`zql/src/ivm`, `zql/src/builder`)                                 | ~10.7k              | 1:1 operator port                                 |
| `zero-client` logic: connection state machine, query manager, poke handler, mutation tracker, mutator proxy | ~11k total          | Faithful behavioral port                          |
| Replicache (dag, B-tree, memdag, persist, refresh, GC)                                                      | ~18.6k              | Keep the semantics, replace the storage machinery |

Rationale:

- **IVM runs on the client.** The server sends rows, not query results. The
  client keeps every synced row in in-memory `MemorySource`s
  (`zero-client/src/client/context.ts`), builds an operator pipeline for every
  materialized query, and updates it incrementally on pokes and optimistic
  mutations. Rebase forks the sources (`ivm-branch.ts`). A 1:1 port keeps the
  edge-case behavior (Take boundaries, Exists with limits, change ordering)
  identical.
- **Replicache semantics stay.** Client and client-group IDs, the pending
  mutation queue, last mutation IDs, cookies, rebase on poke, `cached` result
  tracking, client GC, and client-state-not-found all behave as in JS.
- **Replicache storage machinery goes.** The dag, B-tree, memdag, and refresh
  stack exist mostly for IndexedDB performance and multi-tab sharing. Swift
  uses SQLite tables for persistence and in-memory IVM sources for reads.

### Threading

- The engine (IVM, views, mutator execution) runs on `@MainActor`, as the JS
  client runs on the main thread. `materialize` returns data synchronously, so
  there is no empty first frame, and the vectors' ordering semantics carry over
  directly.
- SQLite I/O, the socket, and poke JSON decoding run off the main actor.

### Storage

- SQLite (system `libsqlite3`), WAL mode.
- Pending mutations are written durably before they are pushed. Synced-row
  writes are coalesced.
- Default location `Application Support/Zero/<name>.sqlite`, configurable (for
  example an App Group container).
- iOS Data Protection default `completeUntilFirstUserAuthentication`,
  configurable. `complete` would make the database unreadable while the device
  is locked and break the background flush.
- No database-level encryption.
- A Swift-side storage version. On a mismatch, drop the store and resync, as JS
  does.
- If the store can't be opened, fall back to memory and call
  `onStorageFailure`, as JS does.
- One process per store in v1: one `Zero` instance per user and storage key per
  process. Concurrent cross-process access (app extensions) is deferred.

### Transport

- The core defines a transport protocol and never touches a socket directly.
- The Apple layer implements it with `URLSessionWebSocketTask`.
- Tests use an in-memory transport that replays scenario vectors, and, on Linux
  end-to-end runs, a test-only SwiftNIO WebSocket transport.
- Wire details follow `zero-protocol`: connect to `/sync/v53/connect`, put
  `initConnection` and the auth token in `Sec-WebSocket-Protocol` while they fit
  under `maxHeaderLength`, and accept binary poke chunks (protocol 52 and later).

### App lifecycle and recovery

- iOS: on background, flush pending pushes inside a short background task, then
  disconnect. Reconnect on foreground. No background sync in v1.
- macOS: stay connected while the app runs. Reconnect after wake.
- Both: reconnect immediately when `NWPathMonitor` reports the network is back.
  This is a platform adaptation in the Apple layer; JS relies on ping/pong only.
- New client group and client-state-not-found: the SDK transparently recreates
  its internal client. Active queries re-materialize and the `@Observable`
  objects keep their identity.
- Version-not-supported and schema-not-supported can't be fixed by reloading a
  native app. They surface as a callback plus an `.updateRequired` connection
  state so the app can prompt for an update.
- Documentation must say that schema changes stay additive while old app
  binaries are in the wild. The server rejects a client whose schema references
  a dropped or retyped column
  (`zero-cache/src/services/view-syncer/client-schema.ts`).

## API

### Scope

In scope for v1 (everything non-deprecated):

- Named queries and mutators, with CRUD inside mutators (`insert`, `upsert`,
  `update`, `delete`).
- The full ZQL builder: `where` (all operators), `whereExists` (including
  `flip` and `scalar`), `related`, `orderBy`, `limit`, `start`, `one`.
- `run` with `unknown`, `cached`, and `complete`; `materialize`; `preload`;
  TTL.
- Connection state; `connect(auth:)`; the `needs-auth` flow.
- `close`, `delete`, `context`.
- `mutateURL` and `queryURL` with their headers; `getTraceparent`.
- `onUpdateNeeded`, `onClientStateNotFound`, `onStorageFailure`.
- Storage options (SQLite file, memory, custom store), logging,
  `maxHeaderLength`, and binary poke chunks.

Out of scope:

- Deprecated: legacy AST queries (`enableLegacyQueries`), legacy CRUD on
  `zero.mutate`, `mutateBatch`, and `_zero_crud` (`enableLegacyMutators`),
  map-style mutators, `syncedQuery*`, the `server` and `getQueriesURL` aliases,
  `online`/`onOnline`, `maxRecentQueries`, `tx.query`.
- Web-only features (see [Non-goals](#goals-and-non-goals)).
- Analytics. The JS client's metrics reporting is disabled
  (`zero-client/src/client/zero.ts`), so there is nothing to match.

Deferred to v1.x: the inspector.

### Schema

The TS schema (`schema.ts`) is the source of truth. `zero-swift codegen` loads
it and emits plain Swift: table descriptors (client and server names, column
types, optional, primary and unique keys, relationships, enums), `Codable` row
structs, and typed column references for the builder.

| TS                 | Swift                                                                                         |
| ------------------ | --------------------------------------------------------------------------------------------- |
| `string()`         | `String`                                                                                      |
| `boolean()`        | `Bool`                                                                                        |
| `number()`         | `Double`; per-column override to `Int` in the codegen config (checked, exact up to 2^53)      |
| `enumeration<…>()` | `enum: String, Codable`                                                                       |
| `json<T>()`        | A generated `Codable` type when `T` is a plain object or literal union, otherwise `JSONValue` |
| `.optional()`      | Swift optional                                                                                |

Row struct names are the table names in PascalCase. The generated code stays
readable enough to hand-write.

### Queries and mutators

Named queries and mutators are hand-written in Swift, including their argument
types. Names are explicit dotted strings that match the TS registries.

```swift
enum IssueQueries {
  static let byStatus = defineQuery("issues.byStatus") { (args: ByStatus, ctx: AuthData?) in
    zql.issue.where(\.status, args.status).orderBy(\.modified, .desc)
  }
}

enum IssueMutators {
  static let create = defineMutator("issue.create") { (tx, args: CreateIssue, ctx: AuthData?) async throws in
    try await tx.mutate.issue.insert(.init(id: args.id, title: args.title))
  }
  static let notify = defineMutator.serverOnly("issue.notify", args: Notify.self)
}

let zero = Zero(schema: .app, context: auth, mutators: [IssueMutators.create, IssueMutators.notify])
let view = zero.materialize(IssueQueries.byStatus(.init(status: .open)))
zero.mutate(IssueMutators.create(.init(id: newID(), title: "…")))
```

- `Zero` is generic over the schema and the context type.
- Mutators need a registry: pending mutations are stored as name plus args and
  replayed by name on rebase, including after a restart. Registering two
  mutators under one name throws at init. A pending mutation with an unknown
  name is reported as an error, not a crash.
- Queries don't need a registry. Every named query needs a Swift body, because
  the client builds the AST locally to compute results.
- `.serverOnly` mutators have no optimistic effect; they only push name and
  args.
- Argument validation is structural, through the `Codable` arg types, with an
  optional hand-written `validate(args)` hook per definition. The server's
  Standard Schema validator stays the authority.

### Query builder

```swift
zql.issue
  .where(\.status, .open)                                                  // ≈ where('status', 'open')
  .where { $0.priority >= 2 && ($0.assignee == me || $0.assignee == nil) } // ≈ where(({cmp, and, or}) => …)
  .whereExists(\.labels) { $0.where(\.name, "bug") }
  .related(\.comments) { $0.orderBy(\.created, .desc).limit(10) }
  .orderBy(\.modified, .desc)
  .limit(50)
```

- Key paths map to columns through the generated table descriptors.
- Generated row structs carry optional relationship properties (for example
  `comments: [Comment]?`) that are filled only when `related` requested them.
- `one()` returns `Row?`.

### Mutate, errors, auth, logging

- `zero.mutate(request)` is `@discardableResult` and returns a handle with
  `client` and `server` async throwing properties, the same split as JS
  `{client, server}`.
- One `ZeroError` enum mirrors the JS kinds: application (with message), input
  validation, storage failure, transaction closed, rejected while disconnected
  or closed, and Zero/server errors. The public API uses plain `throws` because
  mutator bodies can throw arbitrary errors.
- Auth: an `auth` token option and a `.needsAuth` connection state on 401/403.
  The app calls `connection.connect(auth:)`, which sends `updateAuth` when
  already connected. An async token-provider closure is deferred.
- Logging: a `LogSink` protocol and a `logLevel` option defaulting to `.warn`
  (what the JS code does). The default sink is `os.Logger` (subsystem `zero`)
  on Apple platforms and stderr on Linux. No telemetry.

### UI integration

1. **Core view.** `materialize(query)` returns a view with listener callbacks
   and an `AsyncSequence` of immutable snapshots plus per-row diffs.
   Framework-agnostic; builds on Linux.
2. **`@Observable` query object** with `data`, `resultType`, and `error`.
   Works in SwiftUI and is tracked automatically by UIKit and AppKit.
3. **SwiftUI.** Environment injection, a `@Query`-style property wrapper (≈
   `useQuery`), and connection state.
4. **UIKit and AppKit.** A diffable-data-source snapshot adapter keyed by
   primary key.

No Combine publishers.

## Conformance

The public repository has no JS tests for `zql`, `zero-protocol`, `zqlite`, or
`zero-cache`; upstream `rocicorp/mono` doesn't either. Only `replicache`,
`shared`, and `zero-client` have tests. Vectors are therefore generated by
running the JS implementation as the oracle.

`packages/zero-conformance` (private, additive) holds the fixtures, the
generators, and the JS-side runners. Vectors come in three layers:

1. **Primitives.** The xxHash32-based hashes (`hashOfAST`,
   `hashOfNameAndArgs`), primary-key to storage-key encoding, value comparison
   (JS compares strings by UTF-16 code unit; Swift `String <` differs), LIKE
   and ILIKE (reusing `zql/src/builder/like-test-cases.ts`), TTL parsing and
   clamping, AST normalization, and protocol message decode/encode including
   binary poke chunks.
2. **Query semantics.** IVM results plus the per-push sequence of view changes
   (add, remove, edit, child) for queries over fixture data under push
   sequences. Internal operator traces are not recorded, so upstream refactors
   don't churn the vectors.
3. **Client behavior.** Scenarios in a JSON step format: open with options,
   connect, inject a downstream message, advance a fake clock, mutate,
   materialize or destroy, then expect an upstream message, view data or result
   type, or connection state. A JS runner executes each scenario against the
   real `TestZero` and `MockSocket` to fill in and verify expectations; a Swift
   runner replays it. Scenarios are seeded from the existing `zero-client`
   tests (`zero.test.ts`, the poke handler, query manager, mutation tracker, and
   connection manager).

Fixtures:

- `zql/src/query/test/test-schemas.ts`.
- The zbugs schema.
- A synthetic edge-case schema: every column type, compound primary keys,
  optional columns, enums, json, self-relations, and junction tables.

Datasets include empty strings, nulls, Unicode (emoji and surrogate pairs),
`-0`, fractional numbers, integers above 2^31 and near 2^53, and nested JSON.

Vector storage and fuzzing:

- Curated vectors are checked in. CI regenerates them from JS and fails if they
  are stale.
- Seeded random query and push fuzz cases are generated in CI from fixed seeds
  (not checked in) and run against Swift.
- A live differential harness (JS drives a Swift CLI over stdio with random
  seeds and shrinks failures) is deferred to v1.x.

### Drift harness for apps

Hand-written Swift queries and mutators can drift from their TS counterparts.
The drift harness catches it in the app's own tests.

- **TS side.** The app author writes a samples file: argument sets per query,
  and argument sets plus seed rows per mutator. `zero-swift drift record` runs
  the TS definitions and writes `drift-vectors.json`.
- **Swift side.** `ZeroTesting` exposes
  `assertNoDrift(vectors:queries:mutators:)` for Swift Testing or XCTest.

Checks:

- Each query sample's normalized AST hash matches.
- Each mutator sample produces the same row operations.
- Every name in the TS registries has a Swift definition, and the other way
  round.
- Each sample's args JSON decodes into the Swift args type and re-encodes to
  identical JSON. This catches renamed fields and `nil`-versus-omitted
  mismatches (zod's `.optional()` rejects `null`).

A mutator that calls `Date.now()` or generates IDs internally can't be
compared, so the harness flags it. The docs recommend the Zero convention of
passing IDs and timestamps as arguments.

### CI

- **Linux, every PR.** Swift build, unit tests, and conformance vectors; the
  vector staleness check; an end-to-end run against a real `zero-cache` and
  Postgres with the zbugs schema, including the drift harness.
- **macOS, path-filtered to `swift/` and `packages/zero-conformance/`.** The
  Apple layer (URLSession transport against a local mock WebSocket server,
  SwiftUI, UIKit, and AppKit helpers), iOS simulator tests, and the
  `apps/zbugs-swift` build.
- **Benchmarks.** Ports of the key JS benchmarks
  (`zero-client/src/client/zero.bench.ts`, the shared B-tree and hash
  benchmarks) plus Swift benchmarks for query hydration over N rows, poke
  apply, mutation round trip, and memory per 100k rows. Tracked next to the JS
  numbers, non-blocking in v1, with a goal of no slower than JS.

## Dogfood app: `apps/zbugs-swift`

- A SwiftUI multiplatform app (iOS and macOS), plus one UIKit screen and one
  AppKit screen (the issue list through the diffable adapter), so every UI
  layer gets exercised.
- Features: issue list with filters, issue detail, comments, labels, create
  and edit issue.
- Built in macOS CI; not shipped to TestFlight or the App Store in v1.
- Auth: zbugs login sets the JWT as a browser cookie and redirects
  (`apps/zbugs/api/index.ts`), which a native app can't read. An additive
  change to the zbugs API allows a `zbugs-swift://` redirect that carries the
  JWT, used through `ASWebAuthenticationSession`. A dev script mints a JWT
  with the dev key for local and CI end-to-end runs.

## Milestones

Each milestone is done when its conformance layer passes.

| #    | Milestone                                                                                                                                                                                             | Done when                                |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| M0   | Scaffolding: `swift/` with the three targets, Linux and macOS CI, `swift-format`, the `packages/zero-conformance` skeleton, the mirror job                                                            | CI green on an empty package             |
| M1   | Primitives: JSON value model, hashes, key encoding, UTF-16 value comparison, LIKE, TTL, AST and normalization, protocol codecs including binary poke chunks                                           | Primitive vectors pass                   |
| M1.5 | Protocol-only walking skeleton: connect to a real `zero-cache`, handshake, receive pokes, log rows                                                                                                    | Talks to a real server                   |
| M2   | IVM: `MemorySource`, all client operators, pipeline builder, change application, the builder API                                                                                                      | Curated and seeded-fuzz IVM vectors pass |
| M3   | Schema codegen CLI; generate the zbugs and fixture schemas                                                                                                                                            | Generated code compiles and drives M2    |
| M4   | Client engine on memory storage: connection state machine, query manager (named queries, TTL, cached/complete), poke handler, mutators, `tx`, pending queue, rebase, mutation tracker, auth, recovery | Client scenario vectors pass             |
| M5   | SQLite persistence, client and client-group lifecycle and GC, storage version, memory fallback                                                                                                        | Scenarios pass on SQLite                 |
| M6   | Apple layer: URLSession transport, lifecycle, `NWPathMonitor`, `os.Logger`, `@Observable` queries, SwiftUI wrapper and environment, UIKit and AppKit diffable adapters                                | macOS CI green                           |
| M7   | Linux end-to-end against real `zero-cache` and Postgres; drift harness; zbugs queries and mutators ported to Swift                                                                                    | End-to-end and drift checks pass         |
| M8   | `apps/zbugs-swift` (iOS and macOS, UIKit and AppKit screens, auth), benchmarks, DocC, parity matrix                                                                                                   | `0.1.0` ready to tag on the mirror       |

Deferred to v1.x: the inspector, the live differential fuzzer, an async auth
provider, cross-process access through App Groups, validator-derived drift
samples, and making the mirror public at launch.

Creating the mirror repository, its CI token, the `0.1.0` tag, and the npm
`zero-swift` publish are external actions that need explicit approval.
