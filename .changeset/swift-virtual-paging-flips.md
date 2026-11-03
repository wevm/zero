---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` fix two cases where the list loaded pages without settling:

- Deleting the row a short list was opened at (`permalinkID`), when all the list's rows fit in one page, no longer makes the list load pages back and forth forever, each page missing its first or last row. This came with 0.6.2.
- Scrolled down past the loaded rows when the rows are shorter than `estimatedRowHeight`, the next page now comes in where the rows end. Before, it came in further down, so the screen could go blank and the list page back to its start.
