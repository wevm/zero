# Parity with the JS client

What the Rust client (`zero-client`) has ported from the JS client, how it is
checked, and where it deliberately differs. The conformance runner
(`cargo run -p zero-conformance`) prints the live numbers; the table below is
updated with each milestone.

## Ported

| Area                            | JS source                                                                                                                                                                                | Rust                                           | Vectors                                                                                                                                                   |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| JSON parse and stringify        | `JSON.parse`, `JSON.stringify`, property order                                                                                                                                           | `json`                                         | `json-parse`, `json-stringify-strings`, `sweep-json`, `harden-json`, `generated/harden-json-fuzz`                                                         |
| JS numbers                      | `Number#toString`, `Number(string)`                                                                                                                                                      | `json::number`                                 | `number-format`, `number-parse`                                                                                                                           |
| String orders                   | `compareUTF8`, JS `<`                                                                                                                                                                    | `strings`                                      | `string-compare`                                                                                                                                          |
| Hashes                          | `packages/shared/src/hash.ts`                                                                                                                                                            | `hash::xxhash32`                               | `hash`                                                                                                                                                    |
| Value comparison                | `packages/zql/src/ivm/data.ts`                                                                                                                                                           | `compare`                                      | `compare-values`                                                                                                                                          |
| Row keys                        | `packages/zero-client/src/client/keys.ts`                                                                                                                                                | `keys`                                         | `primary-key-string`, `aggregate-row-key`                                                                                                                 |
| `LIKE` and `ILIKE`              | `packages/zql/src/builder/like.ts`                                                                                                                                                       | `like`                                         | `like`                                                                                                                                                    |
| TTL                             | `packages/zql/src/query/ttl.ts`                                                                                                                                                          | `ttl`                                          | `ttl`                                                                                                                                                     |
| AST, normalization, wire schema | `packages/zero-protocol/src/ast.ts`                                                                                                                                                      | `ast`                                          | `ast`, `ast-features`, `generated/ast-fuzz`                                                                                                               |
| Query hashes                    | `packages/zero-protocol/src/query-hash-visitor.ts`                                                                                                                                       | `hash::query`                                  | `ast`, `query-internals-hash`, `name-args-hash`, `sweep-name-args-hash`                                                                                   |
| Query builder                   | `packages/zql/src/query/query-impl.ts`, `expression.ts`                                                                                                                                  | `query`                                        | `sweep-query-hash`                                                                                                                                        |
| Name mapping                    | `packages/zero-types/src/name-mapper.ts`                                                                                                                                                 | `schema::NameMapper`                           | `ast-features`                                                                                                                                            |
| Wire messages                   | `packages/zero-protocol/src` (`up.ts`, `down.ts`)                                                                                                                                        | `protocol`                                     | `protocol-upstream`, `protocol-downstream`, `sweep-downstream`, `protocol-client-schema`, `generated/protocol-*-fuzz`, `generated/harden-downstream-text` |
| Binary poke chunks              | `decodePokeChunks`                                                                                                                                                                       | `protocol::poke_chunk`                         | `protocol-poke-chunks`, `generated/protocol-poke-chunks-fuzz`, `generated/harden-poke-bytes`                                                              |
| `Sec-WebSocket-Protocol`        | `encodeSecProtocols`, `decodeSecProtocols`                                                                                                                                               | `protocol::sec_protocols`                      | `protocol-sec-protocols`                                                                                                                                  |
| Connect URL                     | `createConnectionURL` in `zero.ts`                                                                                                                                                       | `protocol::ConnectionUrl`                      | a unit test against `URLSearchParams`; `tests/walking_skeleton.rs`                                                                                        |
| Client schema                   | `clientSchemaFrom`                                                                                                                                                                       | `schema::Schema::client_schema`                | `tests/walking_skeleton.rs` (zero-cache rejects a wrong one)                                                                                              |
| Protocol version                | `PROTOCOL_VERSION` (57)                                                                                                                                                                  | `protocol::PROTOCOL_VERSION`                   | `meta`                                                                                                                                                    |
| IVM operators and memory source | `packages/zql/src/ivm`                                                                                                                                                                   | `ivm`                                          | `ivm`, `ivm-features`, `bughunt-ivm`, `generated/ivm-fuzz`, `generated/ivm-features-fuzz`, `generated/sweep-ivm-*`, `generated/harden-ivm-*`              |
| Pipeline builder                | `packages/zql/src/builder/builder.ts`                                                                                                                                                    | `builder`                                      | the IVM vectors above                                                                                                                                     |
| Views and change application    | `array-view.ts`, `view-apply-change.ts`                                                                                                                                                  | `ivm::ArrayView`                               | the IVM vectors above                                                                                                                                     |
| Typed queries and codegen       | `packages/zql/src/query/query.ts`, `packages/zerogen`                                                                                                                                    | `typed`, `zero-rust codegen`                   | the query vectors through `AnyQuery`, which typed queries build (`rust/codegen-tests`)                                                                    |
| Typed CRUD in mutators          | `tx.mutate.<table>` (`crud.ts`)                                                                                                                                                          | `typed::TypedTableMutator`, generated `mutate` | `rust/codegen-tests/tests/typed.rs`; the CRUD itself is `client::Transaction`'s, which the client vectors check                                           |
| Client engine                   | `packages/zero-client/src/client` (`zero.ts`, `connection-manager.ts`, `query-manager.ts`, `zero-poke-handler.ts`, `mutation-tracker.ts`, `context.ts`, `ivm-branch.ts`, `crud-impl.ts`) | `client`, on memory storage                    | `client`, `client-harden-connection`, `client-harden-mutations`, `harden-queries`, `bughunt-client`                                                       |
| Synced aggregates and groups    | `packages/zql/src/builder/builder.ts`, `crud-impl.ts`                                                                                                                                    | `builder`, `client::Transaction`               | the client vectors above                                                                                                                                  |
| WebSocket and timers            | the browser `WebSocket`, `setTimeout`, microtasks                                                                                                                                        | `tokio_transport` (`tokio-transport` feature)  | `tests/tokio_transport.rs`; `tests/client_e2e.rs` against `zero-cache`                                                                                    |

## Not ported yet

Persistence: the client runs on memory storage only, keyed by database name
for the life of the process, like JS `kvStore: 'mem'`; the storage trait and
SQLite come with M5.

## How some vectors are compared

- `ast-features` checks that the wire schema rejects what JS rejects, but not
  valita's error messages. Its name mapping uses the JS test schema as JS
  wrote it into `ivm.json`'s shared fixtures.
- Protocol messages are compared as JSON values, not text: Rust re-encodes a
  message from its fields, in an order of its own (`sweep-downstream`
  compares text with keys sorted). The header encoder is checked by decoding
  what it writes, as Swift checks it, so its exact escaping isn't pinned by a
  vector.

- `sweep-query-hash` replays each program of builder calls and compares
  both the AST and the hash, and requires the query to be valid, since JS
  built every one without throwing. The builder's misuse messages are
  checked by `ivm-features`' builder errors (M2); none of the vectors reach
  `limit` or `orderBy` inside a junction relationship, so a unit test in
  `query.rs` pins those two messages.

- The client vectors (`client/harness.ts`) are scenarios: an app and a
  server take steps against one client, on a manual clock and mock sockets.
  Rust replays each on a `Zero` with `zero_client::testing`'s scheduler and
  socket factory and compares, after every step, what the app saw (views,
  listener calls, mutation and `run` settlements, connection states) and
  every message the server received, as Swift's `ClientConformanceTests`
  does.

- The IVM vectors (`ivm/harness.ts`) are programs of query builder calls.
  Rust replays each with its own builder, must build JS's AST and format or
  fail with JS's message, then runs JS's AST over memory sources, as Swift
  does: JS runs the AST with each ordering naming a column once, and Rust
  must give that answer to the AST as written. Each push (or, for
  `harden-ivm`, each batch) compares the changes the view received and the
  view itself; `harden-ivm` also compares whether listeners were notified,
  and tags numbers JSON can't carry. Errors are compared by message,
  including JS's internal assertions: Rust reports "Unexpected early return
  prevented full hydration" where JS's `Take` and `Cap` assert it, which the
  Swift client skips.

## Documented differences

Each is counted separately in the conformance report, never as an exact
match.

| Difference      | Why                                                                                                                                                                                                                                                                                                                         |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lone surrogate  | A Rust `String` can't hold a lone surrogate, which a JS string can. The parser reads an escaped one (`"\ud800"`) as U+FFFD, as the Swift client does; such a case matches when JS's output, with each such escape read as U+FFFD, is what Rust writes. Real servers send valid UTF-8, so this only shows in fuzzing.        |
| Nesting         | JSON nested deeper than `json::MAX_DEPTH` (1,500) is rejected. Dropping, comparing, and writing a value recurse, and a debug build overflows a 2 MB thread stack by 3,000 levels. JS has no limit short of its own stack (zero-cache writes about 4,100 levels). The Swift client has the same limit.                       |
| Unwritable path | A JSON path index is a `u32`, so a program with a negative or fractional index (`json('metadata', -1)`) can't be written in Rust. JS rejects it with "Invalid JSON path segment …"; such a case matches when JS rejects it that way. An index past `MAX_JSON_PATH_INDEX` can be written, and is rejected with JS's message. |
