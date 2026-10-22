---
'zero-swift': patch
---

Parse incoming server messages, including binary poke chunks, off the main actor, so a large initial sync no longer holds the main thread while it parses. Messages still apply on the main actor in the order they arrive.
