---
'zero-rust': minor
---

`ZeroStorage::Sqlite { directory: None }` now keeps each app's databases in a folder of its own. On macOS and Mac Catalyst it is `~/Library/Application Support/<bundle ID>/Zero`, or `~/Library/Application Support/<executable>/Zero` for an app outside an app bundle; a sandboxed app keeps `~/Library/Application Support/Zero`, which is already in its own container. On Windows it is `%LOCALAPPDATA%\<executable>\Zero`, without `.exe`. On Linux and the other platforms it is `$XDG_DATA_HOME/<executable>/zero`, or `~/.local/share/<executable>/zero`. On iOS, tvOS, watchOS, and visionOS it stays `~/.local/share/zero`, since the app's home is its own container there. A `directory` the app passes is used as is.

Every app using zero-rust's default storage shared one folder before, so one app's cleanup of old databases, or its `drop_all_databases`, could delete another app's data, pending mutations included, and all of them shared one profile ID.

On macOS and Mac Catalyst, each launch excludes the default folder from backups.

The first time an updated app opens its default storage, it moves that database from the old folder, pending mutations included, and copies the profile ID. A database another process still has open, or one on another disk than the new folder, stays where it is. The app's other databases, such as those of other users, stay in the old folder too, and sync again from scratch.
