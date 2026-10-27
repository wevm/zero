---
'zero-swift': patch
---

Documents that the main thread waits up to about 5 s when it opens a database another program holds locked, and up to about 10 s in `flush()` or `close()` on a busy one, and that two generated schemas with a table of the same name need modules of their own.
