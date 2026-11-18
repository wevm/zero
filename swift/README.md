# Zero for Swift

A native [Zero](https://zero.rocicorp.dev) client for iOS and macOS. Your
app queries a local replica of the server's data, and the replica stays in
sync. Mutations apply to the replica right away and run again on your
server, whose result wins.

It's a port of the JS client (`@rocicorp/zero`) and behaves the same:
conformance vectors generated from the JS client check it, and so do
end-to-end tests against `zero-cache` and the zbugs app.

> [!NOTE]
> Before 1.0, minor versions may change the API.

## Install

```swift
// Package.swift
.package(url: "https://github.com/wevm/zero-swift", from: "0.8.0"),
// and in your target's dependencies:
.product(name: "Zero", package: "zero-swift"),
.product(name: "ZeroUI", package: "zero-swift"),
```

```sh
npm install --save-dev zero-swift
npx zero-swift codegen       # reads zero-swift.json
npx zero-swift drift record  # what the drift harness checks your Swift against
```

Swift 6.2, iOS 26 or macOS 26, and a `zero-cache` that speaks sync protocol
57 (Zero 1.12 or later). `zero-swift codegen` generates a row struct per
table with its `Update` and `Key`, the `ZeroSchema`, a `zql` builder, and a
typed `tx.mutate.<table>` from your TypeScript schema; `--check` fails in CI
when the output is stale. See the
[CLI's README](https://github.com/wevm/zero/tree/main/packages/zero-swift-cli)
for `zero-swift.json`.

## Quick start

Write the queries and mutators your app runs in Swift, named as in your
TypeScript `defineQueries` and `defineMutators`: the client runs them on
the replica, and `zero-cache` has your server run the TypeScript ones.

```swift
import SwiftUI
import Zero
import ZeroUI

/// What queries and mutators get besides their arguments.
typealias Context = String?

struct ProjectArgs: Codable, Sendable { var projectID: String }
struct CloseArgs: Codable, Sendable { var id: String }

enum IssueQueries {
  static let open = defineQuery("issues.open") { (args: ProjectArgs, _: Context) in
    zql.issue.where(\.projectID, args.projectID).where(\.open, true).orderBy(\.modified, .desc)
  }
}

enum IssueMutators {
  static let close = defineMutator("issue.close") { (tx, args: CloseArgs, _: Context) in
    try tx.mutate.issue.update(.init(id: args.id, open: false))
  }
}

// The client runs on the main actor. Register every mutator the app may run.
let zero = try Zero(
  schema: .app, context: userID, mutators: [IssueMutators.close],
  options: ZeroOptions(cacheURL: "https://zero.example.com", userID: userID, auth: token))

// Rows read as `Issue`s, observable and kept current.
let issues = try zero.liveQuery(IssueQueries.open(.init(projectID: "p1")))
issues.data        // [Issue]
issues.resultType  // .complete once the server has confirmed them

// The same in a SwiftUI view, under `.zero(zero)`.
struct IssueList: View {
  @ZeroQuery(IssueQueries.open(.init(projectID: "p1"))) var issues
  var body: some View { List(issues) { Text($0.title) } }
}

// Applies to the replica before it returns, then runs on your server.
let result = zero.mutate(IssueMutators.close(.init(id: "i1")))
_ = try await result.server.value

// Every mutation's server outcome, in one place.
zero.addMutationOutcomeListener { o in
  if case .appError(let message, _) = o.outcome { print("\(o.name) was rejected: \(message)") }
}

// `.needsAuth` when the server rejects the token: get a new one, then connect with it.
try zero.connection.connect(.set(newToken))
```

## API at a glance

The DocC catalogs document every symbol; open them in Xcode with Product ›
Build Documentation.

| Concept            | API                                                                                                               | Notes                                                      |
| ------------------ | ----------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| Client             | `try Zero(schema:context:mutators:options:)`                                                                      | Main actor                                                 |
| Options            | `ZeroOptions(cacheURL:userID:auth:storage:)`, then `mutateURL`, `queryURL`, `logLevel`, ...                       | JS's option names                                          |
| Named queries      | `defineQuery("name") { (args: Args, ctx: Context) in query }`, called as `Q(args)`                                | `NoArgs` for none                                          |
| Query builder      | `zql.issue`, `.where(\.open, true)`, `.where { ... }`, `.whereExists`, `.related`, `.orderBy`, `.limit`, `.one()` | Also `count`, `sum`, `groupBy`, `distinct`                 |
| Read, kept current | `zero.liveQuery(query)`, a `LiveQuery` with `data`, `resultType`, `error`; `@ZeroQuery` in SwiftUI                | `useQuery`; observable                                     |
| Read otherwise     | `materialize(_:ttl:)`, `try await run(_:type:ttl:)`, `preload(_:ttl:)`                                            | Untyped view, read once, sync without a view               |
| Named mutators     | `defineMutator("name") { (tx, args: Args, ctx: Context) in ... }`                                                 | Synchronous `throws`; runs again on each rebase            |
| Writes in mutators | `try tx.mutate.issue.insert(.init(...))`, `.upsert`, `.update`, `.delete`; `try tx.run(query)`                    | Generated                                                  |
| Mutate             | `zero.mutate(M(args))`, a `MutatorResult` with `client` and `server` outcomes                                     |                                                            |
| Server outcomes    | `zero.addMutationOutcomeListener { ... }`, or `for await o in zero.mutationOutcomes`                              | Also hears mutations replayed after a reconnect            |
| Connection         | `zero.connection.state` (observable), `.subscribe(_:)`, `try .connect(.keep \| .set(token) \| .clear)`            | `.needsAuth` when the server rejects the token             |
| Background         | follows the app's lifecycle and the network; `zero.setVisible(false)`, `await zero.pendingSettled(timeout:)`      | Disconnects in the background, or when the app hides it    |
| Storage            | `.sqlite()` (default) or `.memory`; `await zero.delete()`, `await dropAllDatabases(_:)`                           |                                                            |
| Inspector          | `zero.inspector`: `metrics()`, `client.queries()`, `analyzeQuery(_:)`, `serverVersion()`                          | Logs slow materializations past `slowMaterializeThreshold` |
| Testing            | `ZeroTesting`: `Drift.failures`, `ManualScheduler`, `MockSocketFactory`                                           | Drift checks your Swift against the TypeScript             |

## Products

| Product       | Adds                                                                                                         | Platforms                                  |
| ------------- | ------------------------------------------------------------------------------------------------------------ | ------------------------------------------ |
| `Zero`        | The client                                                                                                   | iOS, macOS, Linux                          |
| `ZeroUI`      | `@ZeroQuery`, `.zero(zero)`, and `DiffableAdapter` for UIKit and AppKit                                      | iOS, macOS                                 |
| `ZeroVirtual` | `ZeroVirtualizer`, which pages a long list in from your queries, and its `VirtualList` and `VirtualListView` | iOS, macOS (the virtualizer also on Linux) |
| `ZeroTesting` | The drift harness and a client without a server                                                              | iOS, macOS, Linux                          |

On Linux, `Zero` has no socket: pass your own `SocketFactory` to sync.

## Differences from JS

- **The main actor.** Queries, views, and mutators run synchronously on it;
  only the network and storage are asynchronous. Views are current when
  `mutate` returns.
- **Queries and mutators are written in Swift**, under the TypeScript
  names, and the drift harness checks them against the TypeScript ones.
  CRUD mutators (`zero.mutate.issue.insert`) aren't ported: write a mutator.
- **Storage is SQLite**, where JS uses IndexedDB.
- **Background:** on Apple platforms the client pushes pending mutations and
  disconnects when the app goes to the background, and reconnects when it
  comes back; there's no `hiddenTabDisconnectDelay`. For a window, scene, or
  screen that can be out of sight in the foreground, the app calls
  `setVisible(_:)`, after `await pendingSettled(timeout:)` to push pending
  mutations first.
- **UI:** `ZeroUI` stands in for zero-react; `useSuspenseQuery` isn't ported.
- **An unnamed query only sees local data**, as in JS without
  `enableLegacyQueries`: the server syncs a `defineQuery` request.

[JSParity.md](Sources/Zero/Zero.docc/JSParity.md) maps each part of the JS
API to its Swift name, and lists what isn't ported and each documented
difference.

## Compatibility

| Zero for Swift | Sync protocol | `zero-cache` | Conformance vectors from |
| -------------- | ------------- | ------------ | ------------------------ |
| 0.1–0.5.5      | 53            | 1.11+        | Zero 1.12                |
| 0.5.6+         | 57            | 1.12+        | Zero 1.12                |

A Zero 1.11 `zero-cache` rejects protocol 57 with `VersionNotSupported`:
with one, use zero-swift 0.5.5.

## Documentation

- [Zero's docs](https://zero.rocicorp.dev/docs/introduction), and for coding
  agents [llms.txt](https://zero.rocicorp.dev/llms.txt)
- The DocC catalogs:
  [Getting started](Sources/Zero/Zero.docc/GettingStarted.md),
  [Coming from the JS client](Sources/Zero/Zero.docc/JSParity.md),
  [`ZeroUI`](Sources/ZeroUI/ZeroUI.docc/ZeroUI.md),
  [`ZeroVirtual`](Sources/ZeroVirtual/ZeroVirtual.docc/ZeroVirtual.md), and
  [`ZeroTesting`](Sources/ZeroTesting/ZeroTesting.docc/ZeroTesting.md)
- [Benchmarks](BENCHMARKS.md) against the JS client, and
  [the design](DESIGN.md)
- The example app,
  [`apps/zbugs-swift`](https://github.com/wevm/zero/tree/main/apps/zbugs-swift)

## License

Apache-2.0
