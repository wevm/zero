# ``ZeroTesting``

Check your Swift queries and mutators against the TypeScript ones, and
test code that uses a client without a server.

## Overview

### The drift harness

You write the Swift queries and mutators by hand, so they can drift from
the TypeScript ones the server runs. `zero-swift drift record` runs the
TypeScript definitions on sample arguments and contexts and writes a
vectors file. `Drift` checks the Swift definitions against those vectors:

```swift
@Test @MainActor func matchesTheTSQueriesAndMutators() throws {
  let vectors = try String(contentsOf: vectorsURL, encoding: .utf8)
  Drift.expectNoDrift(
    vectors: vectors, schema: .app, queries: Queries.all, mutators: Mutators.all)
}
```

The test fails when:

- a name is in one registry but not the other;
- a sample's arguments don't round-trip through the Swift argument type;
- a query builds a different AST;
- a mutator, run on the sample's rows, changes different rows or ends
  differently.

### A client without a server

`ManualScheduler` is a clock that moves only when the test advances it.
`MockSocketFactory` hands the client sockets that the test drives as the
server:

```swift
let scheduler = ManualScheduler()
let sockets = MockSocketFactory()
let zero = try Zero(
  schema: .app, context: auth, options: ZeroOptions(cacheURL: "http://zero.test"),
  scheduler: scheduler, socketFactory: sockets)

scheduler.runDueTimers()                 // the client opens its socket
let socket = try #require(sockets.sockets.first)
socket.receive(text: #"["connected",{"wsid":"w1"}]"#)
#expect(zero.connection.state == .connected)
```

`RecordingLogSink` keeps the client's log messages for a test to check.

`ZeroTesting` re-exports `Zero`.

## Topics

### Drift

- ``Drift``

### Test doubles

- ``ManualScheduler``
- ``MockSocketFactory``
- ``MockSocket``
- ``RecordingLogSink``
