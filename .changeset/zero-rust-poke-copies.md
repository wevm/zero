---
'zero-rust': patch
---

Applying pokes takes less time. A first sync of 2,000 rows takes 3.2 ms instead of 3.6 ms on a Mac, and 6.6 ms instead of 7.0 ms with zbugs' issue list; on Linux, 7.8 ms instead of 8.8 ms, and 14.9 ms instead of 16.0 ms. A poke of 1,000 changed rows takes 2.33 ms instead of 2.45 ms on a Mac. A patch's operations are no longer copied to keep the last of each key, a one-column primary key is written straight into its row's key, and a source's index shares its sort order instead of copying it on every lookup.
