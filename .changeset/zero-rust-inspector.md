---
'zero-rust': minor
---

Add JS's inspector and query metrics. `Zero::inspector()` returns an `Inspector` that lists the clients in the local store and their queries (with each query's ZQL, row count, and timings), reads a client's stored data and rows, fetches zero-cache's version and the client's metrics merged with the server's, and asks zero-cache to analyze a query (`analyze_query`, `analyze_server_ast`, `analyze_named_query`). When zero-cache requires its admin password, an inspector call fails until `authenticate(password)` succeeds, and the accepted password is presented again after a reconnect. The client now records JS's `query-materialization-client`, `query-materialization-end-to-end`, and `query-update-client` metrics as `tdigest::TDigest`s, and logs each query's end-to-end materialization, as a warning when it takes longer than the new `ZeroOptions::slow_materialize_threshold_ms` (5,000 by default).
