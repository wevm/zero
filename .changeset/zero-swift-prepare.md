---
'zero-swift': minor
---

`Zero.prepare(schema:context:mutators:options:queries:)` opens the client's store, reads its replica, and hydrates the queries the app's first frame shows, off the main actor, and `Zero(prepared)` takes the result over on the main actor. Call `prepare` as early as the app knows its options, such as in `App.init`, and make the client where the first frame needs it: it waits for whatever is left, and the views of the prepared queries have their rows at once, as with `Zero.init`. At launch, Zero then takes about 2 ms of the main thread on a Mac however large the replica is, instead of 10 ms for a replica of 2,000 rows and 25 ms for 8,000. `Zero.init` and `materialize` don't change.
