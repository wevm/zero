# zero-rust

## 0.5.0

### Minor Changes

- 607f99d: `Zero::prepare(schema, context, mutators, options)` opens the client's SQLite store and reads its replica on a thread of its own, and `Zero::from_prepared(prepared, scheduler, socket_factory)` makes the client from it on the app's thread. Call `prepare` as early as the app knows its options, and `from_prepared` where the first frame needs the client: it waits for whatever is left, and the client and its views have their rows at once, as with `Zero::new`. At launch, Zero then takes 5.6 ms of the app's thread on a Mac for a replica of 8,000 rows instead of 17.8 ms, and 2.8 ms instead of 7.1 ms for 2,000. Loading the replica into the query sources and hydrating the first views stay on the app's thread. `Zero::new` and `materialize` don't change.

### Patch Changes

- edec2e7: `Zero::new` takes less time on a replica of thousands of rows: on a Mac, 19.0 ms instead of 23.0 ms for 8,000 rows, and 7.0 ms instead of 7.6 ms for 2,000. The replica goes into the query sources without a diff against an empty one, the first report of its queries and mutation results reads only those keys, and a diff of two whole copies of the replica walks them once instead of sorting their keys.

## 0.4.0

### Minor Changes

- 65128ca: `ZeroStorage::Sqlite { directory: None }` now keeps each app's databases in a folder of its own. On macOS and Mac Catalyst it is `~/Library/Application Support/<bundle ID>/Zero`, or `~/Library/Application Support/<executable>/Zero` for an app outside an app bundle; a sandboxed app keeps `~/Library/Application Support/Zero`, which is already in its own container. On Windows it is `%LOCALAPPDATA%\<executable>\Zero`, without `.exe`. On Linux and the other platforms it is `$XDG_DATA_HOME/<executable>/zero`, or `~/.local/share/<executable>/zero`. On iOS, tvOS, watchOS, and visionOS it stays `~/.local/share/zero`, since the app's home is its own container there. A `directory` the app passes is used as is.

  Every app using zero-rust's default storage shared one folder before, so one app's cleanup of old databases, or its `drop_all_databases`, could delete another app's data, pending mutations included, and all of them shared one profile ID.

  On macOS and Mac Catalyst, each launch excludes the default folder from backups.

  The first time an updated app opens its default storage, it moves that database from the old folder, pending mutations included, and copies the profile ID. A database another process still has open, or one on another disk than the new folder, stays where it is. The app's other databases, such as those of other users, stay in the old folder too, and sync again from scratch.

- 6e8c363: Add `Zero::pending_settled(timeout_ms)`, the Swift client's `pendingSettled(timeout:)`, which JS has no counterpart for. Await it before hiding the client with `set_visible(false)` to let the server confirm the pending mutations first: it resolves with whether none are left once the server has confirmed them, at once when it can't (the client isn't connected, it's closed, or its client group is disabled), or after `timeout_ms` on the client's scheduler. Mutations still pending then stay in the local store, and are sent again when the client next connects.

### Patch Changes

- 21e2ce1: `Ast::to_json` writes an AST's fields in the order JS's `normalizeAST` writes them, so an app can compare its queries' ASTs with a server's AST JSON as text: `json::stringify` of a normalized AST (`Ast::normalized`) is the text JS writes, byte for byte. The JSON is unchanged except for the key order of a relationship's fields: `hidden` now comes before `subquery`, as in JS. Query hashes are unchanged.
- 343582f: A client that replaces itself after the server lost its state (`ClientNotFound`) now keeps a `set_visible(false)` the app made less than `hidden_disconnect_delay_ms` before. It disconnects when the hidden client would have; before, it stayed connected until the app called `set_visible` again.

## 0.3.1

### Patch Changes

- 6af328f: Publish the aligned README (#220) to crates.io, npm, and the wevm/zero-rust mirror. 0.3.0 shipped with the previous README. No code changes.

## 0.3.0

### Minor Changes

- 23f9185: `Connection::connect` now matches JS's `connection.connect({auth})`. It takes a `ConnectAuth`: `Keep` keeps the token, `Set(token)` replaces it, and `Clear` removes it, which JS's `auth: null` does and Rust couldn't. It returns a `Result`, `Err` where JS's promise rejects: a token for a client without a `user_id` is the `Internal` connection error "ZeroOptions.userID is required when auth is set." and changes nothing, where it was logged and ignored. Write `connect(Some(token))` as `connect(ConnectAuth::Set(token))?` and `connect(None)` as `connect(ConnectAuth::Keep)?`.
- d1b27f8: `ZeroOptions::hidden_disconnect_delay_ms` now defaults to 5 minutes, as JS's `hiddenTabDisconnectDelay` does, instead of 0: after `Zero::set_visible(false)`, the client stays connected for 5 minutes before it disconnects. An app that wants the old behavior, such as a mobile app the system suspends in the background, sets `hidden_disconnect_delay_ms: 0.0`.
- abfc5d8: Add `LiveView`, a typed live query after the Swift client's `LiveQuery` (zero-react's `useQuery`). `Zero::live(&query, &ttl)` takes a typed `Query` or a named query's `QueryRequest` and returns a `LiveView<T>` whose `value()` is the query's rows, `one()` row, or aggregate read as its Rust type and kept current, with `result_type()`, `error()` (a `LiveError`: the server's query error, or rows that didn't read, which keep the last value), `add_listener`, `update_ttl`, and `view()` for the `MaterializedView`. An update reads only the rows whose entries changed, so an optimistic update with a live view of 100,000 rows read as structs no longer reads every row again. A live view follows the client when it replaces itself, and dropping it removes its listeners and destroys its view. `QueryValue` gains `update_from_view`, which has a default, and the new `typed::TypedQuery` trait names what a query reads as.
- 8a70783: Add `Zero::add_mutation_outcome_listener`, the Swift client's `addMutationOutcomeListener`, which JS has no counterpart for. The listener hears each mutation's server outcome once, as a `client::MutationServerOutcome` with the mutation's ID, name, arguments, and `MutationOutcome`, and the call returns the function that removes it. Unlike a mutation's `server` result, which settles with a `ZeroError` when the client gives up connecting (`disconnect_timeout_ms`), it hears the real outcome of a mutation replayed after the client reconnects, so an app can show each rejection in one place. Listeners stay with the `Zero` when it replaces its client; a mutation still pending when the app quits is replayed on the next launch, but its outcome isn't reported.

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
