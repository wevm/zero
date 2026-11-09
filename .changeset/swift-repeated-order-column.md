---
'zero-swift': patch
---

A query whose subquery orders by one column twice, in opposite directions, such as `.whereExists("comments", flip: true) { $0.orderBy("text", .desc).orderBy("text", .asc) }`, no longer keeps rows it should drop. Looking up the subquery's rows for one parent row started past all of them, so a flipped `whereExists` found no comments for a deleted issue and left the issue in the view. The lookup now starts at the first row for that parent, as it does for any other ordering. JS's client still has this bug.
