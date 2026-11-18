---
'zero-swift': minor
---

`ZeroStorage.sqlite()` now keeps each app's databases in a folder of its own, as IndexedDB keeps each origin's in JS. Unsandboxed Mac apps, and apps on Linux, shared one folder, `~/Library/Application Support/Zero` or `$XDG_DATA_HOME/zero`, so one app's cleanup of databases unused for two days, or its `dropAllDatabases()`, deleted other apps' databases, pending mutations included, and every app reported the same profile ID. The default folder is now `~/Library/Application Support/<bundle ID>/Zero` for an unsandboxed Mac app (Mac Catalyst too), named for the executable when there's no bundle ID, and `$XDG_DATA_HOME/<executable>/zero` (or `~/.local/share/<executable>/zero`) on Linux. iOS apps and sandboxed Mac apps keep `Library/Application Support/Zero` in their container, which is theirs already. On Apple platforms the folder is excluded from backups (Time Machine, iCloud and device backups) each time it's opened, since the server can sync it again.

The first time an updated app opens its default storage, it moves that database from the old folder into its own, with its pending mutations, so its user doesn't sync again. The app's other databases there, such as another user's, stay behind and sync again from scratch when they're next used. A `directory` the app passes to `.sqlite(directory:)` is used as before.
