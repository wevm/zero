---
'zero-swift': patch
---

A `ZeroVirtualizer` list scrolled away from its start no longer goes blank for good when every row from its loaded window's start on is deleted. The window now pages back to the rows before it; before, it stayed empty, and scrolling didn't bring the rows back. zero-virtual still stays empty there.
