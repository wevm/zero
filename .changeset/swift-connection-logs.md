---
'zero-swift': patch
---

The client now logs a connection's life with zero-client's messages and levels: "Connecting...", "Connecting to", "Got socket open event", "Waiting for connection to be acknowledged", "Connected", "Connection attempt timed out", the ping's outcome, "disconnecting", and the close. At `.debug`, it also logs a WebSocket send that hasn't finished after a second, which holds up the frames and the close after it, and when it sends the close.
