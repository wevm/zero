---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` no longer leave a gap at the top when rows are removed from the top of a list that is scrolled all the way up. The removed rows' space stayed above the next row until the next scroll. At the top, the rows below now move up into their place, the same way rows added there are revealed. Removals above a scrolled list still keep what's on screen in place. `LoopbackServer` gains `delete(_:_:primaryKey:)`.
