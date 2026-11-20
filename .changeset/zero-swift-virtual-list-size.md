---
'zero-swift': patch
---

`VirtualList` scrolls with less work on the Mac's main thread: it tells SwiftUI its size (what it's offered, as before) instead of SwiftUI asking Auto Layout for it, over every row's views, each time the stack around the list laid out, which it did for each snapshot. In the TestHost's issue list on a Mac, the main thread's work per frame while scrolling falls by 15–30%. The list's size is the same in every layout.
