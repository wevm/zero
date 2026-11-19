# Zero for Rust

A native Rust client for [Zero](https://zero.rocicorp.dev). Your app
queries a local replica of the server's data, and the replica stays in
sync. Mutations apply to the replica right away and run again on your
server, whose result wins.

It's a port of the JS client (`@rocicorp/zero`) and behaves the same:
conformance vectors generated from the JS client check it, and so do
end-to-end tests against `zero-cache` and Postgres.

> [!NOTE]
> Before 1.0, minor versions may change the API.

## Install

```sh
cargo add zero-client --features tokio-transport,tls,sqlite,serde
cargo add serde --features derive
cargo add tokio --features rt,macros
npx zero-rust codegen --schema-file shared/schema.ts --out src/schema.rs
```

Rust 1.88 or later, and a `zero-cache` that speaks sync protocol 57 (Zero
1.12 or later). `zero-rust codegen` generates a row struct per table, the
schema, a typed query builder (`zql`), and typed CRUD for mutators
(`mutate`) from your TypeScript schema; `--check` fails in CI when the
output is stale.

## Quick start

Write the queries and mutators your app runs in Rust, named as in your
TypeScript `defineQueries` and `defineMutators`: the client runs them on
the replica, and `zero-cache` has your server run the TypeScript ones.

```rust
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use zero_client::client::{
    ConnectAuth, ConnectionState, MutationOutcome, Zero, ZeroOptions, ZeroStorage,
};
use zero_client::define::{MutatorDefinition, QueryDefinition, define_mutator, define_query};
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};
use zero_client::ttl::Ttl;
use zero_client::typed::Direction;

#[rustfmt::skip]
mod schema; // from `zero-rust codegen`
use schema::{Issue, IssueUpdate, mutate, zql};

/// What queries and mutators get besides their arguments.
type Context = Option<String>;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectArgs { project_id: String }

#[derive(Serialize, Deserialize)]
struct CloseArgs { id: String }

const OPEN_ISSUES: QueryDefinition<ProjectArgs, Context, Issue> =
    define_query("issues.open", |args, _ctx| {
        zql::issue()
            .where_eq(Issue::PROJECT_ID, args.project_id.clone())
            .where_eq(Issue::OPEN, true)
            .order_by(Issue::MODIFIED, Direction::Desc)
    });

const CLOSE_ISSUE: MutatorDefinition<CloseArgs, Context> =
    define_mutator("issue.close", |tx, args, _ctx| {
        mutate::issue(tx).update(&IssueUpdate { open: Some(false), ..IssueUpdate::new(args.id) })
    });

// The client is single-threaded: make it, and call it, on a tokio `LocalSet`.
let zero = Zero::new(
    schema::schema(),
    Some(user_id.clone()),
    vec![CLOSE_ISSUE.mutator()], // every mutator the app may run
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

// Rows read as `Issue`s and kept current, until `issues` drops.
let issues = zero.live(&OPEN_ISSUES.with(ProjectArgs { project_id: "p1".into() }), &Ttl::default())?;
let _remove = issues.add_listener(|issues, result_type| {
    // Show them. `result_type` is `Complete` once the server has confirmed them.
});

// Applies to the replica before it returns, then runs on your server.
let result = zero.mutate_request(CLOSE_ISSUE.with(CloseArgs { id: "i1".into() }));

// Every mutation's server outcome, in one place.
let _remove = zero.add_mutation_outcome_listener(|o| {
    if let MutationOutcome::AppError { message, .. } = &o.outcome {
        eprintln!("{} was rejected: {message}", o.name);
    }
});

// `NeedsAuth` when the server rejects the token: get a new one, then connect with it.
let _unsubscribe = zero.connection().subscribe(|state| {
    if let ConnectionState::NeedsAuth(_) = state { /* fetch a new token */ }
});
zero.connection().connect(ConnectAuth::Set(new_token))?;
```

## API at a glance

The [API reference](https://docs.rs/zero-client) documents every item. An
app uses `client`, `define`, `typed` (through generated code), `ttl`, and
`testing`.

| Concept            | API                                                                                                    | Notes                                                           |
| ------------------ | ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| Client             | `Zero::new(schema, context, mutators, options, scheduler, socket_factory)?`                            | Not `Send`: one thread                                          |
| Client at launch   | `Zero::prepare(..)`, then `Zero::from_prepared(prepared, scheduler, socket_factory)?`                  | Opens and reads the SQLite store on another thread meanwhile    |
| Options            | `ZeroOptions { cache_url, user_id, auth, storage, mutate_url, query_url, log_level, .. }`              | JS's options in snake case                                      |
| Named queries      | `define_query(name, \|args, ctx\| query)`, called as `Q.with(args)`                                    | `serde` feature                                                 |
| Query builder      | `zql::issue()`, `.where_eq`, `.where_expr`, `.where_exists`, `.related`, `.order_by`, `.limit`, `.one` | Also `count`, `sum`, `group_by`, `distinct`                     |
| Read, kept current | `zero.live(&query, &ttl)?`, a `LiveView<T>`: `value()`, `result_type()`, `error()`, `add_listener`     | `useQuery`; dropping it destroys the view                       |
| Read otherwise     | `materialize(&query, &ttl)?`, `run(&query, RunType::Complete, None)`, `preload(&query, &ttl)`          | Untyped view, read once, sync without a view                    |
| Named mutators     | `define_mutator(name, \|tx, args, ctx\| ...)`, registered as `M.mutator()`                             | Synchronous; runs again on each rebase                          |
| Writes in mutators | `mutate::issue(tx).insert(&row)?`, `.upsert`, `.update`, `.delete`; `tx.run(&query)?`                  | Generated                                                       |
| Mutate             | `zero.mutate_request(M.with(args))`, a `MutatorResult` with `client` and `server` outcomes             | `zero.mutate(name, json)` untyped                               |
| Server outcomes    | `zero.add_mutation_outcome_listener(\|o\| ...)`                                                        | Also hears mutations replayed after a reconnect                 |
| Connection         | `connection().state()`, `.subscribe(listener)`, `.connect(ConnectAuth::Keep \| Set(token) \| Clear)?`  | `NeedsAuth` when the server rejects the token                   |
| Background         | `pending_settled(timeout_ms)`, `set_visible(false)`, then `hidden_disconnect_delay_ms`                 | 5 minutes, as in JS; 0 on mobile                                |
| Storage            | `ZeroStorage::Memory` (default), `Sqlite { directory }`, `Custom`; `flush_storage()`, `delete()`       | `client::drop_all_databases`                                    |
| Inspector          | `zero.inspector()`: `metrics()`, `client().queries()`, `analyze_query(..)`, `server_version()`         | Logs slow materializations past `slow_materialize_threshold_ms` |
| Testing            | `testing::{ManualScheduler, MockSocketFactory, RecordingLogSink}`                                      | A client without a server                                       |

## Features

The default build has no dependencies. Each of these is opt-in:

| Feature           | Adds                                                                             |
| ----------------- | -------------------------------------------------------------------------------- |
| `tokio-transport` | `tokio_transport`: a scheduler and a WebSocket transport on tokio                |
| `tls`             | `wss://` for that transport, with rustls on `ring`                               |
| `sqlite`          | `ZeroStorage::Sqlite`, on the system `libsqlite3`                                |
| `sqlite-bundled`  | `sqlite`, with SQLite compiled in                                                |
| `serde`           | `define`: typed `define_query` and `define_mutator`, and serde for `json::Value` |

**Without the `sqlite` feature the client keeps nothing across restarts**:
the default storage is memory only, so a restarted app starts with no
synced rows and no pending mutations. Without `tokio-transport`, bring your
own event loop with `client::Scheduler` and `client::SocketFactory`.

`ZeroStorage::Sqlite { directory: None }` keeps each app's databases in a
folder of its own: `~/Library/Application Support/<bundle ID>/Zero` on
macOS and Mac Catalyst (the executable's name outside an app bundle),
excluded from backups; `%LOCALAPPDATA%\<app>\Zero` on Windows; and
`~/.local/share/<app>/zero` (or `$XDG_DATA_HOME/<app>/zero`) elsewhere,
where `<app>` is the executable's name. On iOS and Apple's other platforms
it stays `~/.local/share/zero`, in the app's container.

## Differences from JS

- **One thread.** `Zero` is neither `Send` nor `Sync`; it runs on your event
  loop (a tokio `LocalSet` with `tokio-transport`). Views are current when
  `mutate` returns.
- **Queries and mutators are written in Rust**, under the TypeScript names.
  Mutators are synchronous functions. CRUD mutators
  (`zero.mutate.issue.insert`) aren't ported: write a mutator.
- **Storage defaults to memory**, where JS uses IndexedDB.
- **Background:** the app calls `set_visible`. The client disconnects after
  `hidden_disconnect_delay_ms` hidden, 5 minutes as in JS; a mobile app the
  system suspends sets 0. To push pending mutations before hiding, the app
  awaits `pending_settled(timeout_ms)` first.
- **No UI bindings.** `LiveView` is the typed `useQuery`, with listeners a UI
  wakes itself from.
- **An unnamed query only sees local data**, as in JS without
  `enableLegacyQueries`: the server syncs a `define_query` request.

[PARITY.md](https://github.com/wevm/zero-rust/blob/main/PARITY.md) maps
each part of the JS API to its Rust name, and lists what isn't ported and
each documented difference.

## Compatibility

| zero-client | Sync protocol | `zero-cache` | Conformance vectors from |
| ----------- | ------------- | ------------ | ------------------------ |
| 0.1+        | 57            | 1.12+        | Zero 1.12                |

A Zero 1.11 `zero-cache` rejects protocol 57 with `VersionNotSupported`.

## Documentation

- [Zero's docs](https://zero.rocicorp.dev/docs/introduction), and for coding
  agents [llms.txt](https://zero.rocicorp.dev/llms.txt)
- [API reference](https://docs.rs/zero-client) on docs.rs
- [Coming from the JS client](https://github.com/wevm/zero-rust/blob/main/PARITY.md)
- [Benchmarks](https://github.com/wevm/zero-rust/blob/main/BENCHMARKS.md)
  against the JS and Swift clients, and
  [the design](https://github.com/wevm/zero-rust/blob/main/DESIGN.md)
- Example apps on one shared model
  ([`apps/zbugs-model`](https://github.com/wevm/zero/tree/main/apps/zbugs-model)):
  [`zbugs-rune`](https://github.com/wevm/zero/tree/main/apps/zbugs-rune) and
  [`zbugs-gpui`](https://github.com/wevm/zero/tree/main/apps/zbugs-gpui)

## Contributing

This directory is published from [wevm/zero](https://github.com/wevm/zero)
(`rust/`); send changes there. From `rust/` in that repository:

```sh
cargo test --workspace            # unit tests, and every replayed vector matches JS
cargo run -p zero-conformance     # the conformance report, file by file
```

## License

Apache-2.0
