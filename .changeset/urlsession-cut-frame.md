---
'zero-swift': patch
---

A server that goes away partway through a WebSocket frame no longer stops the client in the `error` state until the app calls `connection.connect()`. `URLSessionTransport` delivered the part of the frame it had as a whole message, which the client rejected as invalid; it now drops it and reports an abrupt close, as a browser does, and the client reconnects. A message with no other right behind it can arrive up to 5 ms later than before.
