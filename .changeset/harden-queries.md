---
'zero-swift': patch
---

A flipped `whereExists` over a subquery with a `limit` no longer crashes the app: `materialize` and `liveQuery` throw "Partitioned take does not allow unpartitioned fetches", and `run` rejects with it, as in JS. View listeners are notified as in JS when one of them changes the client: a `mutate` from a listener runs once the notification is done, so listeners after it no longer end on stale rows and other views see every change; a listener removed during a notification is skipped, one added is called in the same pass, and destroying a view from its own listener still lets its other listeners hear that change.
