---
'zero-swift': patch
---

A `VirtualList` or `VirtualListView` scrolled once a jump (`scrollToItem`, or opening at a `permalinkID`) has put its row on screen keeps what's on screen with the scroll as rows near it load and are measured. A scroll that began before the jump's rows had settled, within about a round trip of the jump, as when a chat that opens on its latest message is scrolled up right away, moved the rows on screen against it whenever a row above came out another height than estimated, until the scroll stopped, and the pages it loaded could leave the viewport blank.
