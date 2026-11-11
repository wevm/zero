# Zero for Rust

A native Rust client for Zero, in progress. See [DESIGN.md](DESIGN.md) for
the plan, the milestones, and the documented differences from JS.

```sh
cd rust
cargo test --workspace            # unit tests, and every replayed vector matches JS
cargo run -p zero-conformance     # the conformance report, file by file
```

The conformance runner reads `packages/zero-conformance/vectors`, and the
seeded fuzz corpora in `packages/zero-conformance/generated` when they are
there (`pnpm --filter zero-conformance run test` writes them).

## Generated types

`zero-rust codegen` (the npm package `zero-rust`, in `packages/zero-rust-cli`)
generates a row struct per table, the schema, and a typed query builder from
an app's TypeScript schema. It isn't on npm yet; from a checkout of this
repository:

```sh
node packages/zero-rust-cli/src/bin.ts codegen --schema-file path/to/schema.ts --out path/to/src/schema.rs
```

```rust
#[rustfmt::skip]
mod schema;
use schema::{Comment, Issue, zql};
use zero_client::typed::Direction;

let open = zql::issue()
    .where_eq(Issue::CLOSED, false)
    .related(Issue::COMMENTS, |q| q.order_by(Comment::CREATED_AT, Direction::Desc).limit(10))
    .order_by(Issue::CREATED_AT, Direction::Desc);
```

In a mutator, the generated `mutate` module does the table's CRUD:

```rust
use schema::{IssueUpdate, mutate};
use zero_client::client::Mutator;

let close = Mutator::new("issue.close", |tx, args, _ctx: &()| {
    let id = args.and_then(|a| a.as_str()).unwrap_or_default().to_owned();
    mutate::issue(tx).update(&IssueUpdate { closed: Some(true), ..IssueUpdate::new(id) })?;
    Ok(None)
});
```

See [DESIGN.md](DESIGN.md#codegen-m3).
