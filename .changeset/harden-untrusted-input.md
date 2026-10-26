---
'zero-swift': patch
---

Bad server data, storage contents, and options no longer crash the app. A poke whose last mutation ID isn't a safe integer, or whose row update moves a row off its primary key, is rejected and the client disconnects; stored mutations with invalid IDs are dropped at launch. Huge or non-finite timer delays fire at once, as `setTimeout` does. A query built with a negative or out-of-range `limit`, a `limit` or `orderBy` in a junction relationship, or `start` or `related` inside `exists` throws JS's message when materialized, run, or preloaded. An `Int` column holding -2^63 fails to read instead of trapping, and `minPageSize` is clamped.
