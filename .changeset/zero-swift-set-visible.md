---
'zero-swift': patch
---

Add `Zero.setVisible(_:)` and `Zero.pendingSettled(timeout:)`, for a part of the app that can be out of sight while the app is in the foreground, such as a Mac window, a menu bar app's popover, or a screen that only sometimes needs to sync. `setVisible(false)` disconnects the client until `setVisible(true)`, while its views and local queries and mutations keep working; the client is visible only when both the app and its lifecycle say so, so one the app hid stays hidden after the Mac wakes or the app comes back to the foreground. `await pendingSettled(timeout:)` waits for the server to confirm the pending mutations, as before hiding the client, and returns whether none are left; it returns at once when the client isn't connected. Until now an app could only `close()` the client and make a new one, which queried everything again.
