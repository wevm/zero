---
'zero-swift': patch
---

Add `VirtualList.maxKeptRows(_:)` and `VirtualListView.maxKeptRows`: how many rows that page out of the list keep their views for when they come back. A kept row scrolled back to is shown at once, with its state, instead of being made and measured again in the frame that brings it, which is what stalls a fast scroll back over rows it paged out. Kept rows hold their views, and their own queries stay open, until they come back or are among the oldest beyond the limit. The default, 0, keeps none, as before.
