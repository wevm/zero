---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` make at most two new rows off screen per layout pass. Rows on screen are still made and measured in the pass that shows them; the rest of a newly loaded page is made in the frames after, so loading the next page while scrolling no longer drops frames.
