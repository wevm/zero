---
'zero-rust': patch
---

`Zero::new` takes less time on a replica of thousands of rows: on a Mac, 19.0 ms instead of 23.0 ms for 8,000 rows, and 7.0 ms instead of 7.6 ms for 2,000. The replica goes into the query sources without a diff against an empty one, the first report of its queries and mutation results reads only those keys, and a diff of two whole copies of the replica walks them once instead of sorting their keys.
