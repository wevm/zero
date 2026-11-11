---
'zero-rust': minor
---

The first release of the Rust client for Zero: the `zero-client` crate, and the `zero-rust` CLI, whose `codegen` generates an app's row structs, schema, and typed query builder from its TypeScript schema. The client syncs named queries into a local replica, keeps materialized views current, and runs mutators optimistically, as the JS client does. Every one of the 63,453 cases of the conformance vectors generated from the JS client replays to JS's result, 2,255 of them under a documented difference. Its default build has no dependencies; the `tokio-transport`, `tls`, and `sqlite` features add a WebSocket transport on tokio, `wss://`, and SQLite storage. It speaks sync protocol 57, which `zero-cache` 1.12 serves.
