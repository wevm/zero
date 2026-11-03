---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` fix three cases where the list was left wrong until the next scroll:

- Rows removed or shrinking at the bottom of a list, or the viewport getting taller, no longer leave blank space below the last row. The scroll position comes back within the content, as a browser keeps it. A list that became shorter than its viewport could show no rows at all and keep loading pages.
- A list stuck to the bottom (`stickToBottom`) keeps the bottom in view when its viewport gets shorter, as when the keyboard comes up, and keeps following new rows. Before, it lost the bottom and stopped following.
- Deleting the row a list was opened at (`permalinkID`) or scrolled to (`scrollToItem`) no longer sends the list to the top. The rows below the deleted row move up into its place.
