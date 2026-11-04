---
'zero-swift': patch
---

`VirtualListView` no longer makes UIKit log "Observation tracking feedback loop detected" for its scroll view when its rows change often, as in a list whose rows the server updates many times a second. The list read the virtualizer's `snapshot` while laying a snapshot out, so UIKit and AppKit, which follow what a view's layout reads, laid it out again for every snapshot after that, on top of the layout pass the list already asks for. The list now reads the snapshot without observing it. `VirtualList` does the same.
