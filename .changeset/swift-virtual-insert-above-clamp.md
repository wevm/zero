---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` no longer move the rows on screen when rows are added above them near the end of a list that one page held. The added rows pushed the last rows on screen out of the page, so the scroll that keeps the rows on screen in place was clamped where the page now ended, and it stayed there once the next page loaded. It now scrolls on to where the rows were as that page loads, unless the user scrolls first.
