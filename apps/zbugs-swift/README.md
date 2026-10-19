# zbugs on the Zero Swift client

The schema, queries, and mutators of [zbugs](../zbugs) (`apps/zbugs/shared`),
ported to Swift as the `ZBugs` library.

- `Sources/ZBugs/Schema.swift` is generated from `apps/zbugs/shared/schema.ts`
  by `zero-swift codegen` (config in `zero-swift.json`).
- `Queries.swift` and `Mutators.swift` are ports of `queries.ts` and
  `mutators.ts` (with `auth.ts` in `Auth.swift`).
- `Tests/ZBugsTests/drift-vectors.json` records what the TS queries and
  mutators do on the samples in `drift-samples.ts`. `DriftTests` fails when
  the Swift port does something else, or when a query or mutator exists on
  only one side.

After changing a TS query or mutator (or the samples), re-record and port the
change:

```sh
cd packages/zero-swift-cli
UPDATE_GENERATED=1 pnpm test   # rewrites Schema.swift and drift-vectors.json
cd ../../apps/zbugs-swift
swift test
```

CI fails while the record is out of date (`pnpm --filter zero-swift run test`)
or the port drifts from it (`swift test` here).
