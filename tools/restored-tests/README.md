# Restored upstream tests

rocicorp/mono stopped publishing the tests of zql, zqlite, zero-cache and some
other packages on 2026-09-30 and rewrote its history to remove them. This fork
restores the client-side ones from the last published copy, so they keep
checking the zql that the Swift client ports.

- **Source:** [rocicorp/mono](https://github.com/rocicorp/mono) commit
  `7ebeb2172fc9e621ecfc2f262c4d228b16e9684e` (2026-09-26, the last `main` before
  the rewrite). No ref points at it, but GitHub still serves it by SHA:
  `git fetch https://github.com/rocicorp/mono 7ebeb2172fc9e621ecfc2f262c4d228b16e9684e`.
- **License:** Apache License 2.0, as published by Rocicorp; see `LICENSE` at
  the repository root. The files carry no copyright headers of their own.
- **Where:** at their original paths. [`verbatim.txt`](verbatim.txt) lists the
  172 files restored unchanged, each with its git blob ID at that commit; the
  Restored Tests workflow (`.github/workflows/restored-tests.yml`) fails if one
  changes or goes missing.

## What runs

Counts are with the restored files at the state of this fork's `main` on
2026-11-13, in CI mode (`CI=true`, so the browser suites run in Chromium,
Firefox and WebKit).

| Package                 | Restored test files                        | Result                                                                    |
| ----------------------- | ------------------------------------------ | ------------------------------------------------------------------------- |
| `zql`                   | 72 of 96, and 3 helpers                    | 1,102 pass, 2 skipped upstream                                            |
| `zqlite`                | 13 of 16                                   | 164 pass                                                                  |
| `zero-protocol`         | 7 of 10, and 2 type tests                  | 135 pass (45 per browser)                                                 |
| `zero-schema`           | 4 of 4, and 1 type test                    | 23 pass                                                                   |
| `zero-types`            | 1 of 1                                     | 18 pass (6 per browser)                                                   |
| `zqlite-zql-test`       | the package (zql's suite on `TableSource`) | 2,012 pass, 4 skipped upstream (two modes)                                |
| `zql-integration-tests` | 26 of 37, and their helpers                | 75 pass without Postgres; with Postgres 17, 215 pass, 10 skipped upstream |

The zero-cache tests, and those of z2s, zero-server, ast-to-zql,
analyze-query, zero-permissions and zql-benchmarks, are server-side and not
restored. Nor are zql's planner design notes and `tool/bench-ivm-inmem.ts`.

## What doesn't, and why

Each of these fails against today's code because upstream changed it after
2026-09-26. They are left out, unchanged at the source commit; none points at
a zql bug. A file that fails `tsc` is left out too, since Public Checks type
checks every package.

A scratch run regenerated the snapshots of the 16 failing zql test files that
load (all but `connection-index`). 17 failures were left: 7 in
`query-impl.ast` that compare an AST's keys, and 10 in `join-utils` and
`join.push` that call removed helpers. Every regenerated snapshot was an
operator trace, operator storage, a cache-hit count or an AST's shape; none
was a view's rows or the changes pushed to it.

| Files                                                                                                                                          | Why                                                                                                                                                                                                                                          |
| ---------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| zql `ivm/cap.push`, `ivm/take.fetch`, `ivm/take.push`                                                                                          | Cap and Take storage keys are `cap:s<value>` instead of a JSON array (fd30e6437, "cheaper single-column state and cache keys").                                                                                                              |
| zql `ivm/exists.fetch`, `ivm/yield.fetch`                                                                                                      | Exists reuses the children it counted, so the fetch traces lost a fetch per parent (ae794cc0e, 8702aea45).                                                                                                                                   |
| zql `ivm/flipped-join.push`, `ivm/flipped-join.more-fetch`, `ivm/join.fetch`                                                                   | The join parent index moved into memory: traces gained `:join-index` steps and storage lost the index (5c2a12e8d).                                                                                                                           |
| zql `ivm/join-utils`, `ivm/join.push`                                                                                                          | They test storage-key helpers that went with it (`splitPartitionAndPk`, `decodeCanonicalValue` and others; 5c2a12e8d, a9c313137). 54 of `join.push`'s 56 tests pass.                                                                         |
| zql `ivm/connection-index`                                                                                                                     | `connection-index.ts` is gone (b0c414b71).                                                                                                                                                                                                   |
| zql `query/complete-ordering`, `named`, `query-impl.ast`, `query-impl.interning`, `query-impl.query-kitchen-sink`, `test/query-gen`            | ASTs carry `aggregate` and `groupBy` keys (c9d1b15fc, 5493ba549).                                                                                                                                                                            |
| zql `ivm/array-view`, `flipped-join.chunked`, `flipped-join.fetch`, `flipped-join.sibling`, `join.sibling`, `predicate-pushdown`, `yield.push` | They pass, but fail `tsc`: they give `Join` and `FlippedJoin` a `storage` they no longer take (5c2a12e8d).                                                                                                                                   |
| zql `query/test/query-gen.ts`                                                                                                                  | A helper only `query-gen.test` used.                                                                                                                                                                                                         |
| zqlite `db`                                                                                                                                    | The slow-iterator log message and fields changed (959b12cf2, d93129a53, ef16dc985).                                                                                                                                                          |
| zqlite `query-builder`                                                                                                                         | `buildSelectQuery` refuses a fetch that needs a second SELECT for its NULL group, and says to use `buildSelectQueries` (34fcd8997).                                                                                                          |
| zqlite `table-source`                                                                                                                          | `TableSource` no longer checks that an added row is new or a removed one exists (b0c414b71), and reads rows in batches rather than through `Statement.iterate`, which one test patches (5a5fb1af1). 48 of 52 pass.                           |
| zero-protocol `ast`, `protocol-version`                                                                                                        | The AST schema and its hash changed with aggregates, groupBy and JSON paths (c9d1b15fc and others).                                                                                                                                          |
| zero-protocol `query-hash-visitor`                                                                                                             | Fails `tsc` against the new AST type.                                                                                                                                                                                                        |
| zql-integration-tests `collate.pg`, `compiler-bigint.pg`, `compiler-types-with-args.pg`                                                        | Fail `tsc`: `fromSQLiteTypes` takes a `SQLiteRow` (9e0e85b62).                                                                                                                                                                               |
| zql-integration-tests `chinook/chinook-zero-cache-fuzzer*` (6 tests, 2 helpers)                                                                | zero-cache fuzzers; the helper fails `tsc` against today's zero-cache options (`pgChangeLogEnabled` is gone).                                                                                                                                |
| zql-integration-tests `chinook/planner-exec.pg`, `pagila/planner-exec.pg`                                                                      | Server planner cost validation: 4 of 26 and 4 of 18 fail on the cost model's estimates. Their helpers and the pagila schema go with them, as do `helpers/correlation.ts` and `helpers/data-gen.ts`, which only they and `query-gen.ts` used. |

`zqlite-zql-test` runs zql's restored suite on zqlite's `TableSource`. In both
of its modes, `ivm/source.test.ts > push` fails, because it expects a
duplicate add to throw, which `TableSource` no longer checks (b0c414b71).
`MemorySource`, which the client uses, still throws. That file is excluded
there and still runs in zql's own suite.

### Postgres

The `*.pg.test.ts` files run against Postgres 17 (testcontainers in CI). With
`TEST_PG_17` pointing at a local Postgres 17 set up like the container
(`wal_level=logical`, `timezone=UTC+1`), every kept file passes; the skipped
tests run only with `CHINOOK_SCALE`, `PROFILE` or the extended-push lane's
switch set. One harness
bug: `helpers/setup.ts` names every replica `collate_test`, so two files that
use it at once fight over one replication slot. With `collate.pg` left out,
only `text-semantics.pg` uses it.

Integration tests without Postgres run one file at a time in CI. The chinook
fuzzer's `decorated push memory parity (n=2)` takes about 55 to 57 s of its
60 s budget even alone, and it ran out of time when files ran in parallel.

## Changes to restored files

- `packages/zql-integration-tests/package.json` and
  `packages/zqlite-zql-test/package.json`: their dependencies are the
  specifiers `pnpm-lock.yaml` has for them. mono no longer publishes these
  manifests, but its lockfile still lists both packages, as of today's
  versions, so `pnpm install --frozen-lockfile` needs them to match.
- `packages/zqlite-zql-test/vitest.config.write-through.ts` and
  `vitest.config.deferred-writes.ts`: exclude `../zql/src/ivm/source.test.ts`
  (see above), with a comment saying so.

## Conformance vectors

`packages/zero-conformance` turns two of these into vectors that the Swift
client replays (`swift/Tests/ConformanceTests/RestoredIVMTests.swift`):

- `src/ivm/restored-ivm.test.ts` runs the restored zql tests that build a
  pipeline from an AST, with their own assertions. It records each case they
  pass to `runPushTest` or `runFetchTest` and writes `vectors/restored-ivm.json`:
  the view and its changes after hydration and after each push.
- `src/ivm/restored-chinook.test.ts` builds the chinook fuzzer's decorated
  push lane from its modules into `generated/restored-chinook-decpush.json`,
  which isn't checked in. Pull requests run the one-row lane; the nightly adds
  the two-row lane.

## Upstream syncs

The weekly sync merges rocicorp/mono into this fork. mono's history after the
rewrite never had these paths, so the merge leaves them alone unless upstream
adds a file at one of them. Two things can still need a hand after a merge:

1. **The lockfile.** When mono bumps a dependency of `zql-integration-tests`
   or `zqlite-zql-test`, the merge changes those packages' lockfile entries,
   and `pnpm install --frozen-lockfile` fails until the manifests follow. Run
   `node tools/restored-tests/align-manifests.mjs`, which sets their
   dependencies to the lockfile's, and commit the result as its own commit.
   It doesn't touch the lockfile.
2. **A restored test that breaks.** Don't change zql to fix it, and don't edit
   the restored file. Move it to the table above with the upstream commit that
   broke it, remove it from `verbatim.txt`, and delete it. If
   `restored-ivm.test.ts` imports it, remove it from that list as well, and
   regenerate the vectors.

To restore a file again, fetch the commit above and run
`git show 7ebeb2172fc9e621ecfc2f262c4d228b16e9684e:<path> > <path>`.
