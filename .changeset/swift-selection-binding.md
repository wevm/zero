---
'zero-swift': patch
---

`VirtualList` keeps the selected row highlighted when it draws a new snapshot. It read its `selection` binding's value from when the list was made, so a snapshot after a tap could set the selection back and hide the highlight.
