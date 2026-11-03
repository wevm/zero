---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` fix three more cases where the list was left wrong until the next scroll:

- `scrollToItem` to a loaded row near the end of the loaded rows, which the list couldn't yet scroll far enough to line up, now lines the row up once the rows below it load. Before, it stopped where it was clamped.
- After a fast scroll up past the loaded rows, a row on screen changing height, such as an image loading, moves only the rows below it. Before, it could move the rows above it.
- Scrolled up past the rows of a list opened at a permalink, the page above now loads on screen. Before, the screen stayed blank while the list loaded pages back to its start and forward again.
