---
'zero-swift': patch
---

The replica takes a fifth less memory: the rows it keeps as JSON objects no longer keep room for up to twice their columns. Parsed objects start as big as the last one parsed like them was and give up much spare room when they close, and rows turned into JSON objects start at their size. On a Mac, 100,000 rows of 10 columns with a view of them take 129 MB instead of 161 MB. Opening a stored replica and big pokes take 2% less time.
