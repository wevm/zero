---
'zero-swift': patch
---

`ZeroVirtualizer` has an opt-in `totalRows`: set it to the list's row count, as from a `count()` the server has confirmed, and the scroll bar of a `VirtualList` or `VirtualListView` spans every row of the list at the estimated row height, rather than only the rows paged in so far. Dragging or jumping the thumb far into rows it hasn't loaded loads them from whichever end of the list is nearer. A list that opens away from its start, at a `permalinkID` or from a `scrollState`, asks its new `indexQuery` (a `count()` of the rows before a `VirtualIndexRequest`'s start row) how many rows are above it, and then moves its thumb to where it is in the list without moving anything on screen. Without `totalRows`, nothing changes.
