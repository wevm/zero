---
'zero-rust': minor
---

Add typed named queries and mutators behind a new `serde` feature, shaped like the Swift client's `defineQuery` and `defineMutator`. `define::define_query(name, |args, ctx| query)` and `define::define_mutator(name, |tx, args, ctx| ...)` take serde argument types, and `.with(args)` makes a `QueryRequest` for `materialize`, `run`, and `preload` (built with the `Zero`'s context) or a `client::MutateRequest` for the new `Zero::mutate_request`. `json::Value` implements serde, with `json::to_value` and `json::from_value`. `materialize`, `run`, and `preload` now take any `query::ZeroQuery` (an `AnyQuery`, a typed `Query`, or a `QueryRequest`), so existing calls keep working; the default build still has no third-party dependencies.
