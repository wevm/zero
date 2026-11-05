---
'zero-swift': patch
---

On Apple platforms, the client now reconnects when the server's connection ends partway through a message, even if URLSession is slow to report the end. URLSession hands over the part of the message it has, and the client used to wait 1 second for the connection to end before deciding the message was really bad. On a busy device URLSession can report the end several seconds later, so the client stopped in `error` instead of reconnecting. The client now sends a WebSocket ping instead. If the ping fails, the message was cut off and the client reconnects. If the pong comes back, the message was really bad and the error stands, as before. If neither comes within 10 seconds, the error also stands.
