---
'zero-swift': patch
---

A mutate from inside a mutator runs after it, as in JS, instead of sharing its mutation ID and dropping it from the push. A poke's `desiredQueriesPatches` apply in the server's order. Closing a connecting socket aborts its handshake, so a timed-out attempt no longer leaks a socket. App errors drop falsy `details`, as JS does.
