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
  174 files restored unchanged, each with its git blob ID at that commit; the
  Restored Tests workflow (`.github/workflows/restored-tests.yml`) fails if one
  changes or goes missing. Ten more zql tests run as
  [re-baselined copies](#re-baselined-copies) beside where the originals were.

## What runs

Counts are with the restored files at the state of this fork's `main` on
2026-11-13, in CI mode (`CI=true`, so the browser suites run in Chromium,
Firefox and WebKit).

| Package                 | Restored test files                                     | Result                                                                    |
| ----------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------- |
| `zql`                   | 72 of 96, 10 more as re-baselined copies, and 4 helpers | 1,313 pass, 2 skipped upstream                                            |
| `zqlite`                | 13 of 16                                                | 164 pass                                                                  |
| `zero-protocol`         | 7 of 10, and 2 type tests                               | 135 pass (45 per browser)                                                 |
| `zero-schema`           | 4 of 4, and 1 type test                                 | 23 pass                                                                   |
| `zero-types`            | 1 of 1                                                  | 18 pass (6 per browser)                                                   |
| `zqlite-zql-test`       | the package (zql's suite on `TableSource`)              | 2,434 pass, 4 skipped upstream (two modes)                                |
| `zql-integration-tests` | 26 of 37, and their helpers                             | 75 pass without Postgres; with Postgres 17, 215 pass, 10 skipped upstream |

The zero-cache tests, and those of z2s, zero-server, ast-to-zql,
analyze-query, zero-permissions and zql-benchmarks, are server-side and not
restored. Nor are zql's planner design notes and `tool/bench-ivm-inmem.ts`.

## What doesn't, and why

Each of these fails against today's code because upstream changed it after
2026-09-26. They are left out, unchanged at the source commit; none points at
a zql bug. A file that fails `tsc` is left out too, since Public Checks type
checks every package.

Ten zql files failed only on snapshots of internals and pass `tsc`; they run
as [re-baselined copies](#re-baselined-copies) and aren't listed here.

| Files                                                                                                                                          | Why                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| zql `ivm/exists.fetch`, `ivm/yield.fetch`                                                                                                      | Exists reuses the children it counted, so the fetch traces lost a fetch per parent (ae794cc0e, 8702aea45). Only snapshots fail, but both also fail `tsc`: they give `Join` a `storage` it no longer takes (5c2a12e8d). |
| zql `ivm/join.fetch`                                                                                                                           | The join parent index moved into memory: traces gained `:join-index` steps and storage lost the index (5c2a12e8d). Only snapshots fail, but it also fails `tsc`, like `exists.fetch`.                                  |
| zql `ivm/join-utils`, `ivm/join.push`                                                                                                          | They test storage-key helpers that went with it (`splitPartitionAndPk`, `decodeCanonicalValue` and others; 5c2a12e8d, a9c313137). 54 of `join.push`'s 56 tests pass.                                                   |
| zql `ivm/connection-index`                                                                                                                     | `connection-index.ts` is gone (b0c414b71).                                                                                                                                                                             |
| zql `query/query-impl.ast`                                                                                                                     | Seven assertions compare an AST's keys, which now include `aggregate` and `groupBy` (c9d1b15fc, effd2da27).                                                                                                            |
| zql `ivm/array-view`, `flipped-join.chunked`, `flipped-join.fetch`, `flipped-join.sibling`, `join.sibling`, `predicate-pushdown`, `yield.push` | They pass, but fail `tsc`: they give `Join` and `FlippedJoin` a `storage` they no longer take (5c2a12e8d).                                                                                                             |
| zqlite `db`                                                                                                                                    | The slow-iterator log message and fields changed (959b12cf2, d93129a53, ef16dc985).                                                                                                                                    |
| zqlite `query-builder`                                                                                                                         | `buildSelectQuery` refuses a fetch that needs a second SELECT for its NULL group, and says to use `buildSelectQueries` (34fcd8997).                                                                                    |
| zqlite `table-source`                                                                                                                          | `TableSource` no longer checks that an added row is new or a removed one exists (b0c414b71), and reads rows in batches rather than through `Statement.iterate`, which one test patches (5a5fb1af1). 48 of 52 pass.     |
| zero-protocol `ast`, `protocol-version`                                                                                                        | The AST schema and its hash changed with aggregates, groupBy and JSON paths (c9d1b15fc and others).                                                                                                                    |
| zero-protocol `query-hash-visitor`                                                                                                             | Fails `tsc` against the new AST type.                                                                                                                                                                                  |
| zql-integration-tests `collate.pg`, `compiler-bigint.pg`, `compiler-types-with-args.pg`                                                        | Fail `tsc`: `fromSQLiteTypes` takes a `SQLiteRow` (9e0e85b62).                                                                                                                                                         |
| zql-integration-tests `chinook/chinook-zero-cache-fuzzer*` (6 tests, 2 helpers)                                                                | zero-cache fuzzers; the helper fails `tsc` against today's zero-cache options (`pgChangeLogEnabled` is gone).                                                                                                          |
| zql-integration-tests `chinook/planner-exec.pg`, `pagila/planner-exec.pg`                                                                      | Server planner cost validation: 4 of 26 and 4 of 18 fail on the cost model's estimates. Their helpers and the pagila schema go with them, as does `helpers/correlation.ts`, which only they used.                      |

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

The chinook fuzzer's `decorated push memory parity (n=2)` has a 60 s budget. It
takes 18 s on GitHub's runner, but 55 to 57 s alone on a slower machine (an
Amp orb), where it ran out of time with the other files running beside it.

## Changes to restored files

- `packages/zql-integration-tests/package.json` and
  `packages/zqlite-zql-test/package.json`: their dependencies are the
  specifiers `pnpm-lock.yaml` has for them. mono no longer publishes these
  manifests, but its lockfile still lists both packages, as of today's
  versions, so `pnpm install --frozen-lockfile` needs them to match.
- `packages/zqlite-zql-test/vitest.config.write-through.ts` and
  `vitest.config.deferred-writes.ts`: exclude `../zql/src/ivm/source.test.ts`
  (see above), with a comment saying so.

## Re-baselined copies

These zql tests fail against today's code only on inline snapshots of
internals: operator traces, operator storage and ASTs. Every assertion on a
view's rows, on the changes pushed to it, or on anything else passes as
published. The originals stay out, as above. Each runs instead as a copy
named `<original>.rebaselined.test.ts`, in the original's directory, so zql's
suite and `zqlite-zql-test` both pick it up. A copy is the original as
published, below a header naming the original, the source commit and the
upstream commits in this table, with those snapshots regenerated. No file in
`verbatim.txt` is edited.

| Copy of (in `packages/zql/src/`)      | Snapshots changed         | Upstream commits that changed them                                                                     |
| ------------------------------------- | ------------------------- | ------------------------------------------------------------------------------------------------------ |
| `ivm/cap.push`                        | 19 of 41: storage, trace  | fd30e6437 (Cap storage keys are `cap:s<value>`), 8702aea45 (Cap's fetch passes `countOnly`)            |
| `ivm/take.fetch`                      | 9 of 63: storage          | fd30e6437 (Take storage keys are `take:s<value>`)                                                      |
| `ivm/take.push`                       | 17 of 197: storage        | fd30e6437                                                                                              |
| `ivm/flipped-join.push`               | 39 of 173: trace, storage | 5c2a12e8d (traces gain `:join-index` steps; the in-memory join index holds only parents in the result) |
| `ivm/flipped-join.more-fetch`         | 1 of 3: trace             | 5c2a12e8d                                                                                              |
| `query/complete-ordering`             | 14 of 14: AST             | c9d1b15fc and effd2da27 (ASTs carry `aggregate` and `groupBy` keys)                                    |
| `query/named`                         | 1 of 3: AST               | c9d1b15fc, effd2da27                                                                                   |
| `query/query-impl.interning`          | 1 of 2: AST               | c9d1b15fc, effd2da27                                                                                   |
| `query/query-impl.query-kitchen-sink` | 1 of 2: AST               | c9d1b15fc, effd2da27                                                                                   |
| `query/test/query-gen`                | 1 of 1: AST               | c9d1b15fc, effd2da27                                                                                   |

The copies hold 28,553 lines: 78 of headers, 1,437 the re-baseline added (it
removed 91), and 27,038 as published. They duplicate only mono's published
files: the originals aren't in this repository, and
`git show 7ebeb2172fc9e621ecfc2f262c4d228b16e9684e:<path>` prints one.
`flipped-join.push` and `flipped-join.more-fetch` have NUL bytes in their
storage-key snapshots, as published, so git diffs them as binary;
`git diff --text` shows them.

To re-baseline a copy again, run `pnpm exec vitest run --update=all <copy>` in
`packages/zql`, put each changed snapshot back at its old indentation (vitest
ignores it), and check that only `toMatchInlineSnapshot` bodies of traces,
storage or ASTs changed. Then add the upstream commit to the copy's header
and to this table, in a commit of its own. A copy that fails anywhere else
has found a behavior change: treat it like any restored test that breaks.

## Conformance vectors

`packages/zero-conformance` turns two of these into vectors that the Swift
client (`swift/Tests/ConformanceTests/RestoredIVMTests.swift`) and the Rust
client (`restored` in `rust/conformance/src/suites/ivm.rs`) replay:

- `src/ivm/restored-ivm.test.ts` runs the restored zql tests that build a
  pipeline from an AST, with their own assertions. It records each case they
  pass to `runPushTest` or `runFetchTest` and writes `vectors/restored-ivm.json`:
  the view and its changes after hydration and after each push. It runs the
  re-baselined copies `cap.push`, `flipped-join.push`,
  `flipped-join.more-fetch` and `take.push` too, and writes their 124 cases
  to `vectors/restored-ivm-rebaselined.json`, leaving out any case
  `restored-ivm.json` already has. Those vectors record views and changes,
  not the snapshots of internals, so re-baselining a copy again leaves them
  alone.
- `src/ivm/restored-chinook.test.ts` builds the chinook fuzzer's decorated
  push lane from its modules into `generated/restored-chinook-decpush.json`,
  which isn't checked in. Pull requests run the one-row lane; the nightly adds
  the two-row lane.

## Upstream syncs

The weekly sync merges rocicorp/mono into this fork. mono's history after the
rewrite never had these paths, nor the copies' `*.rebaselined.test.ts` ones, so
the merge leaves them alone unless upstream adds a file at one of them. Three
things can still need a hand after a merge:

1. **The lockfile.** When mono bumps a dependency of `zql-integration-tests`
   or `zqlite-zql-test`, the merge changes those packages' lockfile entries,
   and `pnpm install --frozen-lockfile` fails until the manifests follow. Run
   `node tools/restored-tests/align-manifests.mjs`, which sets their
   dependencies to the lockfile's, and commit the result as its own commit.
   It doesn't touch the lockfile.
2. **A restored test that breaks.** The sync reports the failing file and the
   upstream commit that broke it, and changes neither zql nor the test: the
   break may be a behavior change Swift has to port, which deleting the test
   would hide. Once that's settled, and only with tmm's OK, a follow-up moves
   it to the table above, removes it from `verbatim.txt` and deletes it, or,
   if it fails only on snapshots of internals, replaces it with a re-baselined
   copy. If `restored-ivm.test.ts` imports it, it comes out of that list as
   well, and the vectors are regenerated.
3. **A re-baselined copy that goes stale.** When upstream changes a trace,
   operator storage or the AST's shape again, a copy's snapshots fail as the
   originals' did: Restored Tests goes red on the sync PR, and the sync
   reports it as in 2. Re-baselining it again, as above, needs tmm's OK.
   Snapshots of internals break more often than assertions on data: five
   upstream commits between 2026-09-26 and 2026-11-13 broke these ten files.

To restore a file again, fetch the commit above and run
`git show 7ebeb2172fc9e621ecfc2f262c4d228b16e9684e:<path> > <path>`.
