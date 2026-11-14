---
'zero-rust': patch
---

Add `ZeroOptions::get_traceparent`, JS's `getTraceparent`: its W3C `traceparent` goes with each query change, `initConnection` message, and push the client sends, so zero-cache and the API server can join the app's trace. Add `Ttl::DEFAULT_PRELOAD`, the TTL JS's `preload` uses when it isn't given one.
