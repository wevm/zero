---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` no longer move the rows on screen when rows are added above them while the list rests at its end, past its first page. The added rows pushed the last rows out of the page. Their estimated height then took their place below the page, so when they were taller than estimated, the content got shorter under the scroll position and the view clamped it. Once the next page brought those rows back, everything on screen had shifted by the difference. The space below the page now keeps the height the rows were laid out at until the page reaches them again.
