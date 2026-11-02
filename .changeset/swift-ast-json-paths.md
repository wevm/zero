---
'zero-swift': minor
---

The AST has the query features of sync protocols 54–57: `aggregate` on a query and on a relationship, `groupBy`, and JSON path references (`AST.ValuePosition.json`), with JS's wire checks, normalization, hashes and name mapping. `where` can compare a value inside a `json` column: `$0.json(\.metadata, "tags", 0) == "bug"` in the typed builder, `eb.cmp(eb.json("metadata", "tags", 0), "bug")` in the untyped one. As in JS, a value of another JSON type than the one compared with never equals it.

Breaking: `AST.ValuePosition` has a new case, `.json`; an exhaustive `switch` over it needs one too. Building a pipeline for an AST with an aggregate or a `groupBy` throws until the Swift client supports them.
