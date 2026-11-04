---
'zero-swift': patch
---

`VirtualList` and `VirtualListView` no longer go to the top when the row a jump landed on is deleted before the server has answered the queries of the rows around it. The jump draws its row from the rows the client already has; deleted then, the row was taken for one that never existed, and the list went to its top, then on to wherever the scroll offset left it among the first pages. The rows below the deleted row now move up into its place, as they do once those queries complete.
