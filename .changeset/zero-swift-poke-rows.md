---
'zero-swift': patch
---

Pokes now read each synced row straight into the row the replica and the queries' sources keep, without a JSON object in between, and so does reopening a stored replica. Pokes and first syncs apply 7–19% faster, and a client opens on a stored replica about 10% faster.
