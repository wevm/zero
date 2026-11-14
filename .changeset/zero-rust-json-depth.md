---
'zero-rust': patch
---

Take JSON nested up to 5,000 levels deep, more than zero-cache can write, instead of rejecting it past 1,500: a row with a JSON column nested that deep no longer makes the client disconnect after every reconnect. Cloning, comparing, and writing such a value no longer recurse past a hundred or so levels, so it fits in a 2 MB thread stack even in a debug build.
