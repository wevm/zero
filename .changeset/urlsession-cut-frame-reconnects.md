---
'zero-swift': patch
---

When the connection drops partway through a message, the client now reconnects instead of stopping in the `error` state. URLSession hands over the partial message as if it were whole, which a browser never does; the client now waits briefly before treating an unreadable message as an error, and reconnects if the connection ends without a close frame.
