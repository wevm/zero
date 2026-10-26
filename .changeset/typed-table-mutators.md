---
'zero-swift': minor
---

**Breaking:** `tx.mutate.<table>` is typed, as in JS. `insert` and `upsert` take the row struct, `update` its generated `Update` (the primary key and the columns to set), and `delete` its generated `Key`. An optional column in an `Update` is a `ColumnUpdate`, so leaving it (`.unchanged`, the default) differs from setting it to null (`.null`).

To migrate, regenerate with `npx zero-swift codegen`, then replace dictionaries passed to `tx.mutate.<table>` with the typed values:

```diff
- try tx.mutate.insert(Issue(id: id, title: title, ...))
+ try tx.mutate.issue.insert(.init(id: id, title: title, ...))

- try tx.mutate.issue.insert(["id": .string(id), "title": .string(title), ...])
+ try tx.mutate.issue.insert(.init(id: id, title: title, ...))

- try tx.mutate.issue.update(["id": .string(id), "title": .string(title)])
+ try tx.mutate.issue.update(.init(id: id, title: title))

- try tx.mutate.issue.update(["id": .string(id), "assigneeID": .null])
+ try tx.mutate.issue.update(.init(id: id, assigneeID: .null))

- try tx.mutate.issue.delete(["id": .string(id)])
+ try tx.mutate.issue.delete(.init(id: id))
```

`tx.mutate.insert(row)` and `tx.mutate.upsert(row)` are gone: `tx.mutate.<table>` is the one way to write. Generic code can use `TypedTableMutator<R>(tx.mutate)`, and `tx.mutate.table("issue")` takes dictionaries for untyped writes.
