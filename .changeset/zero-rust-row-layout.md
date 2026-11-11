---
'zero-rust': patch
---

The rows of a table share one list of column names, their layout, instead of each keeping its own copy, and a synced row no longer allocates its column names when it is parsed. 100,000 rows and a view of them take 83 MB instead of 181 MB, materializing them takes 10.5 ms instead of 20.9 ms, and reading them as structs 23.8 ms instead of 48.3 ms.
