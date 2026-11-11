# zero-client

A native Rust client for [Zero](https://zero.rocicorp.dev). Your app
queries a local replica of the server's data, and the replica stays in
sync. Mutations apply to the replica right away and run again on your
server, whose result wins.

It's a port of the JS client (`@rocicorp/zero`) and behaves the same: every
one of the 63,453 cases of the conformance vectors generated from the JS
client
([`packages/zero-conformance`](https://github.com/wevm/zero/tree/main/packages/zero-conformance))
replays to JS's result, 2,255 of them under a documented difference, and
end-to-end tests run it against `zero-cache` and Postgres, through a chaos
proxy.

> [!NOTE]
> Before 1.0, minor versions may change the API.

## Install

```sh
cargo add zero-client --features tokio-transport,tls,sqlite
```

The default build has no dependencies. The features:

- `tokio-transport`: a scheduler and a WebSocket transport on tokio. An app
  can bring its own instead (`Scheduler`, `SocketFactory`).
- `tls`: `wss://` for that transport, with rustls on `ring`.
- `sqlite`: SQLite storage on the system `libsqlite3`; `sqlite-bundled`
  compiles SQLite in. An app can bring its own store
  (`ZeroStorage::Custom`).

**Without the `sqlite` feature the client keeps nothing across restarts**:
the default storage is memory only, so a restarted app starts with no
synced rows and no pending mutations.

## Quick start

The `zero-rust` CLI generates a row struct per table, the schema, and a
typed query builder from your TypeScript schema:

```sh
npx zero-rust codegen --schema-file shared/schema.ts --out src/schema.rs
```

The client is single-threaded: make it, and call it, on a tokio `LocalSet`.

```rust,ignore
use std::rc::Rc;
use zero_client::client::{Zero, ZeroOptions, ZeroStorage};
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};
use zero_client::ttl::Ttl;
use zero_client::typed::Direction;
use schema::{Issue, zql};

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

let open = zql::issue()
    .where_eq(Issue::CLOSED, false)
    .order_by(Issue::CREATED_AT, Direction::Desc);
let view = zero.materialize(open.any(), &Ttl::default())?;
let _unsubscribe = view.add_listener(move |data, _result_type, _error| {
    let issues: Vec<Issue> = open.read(data).unwrap();
    // Show them.
});
```

A query the server syncs is named (`AnyQuery::name_and_args`, the name and
arguments JS's `defineQueries` gives it); an unnamed one only sees local
data. `zero_client::testing` has a manual clock and
in-memory sockets for testing an app without a server.

## Compatibility

| zero-client | Sync protocol | `zero-cache`   | Conformance vectors from |
| ----------- | ------------- | -------------- | ------------------------ |
| 0.1         | 57            | 1.12+ (canary) | Zero 1.12 (`main`)       |

A Zero 1.11 `zero-cache` rejects protocol 57 with `VersionNotSupported`.

## More

- [Coming from the JS client](https://github.com/wevm/zero/blob/main/rust/PARITY.md):
  what each part of the JS API is called here, what isn't ported, and the
  documented differences
- [Benchmarks](https://github.com/wevm/zero/blob/main/rust/BENCHMARKS.md)
  against the JS and Swift clients
- [Design](https://github.com/wevm/zero/blob/main/rust/DESIGN.md): how the
  port is built and tested

## License

Apache-2.0
