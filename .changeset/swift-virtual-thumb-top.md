---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` no longer show a gap above the first row after the scroll thumb is dragged quickly to the top, with rows shorter than `estimatedRowHeight`. The gap stood there until the list came to rest, about a sixth of a second, and then the rows jumped up into it. The rows now start at the top in the first frame drawn there.
