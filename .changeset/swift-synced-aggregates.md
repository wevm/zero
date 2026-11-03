---
'zero-swift': patch
---

Aggregates, `distinct()` and `groupBy()` sync from zero-cache: a named query's view reads the rows the server computes, stored under `a/`, instead of dropping them from pokes. Until the server answers, a view computes nothing and shows the empty value (`0` for a count). A local mutation moves a synced relationship aggregate or grouped count at once, as the JS client's optimistic deltas do, and the server's rows replace them when it confirms. A mutator's `tx.run` reads the synced rows for a query the server has answered, and computes them otherwise. A pending mutation's moves of synced aggregates are stored with it, as JS persists them about a second after a change, for the aggregates registered then, so a relaunched client shows the moves the JS client would.
