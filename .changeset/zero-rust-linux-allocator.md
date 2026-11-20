---
'zero-rust': patch
---

The README suggests jemalloc as a Linux app's global allocator. glibc's `malloc` is slow at the client's many small allocations: with jemalloc, `Zero::new` on 8,097 stored rows takes 25 ms instead of 34 ms, and a poke of 1,000 rows on SQLite 4.9 ms instead of 6.1 ms, with the same memory. mimalloc is about as fast but holds a quarter more memory for 100,000 rows.
