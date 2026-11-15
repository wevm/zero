---
'zero-rust': minor
---

Add `LiveView`, a typed live query after the Swift client's `LiveQuery` (zero-react's `useQuery`). `Zero::live(&query, &ttl)` takes a typed `Query` or a named query's `QueryRequest` and returns a `LiveView<T>` whose `value()` is the query's rows, `one()` row, or aggregate read as its Rust type and kept current, with `result_type()`, `error()` (a `LiveError`: the server's query error, or rows that didn't read, which keep the last value), `add_listener`, `update_ttl`, and `view()` for the `MaterializedView`. An update reads only the rows whose entries changed, so an optimistic update with a live view of 100,000 rows read as structs no longer reads every row again. A live view follows the client when it replaces itself, and dropping it removes its listeners and destroys its view. `QueryValue` gains `update_from_view`, which has a default, and the new `typed::TypedQuery` trait names what a query reads as.
