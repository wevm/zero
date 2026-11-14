---
'zero-swift': patch
---

Cut the main-thread work of scrolling a `VirtualList` on macOS. Each row's hosting view followed the safe area, so on every frame the list scrolled, every row it kept (on screen or not) worked out its safe area insets and size constraints again and laid itself out: about 50 row layouts a frame. Rows no longer follow the safe area, as on iOS. A layout pass also finds each row by its key instead of searching the snapshot's rows for it.
