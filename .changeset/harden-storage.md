---
'zero-swift': patch
---

Storage hardening. Collecting old databases no longer deletes one a client opens while the collection runs, files of a newer layout or format, or files that aren't Zero databases. A client of a group that is behind another no longer writes an older snapshot over a newer one. `zero.delete()` and `dropAllDatabases` also drop the memory databases clients fell back to after their SQLite store failed. Stored data that doesn't decode now makes the client delete the database and start a new one, calling `onClientStateNotFound` if set, instead of running on memory at every launch. Writes kept because the database was busy are written by `flush()` and `close()`, or reported as a storage failure, instead of being lost. Dropping a database twice no longer deletes the new one a replaced client started.
