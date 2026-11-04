---
'zero-swift': minor
---

`VirtualList` and `VirtualListView` no longer load pages without settling at the end of a list stuck to the bottom (`stickToBottom`). When the window at the end held only a few rows, as after messages near the end were deleted and the user scrolled back down, the page loaded above them left out the last row. The space estimated in its place moved the bottom, the list followed it, and it loaded the end again, over and over.

`VirtualAnchor.backward`'s `startRow` is now optional, which breaks code that matches it as non-optional. A nil start row is the page that ends at the list's end. Your page query gets it as a backward `VirtualPageRequest` with no `start`, as its documentation already allowed: return the list's last rows, closest to the end first, as a backward page does after its start row.
