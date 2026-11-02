---
'zero-swift': patch
---

`URLSessionTransport` no longer leaves a TCP connection open when a socket is closed during its handshake. A `URLSessionWebSocketTask` cancelled just as the server's 101 arrives completes as cancelled but keeps the upgraded connection open until the server closes it. Closing a connecting socket now lets a handshake that is still finishing open and close with a close frame, and cancels it only if it hasn't ended within a second.
