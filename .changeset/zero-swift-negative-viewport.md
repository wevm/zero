---
'zero-swift': patch
---

`VirtualList` no longer crashes when it's shorter than its insets, as on iOS in a window shorter than its safe area: it lays out as a list with no room for rows, and shows them once it has some.
