# zero-rust

## 0.2.0

### Minor Changes

- 5f6e6d7: Add typed named queries and mutators behind a new `serde` feature, shaped like the Swift client's `defineQuery` and `defineMutator`. `define::define_query(name, |args, ctx| query)` and `define::define_mutator(name, |tx, args, ctx| ...)` take serde argument types, and `.with(args)` makes a `QueryRequest` for `materialize`, `run`, and `preload` (built with the `Zero`'s context) or a `client::MutateRequest` for the new `Zero::mutate_request`. `json::Value` implements serde, with `json::to_value` and `json::from_value`. `materialize`, `run`, and `preload` now take any `query::ZeroQuery` (an `AnyQuery`, a typed `Query`, or a `QueryRequest`), so existing calls keep working; the default build still has no third-party dependencies.
- 16ee0b8: Add JS's inspector and query metrics. `Zero::inspector()` returns an `Inspector` that lists the clients in the local store and their queries (with each query's ZQL, row count, and timings), reads a client's stored data and rows, fetches zero-cache's version and the client's metrics merged with the server's, and asks zero-cache to analyze a query (`analyze_query`, `analyze_server_ast`, `analyze_named_query`). When zero-cache requires its admin password, an inspector call fails until `authenticate(password)` succeeds, and the accepted password is presented again after a reconnect. The client now records JS's `query-materialization-client`, `query-materialization-end-to-end`, and `query-update-client` metrics as `tdigest::TDigest`s, and logs each query's end-to-end materialization, as a warning when it takes longer than the new `ZeroOptions::slow_materialize_threshold_ms` (5,000 by default).
- 8f4916a: Add two of JS's options to `ZeroOptions`. `get_traceparent` (`getTraceparent`) puts a W3C `traceparent` on each query change, `initConnection` message, and push the client sends, so zero-cache and the API server can join the app's trace. `batch_view_updates` (`batchViewUpdates`) wraps each new view's creation and each change that updates the views, with their listener calls, so a UI can render them at once. Add `Ttl::DEFAULT_PRELOAD`, the TTL JS's `preload` uses when it isn't given one.

### Patch Changes

- c3cb36c: Take JSON nested up to 5,000 levels deep, more than zero-cache can write, instead of rejecting it past 1,500: a row with a JSON column nested that deep no longer makes the client disconnect after every reconnect. Cloning, comparing, and writing such a value no longer recurse past a hundred or so levels, so it fits in a 2 MB thread stack even in a debug build.

## 0.1.3

### Patch Changes

- aa07557: Fix a panic after a poke or mutation failed the views (for example, a `one` relationship that got a second row): the next poke with pending mutations, or the next persist, ran over the failed branch and panicked in `MemorySource` ("Row not found"). As in JS, the poke now fails with the views' error and the client disconnects, and the persist is skipped.
- cab85e8: Make optimistic updates and round trips cost the same with any view size: a memory source's indexes are now persistent B+ trees, as in Swift, so the fork each mutation takes shares the rows and a write copies only the nodes on its path. Before, the first write to a fork copied the whole index: with a 100,000-row view, an optimistic update took 852 µs and is now 19 µs.
- fb63ae5: Fix the replica losing rows: after a delete emptied the first child of a node of its B+tree and the node later merged into its neighbor, the client couldn't find a row it still held. A mutator or a poke then couldn't delete the row, and putting it again panicked ("Row already exists").

## 0.1.2

### Patch Changes

- c5317d6: The rows of a table share one list of column names, their layout, instead of each keeping its own copy, and a synced row no longer allocates its column names when it is parsed. 100,000 rows and a view of them take 83 MB instead of 181 MB, materializing them takes 10.5 ms instead of 20.9 ms, and reading them as structs 23.8 ms instead of 48.3 ms.

## 0.1.1

### Patch Changes

- 99299d6: The Rust client shares a row between its replica, its diffs, and its IVM sources instead of copying it: a `json::Object`'s copies now share its entries until one of them is changed. 100,000 rows and a view of them take 181 MB instead of 467 MB (JS takes 245 MB), a first sync of zbugs' 2,058 rows takes 19 ms instead of 26 ms, and a poke of 1,000 rows takes 8.8 ms instead of 15 ms.

## 0.1.0

### Minor Changes

- cbacdec: The first release of the Rust client for Zero: the `zero-client` crate, and the `zero-rust` CLI, whose `codegen` generates an app's row structs, schema, and typed query builder from its TypeScript schema. The client syncs named queries into a local replica, keeps materialized views current, and runs mutators optimistically, as the JS client does. Every one of the 63,453 cases of the conformance vectors generated from the JS client replays to JS's result, 2,255 of them under a documented difference. Its default build has no dependencies; the `tokio-transport`, `tls`, and `sqlite` features add a WebSocket transport on tokio, `wss://`, and SQLite storage. It speaks sync protocol 57, which `zero-cache` 1.12 serves.
