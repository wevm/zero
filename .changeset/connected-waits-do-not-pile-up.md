---
'zero-swift': patch
---

A client that stays connected no longer keeps two closures per ping (about 35,000 a day) waiting for a connection state change that never comes. The run loop's superseded waits are now dropped when it waits again. JS keeps them too, as `.then` callbacks on its long-lived state-change promise.
