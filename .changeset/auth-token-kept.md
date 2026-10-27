---
'zero-swift': patch
---

`connection.connect(auth:)` no longer loses its token: a client replaced after `ClientNotFound` now connects with the last token the app gave instead of the one in `ZeroOptions.auth`, a token set right after `Zero` is created is no longer overwritten when the client starts connecting, and a logged-out client (no `userID`) ignores a token and logs an error, as the JS client rejects it, instead of sending it.
