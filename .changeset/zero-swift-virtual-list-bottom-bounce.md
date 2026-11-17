---
'zero-swift': patch
---

A `VirtualList` or `VirtualListView` on iOS now bounces at its bottom as it does at its top: a fling or a pull past the last row springs back instead of stopping dead, at the list's end and at the end of the rows loaded so far. The list took any offset past the end of its content back to the end on every layout pass, to keep rows that went or got shorter at the bottom from leaving blank space, and that took the scroll view's own bounce back too. It now keeps the bounce and still takes back what the content shrinking leaves.
