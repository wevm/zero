---
'zero-rust': patch
---

On Linux, a client on SQLite storage applies pokes and mutations faster. A first sync of 2,000 rows with zbugs' issue list takes 17.5 ms of the app's thread instead of 26.4 ms, a poke of 1,000 changed rows 6.1 ms instead of 8.2 ms, and a mutation's round trip 51 µs instead of 107 µs. The store's thread freed the writes it was handed, which glibc does under the lock of the thread that allocated them, so the client's thread stalled on its own allocations; the writes now go back to be dropped on the client's thread. macOS didn't stall this way, and doesn't change.
