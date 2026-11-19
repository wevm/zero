---
'zero-rust': minor
---

`Zero::prepare(schema, context, mutators, options)` opens the client's SQLite store and reads its replica on a thread of its own, and `Zero::from_prepared(prepared, scheduler, socket_factory)` makes the client from it on the app's thread. Call `prepare` as early as the app knows its options, and `from_prepared` where the first frame needs the client: it waits for whatever is left, and the client and its views have their rows at once, as with `Zero::new`. At launch, Zero then takes 5.6 ms of the app's thread on a Mac for a replica of 8,000 rows instead of 17.8 ms, and 2.8 ms instead of 7.1 ms for 2,000. Loading the replica into the query sources and hydrating the first views stay on the app's thread. `Zero::new` and `materialize` don't change.
