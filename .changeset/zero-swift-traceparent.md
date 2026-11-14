---
'zero-swift': patch
---

Add `ZeroOptions.getTraceparent`, JS's `getTraceparent`: its W3C `traceparent` goes with each query change, `initConnection` message, and push the client sends, so zero-cache and the API server can join the app's trace.
