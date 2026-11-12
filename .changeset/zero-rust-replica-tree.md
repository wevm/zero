---
'zero-rust': patch
---

Fix the replica losing rows: after a delete emptied the first child of a node of its B+tree and the node later merged into its neighbor, the client couldn't find a row it still held. A mutator or a poke then couldn't delete the row, and putting it again panicked ("Row already exists").
