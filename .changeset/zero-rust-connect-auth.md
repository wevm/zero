---
'zero-rust': minor
---

`Connection::connect` now matches JS's `connection.connect({auth})`. It takes a `ConnectAuth`: `Keep` keeps the token, `Set(token)` replaces it, and `Clear` removes it, which JS's `auth: null` does and Rust couldn't. It returns a `Result`, `Err` where JS's promise rejects: a token for a client without a `user_id` is the `Internal` connection error "ZeroOptions.userID is required when auth is set." and changes nothing, where it was logged and ignored. Write `connect(Some(token))` as `connect(ConnectAuth::Set(token))?` and `connect(None)` as `connect(ConnectAuth::Keep)?`.
