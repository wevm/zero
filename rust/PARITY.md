# Parity with the JS client

What the Rust client (`zero-client`) has ported from the JS client, how it is
checked, and where it deliberately differs. The conformance runner
(`cargo run -p zero-conformance`) prints the live numbers; the table below is
updated with each milestone.

## Ported

| Area                     | JS source                                      | Rust             | Vectors                                                                                           |
| ------------------------ | ---------------------------------------------- | ---------------- | ------------------------------------------------------------------------------------------------- |
| JSON parse and stringify | `JSON.parse`, `JSON.stringify`, property order | `json`           | `json-parse`, `json-stringify-strings`, `sweep-json`, `harden-json`, `generated/harden-json-fuzz` |
| JS numbers               | `Number#toString`, `Number(string)`            | `json::number`   | `number-format`, `number-parse`                                                                   |
| String orders            | `compareUTF8`, JS `<`                          | `strings`        | `string-compare`                                                                                  |
| Hashes                   | `packages/shared/src/hash.ts`                  | `hash::xxhash32` | `hash`                                                                                            |
| Value comparison         | `packages/zql/src/ivm/data.ts`                 | `compare`        | `compare-values`                                                                                  |
| Row keys                 | `packages/zero-client/src/client/keys.ts`      | `keys`           | `primary-key-string`, `aggregate-row-key`                                                         |
| `LIKE` and `ILIKE`       | `packages/zql/src/builder/like.ts`             | `like`           | `like`                                                                                            |
| TTL                      | `packages/zql/src/query/ttl.ts`                | `ttl`            | `ttl`                                                                                             |

## Not ported yet

The query builder (`sweep-query-hash`), wire messages and poke chunks, IVM, the client engine, persistence. The runner lists their vector
files as not implemented.

`ast-features` checks that the wire schema rejects what JS rejects, but not
valita's error messages. Its name mapping uses the JS test schema as JS wrote
it into `ivm.json`'s shared fixtures.

## Documented differences

Each is counted separately in the conformance report, never as an exact
match.

| Difference     | Why                                                                                                                                                                                                                                                                                                                  |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lone surrogate | A Rust `String` can't hold a lone surrogate, which a JS string can. The parser reads an escaped one (`"\ud800"`) as U+FFFD, as the Swift client does; such a case matches when JS's output, with each such escape read as U+FFFD, is what Rust writes. Real servers send valid UTF-8, so this only shows in fuzzing. |
| Nesting        | JSON nested deeper than `json::MAX_DEPTH` (1,500) is rejected. Dropping, comparing, and writing a value recurse, and a debug build overflows a 2 MB thread stack by 3,000 levels. JS has no limit short of its own stack (zero-cache writes about 4,100 levels). The Swift client has the same limit.                |
