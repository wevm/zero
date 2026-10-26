# Coming from the JS client

What each part of `@rocicorp/zero` is called in Swift, and what isn't
ported.

## Overview

The Swift client behaves as the JS client does. Conformance vectors
generated from the JS client check it, and so do end-to-end tests against
`zero-cache`. Names follow JS where Swift allows. Swift differs in three
ways: queries and mutators are written per app instead of shared from
TypeScript, everything runs synchronously on the main actor, and some
web-only options have no counterpart.

## Options

`ZeroOptions` keeps the JS option names. `schema`, `mutators`, and `context`
are arguments of
``Zero/init(schema:context:mutators:options:scheduler:socketFactory:)``.

| JS option                                                                                         | Swift                                                                                                                            |
| ------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `cacheURL` (`server`, deprecated)                                                                 | `cacheURL`                                                                                                                       |
| `auth`, `userID`, `storageKey`                                                                    | the same                                                                                                                         |
| `logLevel`, `logSink`                                                                             | `logLevel`, `logSink` (a ``LogSink``)                                                                                              |
| `mutateURL`, `mutateHeaders`                                                                      | the same                                                                                                                         |
| `queryURL` (`getQueriesURL`, deprecated), `queryHeaders`                                          | `queryURL`, `queryHeaders`                                                                                                       |
| `onUpdateNeeded`                                                                                  | `onUpdateNeeded`, and ``Connection/updateNeeded``                                                                                  |
| `onClientStateNotFound`, `onStorageFailure`                                                       | the same                                                                                                                         |
| `disconnectTimeoutMs`, `pingTimeoutMs`, `maxHeaderLength`, `queryChangeThrottleMs`                | the same                                                                                                                         |
| `kvStore` (`'idb'` or `'mem'`)                                                                    | `storage`: `.sqlite()` or `.memory`                                                                                              |
| `hiddenTabDisconnectDelay`                                                                        | none: the client pushes pending mutations and disconnects when the app goes to the background, and reconnects when it comes back |
| `onOnlineChange` (deprecated)                                                                     | ``Connection/subscribe(_:)``                                                                                                       |
| `getTraceparent`, `slowMaterializeThreshold`, `batchViewUpdates`, `maxRecentQueries` (deprecated) | not ported                                                                                                                       |

## The client

| JS                                                                                                                                                                                                | Swift                                                                                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `new Zero(options)`                                                                                                                                                                               | `try Zero(schema:context:mutators:options:)`                                                          |
| `zero.clientID`, `zero.userID`, `zero.schema`, `zero.context`, `zero.closed`                                                                                                                      | the same                                                                                              |
| `await zero.clientGroupID`                                                                                                                                                                        | `zero.clientGroupID`, known at once                                                                   |
| `zero.query.issue`, `zql.issue`                                                                                                                                                                   | the generated `zql.issue`                                                                             |
| `zero.materialize(query)`                                                                                                                                                                         | `zero.materialize(_:ttl:)` for a ``MaterializedView``, `zero.liveQuery(_:ttl:)` for a typed ``LiveQuery`` |
| `zero.run(query, {type})`                                                                                                                                                                         | `try await zero.run(_:type:ttl:)`                                                                     |
| `zero.preload(query)`                                                                                                                                                                             | `zero.preload(_:ttl:)`, a ``Preload`` with `cleanup`, `cached`, and `complete`                          |
| `zero.mutate(request)`                                                                                                                                                                            | `zero.mutate(_:)`, a ``MutatorResult`` whose `client` and `server` are ``Deferred`` outcomes              |
| `zero.connection.state`, `.subscribe`, `.connect({auth})`                                                                                                                                         | ``Connection/state``, ``Connection/subscribe(_:)``, ``Connection/connect(auth:)``                           |
| `zero.close()`                                                                                                                                                                                    | `zero.close()`                                                                                        |
| `zero.mutate.issue.insert(...)` (CRUD mutators)                                                                                                                                                   | not ported: write a mutator                                                                           |
| `zero.delete()`, `dropAllDatabases()` | `await zero.delete()`, `await dropAllDatabases(_:)`, which return the ``DroppedDatabases`` |
| `zero.mutateBatch`, `zero.online`, `zero.onOnline`, `zero.inspector`, `zero.server`, `zero.storageKey`, `zero.idbName`, `zero.kvStore`, `zero.schemaVersion` | not ported |
| `zero.version`                                                                                                                                                                                    | ``protocolVersion``, the sync protocol version                                                          |

When the server no longer has the client's state, JS asks the app to
reload. Swift replaces the client inside the same `Zero` instead: views,
live queries, and preloads move to the new client and keep their identity.

A pending `run`, and a preload's pending `cached` and `complete`, belong to
the client that started them. When that client closes, by `close()` or by
being replaced, they throw a ``QueryError`` saying Zero was closed, as they
reject in JS. After a replacement, run it again: the same `Zero` now uses
the new client. Unlike JS, where only the calls pending at close reject and
a new one never settles, a new `run` that waits for `.complete` or `.cached`
on a closed `Zero`, and a new preload's `cached` and `complete`, throw that
same error at once; such a preload registers nothing. A `run` with
`.unknown` still returns the local rows.

## Queries

| JS                                                                   | Swift                                                                                          |
| -------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `defineQueries({issues: {open: defineQuery(({args, ctx}) => ...)}})` | `defineQuery("issues.open") { (args: Args, ctx: Context) in ... }`, named by its registry path |
| `.where('title', 'LIKE', '%x%')`                                     | `.where(\.title, .like, "%x%")`                                                                |
| `.where('open', true)`                                               | `.where(\.open, true)`                                                                         |
| `.where(({cmp, and, or, not, exists}) => ...)`                       | `.where { $0.open == true && $0.exists(\.labels) }` (``RowExpression``)                          |
| `.whereExists('labels', q => ...)`                                   | `.whereExists(\.labels) { $0.where(...) }`                                                     |
| `.related('comments', q => q.limit(10))`                             | `.related(\.comments) { $0.limit(10) }`                                                        |
| `.orderBy('created', 'desc')`                                        | `.orderBy(\.created, .desc)`                                                                   |
| `.limit(n)`, `.start(row)`, `.one()`                                 | the same                                                                                       |
| `escapeLike(text)`                                                   | ``escapeLike(_:)``                                                                               |

Columns and relationships are key paths of the generated row types. Rows
are structs; a relationship the query didn't ask for with `related` is nil.

Comparing with nil differs by form. The two-argument
`where(\.ownerID, nil)` uses `IS`, since `= NULL` matches nothing. The
three-argument form compares as JS compares `undefined`, so
`where(\.ownerID, .eq, nil)` matches no row.

An unnamed query, a `Query` such as `zql.issue` rather than a
``QueryRequest`` from `defineQuery`, only sees local data, as in JS without
`enableLegacyQueries`. It is for `tx.run` in mutators and for local reads.
The server never confirms it, so `run(query, type: .complete)` (or
`.cached`) never returns unless its task is cancelled, and `preload` of it
does nothing. The client logs a warning the first time it sees each such
query.

## Mutators

| JS                                                                                 | Swift                                                                                                |
| ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `defineMutators({issue: {create: defineMutator(async ({tx, args, ctx}) => ...)}})` | `defineMutator("issue.create") { (tx, args: Args, ctx: Context) in ... }`, a plain `throws` function |
| `await tx.mutate.issue.insert(row)`, `.upsert(row)`                                | `try tx.mutate.issue.insert(.init(...))`, `.upsert(...)`                                             |
| `await tx.mutate.issue.update({id, title})`                                        | `try tx.mutate.issue.update(.init(id: id, title: title))`                                            |
| `await tx.mutate.issue.delete({id})`                                               | `try tx.mutate.issue.delete(.init(id: id))`                                                          |
| `await tx.run(query)`                                                              | `try tx.run(query)`                                                                                  |
| `tx.location`, `tx.reason`, `tx.clientID`, `tx.mutationID`                         | the same                                                                                             |
| `throw new ApplicationError(message, {details})`                                   | `throw ApplicationError(message, details:)`                                                          |

`upsert` takes the whole row struct, so it writes every column: an
optional column left nil sets null on an existing row, where JS keeps a
column the value leaves out.

Each takes a value of a generated type, which `.init(...)` builds inline.
Like a JS object, it can also be built first, changed, and passed later:
`var change = Issue.Update(id: id)`, then `change.title = title`, then
`try tx.mutate.issue.update(change)`.

`update` takes the row struct's generated `Update`: the primary key, and
the columns to set. A column left nil keeps its value, as an `undefined`
one does in JS. An optional column is a ``ColumnUpdate``, so leaving it
(`.unchanged`, the default) differs from setting it to null (`.null`):
`.init(id: id, assigneeID: .null)` unassigns, and a literal or `.set(x)`
sets it. `tx.mutate.table("issue")` is the untyped ``TableMutator``, with
values keyed by column name.

Calling a mutator that isn't registered with the `Zero` throws in JS. In
Swift `mutate` returns a ``MutatorResult`` whose `client` and `server` are
both `.zeroError` with the same message, and logs it as an error. Nothing
is written or sent.

## React and SolidJS

`ZeroUI` covers the same ground for SwiftUI, UIKit, and AppKit.

| JS                                        | Swift                                                                   |
| ----------------------------------------- | ----------------------------------------------------------------------- |
| `<ZeroProvider zero={zero}>`, `useZero()` | `.zero(zero)` on a view, `@Environment(\.zero)`                         |
| `useQuery(query)`                         | `@ZeroQuery(query) var rows`, with `$rows.resultType` and `$rows.error` |
| `useConnectionState()`                    | `zero.connection.state`, which is observable                            |
| `useSuspenseQuery`, `useZeroOnline`       | not ported                                                              |
| none                                      | `DiffableAdapter`, for diffable data sources                            |

## Behavior to know

- **The main actor.** Queries, views, and mutators run synchronously on the
  main actor. Views and live queries are up to date when `mutate` returns.
  Only the network and storage are asynchronous. The exception is a
  `mutate` from a view listener: as a JS mutator commits after the
  notification that ran it, it runs once the current notification and the
  rest of its task are done, so every listener ends on the latest rows and
  every view sees each change. Its `client` outcome settles then.
- **Queries that can't be built.** A query JS rejects when it is
  materialized, such as a flipped `whereExists` over a subquery with a
  `limit`, makes `materialize` and `liveQuery` throw and `run` reject with
  the same message. Unlike JS, nothing stays registered with the server.
- **Updates a query can't take.** When applying a change makes a view's
  pipeline fail where JS throws (such as a `one` relationship that two rows
  match, or a flipped `whereExists` over a `limit` that a new row reaches),
  the views keep what they got of the change, and from then on, as in JS,
  the client's views can't be updated: a poke that fails puts the
  connection in the `error` state, a mutation that fails is committed and
  its `client` outcome is the error, and later mutations, `materialize`,
  and `run` fail with the same message.
- **Nulls in columns that can't be null.** When the server sends null for
  a column the schema says isn't optional, a `String`, `Double`, `Int`, or
  `Bool` property reads as `""`, `0`, or `false`. JS hands the null
  through. An enum or JSON column still fails to read.
- **Values of the wrong type for their column.** JS can't compare values
  of different types, such as a number in a column of strings: a mutation
  that writes one to a column a view sorts by fails, and so does every
  mutation after it, and a poke that brings one disconnects the client
  again after every reconnect. Swift sorts such a value with the others
  (by type, then by its JSON) and keeps working. The typed
  `tx.mutate.<table>` calls can't write one; a mutator can only write one
  through the untyped `tx.mutate.table(_:)`, and the server rejects such a
  mutation, so its row goes when the next poke comes.
- **Error messages.** Messages about data the client can't read follow
  valita's wording, as JS's do. A server frame that isn't JSON is the
  exception: JS shows the engine's `JSON.parse` message, and Swift its
  own.
- **JSON objects.** A `JSONValue` object is a ``JSONObject``, which keeps
  its keys as a JS object does: integer-like keys first in ascending order,
  then the others in the order they were set. Keys are compared by UTF-16
  code unit, so `"é"` and `"e\u{301}"` are two keys, as they are in JS. JSON
  text round-trips as `JSON.stringify` writes it.
- **Lone surrogates.** A Swift `String` can't hold half of a surrogate
  pair, so JSON text with an escaped lone surrogate (`"\ud800"`) reads it
  as U+FFFD, the replacement character; JS keeps the code unit. Two values
  that differ only in lone surrogates, such as the keys `"\ud800"` and
  `"\udc00"` of one object, are then the same value.
- **Mutation IDs.** A poke whose `lastMutationIDChanges` holds a number
  that isn't a safe non-negative integer (`Number.isSafeInteger`) is
  rejected as bad poke data: the client disconnects with a poke error. JS
  takes any number. A stored pending mutation with such an ID is dropped
  when the client opens, and logged as an error.
- **JSON nesting depth.** JSON nested more than 1,500 arrays and objects
  deep is rejected ("JSON nested too deeply"); JS accepts whatever fits its
  stack, and zero-cache writes up to about 4,100 levels. A poke is about 5
  levels before a row's values, and real data, such as a rich-text document
  in a JSON column, rarely passes a few hundred, but a row with a JSON
  column nested deeper than about 1,495 levels makes the client reject the
  message, disconnect, and fail the same way after it reconnects. Values
  that deep are parsed, written, compared, and hashed without recursion.
  Freeing one still recurses: in a release build 1,500 levels fit in a
  512 KB stack (a macOS test or cooperative thread), but a debug build
  overflows one when it frees objects nested more than about 1,000 deep.
- **Mutation outcomes after a disconnect (Swift only).** As in JS, when
  the connection drops before the server answers a mutation, its `server`
  settles with `.zeroError` and doesn't settle again after the mutation is
  replayed. JS has no way to learn the replay's result. Swift adds
  ``Zero/mutationOutcomes`` and ``Zero/addMutationOutcomeListener(_:)``,
  which report the server's outcome of every mutation once, with its
  mutator name and arguments, including mutations replayed after their
  `server` gave up (but not mutations from before the app last launched).
  To show a rejection, use either the stream or `server`,
  not both, or the user sees it twice.
- **Storage.** The replica is a SQLite database per user and URL, not
  IndexedDB. One process uses it at a time; there are no tabs to share it
  with.
- **Corrupt local data.** When the stored replica doesn't decode, JS drops
  the database and calls `onClientStateNotFound`, which reloads the page by
  default. Swift drops it and the client starts on a new, empty database at
  once; `onClientStateNotFound` is called only if the app set it. Pending
  mutations in the dropped database are lost, as in JS.
