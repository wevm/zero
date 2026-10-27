---
'zero-swift': patch
---

`@ZeroQuery` now follows a new client in the environment even when it was made after the previous one was freed, as `zero = nil` followed by a new `Zero` in one update does. The new client can get the freed one's address, and the query used to keep showing the closed client's last rows.
