---
'zero-swift': patch
---

Queries fetch and push their rows faster: a query source finds the index a fetch reads, and writes each index on a change, without writing the index's sort out as JSON to look it up. On a Mac, an optimistic update of an issue in zbugs' issue list takes 12% less time, and its first sync 6% less.
