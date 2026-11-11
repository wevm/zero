# zero-rust

## 0.1.1

### Patch Changes

- 99299d6: The Rust client shares a row between its replica, its diffs, and its IVM sources instead of copying it: a `json::Object`'s copies now share its entries until one of them is changed. 100,000 rows and a view of them take 181 MB instead of 467 MB (JS takes 245 MB), a first sync of zbugs' 2,058 rows takes 19 ms instead of 26 ms, and a poke of 1,000 rows takes 8.8 ms instead of 15 ms.

## 0.1.0

### Minor Changes

- cbacdec: The first release of the Rust client for Zero: the `zero-client` crate, and the `zero-rust` CLI, whose `codegen` generates an app's row structs, schema, and typed query builder from its TypeScript schema. The client syncs named queries into a local replica, keeps materialized views current, and runs mutators optimistically, as the JS client does. Every one of the 63,453 cases of the conformance vectors generated from the JS client replays to JS's result, 2,255 of them under a documented difference. Its default build has no dependencies; the `tokio-transport`, `tls`, and `sqlite` features add a WebSocket transport on tokio, `wss://`, and SQLite storage. It speaks sync protocol 57, which `zero-cache` 1.12 serves.
