---
'zero-swift': patch
---

The replica and the queries' sources now share each synced row instead of each keeping its own copy, so 100,000 synced rows of 10 columns take 75 MB instead of 127 MB. Pokes and optimistic mutations also apply 4–15% faster.
