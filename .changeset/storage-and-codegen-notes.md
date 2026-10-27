---
'zero-swift': patch
---

Documents that the main thread waits up to about 5 s when it opens a database another program holds locked, and up to about 10 s in `flush()` or `close()` on a busy one; that two generated schemas with a table of the same name need modules of their own; and that a flipped `whereExists` reaching a `limit` early, where JS asserts "Unexpected early return prevented full hydration", keeps working in Swift.
