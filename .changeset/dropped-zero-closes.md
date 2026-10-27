---
'zero-swift': patch
---

Dropping the last reference to a `Zero` without calling `close()` now closes it. Before, its engine was freed but its socket stayed open, a timer kept running while it was connecting, and a pending mutation's `server` outcome, a `run` waiting for the server, and a preload's `cached` and `complete` never settled.
