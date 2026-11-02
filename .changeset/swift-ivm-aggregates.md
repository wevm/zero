---
'zero-swift': minor
---

Queries can compute aggregates, `distinct()` and `groupBy()` on the client, with the IVM operators of the JS client (`Aggregate`, `GroupBy`, `GroupProjection` and `LiftField`), kept up to date as rows change. The untyped builder has `count()`, `sum`, `avg`, `min` and `max` (of a column, or of a value at a JSON path: `sum(json: "metadata", "points")`), `distinct(...)`, and `groupBy(...)` followed by an aggregate, with the JS builder's checks. On a query the view holds the value; in `related(...)` the relationship does, for a junction relationship too.

Breaking: `ViewValue` has a new case, `.value`, an aggregate's value; an exhaustive `switch` over it needs one too.
