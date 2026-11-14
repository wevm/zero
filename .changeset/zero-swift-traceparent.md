---
'zero-swift': patch
---

Add two of JS's options to `ZeroOptions`. `getTraceparent` puts a W3C `traceparent` on each query change, `initConnection` message, and push the client sends, so zero-cache and the API server can join the app's trace. `batchViewUpdates` wraps each new view's creation and each change that updates the views, with their listener calls, so a UI can render them at once.
