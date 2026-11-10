---
'zero-swift': patch
---

The client reconnects sooner when its connection drops or the server stops answering, as JS's client now does: the first retry comes within half a second, and each further failure in a row doubles the wait, up to the usual 5 seconds. A connection that stays up for 5 seconds starts the schedule over. Errors the server sends keep the full interval and any backoff they ask for.
