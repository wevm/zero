---
'zero-rust': patch
---

Opening a query with joins takes less time. zbugs' issue list at a limit of 1,000, on a replica of 8,000 rows, opens and closes in 6.3 ms instead of 6.6 ms on a Mac, and 21.3 ms instead of 24.2 ms on Linux. A join's index of its parent rows keeps no set for a join key with one parent, and none for a join on the parent's primary key.
