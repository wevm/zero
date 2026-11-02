# Porting upstream's query features to the Swift client

Synced aggregates (`count/sum/avg/min/max`), JSON paths in aggregates and in `where`, `distinct()`, and `groupBy()`: sync protocol 53 → 57, upstream commits c9d1b15fc, e2ecf69ce, 0a1a0f3d4, 642aece86, 8b716b3fe, effd2da27, 5493ba549, fe168e640, 523d3b97e (in wevm/zero PR #65, branch `sync-upstream-6`).

Status: plan done; implementation in progress, with the decisions below taken (the user asked for the recommendation on each to be taken and recorded here).

## TL;DR

- Swift already sends `/sync/v57`, but its AST has none of the new fields, and its poke handler drops every `aggregate:` row (they aren't schema tables).
- The port is five stacked PRs: **AST, wire and JSON path filters → IVM operators (compute mode) → synced client (source mode, storage, optimism) → typed Swift API and codegen → zbugs `issueListCount`**. (Planned as six; JSON path filters moved into the first PR, since the AST's new `.json` value position needs its predicate anyway.)
- Every behavior is checked against JS-generated vectors: new AST/hash vectors, new IVM program ops, and client scenarios whose server rows come from JS's own compute pipeline.
- Public API breaks (new `ViewValue` and `AST.ValuePosition` cases, `Query`'s result constraint) are declared with a `minor` changeset; zero-swift is 0.5.x.

## 1. What JS does

### AST and protocol (54–57)

| Version | Addition | Where |
| --- | --- | --- |
| 54 | `aggregate: {fn, field?}` on `AST` (top level) and on `CorrelatedSubquery` (per parent) | `zero-protocol/src/ast.ts` |
| 55 | `{type: 'json', value: ColumnReference, path: (string\|number)[]}` as a condition's left side | same |
| 56 | `aggregate.path`: a JSON path into `field` (not for `count`) | same |
| 57 | `AST.groupBy: string[]` (alone it is `SELECT DISTINCT`), `aggregate.distinct: string[]` (count of distinct combinations) | same |

Wire checks (valita asserts): a path is non-empty, numeric segments are integers in `[0, 2^31-1]`; `path` needs `field` and no `count`; `distinct` only on `count` with no field; `groupBy` non-empty; a JSON-path `IN` takes a homogeneous list (or `null`), other operators a scalar.

`normalizedAST` writes `aggregate` and `groupBy` last, after `orderBy`. The query hash visitor writes **nothing** for absent fields, so old hashes are unchanged, and new tags `TAG_AGGREGATE 0x200e`, `TAG_JSON 0x200f`, `TAG_GROUP_BY 0x2010`, `TAG_DISTINCT 0x2011`. A relationship's aggregate is hashed before its subquery. `Format` gains `aggregate: {fn, field?, path?}`, which the query-internals hash also covers.

Name mapping: `aggregate.field` and `distinct` map client → server; for a junction aggregate the field is on the destination (`related[0]`); a JSON path's column goes through `jsonColumnName` (rejects a non-json column); the path itself is data.

### Synthetic tables and rows

```
aggregate:<queryID>                 top-level aggregate      key {_0_key: 0}
aggregate:<queryID>:<alias path>    relationship aggregate   key = correlation child columns (server names)
aggregate:<queryID>:#group          groupBy / distinct       key {_0_key: valueToKey(v) | JSON([v…])}

payload: _0_value        the result
         _0_sum _0_count  sum/avg components (for optimistic deltas)
         _0_rows          grouped sum/avg: rows in the group, nulls included
```

Alias paths escape each alias with `encodeURIComponent` plus `.`→`%2E`, joined by `.`. `createSchema` rejects table names starting with `aggregate:`.

### IVM operators

```
 compute mode (server, local queries, mutators without a synced result)

  source ─ filters ─ [take/skip] ─► Aggregate(groupKey=[])            top-level
  child  ─ filters ───────────────► Aggregate(groupKey=childField) ─► Join(parent)
  junction ─ Join(dest) ─ LiftField(dest.field) ─► Aggregate ─► Join  many-to-many sum/avg/min/max
  source ─ filters (sorted by group cols) ─► GroupBy ─► GroupProjection ─► view
  source ─ … ─► GroupBy(count) ─► Aggregate(count)                     count of distinct()

 source mode (synced client: aggregatesFromSource = true)

  MemorySource("aggregate:<qid>")            ─► view
  MemorySource("aggregate:<qid>:<alias>")    ─► Join(parent)
  MemorySource("aggregate:<qid>:#group")     ─► GroupProjection ─► view
```

- `Reducer` (count/sum/avg/min/max): count/sum/avg are invertible (O(1) per change); min/max mark a group dirty when its extreme leaves and re-read it in `reconcile`. Numbers order before strings for JSON leaves. An edit is emitted only when the value (or, for sum/avg, a component) changes.
- `Aggregate` keeps a state per group key, created on fetch; pushes to an unfetched group are ignored. A `count` asks the source (`SourceInput.count`) when nothing between drops rows.
- `GroupBy` folds runs of a sorted input; add/remove/move rows of groups; `groupCounts` source fast path.
- `GroupProjection` renames to view names and puts the value under the function name (`count`, `sum`, …); a distinct hides the value.
- `LiftField` copies the destination's field (or JSON leaf) onto the junction row; destination changes become edits of the lifted column.
- View: an aggregate relationship is the bare scalar, `0`/`null` before any row (also for a parent whose correlation value is null). A top-level aggregate view starts at `0`/`null` (642aece86).

### Builder

- `buildPipeline`: groupBy or `aggregate.distinct` forces `orderBy := groupOrdering(ast)`. Source mode short-circuits top-level aggregates and groups before touching the table.
- `applyCorrelatedSubQuery` with `sq.aggregate`: compute → child pipeline (+ LiftField for a junction) + `Aggregate`; source → `getAggregateSource(name, columns, childKey, optimisticDelta?)`, keyed by server column names.
- Optimistic delta metadata only for invertible, direct, unbounded aggregates whose `where` is per-row evaluable (`transformFilters` with no `conditionsRemoved`).

### Client storage and pokes

```
 poke rowsPatch ──► isAggregateTableName? ──yes──► aggregateRowKey → a/<name without "aggregate:">/<key>   (no name mapping)
                                          └─no───► schema table? → e/<table>/<pk> : drop

 store ─► IVMSourceBranch.applyDiffs: a/ then e/
           a/ source missing → top-level: provision from name; else inferAggregateSchema(row) (reload)
           after each advance: drop empty, unconnected aggregate sources + their optimism registrations
```

- `a/` instead of `e/` (8b716b3fe) so an older client, which loads every `e/` row into a schema table, can still open the database. No storage version bump.
- CRUD (`crud-impl.ts`): after each insert/update/delete, `applyOptimisticAggregateDeltas` writes the moved aggregate row in the same transaction and pushes it into the branch, so views and `tx.run` see it; the rebase replays it until the server confirms. Grouped queries add a group with its first row and drop it with its last.
- `tx.run` in a custom mutator (`MutatorContext`): reads synced aggregate rows when `g/<queryID>` exists, otherwise computes from the branch's rows.

### Query API and result types

- `count()` → `number`; `sum/avg` → `number | null`; `min/max` → the field's type `| null`. On a top-level query the materialized value is the scalar; inside `related` the relationship becomes that scalar.
- `sum(({json}) => json('metadata','points'))` and the same for avg/min/max.
- `where(({cmp, json}) => cmp(json('metadata','priority'), '=', 'high'))`, type-strict leaf comparisons: a type mismatch is a non-match, or a match for a negated operator.
- `distinct(...cols)` → rows of those columns; `.count()` after it counts combinations. `groupBy(...cols).count()/sum()/…` → `{...cols, count}` rows. Both reject `limit/start/one/related`, json columns, named-query bases, and `orderBy` of other columns.
- Misuse throws from the builder: aggregate after `limit/start/one`, aggregate on a named query, `exists` over an aggregate, grouped subqueries, `related` under an aggregate.
- React suspense waits for `complete`/`cached` for a top-level aggregate (523d3b97e): a `0` isn't "partial data".

## 2. Swift mapping

### Engine (package-internal)

| JS | Swift |
| --- | --- |
| `AST.aggregate`, `groupBy`, `CorrelatedSubquery.aggregate`, `JsonPathReference` | `AST.swift`, `ASTCoding.swift`, `ASTNormalize.swift`, `StrictAST.swift`, `QueryHash.swift`, `NameMapper.swift` |
| `Format.aggregate` | `IVM/View.swift` `Format` (+ its JSON for the hash) |
| `filter.ts` json predicate, `jsonValueAtPath`, type-strict compare | `Query/Predicate.swift`; `PredicatePushdown.swift` and `Constraint.swift` skip json refs |
| `aggregate.ts`, `group-by.ts`, `lift-field.ts` | new `IVM/Aggregate.swift`, `IVM/GroupBy.swift`, `IVM/LiftField.swift` (1:1 ports, as the other operators) |
| `MemorySource.count / groupCounts / connected` | `IVM/MemorySource.swift` |
| builder compute + source modes | `Builder/Builder.swift`, `BuilderDelegate.aggregatesFromSource`, `getAggregateSource`, `aggregateKeyColumns` |
| view scalar values | `ViewValue.value(JSONValue)` (new case), `ViewApply` aggregate branch, `ArrayView` empty value |
| `keys.ts` `a/`, `aggregateRowKey`, `sourceNameFromKey` | `Client/Keys.swift` |
| poke routing | `Client/PokeHandler.swift` `rowPatchOp` |
| `IVMSourceBranch` aggregate sources, registry, release | `Client/IVMBranch.swift` |
| `crud-impl.ts` optimistic deltas | `Client/Transaction.swift` |
| `MutatorContext` | `Client/QueryContext.swift` (`BranchDelegate`), `Transaction.run` |

Storage: the SQLite `entries` table holds every key, so `a/` rows persist with no schema change. Released Swift clients only apply `e/` keys to their sources (`IVMBranch.apply`), so they ignore `a/` rows and can share a database with a newer build, as upstream intended. No Swift layout version bump.

### Public Swift API: options and decisions

**D1. Top-level aggregate result.** Options: (a) `Query<Issue, Int>` for `count()`, `Query<Issue, Double?>` for sum/avg, `Query<Issue, V?>` for min/max; (b) a wrapper `Query<Issue, Aggregate<Int>>` with `.value`; (c) a separate `AggregateQuery` type with its own `materialize/run/liveQuery/defineQuery` overloads.
**Decision: (a).** It reads like JS and needs no new entry points. It takes a new `QueryValue` protocol (what `run`, `LiveQuery`, `@ZeroQuery` and `defineQuery` accept), with `QueryResult` (rows) refining it, and dropping `Query`'s `Result.Row == Row` constraint. `Optional`'s two uses (row or scalar) go through one conditional conformance over a shared element protocol, since Swift allows only one. Breaking at the type level only → `minor` changeset.

**D2. Relationship aggregates.** A generated row struct's `comments: [Comment]?` can't hold a number. Options: (a) codegen adds `aggregates: RelationshipAggregates<Issue>?`, nil unless the query asked for an aggregate, read as `issue.aggregates?.count(\.comments)` / `.sum(\.comments)` / `.value(\.comments)`; (b) only an untyped `ViewEntry.relationships["comments"]`; (c) aliasing, which JS doesn't have.
**Decision: (a)**, plus (b) underneath. Old generated code keeps compiling (it just doesn't read them). `related(\.comments) { $0.count() }` keeps the `related` call JS has.

**D3. `groupBy` / `distinct` rows.** Options: (a) generic `GroupRow<Issue, Int>` / `DistinctRow<Issue>` read by key path (`row[\.assigneeID]`, `row.value`); (b) codegen per-query structs (impossible: codegen sees the schema, not queries); (c) raw `JSONObject`s.
**Decision: (a).** `zql.issue.groupBy(\.assigneeID).count()` → `Query<Issue, [GroupRow<Issue, Int>]>`; `distinct(\.assigneeID)` → `Query<Issue, [DistinctRow<Issue>]>`; `distinct(…).count()` → `Query<Issue, Int>`. Reading a column that isn't grouped is a precondition failure, as a non-column key path is today.

**D4. JSON paths.** Options: (a) untyped segments: `$0.json(\.metadata, "points") > 3`, `.sum(json: \.metadata, "points")`; (b) key paths into the generated `Codable` JSON types (a key path doesn't expose its property names at runtime, and `JSONValue` columns have none).
**Decision: (a)**, with `JSONPathSegment` literals (`"key"`, `0`). Literal types follow JS's runtime rule (strings, numbers, bools; homogeneous lists for `in`).

**D5. Builder misuse.** Follow the existing Swift pattern: what JS throws for marks the query invalid (`AnyQuery.invalid`), and materialize/run/preload throw JS's message. Programming errors JS asserts on (unknown relationship) stay preconditions.

**D6. Optimistic aggregates.** Port in full (count/sum/avg, grouped add/remove, `where` predicates, junction and min/max server-authoritative). Parity, and `tx.run` reads its own writes.

**D7. Suspense parity (523d3b97e).** Swift has no suspense. `LiveQuery`/`@ZeroQuery` keep reporting `resultType`; a top-level aggregate is `0` + `.unknown` until the server answers. Documented in `JSParity.md`; no code.

### Codegen (`packages/zero-swift-cli`)

- Row structs: add `aggregates: RelationshipAggregates<Self>?` and read it in `init(entry:)`.
- Nothing else: aggregate tables are never schema tables, and the generated column references already serve `groupBy`/`distinct`/`sum`.
- The sweep and codegen tests regenerate every checked-in `Generated/` file.

## 3. Conformance vectors (all from JS)

| Vector file | Generator | New cases |
| --- | --- | --- |
| `ast-features.json` (new) | `primitives/ast-features.test.ts` | strictASTSchema verdict on every new field, incl. each valita rejection; normalize, hash and `mapAST` of aggregates (junction field), json refs, distinct, groupBy |
| `query-internals-hash.json` (new) | same | `hashOfQueryInternals` with `Format.aggregate` |
| `primary-key-string.json` | `keys.test.ts` | `aggregateRowKey` for the three table kinds, `sourceNameFromKey` of `a/` keys |
| `ivm-features.json` (new; datasets with varied `user.metadata`) | `ivm/ivm-features.test.ts` | new program ops `count/sum/avg/min/max` (field or json), `distinct`, `groupBy`, `whereJson`; related aggregates incl. junction (LiftField), min/max extreme removal, group moves, json-path type strictness; builder-misuse cases record JS's error |
| `client.json` | `client/scenarios/aggregates.ts` (new) | named aggregate queries synced from pokes, empty value before the server, optimistic count/sum/avg and grouped add/remove, rebase, restart reload from `a/`, source release, `tx.run` synced vs computed |

Server rows in client scenarios come from JS, not by hand: a helper builds the named query's AST, runs it through `buildPipeline` in compute mode with the real query ID over the scenario's rows, and turns the emitted rows into `rowsPatch` puts, as zero-cache's pipeline driver streams them. The JS runner then records what the real `TestZero` shows. An end-to-end test also syncs a `count()` and a relationship `count()` from a real zero-cache and Postgres.

## 4. PR sequence

```
 main (with #65) ◄─ PR1 ◄─ PR2 ◄─ PR3 ◄─ PR4 ◄─ PR5     (stacked)
```

| PR | Scope | Checks |
| --- | --- | --- |
| 1. AST, wire, JSON path filters | AST fields + json value position, coding, normalization, strict decoding, hash, name mapping, `Format.aggregate`; `json(...)` in `where` (untyped and typed) and its predicate | `ast-features`, `query-internals-hash`, `ivm-features` (json filters) vectors; minor changeset (public enum case) |
| 2. IVM compute mode | `Aggregate`, `GroupBy`, `GroupProjection`, `LiftField`, source counts, builder compute path, `ViewValue.value`, untyped `AnyQuery` builder + validations | curated + fuzz ivm vectors |
| 3. Synced client | poke routing to `a/`, branch aggregate sources, source-mode builder, reload, release, optimistic deltas, `tx.run` | client scenario vectors on memory and SQLite; e2e count against zero-cache |
| 4. Typed API + codegen | `count/sum/avg/min/max`, `distinct`, `groupBy`, `QueryValue`, `RelationshipAggregates`, LiveQuery/ZeroQuery/run/defineQuery; DocC + `JSParity.md` | `TypedQueryTests`, codegen tests, docs, api-breaks |
| 5. zbugs | port `issueListCount`, remove the drift exemption, show the count in the list | zbugs drift test passes with no exemption; zbugs Linux + macOS builds |

Every PR: Linux Swift, vectors staleness, CLI, api-breaks, zbugs, e2e, and macOS (path-filtered) green before review.

## 5. Risks and open questions

| Risk | Mitigation |
| --- | --- |
| Conditional-conformance rules and D1's `Optional` dual use | Prototyped with Swift 6.2.4: `Optional: QueryValue where Wrapped: OptionalQueryElement` (rows and scalars both conform) plus `Optional: QueryResult where Wrapped: ZeroRow` compiles and dispatches correctly |
| Synced-row shapes drift from zero-cache (stamped `_0_version`, server column names) | Scenario rows come from JS's compute pipeline; e2e against real zero-cache |
| JS assertion bugs (like the flipped-join one in DESIGN.md) in new operators | Fuzz vectors; any JS bug found is recorded in DESIGN.md's list, and Swift does what JS should |
| `ViewValue` new case breaks apps' exhaustive switches | Declared minor; release notes say to add a `.value` case |
| Performance of grouped hydration on large tables | `groupCounts`/`count` source fast paths ported; benchmarks unchanged otherwise |

Nothing here needs the user's decision before implementation; D1–D7 are taken as recommended. Before 1.0 each is changeable with a minor release. The one to look at first is D2 (`aggregates` on generated rows), the least JS-like.
