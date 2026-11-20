---
'zero-swift': patch
---

A `LiveQuery` of many rows (and `@ZeroQuery`) updates faster: the rows it reads again copy the unchanged rows around them in runs instead of one at a time. On a Mac, an optimistic update with a live query of 1,000 rows takes 31 µs instead of 51 µs.
