---
'zero-swift': minor
---

`Connection.connect(_:)` now matches JS's `connection.connect({auth})`, and replaces `connect(auth:)`. It takes a `ConnectAuth`: `.keep` (the default) keeps the token, `.set(token)` replaces it, and `.clear` removes it, which JS's `auth: null` does and Swift couldn't. It throws where JS's promise rejects: a token for a client without a `userID` throws the `Internal` error "ZeroOptions.userID is required when auth is set." and changes nothing, where it was logged and ignored. Write `connect(auth: token)` as `try connect(.set(token))` and `connect()` as `try connect()`.
