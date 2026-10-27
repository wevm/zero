---
'zero-swift': patch
---

Opening a database that another program holds locked, such as a SQLite browser, no longer blocks `Zero.init` on the main thread for 5 s. The open now waits at most 250 ms, then the client runs on memory and calls `onStorageFailure`, as before.
