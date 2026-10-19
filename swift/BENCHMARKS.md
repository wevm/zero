# Benchmarks

`Sources/ZeroBenchmarks` has ports of the JS client's benchmarks, under the
same names, and benchmarks of the Swift client's own:

```sh
cd swift
swift run -c release ZeroBenchmarks                 # everything, about a minute
swift run -c release ZeroBenchmarks hydration "pokes > put 100" --time 3 --json out.json
```

A filter is a substring of `group > benchmark`. Each benchmark runs for
`--time` seconds (default 1) after a warm-up and reports the median time per
call with the 75th and 99th percentiles, as `packages/shared/src/bench.ts`
does.

The JS sides:

| Swift group                           | JS                                                                                          |
| ------------------------------------- | ------------------------------------------------------------------------------------------- |
| `zero.bench.ts`                       | `pnpm --filter zero-client bench`                                                           |
| `btree-set.bench.ts`, `hash.bench.ts` | `pnpm --filter shared bench src/btree-set.bench.ts src/hash.bench.ts`                       |
| `memory`, `hydration`, `mutations`    | `pnpm --filter zero-conformance bench` (`src/bench/client.bench.ts`)                        |
| `pokes`, the round trip               | none: the JS client applies pokes on the next frame, so a JS timing measures the scheduling |

The benchmarks don't run in CI; numbers from shared runners vary too much to
gate on. CI builds them, so they keep compiling.

## Results

Linux, one machine (8-core Intel Xeon at 2.6 GHz), Swift 6.2.4 in release
mode, Node 22.23. Medians; lower is better.

### The client

| Benchmark                                  | JS      | Swift   |
| ------------------------------------------ | ------- | ------- |
| zero.bench: all 1000 rows x 10 columns     | 263 µs  | 197 µs  |
| zero.bench: pk = N                         | 7.72 µs | 8.11 µs |
| zero.bench: lower 500 rows                 | 261 µs  | 206 µs  |
| materialize 1,000 rows                     | 282 µs  | 111 µs  |
| materialize 10,000 rows                    | 2.70 ms | 1.21 ms |
| materialize 100,000 rows                   | 43.2 ms | 11.5 ms |
| memory for 100,000 rows and a view of them | 164 MB  | 128 MB  |
| optimistic update with a 1000-row view     | 178 µs  | 23 µs   |

Swift only:

| Benchmark                                                 | Swift   |
| --------------------------------------------------------- | ------- |
| materialize 1,000 rows as structs                         | 256 µs  |
| materialize 10,000 rows as structs                        | 2.86 ms |
| materialize 100,000 rows as structs                       | 29.7 ms |
| optimistic update with a 1000-row live query of structs   | 91 µs   |
| poke putting 1 row, 1000-row view                         | 29 µs   |
| poke putting 100 rows, 1000-row view                      | 1.00 ms |
| poke putting 1000 rows, 1000-row view                     | 9.82 ms |
| round trip (update, push, confirming poke), 1000-row view | 80 µs   |

JS views hand out their rows; Swift apps read them as structs, which costs
about two and a half times the materialization, and less than JS takes to
materialize the rows alone. A live query reads only the rows that changed
after the first read.

### Data structures

| Benchmark                              | JS                          | Swift                    |
| -------------------------------------- | --------------------------- | ------------------------ |
| BTreeSet values() full scan (1000)     | 3.60 µs                     | 2.71 µs                  |
| BTreeSet valuesFrom() from mid         | 2.35 µs                     | 1.45 µs                  |
| BTreeSet valuesReversed() full scan    | 4.29 µs                     | 0.99 µs                  |
| BTreeSet valuesFromReversed() from mid | 2.55 µs                     | 0.60 µs                  |
| BTreeSet has() hit (x3)                | 67 ns                       | 139 ns                   |
| BTreeSet has() miss (x3)               | 63 ns                       | 61 ns                    |
| BTreeSet get() hit (x3)                | 70 ns                       | 129 ns                   |
| BTreeSet add() then delete()           | 109 ns                      | 149 ns                   |
| BTreeSet add() 100 sequential keys     | 6.29 µs                     | 4.34 µs                  |
| BTreeSet add() 1000 sequential keys    | 63.6 µs                     | 64.3 µs                  |
| BTreeSet sort, then add() 1000 keys    | 104 µs                      | 38 µs                    |
| h32, 64 / 1024 / 8192 chars            | 290 ns / 2.78 µs / 23.7 µs  | 23 ns / 168 ns / 1.26 µs |
| h64, 64 / 1024 / 8192 chars            | 552 ns / 5.46 µs / 41.3 µs  | 35 ns / 318 ns / 2.45 µs |
| h128, 64 / 1024 / 8192 chars           | 1.02 µs / 10.2 µs / 82.0 µs | 54 ns / 628 ns / 5.0 µs  |

As `hash.bench.ts` notes, vitest keeps imported functions from being
inlined, so its JS hash numbers are pessimistic; bundled, they are a few
times faster.

## What they found

- The mutation tracker looked mutations up by scanning a list, so each
  mutation cost more than the last while mutations were pending (229 µs on
  average over a one-second run, now 21 µs).
- A live query read every row again on every change (now only the changed
  ones: 92 µs instead of about 640 µs for 1000 rows).
- B-tree nodes paid for dynamic exclusivity checks on every read, and the
  forward iterator read its leaf's keys through the node for every element
  (a full scan of 1000 took 23 µs, now 2.7 µs). Views and sources iterate
  B-trees, so materializing got faster too.
- Hydrating a view took its row list out of the view and put it back for
  every row.
- Comparing strings in UTF-8 order walked them with iterators rather than
  their bytes.
- A row was a `[String: JSONValue]`, so reading a column hashed its name:
  comparing rows by key and reading rows as structs mostly hashed. Rows are
  now keyed by column index (see DESIGN.md). Reading 100,000 rows as structs
  went from 86 to 30 ms, and materializing them from 23 to 12 ms. The cost
  is memory: a source's row no longer shares its storage with the local
  store's copy, which adds about 30 MB per 100,000 rows of 10 columns.
- B-tree lookups retained each node they passed (a lookup took about
  64 ns, now 43 ns).

Where Swift is still slower: B-tree lookups, which call their comparator
through a closure that V8 inlines, and retain each node's keys as they read
them.
