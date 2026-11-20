---
'zero-rust': patch
---

`Zero::from_prepared` takes less of the app's thread. On a Mac, it takes 2.1 ms instead of 3.8 ms for a replica of 8,000 rows, and 1.6 ms instead of 2.2 ms for 2,000; on Linux, 6.2 ms instead of 9.6 ms for 8,000. `Zero::prepare` now also sorts each table's rows for the query sources on its own thread, so the app's thread only puts them into the sources and hydrates the first views.
