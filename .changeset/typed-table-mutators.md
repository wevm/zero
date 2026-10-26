---
'zero-swift': minor
---

`tx.mutate.<table>` is typed, as in JS: `insert` and `upsert` take the row struct, `update` its generated `Update` (the primary key and the columns to set), and `delete` its generated `Key`. An optional column in an `Update` is a `ColumnUpdate`, so leaving it (`.unchanged`) differs from setting it to null (`.null`). Breaking: code that passed a dictionary to `tx.mutate.<table>` now uses these types, or `tx.mutate.table("<table>")` for the untyped `TableMutator`. Regenerate with `zero-swift codegen`.
