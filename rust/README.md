# Zero for Rust

A native Rust client for Zero, at parity with the JS client. See
[PARITY.md](PARITY.md) for what each part of the JS API is called here and
the documented differences, [DESIGN.md](DESIGN.md) for the plan and the
milestones, and [BENCHMARKS.md](BENCHMARKS.md) for how it compares with the
JS and Swift clients.

```sh
cd rust
cargo test --workspace            # unit tests, and every replayed vector matches JS
cargo run -p zero-conformance     # the conformance report, file by file
```

The conformance runner reads `packages/zero-conformance/vectors`, and the
seeded fuzz corpora in `packages/zero-conformance/generated` when they are
there (`pnpm --filter zero-conformance run test` writes them).

## Quick start

```sh
cargo add zero-client --features tokio-transport,tls,sqlite
```

```rust
use std::rc::Rc;
use zero_client::client::{Zero, ZeroOptions, ZeroStorage};
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};

// On a tokio `LocalSet`: the client is single-threaded.
let zero = Zero::new(
    schema::schema(),
    (),
    vec![/* mutators */],
    ZeroOptions {
        cache_url: Some("https://zero.example.com".into()),
        user_id: Some(user_id),
        auth: Some(token),
        storage: ZeroStorage::Sqlite { directory: None },
        ..ZeroOptions::default()
    },
    Rc::new(TokioScheduler::new()),
    Some(Rc::new(TokioSocketFactory::new())),
)?;
```

**Without the `sqlite` feature the client keeps nothing across restarts**:
the default storage is memory only, and the default build has no
dependencies. The features:

- `tokio-transport`: a scheduler and a WebSocket transport on tokio. An app
  can bring its own instead (`Scheduler`, `SocketFactory`).
- `tls`: `wss://` for that transport, with rustls on `ring`.
- `sqlite`: SQLite storage on the system `libsqlite3`; `sqlite-bundled`
  compiles SQLite in. An app can bring its own store
  (`ZeroStorage::Custom`).

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

## Compatibility

| zero-client | Sync protocol | `zero-cache`   | Conformance vectors from |
| ----------- | ------------- | -------------- | ------------------------ |
| 0.1         | 57            | 1.12+ (canary) | Zero 1.12 (`main`)       |

A Zero 1.11 `zero-cache` rejects protocol 57 with `VersionNotSupported`.
