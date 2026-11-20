---
'zero-rust': patch
---

`Zero::from_prepared` and `Zero::new` take less of the app's thread to load a stored replica. On a Mac, `from_prepared` takes 3.5 ms instead of 5.4 ms for a replica of 8,000 rows, and 2.0 ms instead of 2.6 ms for 2,000; `Zero::new` takes 22.8 ms instead of 23.7 ms for 8,000. On Linux, `from_prepared` takes 9.9 ms instead of 13.1 ms for 8,000 rows. The replica's map is built a level at a time from the stored rows, which come in key order, and the query sources load straight from it instead of from an add made for every row.
