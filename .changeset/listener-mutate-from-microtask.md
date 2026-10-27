---
'zero-swift': patch
---

A `mutate` called from a view listener that fires in a microtask, such as when a view's result type changes to complete, now runs at once. Before, it could wait for the next unrelated event, such as the next ping.
