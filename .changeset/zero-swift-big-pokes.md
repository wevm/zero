---
'zero-swift': patch
---

Big pokes apply faster. A poke's rows are no longer sorted before they go into the replica, where their order doesn't matter, and a poke that changes a large share of the replica, such as the first sync, is diffed in one walk of the replica instead of sorting its keys and looking each one up. On a Mac, zbugs' first sync of 2,058 rows with the issue list open takes 11.7 ms instead of 13.8 ms, 5.7 ms instead of 8.0 ms with no view, and a poke of 1,000 rows into a 1,000-row view 3.3 ms instead of 3.8 ms.
