---
'zero-swift': patch
---

A `VirtualList` or `VirtualListView` no longer shows blank space when it's flung, dragged, or jumped faster than the server confirms its pages. The list pages on the rows the client already has, rather than waiting for the server to confirm each page, which a fast fling outran by a round trip a page, and a jump across the list by one a page it passed. A page that comes back short leaves no space for rows the client doesn't have, as at the list's start before the server confirms it. And a row that scrolls on screen above the rows already there, taller or shorter than estimated, as a long chat message is, now grows up from them instead of pushing them down. Debug builds launched with `-ZeroVirtualTintBlankSpace YES` tint the space behind the rows orange, so any blank space shows.
