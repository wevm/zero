---
'zero-swift': patch
---

Aggregates, `distinct()` and `groupBy()` sync from zero-cache: a named query's view reads the rows the server computes, stored under `a/`, instead of dropping them from pokes. Until the server answers, a view computes nothing and shows the empty value (`0` for a count). A local mutation moves a synced relationship aggregate or grouped count at once, as the JS client's optimistic deltas do, and the server's rows replace them when it confirms. A mutator's `tx.run` reads the synced rows for a query the server has answered, and computes them otherwise. While mutations made before a relaunch are pending, they are rebased when a synced aggregate's query first materializes, so the relaunched client keeps their moves.
