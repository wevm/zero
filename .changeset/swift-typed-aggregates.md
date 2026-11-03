---
'zero-swift': minor
---

The typed query builder has aggregates, `distinct()` and `groupBy()`. `zql.issue.count()` is a `Query<Issue, Int>`; `sum` and `avg` give a `Double?`, and `min` and `max` the column's type, optional; each also takes a JSON path (`sum(json: \.metadata, "points")`). `distinct(\.ownerId)` gives `DistinctRow`s and `groupBy(\.ownerId).count()` gives `GroupRow`s, read by key path (`row[\.ownerId]`, `row.value`). `run`, `liveQuery`, `@ZeroQuery` and `defineQuery` take these queries as they take queries of rows. In `related`, an aggregate goes in the row's new generated `aggregates` property: `issue.aggregates?.count(\.comments)`.

Breaking: `QueryResult` now refines the new `QueryValue` protocol, which `Query`, `LiveQuery`, `QueryDefinition`, `QueryRequest` and `ZeroQuery` take in its place, and `Query` no longer requires `Result.Row == Row`. `ZeroRow` refines `QueryElement` and `OptionalQueryElement`, and `ZeroValue` refines `OptionalQueryElement`. Regenerate row structs with `zero-swift codegen` to get `aggregates`; a table can no longer have a column or relationship property named `aggregates`, which codegen renames.
