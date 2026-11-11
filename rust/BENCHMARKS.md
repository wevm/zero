# Benchmarks

`benchmarks` (the `zero-benchmarks` crate, not published) has ports of the
JS client's benchmarks under the same names, and of the Swift client's own
(`swift/Sources/ZeroBenchmarks`, see `swift/BENCHMARKS.md`), so the three
clients compare on the same data:

```sh
cd rust
cargo run --release -p zero-benchmarks                 # everything, about two minutes
cargo run --release -p zero-benchmarks -- hydration "pokes > put 100" --time 3 --json out.json
```

A filter is a substring of `group > benchmark`. Each benchmark runs for
`--time` seconds (default 1) after a warm-up and reports the median time per
call with the 75th and 99th percentiles and the fastest, as
`packages/shared/src/bench.ts` does. Every benchmark checks that it computed
the right rows, and exits non-zero when one didn't.

The JS sides:

| Rust group                            | JS                                                                                |
| ------------------------------------- | --------------------------------------------------------------------------------- |
| `zero.bench.ts`                       | `pnpm --filter zero-client bench`                                                 |
| `hash.bench.ts`                       | `pnpm --filter shared bench --project shared/bench/node src/hash.bench.ts`        |
| `memory`, `hydration`, `mutations`    | `pnpm --filter zero-conformance bench` (`src/bench/client.bench.ts`)              |
| `pokes`, `first sync`, the round trip | none: the JS client applies pokes on the next frame, so a JS timing measures that |

Two of Swift's aren't ported, because the Rust client has nothing to time:
`btree-set.bench.ts` (memory sources keep sorted `Vec`s, not a B-tree set)
and the optimistic update with a live query of structs (there is no typed
live query; an app reads a view's rows as structs, which `hydration` times).

The benchmarks don't run in CI; numbers from shared runners vary too much to
gate on. CI builds them with the workspace (`cargo clippy --all-targets`,
`cargo test`), so they keep compiling.

## Results

Medians; lower is better. Rust and JS ran one after the other on one
machine: an 8-core Intel Xeon at 2.6 GHz with 16 GB, Linux 6.1, Rust 1.99.1
(release build) and Node 24.22 under vitest. The Swift column is
`swift/BENCHMARKS.md`'s Linux column, from the same kind of machine but not
the same run, with Swift 6.2.4; its cells marked † are from before Swift's
last performance pass.

In short: Rust is 2–5× faster than JS on views of up to 10,000 rows, 15×
on an optimistic update, and 20–40× on hashing (where vitest flatters
Rust), and at or ahead of Swift on small views and single-row changes. It
is slower than JS at materializing 100,000 rows, behind Swift on big
hydrations and big pokes, and uses about twice JS's memory, because a row
is copied between the replica and the sources (see What they found).

| Benchmark                                  | JS (Node)                   | Rust                     | Swift (Linux)  |
| ------------------------------------------ | --------------------------- | ------------------------ | -------------- |
| zero.bench: all 1000 rows x 10 columns     | 236 µs                      | 56.5 µs                  | 70.4 µs        |
| zero.bench: pk = N                         | 11.0 µs                     | 4.56 µs                  | 8.39 µs        |
| zero.bench: lower 500 rows                 | 152 µs                      | 58.7 µs                  | 89.7 µs        |
| materialize 1,000 rows                     | 242 µs                      | 49.8 µs                  | 63.2 µs        |
| materialize 10,000 rows                    | 2.15 ms                     | 887 µs                   | 757 µs         |
| materialize 100,000 rows                   | 29.7 ms                     | 32.2 ms                  | 7.07 ms        |
| memory for 100,000 rows and a view of them | 245 MB                      | 467 MB                   | 128 MB†        |
| optimistic update with a 1000-row view     | 274 µs                      | 18.2 µs                  | 23 µs†         |
| h32, 64 / 1024 / 8192 chars                | 433 ns / 3.87 µs / 32.3 µs  | 10 ns / 151 ns / 1.21 µs |                |
| h64, 64 / 1024 / 8192 chars                | 703 ns / 6.37 µs / 51.5 µs  | 23 ns / 303 ns / 2.42 µs | – / 318 ns / – |
| h128, 64 / 1024 / 8192 chars               | 1.29 µs / 12.5 µs / 95.3 µs | 42 ns / 606 ns / 5.60 µs |                |

As `hash.bench.ts` notes, vitest keeps imported functions from being
inlined, so its JS hash numbers are pessimistic; bundled, they are a few
times faster. The JS memory figure is the change in resident memory, as
Rust's is (`VmRSS`), from one run.

Rust and Swift only:

| Benchmark                                                 | Rust    | Swift (Linux) |
| --------------------------------------------------------- | ------- | ------------- |
| materialize 1,000 rows as structs                         | 171 µs  | 182 µs        |
| materialize 10,000 rows as structs                        | 4.59 ms | 1.94 ms       |
| materialize 100,000 rows as structs                       | 72.5 ms | 24.8 ms       |
| poke putting 1 row, 1000-row view                         | 15.9 µs | 29 µs†        |
| poke putting 100 rows, 1000-row view                      | 1.09 ms | 1.00 ms†      |
| poke putting 1000 rows, 1000-row view                     | 15.0 ms | 9.82 ms†      |
| round trip (update, push, confirming poke), 1000-row view | 43.1 µs | 80 µs†        |
| first sync: zbugs poke of 2,058 rows, issue list view     | 25.7 ms |               |
| first sync: the same poke, no view                        | 17.9 ms |               |

`first sync` replays zbugs' first sync as the zbugs-swift app got it, with
the rows Swift's benchmark generates from the same seed: one poke of 21
parts with 516 issues, 894 comments, 603 issue labels, 30 labels, 14 users,
and a project, into a fresh client with the issue list's first page of
`issueListV2` (or no view, for the cost of the poke itself). Each sample is
timed from the poke's frames arriving to the view being flushed.

## What they found

- **Every write copied the replica.** The replica's map was an
  `Rc<BTreeMap>` shared by its snapshots, so the first write to a copy
  cloned every row and dropping the old copy freed them: an optimistic
  update, a poke, and a rebase each cost O(rows), and 80% of an
  optimistic update went to that copy and its drop. It is now a persistent
  B+tree like Replicache's, whose writes copy only the nodes on their
  path, with each entry behind an `Rc`. With 1000 rows, an optimistic
  update went from 1.17 ms to 25 µs, a poke of one row from 1.15 ms to
  21 µs, and a round trip from 2.28 ms to 52 µs.
- **Rows hashed their column names.** A JSON object kept a SipHash index
  of its keys once it had more than 8, so every column of a 10-column row
  was found by hashing, and each row kept its keys twice. Objects of up to
  16 keys now scan them: all 1000 rows went from 106 to 57 µs,
  materializing 100,000 rows from 64 to 31 ms, a poke of 1000 rows from 28
  to 15 ms, and the memory of 100,000 rows from 735 to 467 MB.

Where Rust is still behind:

- **Rows are copied, not shared.** A poke's row is cloned from the parsed
  message into the patch, the replica, the diff, and the sources (five
  places, about 3% of a first sync each), and the replica and the sources
  each keep their own copy, which is most of the memory gap with JS, where
  one object is shared. Sharing a row's object between them (an `Rc` in
  `json::Value`) is the next step.
- **Big views.** Materializing 100,000 rows takes about 3.6 times as long
  per row as 10,000 do (JS's 1.4 times; Swift's scales linearly), and reading them
  as structs is about 3 times Swift's. Swift keys a row's columns by index
  into its table's layout (see `swift/BENCHMARKS.md`); Rust rows are still
  JSON objects keyed by name.
