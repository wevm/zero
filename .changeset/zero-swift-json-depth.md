---
'zero-swift': patch
---

Take rows whose JSON is nested up to 5,000 levels deep, as the Rust client does, instead of 1,500. zero-cache writes up to about 4,100 levels, which JS accepts; a row nested deeper than about 1,495 levels made the client reject the poke, disconnect, and fail the same way after every reconnect. Values that deep are now also freed without recursion, so they don't overflow a 512 KB thread stack.
