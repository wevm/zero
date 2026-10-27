---
'zero-swift': patch
---

`URLSessionTransport` no longer follows an HTTP redirect of the WebSocket upgrade, which sent the auth token in `Sec-WebSocket-Protocol` to wherever the redirect pointed. The connect fails instead and the client retries, as a browser `WebSocket` does.
