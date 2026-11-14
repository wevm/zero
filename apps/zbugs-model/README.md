# zbugs model (Rust)

[zbugs](../zbugs)' schema, queries, and mutators on the Zero Rust client
(`rust/zero-client`), shared by the Rust zbugs apps (`apps/zbugs-gpui`,
`apps/zbugs-rune`). It has no UI and no transport: each app brings its own,
and depends on this crate by path.

- `src/schema.rs` is generated from `apps/zbugs/shared/schema.ts` by
  `zero-rust codegen` (config in `zero-rust.json`).
- `src/queries.rs`, `src/mutators.rs`, and `src/auth.rs` are ports of
  `queries.ts`, `mutators.ts`, and `auth.ts`, after zbugs-swift's.
- `tests/drift.rs` checks them against what the TS definitions do on the
  samples in `apps/zbugs-swift/drift-samples.ts`. It replays the record
  zbugs-swift's drift test reads, `apps/zbugs-swift/Tests/ZBugsTests/drift-vectors.json`.

```rust
use zbugs_model::args::{IssueListV2Args, ListContextParams, PageDirection};
use zbugs_model::{mutators, queries};

// `define_query` and `define_mutator` definitions; the client's context
// (the logged-in user) comes from `zero`.
let list = queries::ISSUE_LIST_V2.with(args);
let view = zero.materialize(&list, &ttl)?;
let issues = list.read(&view.data())?;

zero.mutate_request(mutators::CREATE_ISSUE.with(create_args));
// Zero::new(schema::schema(), auth, zbugs_model::client_mutators(), …)
```

## Keeping the port current

After changing a TS query or mutator (or the samples), re-record and port
the change:

```sh
cd packages/zero-swift-cli && UPDATE_GENERATED=1 pnpm test   # drift-vectors.json
cd ../zero-rust-cli && UPDATE_GENERATED=1 pnpm test          # src/schema.rs
cd ../../apps/zbugs-model && cargo test
```
