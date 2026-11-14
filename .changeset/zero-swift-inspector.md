---
'zero-swift': patch
---

Add JS's inspector and query metrics. `zero.inspector` lists the client's and its group's queries on the server, with their client and server timings and ZQL, reads the clients the local store knows and their local data, reports the client's query metrics merged with the server's, shows zero-cache's version, and analyzes a query, a server AST, or a named query on the server. Where JS asks for zero-cache's admin password in a dialog, a request throws `InspectorError.authenticationRequired`: call `zero.inspector.authenticate(password:)` first, and the password it accepts is presented again after a reconnect. The client records JS's query metrics (`query-materialization-client`, `query-materialization-end-to-end`, `query-update-client`) in a `TDigest` that gives the same quantiles as JS's, and `ZeroOptions.slowMaterializeThreshold` (5 seconds by default) logs a warning for a query that takes longer to materialize, server and network included.
