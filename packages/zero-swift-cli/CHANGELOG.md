# zero-swift

## 0.7.9

### Patch Changes

- df16c20: A query whose subquery orders by one column twice, in opposite directions, such as `.whereExists("comments", flip: true) { $0.orderBy("text", .desc).orderBy("text", .asc) }`, no longer keeps rows it should drop. Looking up the subquery's rows for one parent row started past all of them, so a flipped `whereExists` found no comments for a deleted issue and left the issue in the view. The lookup now starts at the first row for that parent, as it does for any other ordering. JS's client still has this bug.

## 0.7.8

### Patch Changes

- a0146e4: The client's WebSocket frames, such as mutations and query changes, no longer wait for a busy main thread. The writer that sends them one at a time ran on the main actor and went back to it after every frame, so while the app kept the main thread busy each frame waited its turn: on CI, 107 frames took 30 seconds to go out. The writer now runs off the main actor and still sends the frames, and the close after them, in order.

## 0.7.7

### Patch Changes

- 827a7ad: On Apple platforms, the client now reconnects when the server's connection ends partway through a message, even if URLSession is slow to report the end. URLSession hands over the part of the message it has, and the client used to wait 1 second for the connection to end before deciding the message was really bad. On a busy device URLSession can report the end several seconds later, so the client stopped in `error` instead of reconnecting. The client now sends a WebSocket ping instead. If the ping fails, the message was cut off and the client reconnects. If the pong comes back, the message was really bad and the error stands, as before. If neither comes within 10 seconds, the error also stands.

## 0.7.6

### Patch Changes

- fe16cb1: `VirtualList` and `VirtualListView` no longer move the rows on screen when rows are added above them while the list rests at its end, past its first page. The added rows pushed the last rows out of the page. Their estimated height then took their place below the page, so when they were taller than estimated, the content got shorter under the scroll position and the view clamped it. Once the next page brought those rows back, everything on screen had shifted by the difference. The space below the page now keeps the height the rows were laid out at until the page reaches them again.

## 0.7.5

### Patch Changes

- e417902: The client now logs a connection's life with zero-client's messages and levels: "Connecting...", "Connecting to", "Got socket open event", "Waiting for connection to be acknowledged", "Connected", "Connection attempt timed out", the ping's outcome, "disconnecting", and the close. At `.debug`, it also logs a WebSocket send that hasn't finished after a second, which holds up the frames and the close after it, and when it sends the close.

## 0.7.4

### Patch Changes

- 494e9b0: `VirtualListView` no longer makes UIKit log "Observation tracking feedback loop detected" for its scroll view when its rows change often, as in a list whose rows the server updates many times a second. The list read the virtualizer's `snapshot` while laying a snapshot out, so UIKit and AppKit, which follow what a view's layout reads, laid it out again for every snapshot after that, on top of the layout pass the list already asks for. The list now reads the snapshot without observing it. `VirtualList` does the same.

## 0.7.3

### Patch Changes

- 2e384e1: `VirtualList` and `VirtualListView` no longer move the rows on screen when rows are added above them near the end of a list that one page held. The added rows pushed the last rows on screen out of the page, so the scroll that keeps the rows on screen in place was clamped where the page now ended, and it stayed there once the next page loaded. It now scrolls on to where the rows were as that page loads, unless the user scrolls first.

## 0.7.2

### Patch Changes

- 420027c: `VirtualList` and `VirtualListView` no longer go to the top when the row a jump landed on is deleted before the server has answered the queries of the rows around it. The jump draws its row from the rows the client already has; deleted then, the row was taken for one that never existed, and the list went to its top, then on to wherever the scroll offset left it among the first pages. The rows below the deleted row now move up into its place, as they do once those queries complete.

## 0.7.1

### Patch Changes

- 66da183: `VirtualList` and `VirtualListView` no longer show a gap above the first row after the scroll thumb is dragged quickly to the top, with rows shorter than `estimatedRowHeight`. The gap stood there until the list came to rest, about a sixth of a second, and then the rows jumped up into it. The rows now start at the top in the first frame drawn there.

## 0.7.0

### Minor Changes

- 7e1efb4: `VirtualList` and `VirtualListView` no longer load pages without settling at the end of a list stuck to the bottom (`stickToBottom`). When the window at the end held only a few rows, as after messages near the end were deleted and the user scrolled back down, the page loaded above them left out the last row. The space estimated in its place moved the bottom, the list followed it, and it loaded the end again, over and over.

  `VirtualAnchor.backward`'s `startRow` is now optional, which breaks code that matches it as non-optional. A nil start row is the page that ends at the list's end. Your page query gets it as a backward `VirtualPageRequest` with no `start`, as its documentation already allowed: return the list's last rows, closest to the end first, as a backward page does after its start row.

## 0.6.4

### Patch Changes

- 65ea14e: `VirtualList` and `VirtualListView` fix two cases where the list loaded pages without settling:

  - Deleting the row a short list was opened at (`permalinkID`), when all the list's rows fit in one page, no longer makes the list load pages back and forth forever, each page missing its first or last row. This came with 0.6.2.
  - Scrolled down past the loaded rows when the rows are shorter than `estimatedRowHeight`, the next page now comes in where the rows end. Before, it came in further down, so the screen could go blank and the list page back to its start.

## 0.6.3

### Patch Changes

- 74dcab8: `VirtualList` and `VirtualListView` fix three more cases where the list was left wrong until the next scroll:

  - `scrollToItem` to a loaded row near the end of the loaded rows, which the list couldn't yet scroll far enough to line up, now lines the row up once the rows below it load. Before, it stopped where it was clamped.
  - After a fast scroll up past the loaded rows, a row on screen changing height, such as an image loading, moves only the rows below it. Before, it could move the rows above it.
  - Scrolled up past the rows of a list opened at a permalink, the page above now loads on screen. Before, the screen stayed blank while the list loaded pages back to its start and forward again.

## 0.6.2

### Patch Changes

- e2fbd98: `VirtualList` and `VirtualListView` fix three cases where the list was left wrong until the next scroll:

  - Rows removed or shrinking at the bottom of a list, or the viewport getting taller, no longer leave blank space below the last row. The scroll position comes back within the content, as a browser keeps it. A list that became shorter than its viewport could show no rows at all and keep loading pages.
  - A list stuck to the bottom (`stickToBottom`) keeps the bottom in view when its viewport gets shorter, as when the keyboard comes up, and keeps following new rows. Before, it lost the bottom and stopped following.
  - Deleting the row a list was opened at (`permalinkID`) or scrolled to (`scrollToItem`) no longer sends the list to the top. The rows below the deleted row move up into its place.

## 0.6.1

### Patch Changes

- 9299ded: `VirtualList` and `VirtualListView` no longer leave a gap at the top when rows are removed from the top of a list that is scrolled all the way up. The removed rows' space stayed above the next row until the next scroll. At the top, the rows below now move up into their place, the same way rows added there are revealed. Removals above a scrolled list still keep what's on screen in place. `LoopbackServer` gains `delete(_:_:primaryKey:)`.

## 0.6.0

### Minor Changes

- 680af92: The AST has the query features of sync protocols 54–57: `aggregate` on a query and on a relationship, `groupBy`, and JSON path references (`AST.ValuePosition.json`), with JS's wire checks, normalization, hashes and name mapping. `where` can compare a value inside a `json` column: `$0.json(\.metadata, "tags", 0) == "bug"` in the typed builder, `eb.cmp(eb.json("metadata", "tags", 0), "bug")` in the untyped one. As in JS, a value of another JSON type than the one compared with never equals it.

  Breaking: `AST.ValuePosition` has a new case, `.json`; an exhaustive `switch` over it needs one too. Building a pipeline for an AST with an aggregate or a `groupBy` throws until the Swift client supports them.

- c932969: Queries can compute aggregates, `distinct()` and `groupBy()` on the client, with the IVM operators of the JS client (`Aggregate`, `GroupBy`, `GroupProjection` and `LiftField`), kept up to date as rows change. The untyped builder has `count()`, `sum`, `avg`, `min` and `max` (of a column, or of a value at a JSON path: `sum(json: "metadata", "points")`), `distinct(...)`, and `groupBy(...)` followed by an aggregate, with the JS builder's checks. On a query the view holds the value; in `related(...)` the relationship does, for a junction relationship too. `min` or `max` of values JS can't compare, such as a json column without a path, fails the query with JS's error.

  Breaking: `ViewValue` has a new case, `.value`, an aggregate's value; an exhaustive `switch` over it needs one too.

- 8ff0456: The typed query builder has aggregates, `distinct()` and `groupBy()`. `zql.issue.count()` is a `Query<Issue, Int>`; `sum` and `avg` give a `Double?`, and `min` and `max` the column's type, optional; each also takes a JSON path (`sum(json: \.metadata, "points")`). `distinct(\.ownerId)` gives `DistinctRow`s and `groupBy(\.ownerId).count()` gives `GroupRow`s, read by key path (`row[\.ownerId]`, `row.value`). `run`, `liveQuery`, `@ZeroQuery` and `defineQuery` take these queries as they take queries of rows. In `related`, an aggregate goes in the row's new generated `aggregates` property: `issue.aggregates?.count(\.comments)`.

  Breaking: `QueryResult` now refines the new `QueryValue` protocol, which `Query`, `LiveQuery`, `QueryDefinition`, `QueryRequest` and `ZeroQuery` take in its place, and `Query` no longer requires `Result.Row == Row`. `ZeroRow` refines `QueryElement` and `OptionalQueryElement`, and `ZeroValue` refines `OptionalQueryElement`. Regenerate row structs with `zero-swift codegen` to get `aggregates`; a table can no longer have a column or relationship property named `aggregates`, which codegen renames.

### Patch Changes

- 60b54c8: Aggregates, `distinct()` and `groupBy()` sync from zero-cache: a named query's view reads the rows the server computes, stored under `a/`, instead of dropping them from pokes. Until the server answers, a view computes nothing and shows the empty value (`0` for a count). A local mutation moves a synced relationship aggregate or grouped count at once, as the JS client's optimistic deltas do, and the server's rows replace them when it confirms. A mutator's `tx.run` reads the synced rows for a query the server has answered, and computes them otherwise. A pending mutation's moves of synced aggregates are stored with it, as JS persists them about a second after a change, for the aggregates registered then, so a relaunched client shows the moves the JS client would.

## 0.5.7

### Patch Changes

- f4b415b: `URLSessionTransport` no longer leaves a TCP connection open when a socket is closed during its handshake. A `URLSessionWebSocketTask` cancelled just as the server's 101 arrives completes as cancelled but keeps the upgraded connection open until the server closes it. Closing a connecting socket now lets a handshake that is still finishing open and close with a close frame, and cancels it only if it hasn't ended within a second.

## 0.5.6

### Patch Changes

- bf7a573: `VirtualList` keeps the selected row highlighted when it draws a new snapshot. It read its `selection` binding's value from when the list was made, so a snapshot after a tap could set the selection back and hide the highlight.
- 2b5775b: The client now speaks sync protocol 57, the version the JS client uses. Versions 54–57 only add query features the Swift client doesn't send yet (aggregates, `groupBy`, `distinct`, JSON path filters), so nothing else changes, and zero-cache keeps accepting older versions.

## 0.5.5

### Patch Changes

- baddfb7: When the connection drops partway through a message, the client now reconnects instead of stopping in the `error` state. URLSession hands over the partial message as if it were whole, which a browser never does; the client now waits briefly before treating an unreadable message as an error, and reconnects if the connection ends without a close frame.

## 0.5.4

### Patch Changes

- dcfc49f: `connection.connect(auth:)` no longer loses its token: a client replaced after `ClientNotFound` now connects with the last token the app gave instead of the one in `ZeroOptions.auth`, a token set right after `Zero` is created is no longer overwritten when the client starts connecting, and a logged-out client (no `userID`) ignores a token and logs an error, as the JS client rejects it, instead of sending it.
- dcfc49f: A client that stays connected no longer keeps two closures per ping (about 35,000 a day) waiting for a connection state change that never comes. The run loop's superseded waits are now dropped when it waits again. JS keeps them too, as `.then` callbacks on its long-lived state-change promise.
- dcfc49f: Dropping the last reference to a `Zero` without calling `close()` now closes it. Before, its engine was freed but its socket stayed open, a timer kept running while it was connecting, and a pending mutation's `server` outcome, a `run` waiting for the server, and a preload's `cached` and `complete` never settled.
- dcfc49f: `URLSessionTransport` no longer follows an HTTP redirect of the WebSocket upgrade, which sent the auth token in `Sec-WebSocket-Protocol` to wherever the redirect pointed. The connect fails instead and the client retries, as a browser `WebSocket` does.
- dcfc49f: A `ZeroVirtualizer` list scrolled away from its start no longer goes blank for good when every row from its loaded window's start on is deleted. The window now pages back to the rows before it; before, it stayed empty, and scrolling didn't bring the rows back. zero-virtual still stays empty there.
- dcfc49f: `@ZeroQuery` now follows a new client in the environment even when it was made after the previous one was freed, as `zero = nil` followed by a new `Zero` in one update does. The new client can get the freed one's address, and the query used to keep showing the closed client's last rows.

## 0.5.3

### Patch Changes

- 6aa4092: A `mutate` called from a view listener that fires in a microtask, such as when a view's result type changes to complete, now runs at once. Before, it could wait for the next unrelated event, such as the next ping.

## 0.5.2

### Patch Changes

- 2a066dd: Opening a database that another program holds locked, such as a SQLite browser, no longer blocks `Zero.init` on the main thread for 5 s. The open now waits at most 250 ms, then the client runs on memory and calls `onStorageFailure`, as before.

## 0.5.1

### Patch Changes

- 3615213: Documents that the main thread waits up to about 5 s when it opens a database another program holds locked, and up to about 10 s in `flush()` or `close()` on a busy one; that two generated schemas with a table of the same name need modules of their own; and that a flipped `whereExists` reaching a `limit` early, where JS asserts "Unexpected early return prevented full hydration", keeps working in Swift.

## 0.5.0

### Minor Changes

- 0dee3a3: **Breaking:** `tx.mutate.<table>` is typed, as in JS. `insert` and `upsert` take the row struct, `update` its generated `Update` (the primary key and the columns to set), and `delete` its generated `Key`. An optional column in an `Update` is a `ColumnUpdate`, so leaving it (`.unchanged`, the default) differs from setting it to null (`.null`).

  To migrate, regenerate with `npx zero-swift codegen`, then replace dictionaries passed to `tx.mutate.<table>` with the typed values:

  ```diff
  - try tx.mutate.insert(Issue(id: id, title: title, ...))
  + try tx.mutate.issue.insert(.init(id: id, title: title, ...))

  - try tx.mutate.issue.insert(["id": .string(id), "title": .string(title), ...])
  + try tx.mutate.issue.insert(.init(id: id, title: title, ...))

  - try tx.mutate.issue.update(["id": .string(id), "title": .string(title)])
  + try tx.mutate.issue.update(.init(id: id, title: title))

  - try tx.mutate.issue.update(["id": .string(id), "assigneeID": .null])
  + try tx.mutate.issue.update(.init(id: id, assigneeID: .null))

  - try tx.mutate.issue.delete(["id": .string(id)])
  + try tx.mutate.issue.delete(.init(id: id))
  ```

  `tx.mutate.insert(row)` and `tx.mutate.upsert(row)` are gone: `tx.mutate.<table>` is the one way to write. Generic code can use `TypedTableMutator<R>(tx.mutate)`, and `tx.mutate.table("issue")` takes dictionaries for untyped writes.

## 0.4.8

### Patch Changes

- f7e8a23: A mutate from inside a mutator runs after it, as in JS, instead of sharing its mutation ID and dropping it from the push. A poke's `desiredQueriesPatches` apply in the server's order. Closing a connecting socket aborts its handshake, so a timed-out attempt no longer leaks a socket. App errors drop falsy `details`, as JS does.
- f7e8a23: A flipped `whereExists` over a subquery with a `limit` no longer crashes the app: `materialize` and `liveQuery` throw "Partitioned take does not allow unpartitioned fetches", and `run` rejects with it, as in JS. View listeners are notified as in JS when one of them changes the client: a `mutate` from a listener runs once the notification is done, so listeners after it no longer end on stale rows and other views see every change; a listener removed during a notification is skipped, one added is called in the same pass, and destroying a view from its own listener still lets its other listeners hear that change. A change a view can't take where JS throws, such as two rows for a `one` relationship or a new row reaching a flipped `whereExists` over a `limit`, no longer crashes the app either: as in JS, the poke fails and the connection goes to the `error` state, or the mutation's `client` outcome is the error, and later mutations, `materialize`, and `run` fail with it.
- f7e8a23: Storage hardening. Collecting old databases no longer deletes one a client opens while the collection runs, files of a newer layout or format, or files that aren't Zero databases. A client of a group that is behind another no longer writes an older snapshot over a newer one. `zero.delete()` and `dropAllDatabases` also drop the memory databases clients fell back to after their SQLite store failed. Stored data that doesn't decode now makes the client delete the database and start a new one, calling `onClientStateNotFound` if set, instead of running on memory at every launch. Writes kept because the database was busy are written by `flush()` and `close()`, or reported as a storage failure, instead of being lost. Dropping a database twice no longer deletes the new one a replaced client started.
- f7e8a23: Bad server data, storage contents, and options no longer crash the app. A poke whose last mutation ID isn't a safe integer, or whose row update moves a row off its primary key, is rejected and the client disconnects; stored mutations with invalid IDs are dropped at launch. Huge or non-finite timer delays fire at once, as `setTimeout` does. A query built with a negative or out-of-range `limit`, a `limit` or `orderBy` in a junction relationship, or `start` or `related` inside `exists` throws JS's message when materialized, run, or preloaded. An `Int` column holding -2^63 fails to read instead of trapping, and `minPageSize` is clamped.

## 0.4.7

### Patch Changes

- d0e6132: `VirtualList` and `VirtualListView` reuse row views, as `UICollectionView` reuses cells: a row that scrolls away is hidden and its view (for `VirtualList`, its hosting controller) is given to the next row that comes, with the row's key as its identity, so state in a row's view stays with its row as in a `List`. Views beyond those kept are released a few per frame. Rows on screen are still made and measured in the pass that shows them; the rest of a newly loaded page is made in the frames after, nearest the screen first, up to a few milliseconds each frame, so loading the next page while scrolling no longer drops frames.

## 0.4.6

### Patch Changes

- 803090d: Add `Zero.mutationOutcomes` and `Zero.addMutationOutcomeListener(_:)`, which report the server's outcome of every mutation once, including mutations replayed after a disconnect, whose `server` already settled with `.zeroError`.

## 0.4.5

### Patch Changes

- 76380f0: A `VirtualList` or `VirtualListView` stuck to the bottom (`stickToBottom`) no longer pulls the user's first scroll up back to the bottom. When the list tried to scroll past where it could, its next scroll, the user's, was taken for its own, and the list stayed stuck.

## 0.4.4

### Patch Changes

- f564a2c: `VirtualList` and `VirtualListView` make and measure only the rows within a viewport's height of the viewport. Rows further away use their last measured height, or the estimated row height, until they come near. A row that turns out taller or shorter than its estimate doesn't move what's on screen, and jumps line up again once their row is measured. Before this, the first layout after a sync measured every loaded row.

## 0.4.3

### Patch Changes

- 0a7b9b5: `codegen`'s options are named `schema-file`, `schema-name`, and `type-prefix` in its JSON Schema (`--schema`), so its help prints the "Generate without a config file" example again, with `--schema-file` as it is typed. `--schemaFile`, `--schemaName`, and `--typePrefix` still work.

## 0.4.2

### Patch Changes

- e3d0da3: On a closed `Zero`, a new `run` waiting for `.complete` or `.cached`, and a new preload's `cached` and `complete`, now throw the `QueryError` saying Zero was closed right away instead of never settling. Such a preload registers nothing, and a `run` with `.unknown` still returns the local rows. JS only rejects the calls pending at close, so this differs from JS.
- 151ede9: JSON is parsed, written, compared, and hashed (also as a custom query's arguments) without recursion, so deeply nested JSON no longer overflows the stack. Before, a debug build overflowed at about 300 levels on a thread with a 512 KB stack, such as a test or cooperative thread on macOS. The parser's depth limit is lowered from 5,000 to 1,500 levels, so freeing a value, which still recurses, fits in a 512 KB stack in a release build. JS has no such limit: a row whose JSON column is nested deeper than about 1,495 levels is rejected, as described in JSParity.md. A debug build can still overflow a 512 KB stack when it frees objects nested more than about 1,000 deep.

## 0.4.1

### Patch Changes

- af85342: Calling a mutator that isn't registered with the `Zero` no longer crashes the app: `mutate` logs an error and returns a result whose `client` and `server` are both a `.zeroError` with the message JS throws, and nothing is written or sent. Waiting for `.complete` or `.cached` on an unnamed query, or preloading one, now logs a warning once per query: an unnamed query only sees local data and is never complete, so use a named query to sync it. The docs of `run` and `preload` say what happens to a pending call when the client closes or is replaced. `zero-swift codegen --help` no longer prints the examples `--schemaFile shared/schema.ts` and `--check true`; it shows `--schema-file` and a bare `--check`.

## 0.4.0

### Minor Changes

- 76312e4: `JSONValue.object` holds a `JSONObject` instead of a `[String: JSONValue]`. A `JSONObject` keeps its keys as a JS object does: integer-like keys first in ascending order, then the others in the order they were set, so JSON is written as `JSON.stringify` writes it. Keys are compared by UTF-16 code unit, so keys that differ only in Unicode normalization (`"é"` and `"e\u{301}"`) stay apart instead of one overwriting the other. Before, they merged in pokes (row values, primary keys, merges, client IDs), and custom-query and AST hashes of such keys differed from JS. Dictionary literals, `object[key]`, and `for (key, value) in object` work as before, and `Row(_:)` and `Row.object` convert both ways.

  Migration: where code builds `.object(dict)` from a `[String: JSONValue]` variable, write `.object(JSONObject(dict))` or declare the variable as `JSONObject`. `objectValue` and `ZeroObjectReader.object`/`ZeroObjectWriter.object` are now `JSONObject`; use `.dictionary` where a `[String: JSONValue]` is needed. `AST.Bound.row` is a `JSONObject`, and `AnyQuery.start`, `TableMutator`, and `LoopbackServer.put` take a `JSONObject` as well as a dictionary.

### Patch Changes

- fcc95f0: Two `Zero` instances of one client group in one process (the same user, `storageKey`, and storage, such as two windows) no longer drop each other's queries: `initConnection` lists every running client of the group in `activeClients`, as in JS, so zero-cache doesn't delete the other one. When one closes, the others send `deleteClients` for it once.
- 48ca692: A first poke that changes nothing after a reconnect or relaunch completes the got queries, as in JS. `<=` and `>=` filters no longer match `NaN`. Storage: every client sharing a database hears about a write failure, and a closed client doesn't; writes queued before a failure no longer land after it; a database busy past the busy timeout is retried by the next write instead of being a storage failure; a read failure loading the client group is reported through `onStorageFailure` and moves the client to memory; and the memory fallback after a failed open is shared by name in the process, like `.memory`.
- a548803: `codegen` generates Swift that compiles for any table, column, relationship, JSON field, or enumeration value name. A name that is taken, a keyword Swift doesn't allow even backticked, a member the generated code declares, or a type it uses is renamed by a documented rule, and keeps its original name in the schema, `CodingKeys`, and raw values. Names outside ASCII are kept where Swift allows them, enumeration values with control characters are escaped as Swift allows, and a recursive `json<T>()` type is `JSONValue` where it recurs instead of overflowing the stack.

## 0.3.0

### Minor Changes

- 8a69a95: Depending on zero-swift no longer fetches swift-nio, swift-atomics, swift-collections, and swift-system. The `ZeroNIOTesting` product is removed: its `NIOTransport`, a plain `ws://` transport for Linux, is now part of zero-swift's own end-to-end tests.
- e446e70: `zero.delete()` closes the client and deletes its local data: the replica and pending mutations of its user and `storageKey`, as in JS. `dropAllDatabases(_:)` deletes every Zero database in a storage location. Both return the `DroppedDatabases`.

### Patch Changes

- 9aec532: Cancelling the task of a `zero.run(_:type: .complete)` or `.cached` makes it throw `CancellationError` and release its query; before, it kept waiting. The `.zeroError` mutation outcome now documents that the server may still have applied the mutation, so it isn't safe to retry.

  ZeroVirtual has a DocC catalog: restoring each list's scroll position and selection, selection in a compact `NavigationSplitView`, and making `VirtualListView` rows accessibility elements. Getting Started covers leaving `queryURL` and `mutateURL` unset, and developing against a local backend: sandboxed Mac apps, the iOS simulator, physical devices, and zero-cache's port.

## 0.2.0

### Minor Changes

- c7b53df: The CLI is built on [incur](https://github.com/wevm/incur). Errors print a code and a message and exit 1. `--json` prints results as JSON, `--llms` prints a manifest for coding agents, and `skills add` installs the commands as an agent skill. `codegen` has an option for every config key, such as `--type-prefix` and `--access`. The schema module option is now `--schema-file`, because incur reserves `--schema`; `--schema <path>` still works. The config keys are now documented in the README.

## 0.1.1

### Patch Changes

- 5251897: `zero-swift --help` prints usage, and a missing schema, samples, or config file or an unknown option gets a one-line message instead of a stack trace.
