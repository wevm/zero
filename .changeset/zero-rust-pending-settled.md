---
'zero-rust': minor
---

Add `Zero::pending_settled(timeout_ms)`, the Swift client's `pendingSettled(timeout:)`, which JS has no counterpart for. Await it before hiding the client with `set_visible(false)` to let the server confirm the pending mutations first: it resolves with whether none are left once the server has confirmed them, at once when it can't (the client isn't connected, it's closed, or its client group is disabled), or after `timeout_ms` on the client's scheduler. Mutations still pending then stay in the local store, and are sent again when the client next connects.
